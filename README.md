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

### Configuration

Settings live in `~/.config/seraphim/config.toml` (font, window size,
colors, scrollback, shell, clipboard policy). Print the documented defaults
with:

```bash
cargo run -p seraphim -- --print-default-config
```

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
