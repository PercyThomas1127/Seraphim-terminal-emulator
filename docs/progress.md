# Seraphim: progress against the specification (v0.1)

As of the end of Phase IV. Percentages are rough: the share of each
phase's listed tasks that is done, not a measure of effort.

| Phase (spec §15) | Status | Done |
|---|---|---|
| I. Functional Foundation | Complete | ~100 % |
| II. Visual Identity | Complete (real-GPU measurements pending) | ~100 % |
| III. Programmability | Complete: Lua extensions, API, permissions | ~100 % |
| IV. Full Interface | Complete (detachable tabs not done) | ~95 % |
| V. Divinity Integration | System metrics and Gabriel-event hooks only | ~10 % |
| **Overall** | Four of five phases done | **~80 %** |

The code: 10 crates, ~32,000 lines of Rust, ~600 lines of WGSL and six
example extensions, 325 automated tests (unit, pixel tests through the
real wgpu pipeline, xterm conformance, fuzzing, Lua runtime end to end),
all passing; clippy clean.

## Phase I — Functional Foundation: complete

| Task | Where | Notes |
|---|---|---|
| PTY management | `seraphim-pty` | spawn, resize (with pixel size), signals, process groups |
| VT parser | `seraphim-terminal` (vte + pre-filter) | OSC cap, APC interception |
| Terminal state | `seraphim-terminal` | alt screens, scroll regions, modes, reflow, selection, grapheme clusters, Kitty keyboard, queries; checked against xterm (`docs/conformance.md`) |
| Basic GPU text renderer | `seraphim-render` | instanced quads, glyph atlas, shaping, emoji, Nerd Font, all underline styles |
| Input handling | `seraphim/src/window.rs` | keys, IME, mouse reporting, selection, clipboard, OSC 52 policy |
| Basic sessions | `seraphim-session` | `SessionManager` with create/switch/close (the window uses one session) |
| Shell integration | `shell-integration/` | bash, zsh, fish (OSC 133/7) |

Parser throughput is on par with `alacritty_terminal` on the same machine
(ADR 0004).

## Phase II — Visual Identity: complete

See `docs/phase-2-plan.md` for the full list. In short:

* **Visual engine:** layered scenes (solid, animated gradient, image,
  video, WGSL shader, backdrop blur), blend modes, presets, offscreen
  compositing only when needed.
* **Backgrounds:** PNG/JPEG/WebP/GIF, animated GIF/WebP/APNG, video via
  ffmpeg, blur and color grading.
* **GPU graphics:** Kitty graphics protocol (including animation, Unicode
  placeholders for tmux, shared memory) and Sixel.
* **Transparency:** verified under a compositor.
* **Shaders:** built-in and user WGSL layers and transitions, compiled in
  validation scopes so a broken shader never takes the terminal down.
* **Animation engine:** tweens, springs, keyframes, event rules
  (`[[on]]`), continuous bindings (`[[bind]]`), scene transitions (fade,
  slide, wipe, dissolve, zoom, blur, custom), cursor effects; all
  time-based, paced, paused when hidden, reduced-motion aware.
* **Themes and profiles:** themes, profiles, presets, live reload, menu
  bar.

## Phase III — Programmability: complete

See `docs/phase-3-plan.md`, ADR 0009 and `docs/scripting.md`.

| Task | Where | Notes |
|---|---|---|
| Event bus | `seraphim-events` | non-blocking, filtered, trusted sources (ADR 0003); `Pattern::Any`, retargetable subscriptions, `wants()` |
| Lua runtime | `seraphim-script` | Lua 5.4 (mlua, vendored); one script thread, one sandboxed state per extension; time budgets that `pcall` can't swallow, memory limits, disable after repeated overruns; live reload per extension |
| Scripting API | `seraphim-script/src/api.rs` | `seraphim` (events, timers, custom events, storage), `visual`, `animation`, `terminal`, `ui`, `system`, `process`, `fs`, `net` |
| Reactive properties | `visual.bind` = `[[bind]]` | numbers and colors; metrics, event fields, `terminal.last_exit_code`, `session.root`, … (both §11.4 examples) |
| Extension permissions | `seraphim-script/src/perm.rs` | requested ∩ granted; absent tables; allow-lists for commands, paths and hosts; no input into root sessions |
| Custom visual layers | `visual.add_layer` | any `[[background]]` layer, validated like TOML, kept across profile switches |

Events (spec §11.2) now include `interface.tab.changed`,
`interface.pane.focused`, `terminal.progress`, `terminal.output` (parsed text),
`terminal.output.raw`, `command.output`, `terminal.text.changed`,
`terminal.cursor.moved`, `animation.completed`, `interface.focus.changed`,
`interface.resized` and `interface.menu.toggled`, on top of the session,
shell, command and visual events. Still missing: `audio.*`, `network.*` (Phase V integrations).

Event management (§11.3): subscriptions, filtering, metadata, custom
events, priorities, cancellation (between scripts), asynchronous handling
(callbacks for processes and HTTP), rate limiting (per-handler throttle,
bounded queues).

## Phase IV — Full Interface: complete

See `docs/phase-4-plan.md` and ADR 0010.

| Task | Where | Notes |
|---|---|---|
| Tabs | `workspace.rs`, `tabbar.rs`, `window.rs` | bar (auto/always/never), labels and rename, icons, activity/bell marks, OSC 9;4 progress, root color, drag to reorder, per-tab profiles |
| Split panes | `workspace.rs` | split right/down, focus by direction, resize, zoom, drag dividers, click to focus, dimmed inactive panes; one renderer with per-pane slots |
| Custom panels | `panel.rs`, `panels.rs` | widget trees painted by Seraphim; system monitor, sessions, history, package activity; extension panels via `ui.panel` |
| GUI configuration | `settings.rs` | settings panel editing the config file in place (comments kept), applied live; extension switches |
| Layout management | `[[layout]]` | named trees of commands, directories and panels; `--layout`, `startup_layout`, Settings menu, `ui.open_layout` |
| Session restoration | `restore.rs` | tabs, splits, directories, labels, profiles, panels, last 200 lines per terminal |

Still open from spec §9: detachable tabs, floating panels, custom window
decorations, custom tab designs beyond the label format.

## Phase V — Divinity Integration: ~10 %

| Task | Status |
|---|---|
| Gabriel integration | Event names (`gabriel.install.*`) are consumed by the `heavens` preset; no real Gabriel connection |
| System monitoring | CPU, memory, load and battery readers (for `[[bind]]`); no GPU or network |
| Audio integration | Not started |
| Desktop notifications | In-window notices; `system.notify` for extensions (notify-send) |
| Seraphim Gallery | Not started |
| Official extension SDK | The Lua API, manifest format and examples are the base; no packaging or Gallery tooling |

## Non-functional requirements (spec §13)

| Area | Status |
|---|---|
| Reliability | Shader, image and video failures are contained and reported |
| Performance | Parity with alacritty's parser; frames drawn only when something changes; frame pacing for animations. **Gap:** the cell renderer rebuilds every visible cell each frame (spec §4.4 asks for partial redraws / dirty regions) |
| Isolation, responsiveness | Scripts on their own thread, sandboxed, budgeted; they send commands and never block rendering or I/O |
| Accessibility | Reduced motion everywhere, font zoom; no contrast checks yet |
| Compatibility | xterm conformance suite, vttest, golden screens |
| Portability | Linux; macOS/Windows untested |
| Configuration | One documented TOML file plus profiles/themes/presets, validated with exact errors |
| Extensibility | Data-driven presets, menus, rules; Lua extensions with a versioned API (`api = 1`) |
| Resource usage | `max_fps`, pausing, quotas for images and frames |

## Known gaps worth scheduling

1. Phase V: a real Gabriel connection, audio, desktop integration, the
   Gallery and an SDK.
2. Dirty-region rendering (spec §4.4).
3. Measurements on real GPUs (everything so far ran on llvmpipe).
4. Testing with real image tools (`kitten icat`, `chafa`, `img2sixel`,
   tmux), which aren't available in the development container.
5. WebAssembly extensions for untrusted Gallery code (ADR 0005), with the
   Gallery itself (Phase V).
