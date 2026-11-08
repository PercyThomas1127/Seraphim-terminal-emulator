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
| GPU text renderer: font discovery + fallback, glyph atlas, colors/attributes, wide + combining chars, color emoji scaling, cursor shapes, focus-hollow cursor | `seraphim-render` | pixel tests in `tests/render.rs` (real wgpu pipeline) |
| Window: winit + wgpu surface, keyboard (named keys, Ctrl/Alt, IME commit), resize → PTY, focus reporting, title, drag-and-drop paths, redraw only on change | `seraphim/src/window.rs` | driven under Xvfb with `xdotool`; key bytes verified exactly |
| Performance baseline | `examples/throughput.rs`, `examples/frame_time.rs` | parity with `alacritty_terminal` on the same machine; see ADR 0004 |
| Scrollback viewing: viewport offset that stays on the same text while output arrives, Shift+PageUp/PageDown/Home/End, Ctrl+Shift+Up/Down, wheel, scroll indicator | `seraphim-terminal` (`display_offset`), renderer, window | `tests/selection.rs`, renderer pixel test, Xvfb |
| Selection: simple / word (double-click) / line (triple-click) / block (Ctrl+drag), Shift+click extends, wraps joined, wide chars whole, follows scrolling, cleared when its text changes | `seraphim-terminal/src/selection.rs` | `tests/selection.rs`; Xvfb + `xclip` |
| Clipboard: copy-on-select to PRIMARY, middle-click paste, Ctrl+Shift+C/V, Shift+Insert, bracketed paste | `seraphim/src/window.rs` (arboard) | Xvfb + `xclip`, exact pasted bytes |
| Mouse reporting: X10/1000/1002/1003 with default/UTF-8/SGR encodings, modifiers, wheel; Shift bypasses to selection; alternate-scroll (1007) for less/man | `seraphim-terminal/src/input.rs`, window | unit tests; exact bytes via Xvfb |
| OSC 52 policy: store allowed (decoded, 1 MiB cap, never broadcast on the bus), read denied unless `--allow-clipboard-read` | session queue + window | session test; Xvfb |
| Cursor blink only when the app requests it (DECSCUSR / `?12`); 0 idle CPU otherwise | window | Xvfb pixel sampling + `/proc` CPU ticks |

## Remaining for the Phase I milestone

Ordered by dependency. Each needs its acceptance check before it's done.

1. **Measure ADR 0004 on real hardware:** keypress-to-photon latency,
   cold start, Wayland fractional scaling. (The spike itself is done.)
2. **Font stack, remaining:** ligatures (shaping over same-attribute
   runs), procedural box-drawing/block glyphs (pixel-exact joins),
   configurable fallback list.
3. ~~Clipboard, selection, scrollback viewing, mouse, cursor blink~~ —
   done (see table above). Follow-ups: auto-scroll while drag-selecting
   past the window edge, Ctrl+click to open OSC 8 hyperlinks, a
   right-click menu.
4. **Kitty keyboard protocol** (`CSI > u` push/pop, `CSI ? u` query):
   separate encoder; do *not* answer the `CSI ? u` query until
   implemented, so apps don't detect support we don't have.
5. **Conformance:** run `vttest` (menus 1–3, 8, 11) and `esctest`; add
   colour queries OSC 4/10/11/12, DECRQM, XTVERSION, DECSCUSR already done.
6. **Robustness:** `cargo-fuzz` target on `Terminal::advance` +
   `resize`; OSC length cap (ADR 0002 gap 3).
7. **Performance tracking:** keep `examples/throughput.rs` within 10% of
   `alacritty_terminal` on the same machine (absolute MB/s depends on the
   host; the old "500 MB/s" target was uncalibrated). Add partial
   redraws via `take_damage()` if frame-build time becomes significant.
8. **Reflow on resize** using `Row::wrapped` (currently truncates).
9. **Minimal config:** `~/.config/seraphim/config.toml` with font, size,
   palette, shell, scrollback — just enough for Phase II profiles to build on.

## Known limitations (today)

* Synthetic X11 wheel clicks (XTest: xdotool, some remote-desktop
  servers) count twice, because winit's X11 backend reports both the
  press and the release of buttons 4/5. Physical wheels under libinput
  are unaffected. Not worked around (a timing heuristic could eat real
  fast scrolling).
* On X11, clipboard contents owned by Seraphim disappear when the window
  closes unless a clipboard manager takes them over (normal X11
  behaviour).
* Resizing clears the selection and returns to the live screen.
* Only tested on X11 (Xvfb) with software Vulkan; Wayland and real GPUs
  untested.
* No reflow on resize; no graphics protocols; Kitty graphics is blocked
  on the APC gap (ADR 0002).
* OSC 8 hyperlink table grows for the session's lifetime (needs GC with
  scrollback eviction).
* Linux only.
