# Phase V plan — System integration

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
  * Sounds play through `pw-play`, `paplay` or `aplay`.
  * Audio-reactive visuals read levels from the default output's monitor
    with `parec` or `pw-record`. There is no audio library dependency,
    and nothing runs unless the configuration asks for it.

## Checklist

### 1. Package managers (replacing Gabriel)
- [ ] Command recognition: emerge (Portage), pacman, paru, yay, apt,
  apt-get, dnf, yum, zypper, xbps-install/xbps-remove, apk, eopkg,
  nix-env/nix profile, flatpak, snap, brew; through sudo/doas/run0/pkexec
- [ ] Operations: install, remove, upgrade (whole system), sync (refresh
  the package lists)
- [ ] Progress parsing from each manager's output: the current package,
  `(n/m)` style counts, percentages
- [ ] Events: `package.<op>.started|progress|completed|failed`, published
  by the session; without shell integration, from the foreground process
- [ ] Replace `gabriel.*` everywhere: heavens preset, Package Activity
  panel, tab icons, example extension, docs; `package` is a reserved
  namespace
- [ ] Tests from real output of each manager

### 2. System monitoring
- [ ] GPU: utilization, memory, temperature (AMD/Intel sysfs, NVIDIA via
  nvidia-smi)
- [ ] Network: receive and send rates, online/offline (`network.*` events)
- [ ] Temperatures (hwmon) and disk activity
- [ ] Binding sources, Lua functions, System Monitor panel rows

### 3. Desktop notifications
- [ ] Program notifications: OSC 9 (text), OSC 777;notify, OSC 99 (kitty)
- [ ] Long commands finishing, and bells, while the terminal is out of
  sight
- [ ] `[notifications]` settings; `notification.*` events

### 4. Audio
- [ ] Sounds for events (`[[sound]]`), and an audible bell
- [ ] Audio levels as binding sources (level, bass, mid, treble) for
  audio-reactive backgrounds; `audio.*` events
- [ ] Only runs when configured; reduced motion stops audio-reactive
  visuals

### 5. Close-out
- [ ] ADR, README, user guide, scripting guide, `docs/progress.md`
