# Seraphim: progress against the specification (v0.1)

As of the end of Phase V. Percentages are rough: the share of each
phase's listed tasks that is done, not a measure of effort.

| Phase (spec §15) | Status | Done |
|---|---|---|
| I. Functional Foundation | Complete | ~100 % |
| II. Visual Identity | Complete (real-GPU measurements pending) | ~100 % |
| III. Programmability | Complete: Lua extensions, API, permissions | ~100 % |
| IV. Full Interface | Complete | 100 % |
| V. System Integration (rescoped) | Complete: package managers, metrics, notifications, audio | 100 % |
| **Overall** | All five phases done (Phase V rescoped: no Divinity OS, Gallery or SDK) | **100 %** |

The code: 10 crates, ~38,000 lines of Rust, ~600 lines of WGSL and six
example extensions, 354 automated tests (unit, pixel tests through the
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
shell, command and visual events. Phase V added `package.*`, `network.*`, `audio.playing`, `terminal.notification` and `notification.sent`.

Event management (§11.3): subscriptions, filtering, metadata, custom
events, priorities, cancellation (between scripts), asynchronous handling
(callbacks for processes and HTTP), rate limiting (per-handler throttle,
bounded queues).

## Phase IV — Full Interface: complete

See `docs/phase-4-plan.md` and ADR 0010.

| Task | Where | Notes |
|---|---|---|
| Tabs | `workspace.rs`, `tabbar.rs`, `window.rs` | bar (auto/always/never), labels and rename, icons, activity/bell marks, OSC 9;4 progress, root color, drag to reorder, per-tab profiles; designs (styles, top/bottom, widths, buttons, colors); detach into a new window with the shells running |
| Windows | `window.rs`, `decor.rs` | all windows in one process; Seraphim's own title bar and buttons (or none), resizable edges |
| Split panes | `workspace.rs` | split right/down, focus by direction, resize, zoom, drag dividers, click to focus, dimmed inactive panes; one renderer with per-pane slots |
| Custom panels | `panel.rs`, `panels.rs` | widget trees painted by Seraphim; system monitor, sessions, history, package activity; extension panels via `ui.panel`; docked or floating |
| GUI configuration | `settings.rs` | settings panel editing the config file in place (comments kept), applied live; extension switches |
| Layout management | `[[layout]]` | named trees of commands, directories and panels; `--layout`, `startup_layout`, Settings menu, `ui.open_layout` |
| Session restoration | `restore.rs` | every window: tabs, splits, directories, labels, profiles, panels (floating ones too), last 200 lines per terminal; saved every 30 s and on quit |

Nothing from spec §9 is open. Possible additions: moving a tab into
another existing window, floating terminals.

## Phase V — System integration: complete

Rescoped: no Divinity OS integration, no Seraphim Gallery, no extension
SDK; the package managers people use replace Gabriel. See
`docs/phase-5-plan.md` and ADR 0011.

| Task | Where | Notes |
|---|---|---|
| Package managers (replacing Gabriel) | `seraphim-session/src/packages.rs` | emerge, pacman, paru, yay, pikaur, apt, aptitude, nala, dnf, yum, zypper, xbps, apk, eopkg, nix, flatpak, snap, brew; through sudo/doas/run0/pkexec; progress from their output; `package.*` events; tab progress, panel, heavens preset |
| System monitoring | `seraphim-script/src/metrics.rs` | CPU, memory, load, battery, GPU (AMD, Intel, NVIDIA), temperatures, network rates and online state, disk rates; `network.*` events |
| Desktop notifications | `seraphim-script/src/desktop.rs`, `window.rs` | OSC 9/777/99; long commands, package managers and the bell out of sight; notify-send or gdbus |
| Audio | `seraphim-script/src/audio.rs`, `sounds.rs` | `[[sound]]`, audible bell, audio levels as bindings, `audio.playing` |

## Non-functional requirements (spec §13)

| Area | Status |
|---|---|
| Reliability | Shader, image and video failures are contained and reported |
| Performance | Parity with alacritty's parser; frames drawn only when something changes; frame pacing for animations; partial redraws (only changed rows are rebuilt, only changed bytes uploaded; `docs/benchmarks.md`). **Gap:** measured on two desktop GPUs under X11 only |
| Isolation, responsiveness | Scripts on their own thread, sandboxed, budgeted; they send commands and never block rendering or I/O |
| Accessibility | Reduced motion everywhere, font zoom; no contrast checks yet |
| Compatibility | xterm conformance suite, vttest, golden screens |
| Portability | Linux; macOS/Windows untested |
| Configuration | One documented TOML file plus profiles/themes/presets, validated with exact errors |
| Extensibility | Data-driven presets, menus, rules; Lua extensions with a versioned API (`api = 1`) |
| Resource usage | `max_fps`, pausing, quotas for images and frames |

## Known gaps worth scheduling

1. More hardware: measured so far on an RTX 2060 Super and a Radeon Pro
   WX 7100 under X11 (`docs/benchmarks.md`). Not yet: Wayland, Intel
   GPUs, laptops (integrated + discrete), high-refresh displays.
2. Package managers without shell integration have no exit codes.
3. Portability: macOS and Windows are untested.
