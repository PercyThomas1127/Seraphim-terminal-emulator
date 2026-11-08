# Conformance testing

Seraphim's Terminal Core is checked against **xterm**, the reference
implementation of the VT/xterm protocol. Three tools in
`crates/seraphim-conformance` do this, plus a fuzzer in the core crate.

## 1. Golden screens (runs in CI)

`tests/fixtures/xterm-golden.jsonl` holds **427 screens recorded from real
xterm**: 27 hand-written cases covering each feature family plus 400
generated escape-sequence streams. `cargo test -p seraphim-conformance`
replays each input into Seraphim and requires the identical screen text and
cursor position. No xterm or display is needed to run it.

To re-record (needs `xterm` and an X display, e.g. `Xvfb :99 &`):

```bash
DISPLAY=:99 cargo run -p seraphim-conformance --example record_golden -- 400
```

**Status: 427 / 427 identical.**

## 2. Triage with alacritty_terminal and xterm

```bash
DISPLAY=:99 cargo run -p seraphim-conformance --example triage -- 300
```

Generates streams, compares Seraphim with `alacritty_terminal` (fast, in
process), and for every disagreement asks real xterm who is right. `--shrink
SEED...` delta-debugs a failing stream down to a minimal reproduction.

**Status (300 streams):** all 300 match xterm; in 116 of them alacritty
differs from xterm.

## 3. vttest

```bash
DISPLAY=:99 cargo run -p seraphim-conformance --example vttest -- 1 2 8
```

Drives `vttest` in a PTY (Seraphim answers its queries), and at every
"Push <RETURN>" replays the byte stream into xterm and compares.

**Status: 34 / 35 screens identical** for tests 1 (cursor movement), 2
(screen features) and 8 (VT102 insert/delete). The one difference is in
vttest's 132-column pass: Seraphim deliberately ignores the 80/132-column
switch (DECCOLM), as alacritty and kitty do.

## 4. Fuzzing

```bash
cargo run --release -p seraphim-terminal --example fuzz -- 120
```

Random bytes and structured sequences with random resizes, viewport
scrolling and selections; after every step it checks structural invariants
(cursor in bounds, row widths, wide-char pairing, the rows' written-prefix
bookkeeping, scroll region, scrollback limit). A 1,500-seed debug-mode
(overflow-checked) smoke version runs in `cargo test`.

**Status:** 250k+ runs / 2 GB per 2.5-minute release run, no failures.

## Bugs found and fixed this way

| Found by | Bug | Verified xterm behaviour |
|---|---|---|
| shrinker | DCH/ICH next to a wide char destroyed the neighbouring wide char | only a wide char *straddling* the edit boundary is broken |
| shrinker | REP repeated a character after a control/SGR/REP | REP only repeats a graphic char output immediately before it |
| shrinker | a wide char wrapping from the last column blanked that column | the last column is left as is |
| shrinker | wide char with autowrap off overwrote the previous cell | dropped, or replaces a wide char printed just before it |
| shrinker | `?1049h` while already on the alternate screen didn't clear it | 1049h always clears; 1047h never; 1047l clears on exit |
| shrinker | HT at the right margin cancelled a pending wrap | the wrap stays pending |
| shrinker | VPR (`CSI e`) stopped at the bottom margin | VPR ignores margins (CUD doesn't) |
| fuzzer | panic: remembered wide-char column survived a resize | — |
| fuzzer (debug) | CPR in origin mode underflowed when the cursor was above the region, sending a garbage row number | — |

## Known differences

* `CSI 0 T`: xterm treats an explicit zero count for SD as a no-op; vte
  reports `CSI T` and `CSI 0 T` identically, so Seraphim scrolls one line
  (as alacritty does).
* DECCOLM / DECSCPP (80/132 columns) are ignored by design.

## Oracle details

xterm runs under Xvfb with `+132`, `allowWindowOps: false` and
`printAttributes: 0`; the screen is read with Media Copy (`CSI ? 19 h` to
print the full page, then `CSI i` into a file via `printerCommand`) and the
cursor with DSR 6 after draining any replies to queries contained in the
case. Comparisons trim trailing blanks and apply Unicode NFC (xterm's print
output composes combining sequences). In origin mode, xterm reports the
cursor relative to the scroll region, so cursors are compared only when
origin mode is off.
