# Phase V plan — System integration

**Status: complete.** As built: [ADR 0011](adr/0011-system-integration.md).

**Scope, as revised:** Phase V no longer integrates with Divinity OS.
The Seraphim Gallery and the official extension SDK are out of scope.
The Gabriel integration is replaced by support for the package managers
people use today. What remains of the spec's Phase V (§15) is the generic
Linux part: package managers, system monitoring, desktop notifications
and audio.

**Milestone:** Seraphim reacts to what happens on the system as well as
in the terminal. It knows when packages install, sees GPU and network
activity, tells you when something finishes while you're away, and can
make and hear sound.

## Shape

* **Package activity is recognized, not reported.** No package manager
  sends events to a terminal. Seraphim recognizes their commands, and
  reads their progress from their own output. Each manager is a small
  table of commands, subcommands and progress lines.
  * **Commands** come from shell integration (OSC 133 command lines).
    Without it, Seraphim polls the foreground process's command line.
  * **Privilege wrappers** are seen through: `sudo`, `doas`, `run0`,
    `pkexec`, `env`, `nice` and `ionice`.
  * **Events** take the form `package.<operation>.<phase>`, with the
    manager, the packages, progress and exit codes in their data. These
    replace the `gabriel.*` names. `package` is a trusted source, which
    extensions cannot forge.
* **Metrics are sampled only while something uses them**, as the CPU and
  memory readers already are. GPU readings come from sysfs (AMD, Intel)
  and `nvidia-smi` (NVIDIA). Network readings come from `/proc/net/dev`.
* **Notifications use the desktop's notification service** through
  `notify-send`, and fall back to `gdbus`. Programs can ask for one with
  the OSC 9, OSC 777 and OSC 99 escapes. Seraphim sends its own when a
  long command finishes, or the bell rings, while you can't see the
  terminal.
* **Audio uses the sound server's command-line tools**:
  * Sounds play through `pw-play`, `paplay`, `aplay` or `ffplay`.
  * Audio-reactive visuals read levels from the default output's monitor
    with `parec` or `ffmpeg`. There is no audio library dependency,
    and nothing runs unless the configuration asks for it.

## Checklist

### 1. Package managers (replacing Gabriel)
- [x] Command recognition: emerge (Portage), pacman, paru, yay, apt,
  apt-get, dnf, yum, zypper, xbps-install/xbps-remove, apk, eopkg,
  nix-env/nix profile, flatpak, snap, brew; through sudo/doas/run0/pkexec
- [x] Operations: install, remove, upgrade (whole system), sync (refresh
  the package lists)
- [x] Progress parsing from each manager's output: the current package,
  `(n/m)` style counts, percentages
- [x] Events: `package.<op>.started|progress|completed|failed`, published
  by the session; without shell integration, from the foreground process
- [x] Replace `gabriel.*` everywhere: heavens preset, Package Activity
  panel, tab icons, example extension, docs; `package` is a reserved
  namespace
- [x] Tests from real output of each manager

### 2. System monitoring
- [x] GPU: utilization, memory, temperature (AMD/Intel sysfs, NVIDIA via
  nvidia-smi)
- [x] Network: receive and send rates, online/offline (`network.*` events)
- [x] Temperatures (hwmon) and disk activity
- [x] Binding sources, Lua functions, System Monitor panel rows

### 3. Desktop notifications
- [x] Program notifications: OSC 9 (text), OSC 777;notify, OSC 99 (kitty)
- [x] Long commands finishing, and bells, while the terminal is out of
  sight
- [x] `[notifications]` settings; `notification.*` events

### 4. Audio
- [x] Sounds for events (`[[sound]]`), and an audible bell
- [x] Audio levels as binding sources (level, bass, mid, treble) for
  audio-reactive backgrounds; `audio.*` events
- [x] Only runs when configured: playback for configured sounds, capture
  once something reads a level

### 5. Close-out
- [x] ADR, README, user guide, scripting guide, `docs/progress.md`
