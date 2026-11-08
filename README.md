# Seraphim

The programmable terminal environment of Divinity OS.

> *A terminal that doesn't just let you customize its appearance. It lets you program its personality.*

This repository is at **Phase I (Functional Foundation)**: the Terminal Core,
PTY layer, Session Manager and Event Bus are implemented and tested; the GPU
renderer is next. See [`docs/phase-1-plan.md`](docs/phase-1-plan.md) for
what's done and what's left, and [`docs/adr/`](docs/adr/) for the
architecture decisions that settle spec §18.

## Layout

```text
crates/
  seraphim-terminal/   Terminal Core: VT/xterm semantics, grid, scrollback, modes, input encoding (no I/O)
  seraphim-pty/        PTY creation, shell launch, resize, process metadata
  seraphim-events/     Event Bus: non-blocking, pattern-filtered pub/sub
  seraphim-session/    Sessions: PTY <-> Terminal I/O thread, event publishing, root detection
  seraphim/            Application binary (currently a headless driver)
shell-integration/     Bash/Zsh/Fish scripts emitting OSC 133 + OSC 7
docs/                  ADRs and phase plans
```

## Build and test

```bash
cargo test --workspace
```

The compatibility tests drive real `vim`, `less`, `tmux` and `nano` when they
are installed, and skip otherwise.

## Try it

The binary is a headless driver until the renderer lands: it runs a program in
a PTY-backed session, then prints the final screen and every published event.
This example emits by hand the same sequences the shell integration sends
around a command:

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
