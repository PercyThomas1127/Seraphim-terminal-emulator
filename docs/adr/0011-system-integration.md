# ADR 0011 — System integration: package managers, metrics, notifications, audio

**Status:** Accepted (Phase V)  **Spec:** §5.3, §8.3, §11.2, §15 (Phase V, as rescoped)  **Builds on:** ADR 0003, ADR 0008, ADR 0009

Phase V was rescoped: it no longer integrates with Divinity OS. The
Seraphim Gallery and the extension SDK are out of scope. The package
managers people use replace Gabriel.

## Decisions

**Package activity is recognized, not reported.** No package manager
tells the terminal what it does, so the session recognizes them
(`seraphim-session/src/packages.rs`, pure and tested).

* **Command lines:** they come from shell integration (OSC 133 C).
  Without it, the session reads the foreground process's
  `/proc/PID/cmdline`, at most every 250 ms while output arrives.
* **Wrappers:** `sudo`, `doas`, `run0`, `pkexec`, `env`, `nice`,
  `ionice`, variable assignments, command lists and interpreters
  (`python3 /usr/bin/emerge`, `sh -c …`) are seen through.
* **Managers:** each is a parser of its commands into an operation
  (install, remove, upgrade, sync) and its packages, plus a reader of its
  progress lines:
  * pacman, zypper and apk `(n/m)`;
  * Portage `>>> Emerging (1 of 5)`;
  * dnf's columns and dnf5's `[n/m]`;
  * apt's `Unpacking` and `Setting up`, and its `Progress: [ 45%]`;
  * xbps, flatpak and brew lines.
* **Looking is not doing:** searches, queries and `--pretend` are left
  alone.

The session publishes `package.<op>.<started|progress|completed|failed>`
from the trusted `package` source, at most ten progress events a second.
Exit codes come only with shell integration; without it a run ends when
the shell is back in front, as `completed` with no exit code.
Reading output was preferred over hooks in each manager (pacman hooks,
apt's `DPkg::Post-Invoke`, Portage's bashrc). Those need root to install,
differ per distribution, and say nothing about which terminal ran the
command.

`gabriel.*` is gone. The `heavens` preset, the Package Activity panel,
tab progress, tab icons and the example extension (`package-glow`) use
`package.*`, and `package` is a reserved namespace.

**Metrics are files and one helper process, read on demand.** The
`Sampler` reads each source only when a binding, a panel or a script
asks, and caches it for 150 ms. Rates come from the difference between
two counter readings.

* **GPU:** AMD from `gpu_busy_percent` and VRAM sysfs; Intel from RC6 or
  gtidle idle residency.
* **NVIDIA:** one `nvidia-smi -lms 1000` per process, read on a thread,
  so the UI never waits for it.
* **Network and disk:** network from `/proc/net/dev` and the default
  route; disk from `/proc/diskstats` (whole disks only).
* **Temperatures:** CPU-driver hwmon, else the first thermal zone.

`network.online` and `network.offline` are polled every 2 s, and only
while someone subscribes.

**Notifications go to the desktop's service by its command-line
tools.** `notify-send`, else `gdbus` calling
`org.freedesktop.Notifications`. Both are spawned and reaped on a
thread. A D-Bus library was rejected: these tools are on every desktop,
and a notification is rare enough that a process per notification costs
nothing.

* **Program requests:** OSC 9, OSC 777;notify and OSC 99 (kitty's chunked
  form with base64) are parsed by the terminal core into
  `terminal.notification`. They always show.
* **Seraphim's own:** long commands, package managers and the bell
  notify only when out of sight (window unfocused, or pane not drawn).
* **Limits:** at most five go out every ten seconds. A package
  manager's notice replaces its command's generic "finished" notice.

**Audio uses the sound server's tools too.** Sounds play through the
first of `pw-play`, `paplay`, `aplay` and `ffplay` that starts. The
fallback bell is a tone Seraphim writes once as a WAV file.

* **Playback:** volumes are linear amplitudes, converted to PulseAudio's
  cubic scale for `paplay`. Streams are named Seraphim and marked
  `state.restore-props = false`. Otherwise WirePlumber would remember
  Seraphim's volume under the player's own name and apply it to that
  player's later streams, from any program.
* **Capture:** levels are read from the default output's monitor by
  `parec` (or `ffmpeg -f pulse`) at 24 kHz mono. At 11 kHz, resampling
  had already cut a 5 kHz tone by 13 dB. The thread starts the
  first time something reads a level, and restarts if the server goes
  away.
* **Analysis:** two one-pole filters split bass (< 250 Hz), mid and
  treble (> 2 kHz). Each band is an RMS in dB mapped to 0–1, rising at
  once and falling like a meter.
* **Bindings:** sampled every 33 ms with 60 ms smoothing unless they say
  otherwise.

Audio libraries (cpal, PipeWire bindings) were rejected: they add build
dependencies and a realtime thread for a feature most people leave off.

## Not done

* Exit codes for package managers run without shell integration.
* Per-process GPU usage, more than one GPU, network per interface.
* Notification actions (clicking a notification to focus the terminal).
