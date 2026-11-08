# Seraphim

The programmable terminal environment of Divinity OS.

> *A terminal that doesn't just let you customize its appearance. It lets you program its personality.*

This repository is at the end of **Phase I (Functional Foundation)**: the
Terminal Core, PTY layer, Session Manager, Event Bus, GPU renderer, window,
configuration and conformance suite are implemented and tested. The one
remaining Phase I item is measuring latency, startup and Wayland scaling on
real hardware. See [`docs/phase-1-plan.md`](docs/phase-1-plan.md) for
what's done and what's left, and [`docs/adr/`](docs/adr/) for the
architecture decisions that settle spec §18.

![Seraphim renderer output](docs/images/renderer.png)

## Status

Phases I (functional terminal) and II (visual identity) of the
specification are complete; Phase III (Lua scripting) is next. See
[docs/progress.md](docs/progress.md) for where every part of the spec
stands.

## Layout

```text
crates/
  seraphim-terminal/   Terminal Core: VT/xterm semantics, grid, scrollback, modes, input encoding (no I/O)
  seraphim-pty/        PTY creation, shell launch, resize, process metadata
  seraphim-events/     Event Bus: non-blocking, pattern-filtered pub/sub
  seraphim-session/    Sessions: PTY <-> Terminal I/O thread, event publishing, root detection
  seraphim-render/     GPU text renderer (wgpu): fonts + fallback, glyph atlas, one instanced draw
  seraphim-config/     TOML configuration loading and validation
  seraphim-conformance/ Differential testing against xterm (golden screens, vttest, triage)
  seraphim/            Application: window (winit + wgpu), plus headless `run` / `replay` tools
shell-integration/     Bash/Zsh/Fish scripts emitting OSC 133 + OSC 7
docs/                  ADRs and phase plans
```

## Build and test

```bash
cargo test --workspace
```

The compatibility tests drive real `vim`, `less`, `tmux` and `nano` when they
are installed, and skip otherwise. Renderer pixel tests need a GPU adapter
(any Vulkan/GL driver, including lavapipe) and skip without one.

Conformance against xterm (427 recorded screens) runs as part of
`cargo test`; see [`docs/conformance.md`](docs/conformance.md) for vttest,
the xterm-refereed triage tool and the fuzzer.

Benchmarks:

```bash
cargo run --release -p seraphim-terminal --example throughput
```

```bash
cargo run --release -p seraphim-render --example frame_time
```

## Run it

```bash
cargo run --release -p seraphim
```

opens a window running your shell. Options: `--font FAMILY`,
`--font-size PT`, `--cols N`, `--rows N`, `-e PROGRAM [ARGS...]`.

| Action | Keys / mouse |
|---|---|
| Copy selection | `Ctrl+Shift+C` (selecting also copies to PRIMARY) |
| Paste | `Ctrl+Shift+V`, `Shift+Insert`; middle-click pastes PRIMARY |
| Select | drag; double-click word; triple-click line; `Ctrl`+drag block; `Shift`+click extends |
| Scroll history | wheel, `Shift+PageUp/PageDown`, `Shift+Home/End`, `Ctrl+Shift+Up/Down` |
| Select while an app uses the mouse | hold `Shift` |

Resizing the window reflows soft-wrapped lines (scrollback included).
Applications that opt into the [Kitty keyboard
protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) get
unambiguous keys (Ctrl+I vs Tab, Esc vs Alt) and key release events.

Applications may set the clipboard with OSC 52; reading it requires
`--allow-clipboard-read` (or `clipboard.allow_read` in the config).

### Backgrounds, themes and profiles

![Seraphim visual engine](docs/images/visual.png)

Layers behind the text — solid colors, gradients, images (PNG, JPEG,
WebP, GIF; animated GIF/WebP/APNG play; blur and color grading), videos
(anything ffmpeg plays, looped, if ffmpeg is installed) and WGSL
shaders (built-in `aurora`, `starfield` and `heavens`, or your own) — are declared in
the config:

```toml
theme = "celestial"          # or "monochrome", "daylight", themes/<name>.toml

[[background]]
kind = "gradient"
colors = ["#100c22", "#2a1844", "#0d0f1f"]

[[background]]
kind = "shader"
shader = "aurora"
blend = "screen"
opacity = 0.75
params = { speed = 0.3, hue = 0.78 }
```

**Presets** bundle a scene with its interactions. The built-in `heavens`
preset is an animated sky (aurora, nebula or starfield) with two moods. It
turns red in a root shell, and a wave of light rises when you install
something:

```bash
cargo run --release -p seraphim -- --preset heavens
```

![The heavens preset: calm, install wave, root](docs/images/moods.png)

A preset is an ordinary TOML file of `[[background]]` layers and `[[on]]`
rules. Copy one to `~/.config/seraphim/presets/<name>.toml`, or write your
own, and select it with `preset = "<name>"`. Rules animate any layer
property when an event arrives on the bus:

```toml
[[background]]
id = "glow"
kind = "shader"
shader = "glow.wgsl"                # in ~/.config/seraphim/shaders/
palette = { tint = "theme.bright.5" }   # color(C_tint) in WGSL

[[on]]
event = "command.completed"
when = { exit_code = 1 }
layer = "glow"
param = "pulse"                     # p(P_pulse) in WGSL
from = 1.0
to = 0.0
duration_ms = 600
easing = "ease-out"
```

Bindings (`[[bind]]`) keep a parameter following a live value: CPU,
memory, load, battery, the time of day, or a field of the latest event.
Each new value is eased in:

```toml
[[bind]]
source = "system.cpu"               # percent
layer = "sky"
param = "speed"
output = [0.6, 2.5]                 # idle .. busy

[[bind]]
event = "command.completed"
field = "exit_code"
layer = "glow"
param = "pulse"
map = { "0" = 0.0, default = 1.0 }  # red after a failed command
```

To tweak a preset's layer, give a `[[background]]` the same `id` in your
config, e.g. `params = { style = 2 }` for the nebula. See ADR 0008 for the
design and the Lua/CSS authoring plan.

A `kind = "blur"` layer blurs everything beneath it (frosted glass over a
shader or wallpaper). Gradients animate with `spin` and `flow` parameters.
Scene changes use `visual.transition`: `fade`, `slide`, `wipe`,
`dissolve`, `zoom`, `blur`, `fade-through`, or your own `.wgsl` file
implementing `fn transition(uv, p)` with `from_scene(uv)` and
`to_scene(uv)`.

Profiles in `~/.config/seraphim/profiles/<name>.toml` override any part of
the config; switch with `--profile NAME` or **Ctrl+Shift+P** at runtime
(the background crossfades), reload with **Ctrl+Shift+R**. Animation only
draws frames while something moves, pauses when the window is unfocused, and
stops entirely with `visual.reduced_motion = true`.

### Cursor effects

The cursor glides between cells and fades when it blinks; a new selection
fades in. Optional: a soft glow, a trail while gliding, and a ripple on
every key press (`[cursor]` in the config). `visual.reduced_motion` turns
the moving ones off.

```toml
[cursor]
glide_ms = 80
glow = 0.6
ripple = true
trail = true
```

### Menu bar

![The menu bar](docs/images/menu.png)

File, Edit, View, Profiles and Help, laid out after Konsole's. The bar is
hidden until you tap **Alt** (Option); it opens over the terminal and hides
again when you pick something, press Esc or click elsewhere. Arrows move,
Enter runs, the underlined letter jumps. Alt+key chords still go to the
shell and don't show the bar. To keep it on screen, use **Ctrl+Shift+M**
(View → Show Menubar) or set `window.menu_bar = true`.

The menus are data (`crates/seraphim-config/src/menu.toml`), so you can
add your own from the config. An item runs an action, types text into the
shell or switches profile:

```toml
[[menu]]
title = "&Tools"
items = [
    { label = "&System Monitor", send = "htop\n" },
    { separator = true },
    { label = "&Work Profile", profile = "work" },
]
```

A `[[menu]]` titled like a built-in one appends to it (`replace = true`
replaces it).

### Images

![Kitty graphics and Sixel](docs/images/graphics.png)

Programs can show pictures inline with the **Kitty graphics protocol**
(`kitten icat`, `timg`, `chafa -f kitty`, yazi, ...) or **Sixel**
(`img2sixel`, `chafa -f sixel`, gnuplot, ...). Images scroll with the text
into the history, move with scroll regions, and can sit under or over the
text (kitty z-index). Kitty animations play, Unicode placeholders work (so
images show through tmux), and local tools can hand images over through
shared memory or files. The PTY reports pixel sizes, so tools size
pictures correctly.

### Configuration

Settings live in `~/.config/seraphim/config.toml` (font, window size,
colors, scrollback, shell, clipboard policy). Print the documented defaults
with:

```bash
cargo run -p seraphim -- --print-default-config
```

Changes apply as soon as you save: Seraphim watches the config, the active
profile, theme and preset files, and any shaders and images they use
(`live_reload = false` turns this off; Ctrl+Shift+R reloads by hand). A
notice in the corner says when it reloaded, or what is wrong; a broken
file leaves the current look in place until it is fixed.

Unknown keys and invalid values are reported with their line and column;
a broken config never stops the terminal from opening (it falls back to the
defaults and says why). Command-line options override the file;
`--config PATH` uses another file. `SERAPHIM_DEBUG_INPUT=1` logs raw input events.

Linux build dependencies: a Vulkan or GL driver, plus the usual X11/Wayland
client libraries (`libxkbcommon`, `libwayland-client`). With no GPU,
Mesa's software Vulkan (`mesa-vulkan-drivers`, "lavapipe") works.

### Headless tools

`seraphim run` runs a program in a PTY-backed session without a window, then
prints the final screen and every published event. This example emits by
hand the same sequences the shell integration sends around a command:

```bash
cargo run -p seraphim -- run --cols 40 --rows 4 --events -- sh -c 'printf "\033]133;C;cmdline_url=gabriel%%20bestow%%20firefox\a"; sleep 0.3; printf "installing...\n\033]133;D;0\a"'
```

```text
event command.started    session=Some(1) {"command":"gabriel bestow firefox"}
event command.completed  session=Some(1) {"command":"gabriel bestow firefox","duration_ms":301,"exit_code":0}
```

`seraphim replay FILE` feeds a recorded byte stream (e.g. from
`script -q -c CMD out.log`) through the parser, useful for reproducing
rendering bugs.

## Shell integration

Add to your shell's rc file:

```bash
[[ $TERM_PROGRAM == seraphim ]] && source /path/to/seraphim/shell-integration/seraphim.bash
```

This lets Seraphim publish `shell.prompt`, `command.started`,
`command.completed` (with exit code and duration) and `session.cwd.changed`
without modifying any program it runs. The Gabriel reactions in spec §5.3
build on these events.
