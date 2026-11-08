# Seraphim: progress against the specification (v0.1)

As of the end of Phase II. Percentages are rough: the share of each
phase's listed tasks that is done, not a measure of effort.

| Phase (spec §15) | Status | Done |
|---|---|---|
| I. Functional Foundation | Complete | ~100 % |
| II. Visual Identity | Complete (real-GPU measurements pending) | ~100 % |
| III. Programmability | Event bus and declarative reactions done; no Lua yet | ~30 % |
| IV. Full Interface | Menu bar and overlay only | ~10 % |
| V. Divinity Integration | System metrics and Gabriel-event hooks only | ~10 % |
| **Overall** | Two of five phases done; the core and visuals are solid | **~50 %** |

The code: 9 crates, ~22,500 lines of Rust and ~600 lines of WGSL, 266
automated tests (unit, pixel tests through the real wgpu pipeline, xterm
conformance, fuzzing), all passing; clippy clean.

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

## Phase III — Programmability: ~30 %

| Task | Status |
|---|---|
| Event bus | **Done** (non-blocking, filtered, trusted sources; ADR 0003) |
| Lua runtime | Not started (ADR 0005 proposes mlua, off the render thread) |
| Scripting API | Not started |
| Reactive properties | **Declarative half done:** `[[on]]` rules and `[[bind]]` bindings; Lua handlers will reuse them (ADR 0008) |
| Extension permissions | Not started |
| Custom visual layers | **Declarative half done:** user WGSL layers and transitions; script-driven layers come with Lua |

Events published today: `session.created/closed/privileged/cwd.changed`,
`shell.prompt`, `command.started/completed`, `terminal.output/bell/
title.changed/screen.alternate`, `clipboard.requested`,
`visual.profile.changed`. Missing from spec §11.2: `command.output`,
`terminal.text.changed`, `terminal.cursor.moved`, `animation.completed`,
`interface.*`, `audio.*`, `network.*`.

## Phase IV — Full Interface: ~10 %

| Task | Status |
|---|---|
| Tabs | Not started (sessions are already independent of windows) |
| Split panes | Not started |
| Custom panels | Overlay renderer, panels and notices exist (menu bar); no panel API |
| GUI configuration | Menu bar only; no settings UI |
| Layout management | Not started |
| Session restoration | Not started |

## Phase V — Divinity Integration: ~10 %

| Task | Status |
|---|---|
| Gabriel integration | Event names (`gabriel.install.*`) are consumed by the `heavens` preset; no real Gabriel connection |
| System monitoring | CPU, memory, load and battery readers (for `[[bind]]`); no GPU or network |
| Audio integration | Not started |
| Desktop notifications | In-window notices only |
| Seraphim Gallery | Not started |
| Official extension SDK | Not started |

## Non-functional requirements (spec §13)

| Area | Status |
|---|---|
| Reliability | Shader, image and video failures are contained and reported |
| Performance | Parity with alacritty's parser; frames drawn only when something changes; frame pacing for animations. **Gap:** the cell renderer rebuilds every visible cell each frame (spec §4.4 asks for partial redraws / dirty regions) |
| Isolation, responsiveness | Wait for the Lua runtime and permissions (Phase III) |
| Accessibility | Reduced motion everywhere, font zoom; no contrast checks yet |
| Compatibility | xterm conformance suite, vttest, golden screens |
| Portability | Linux; macOS/Windows untested |
| Configuration | One documented TOML file plus profiles/themes/presets, validated with exact errors |
| Extensibility | Data-driven presets, menus, rules; versioned extension APIs wait for Phase III |
| Resource usage | `max_fps`, pausing, quotas for images and frames |

## Known gaps worth scheduling

1. Lua runtime and scripting API (Phase III, essential per spec §16).
2. Tabs and split panes (Phase IV).
3. Dirty-region rendering (spec §4.4).
4. Measurements on real GPUs (everything so far ran on llvmpipe).
5. Testing with real image tools (`kitten icat`, `chafa`, `img2sixel`,
   tmux), which aren't available in the development container.
