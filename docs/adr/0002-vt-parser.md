# ADR 0002 — VT parser

**Status:** Accepted  **Spec:** §3.1, §18.5

## Options evaluated

| Option | What it provides | Verdict |
|---|---|---|
| **`vte` 0.15** (Alacritty) | DEC-compatible *tokenizer* state machine (Paul Williams' model). No state, no semantics. | **Chosen** |
| `alacritty_terminal` | Full emulator: grid, scrollback, selection, its own event loop and PTY. | Rejected: its grid and event model would *be* our Terminal State; exposing structured state, semantic zones and extension subscriptions our way would mean fighting or forking it. |
| `termwiz` (WezTerm) | Parser + surface model + much of WezTerm's ecosystem. | Rejected for now: heavy, API tuned to WezTerm's needs. Worth re-checking for its Sixel/Kitty-image decoding. |
| Write our own tokenizer | Full control. | Not now: `vte` is battle-tested; revisit only if the gaps below can't be closed cleanly. |

## Decision

Use `vte` purely for tokenization. Seraphim implements `vte::Perform` and
owns every semantic (`crates/seraphim-terminal/src/state.rs`): cursor and
wrap rules, scroll regions, modes, alternate screens, SGR, OSC handling.

## Gaps found during evaluation (must be addressed)

1. **APC strings are discarded.** `vte` enters `SosPmApcString` and drops
   the payload. The **Kitty graphics protocol is APC-based**
   (`ESC _ G ... ESC \`), so it is unreachable through `vte` as-is.
   Plan (Phase II): a small pre-filter in front of `vte` that extracts
   `ESC _ … ST` sequences and routes them to the graphics decoder, or
   upstream an APC callback to `vte`. Decide when the graphics renderer
   starts; both are small.
2. **Sixel** arrives as DCS via `hook/put/unhook` — supported by `vte`,
   not yet handled by us.
3. **OSC payload size.** With `std`, `vte`'s OSC buffer grows without
   limit. **Resolved:** `src/prefilter.rs` sits in front of vte, caps OSC
   at 2 MiB (configurable) by sending CAN and discarding the rest. The
   APC interception for (1) belongs in the same pre-filter.
5. **Omitted vs zero parameters.** vte reports `CSI T` and `CSI 0 T`
   identically; xterm distinguishes them for SD only. Accepted as a known
   difference (see `docs/conformance.md`).
4. At most 16 OSC parameters and 2 intermediates — fine for every
   sequence we implement.

## Verification so far

* 36 unit tests over cursor/erase/scroll-region/origin/alt-screen/SGR
  (both `;` and `:` forms)/wide chars/combining/tabs/DEC graphics/OSC
  7, 8, 52, 133/DSR/DA/resize/damage, plus a pseudo-random fuzz smoke test.
* End-to-end tests drive real `vim`, `less`, `tmux`, `nano` and an
  interactive `bash` through a PTY.

Since then: 427/427 screens identical to recorded xterm output, vttest
34/35 (the exception is the deliberately unsupported 132-column mode), and
an invariant-checking fuzzer — see `docs/conformance.md`.
