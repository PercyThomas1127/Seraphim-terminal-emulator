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
| Queries: OSC 4/10/11/12 (query/set/reset, reply terminator matches request), DECRQM, XTVERSION, DECRQSS (`m`, ` q`, `r`), `CSI 18 t`; app color changes rendered | core `palette.rs`, renderer | `tests/queries.rs`, renderer pixel test |
| Synchronized output (2026) honoured in the window, 150 ms safety timeout | window | — |
| OSC payload cap (2 MiB) via a pre-filter in front of vte | core `prefilter.rs` | unit tests |
| Conformance vs xterm: 427/427 golden screens, vttest 34/35 (132-col by design), 300/300 triage streams | `seraphim-conformance` | see `docs/conformance.md` |
| Fuzzing with invariant checks (stable Rust) | `examples/fuzz.rs`, `tests/fuzz.rs` | 250k+ runs clean |
| Reflow on resize: soft-wrapped lines rewrap/rejoin across scrollback + screen; wide chars never split (wrap padding is not text); cursor and screen top tracked; growing pulls history back; primary reflows under an active alternate screen; 0.44 ms per resize with 10k lines of history | core `reflow.rs` | `tests/reflow.rs`, fuzzer, Xvfb |
| Kitty keyboard protocol: push/pop/set/query (`CSI > u`, `CSI < u`, `CSI = u`, `CSI ? u`) with separate main/alternate stacks; disambiguation, event types (press/repeat/release), alternate keys, all-keys mode, associated text, modifier keys | core `input.rs`, window | `tests/kitty_keyboard.rs`; exact bytes via Xvfb |
| Grapheme clusters (mode 2027, on by default, `terminal.grapheme_clustering`): flags, ZWJ sequences, skin tones, keycaps, VS16 emoji and combining marks are one cell with the cluster's width; only zero-width extenders and emoji sequences join, so text keeps wcwidth-compatible cells | core `state.rs` | `tests/graphemes.rs` |
| Cluster rendering: NFC precomposition in the cell's font, else swash shaping (GPOS marks, GSUB emoji ligatures) with a face covering the cluster; emoji presentation only when an emoji font covers the base; invisible codepoints never drawn | renderer `font.rs`, `lib.rs` | pixel tests |
| Font fallback by purpose: Nerd Font families first for private-use icons, emoji fonts for emoji; icons fitted to the cell (or two cells when followed by a space), Powerline separators full height and one cell | renderer | pixel tests; Powerline prompt screenshot |
| SGR 53/55 overline, 4:4 dotted, 4:5 dashed underline (patterns aligned across cells) | core + renderer | unit + pixel tests |
| Config file `~/.config/seraphim/config.toml`: font, window, colors, scrollback, shell, clipboard; typos are errors with line/column; broken config falls back to defaults with a message | `seraphim-config` | unit tests; Xvfb |

## Remaining for the Phase I milestone

Ordered by dependency. Each needs its acceptance check before it's done.

1. **Measure ADR 0004 on real hardware:** keypress-to-photon latency,
   cold start, Wayland fractional scaling. (The spike itself is done.)
   **This is the only Phase I item left; it needs a real machine.**
2. **Font stack, remaining:** ligatures (shaping over same-attribute
   runs), procedural box-drawing/block glyphs (pixel-exact joins),
   configurable fallback list.
3. ~~Clipboard, selection, scrollback viewing, mouse, cursor blink~~ —
   done (see table above). Follow-ups: auto-scroll while drag-selecting
   past the window edge, Ctrl+click to open OSC 8 hyperlinks, a
   right-click menu.
4. ~~Kitty keyboard protocol~~ — done.
5. ~~Conformance~~ — done (see `docs/conformance.md`). Follow-ups:
   vttest menus 3 (character sets), 6 (reports) and 11 (xterm extensions)
   need menu-aware driving; esctest needs DECRQCRA.
6. ~~Robustness~~ — done: OSC cap and an invariant-checking fuzzer.
   (`cargo-fuzz` coverage-guided fuzzing still needs a nightly toolchain.)
7. **Performance tracking:** keep `examples/throughput.rs` within 10% of
   `alacritty_terminal` on the same machine (absolute MB/s depends on the
   host; the old "500 MB/s" target was uncalibrated). Add partial
   redraws via `take_damage()` if frame-build time becomes significant.
8. ~~Reflow on resize~~ — done.
9. ~~Minimal config~~ — done. Live reload and profiles come with Phase II.
10. ~~Throughput regression~~ — fixed by inlining the print fast path
    into vte's dispatch loop: 87–102% of `alacritty_terminal` (avg ~94%)
    across interleaved runs on the same machine.

## Known limitations (today)

* No bidirectional text or Arabic joining: it needs line-level BiDi
  reordering with cursor/selection mapping (see the terminal-wg BiDi
  proposal); the shaper added for emoji is the building block.
* Remaining follow-ups (not blocking): procedural box-drawing glyphs and
  ligatures (item 2), auto-scroll while drag-selecting, Ctrl+click on OSC 8
  links, a right-click menu, vttest menus 3/6/11.
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
