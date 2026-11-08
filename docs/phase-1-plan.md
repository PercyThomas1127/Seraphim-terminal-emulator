# Phase I — Functional Foundation: status and plan

**Goal (spec §15):** a functional terminal capable of running ordinary shell
applications.

## Done

| Spec item | Where | Verified by |
|---|---|---|
| VT parsing: cursor, erase, insert/delete, scroll regions, origin mode, tabs, autowrap with pending-wrap, insert mode, DECSC/DECRC, RIS/DECSTR, REP, DEC line drawing | `seraphim-terminal/src/state.rs` | `tests/vt.rs` |
| SGR incl. 256-color and truecolor (`;` and `:` forms), curly/double underline, underline color | same | `tests/vt.rs` |
| Alternate screens 47/1047/1049, cursor save per screen | same | vt + vim/less tests |
| UTF-8, wide (CJK/emoji) cells, combining marks | same | `tests/vt.rs` |
| OSC 0/2 title, OSC 7 cwd, OSC 8 hyperlinks, OSC 52 (reported, never applied), OSC 133 semantic prompts incl. command line | same | `tests/vt.rs` |
| Mode tracking: app cursor/keypad, bracketed paste, mouse modes & encodings, focus events, synchronized output, cursor shape | same | `tests/vt.rs` |
| DSR/CPR/DA responses | same | vt + PTY round-trip test |
| Scrollback (bounded), dirty-row damage tracking, resize keeping cursor on screen | same | `tests/vt.rs` |
| Key/paste/focus input encoding (xterm legacy) with paste-injection guard | `seraphim-terminal/src/input.rs` | `tests/vt.rs` |
| PTY creation, controlling TTY, env (TERM, COLORTERM, TERM_PROGRAM, UTF-8 locale fallback), cwd, resize/SIGWINCH, hangup, foreground process, `/proc` process info | `seraphim-pty` | `tests/pty.rs` |
| Sessions independent of UI, I/O thread, event publishing, root detection from trusted metadata, SessionManager | `seraphim-session` | `tests/session.rs` |
| Shell integration (OSC 133 + OSC 7) | `shell-integration/` | Bash: e2e test. Zsh/Fish: **untested** |
| Event Bus (non-blocking, filtered, trusted source) | `seraphim-events` | unit tests |
| Real-app compatibility: vim, less, tmux, nano | — | `seraphim-session/tests/compat.rs` |

## Remaining for the Phase I milestone

Ordered by dependency. Each needs its acceptance check before it's done.

1. **Rendering spike → accept/reject ADR 0004.** winit window, wgpu
   surface, glyph atlas, draw a `Terminal` grid using `take_damage()`.
   *Check:* the five ADR 0004 criteria.
2. **Font stack:** shaping, fallback chain, bold/italic faces, Nerd
   Font/emoji, ligatures, box-drawing drawn procedurally (pixel-exact).
3. **Window input → `input::encode_key`**, IME commit text, mouse
   reporting encoder (X10/normal/SGR from `Modes`), selection + copy,
   OSC 52 policy (store allowed by default, load denied by default).
4. **Kitty keyboard protocol** (`CSI > u` push/pop, `CSI ? u` query):
   separate encoder; do *not* answer the `CSI ? u` query until
   implemented, so apps don't detect support we don't have.
5. **Conformance:** run `vttest` (menus 1–3, 8, 11) and `esctest`; add
   colour queries OSC 4/10/11/12, DECRQM, XTVERSION, DECSCUSR already done.
6. **Robustness:** `cargo-fuzz` target on `Terminal::advance` +
   `resize`; OSC length cap (ADR 0002 gap 3).
7. **Performance baseline:** bytes/sec through `advance()` on a 100 MB
   corpus (`cargo bench`); target ≥ 500 MB/s parse-only in release.
8. **Reflow on resize** using `Row::wrapped` (currently truncates).
9. **Minimal config:** `~/.config/seraphim/config.toml` with font, size,
   palette, shell, scrollback — just enough for Phase II profiles to build on.

## Known limitations (today)

* No window yet; the `seraphim` binary is a headless driver
  (`seraphim run -- PROGRAM`, `seraphim replay FILE`).
* No reflow on resize; no graphics protocols; Kitty graphics is blocked
  on the APC gap (ADR 0002).
* OSC 8 hyperlink table grows for the session's lifetime (needs GC with
  scrollback eviction).
* Linux only.
