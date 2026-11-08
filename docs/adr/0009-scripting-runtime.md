# ADR 0009 — The scripting runtime as built

**Status:** Accepted (implemented in `crates/seraphim-script`)  **Spec:** §8, §11, §13  **Builds on:** ADR 0003, ADR 0005, ADR 0008

ADR 0005 proposed Lua on its own thread with permissions. This record
settles the details as implemented in Phase III.

## Decisions

**One script thread, one Lua state per extension.** Lua 5.4 is embedded
through `mlua` and compiled from source (`vendored`), so there's no system
Lua to depend on. All extensions share the thread `seraphim-script`, and
each gets its own state. Extensions can't see each other's globals, and one
that misbehaves can be stopped and restarted on its own. A thread per
extension would also isolate CPU use, but it costs memory, makes ordering
across extensions non-deterministic, and the time budget already bounds a
handler.

**Scripts send commands; they never touch the renderer or the terminal.**
The API functions queue `Command`s (set a parameter, add a layer, notify,
type text…). The window applies them at the start of its next frame, after
a wake-up through its event-loop proxy. The host validates every command
again, with the same validators as the TOML (`Background::validate`,
`Bind::validate`), so a script layer obeys exactly the rules of a
`[[background]]` table. Reading the terminal goes through a snapshot that
the window refreshes at most every 50 ms, and only while some extension
holds `terminal.read`.

**Events reach the thread through one subscription.** Its pattern is the
union of every handler's pattern (`Pattern::Any`), changed in place
(`PatternHandle`) as handlers come and go. An event therefore arrives once
even when several patterns match it. Delivery keeps the bus's guarantees:
a forwarder thread moves events into the script inbox and waits when 1,024
are queued. The bus then drops events for this one subscriber, and the
terminal I/O thread is never blocked.

**Priorities and cancellation live in the dispatcher (ADR 0003).**
Handlers run by priority across all extensions, in registration order
within one priority. A handler that returns `false` stops the rest. This
cancels only the event's delivery to other scripts: the terminal has
already acted on it, so a script cannot veto output or input.
Per-handler `throttle` delivers the latest event at the end of each
window, which is how scripts follow `terminal.output` cheaply.

**Budgets.** A global instruction hook, inherited by coroutines, checks a
deadline every 1,000 VM instructions. Each call into Lua gets one budget
(`scripting.budget_ms`, 50 ms by default); the main file gets four. The
sandbox wraps `pcall`, `xpcall` and `coroutine.resume`/`wrap` so the budget
error always propagates, and then turns other Rust-side errors into plain
strings for scripts. After three overruns an extension is disabled and
everything it registered is undone (`Command::Reset`). Each state also has
a memory limit.

Known limit: a single C function call (for example `string.rep` with a huge
count, or a pathological `string.find` pattern) runs without the hook. The
memory limit bounds the first. The second can stall only the script thread,
never the terminal.

**Sandbox.** The state loads base, table, string, math, utf8 and coroutine,
plus `os` reduced to `time`, `clock`, `date` and `difftime`. There is no
`io`, `debug`, `package`, `dofile`, `loadfile` or `string.dump`. `load` is
text-only, because crafted bytecode can break out of the VM. `require`
loads only Lua files from the extension's folder, and module names cannot
contain path components.

**Permissions (spec §8.5).**

* Effective permissions are those *requested* in the manifest *and*
  *granted* in `[extensions.<name>]`. The default grant is `visual`,
  `interface` and `metrics`, which only change what Seraphim shows.
* The rest need an explicit grant: `terminal.read`, `terminal.write`,
  `process.read`, `process.exec`, `filesystem`, `network` and `system`.
  The last three are narrowed further by `commands`, `paths` and `hosts`.
* API tables for missing permissions are absent from the state.
* `fs` checks paths after resolving symbolic links.
* `net` does not follow redirects, since a redirect could lead to a host
  that was never approved.
* Script layers may only use files from the extension's folder.

**Root sessions.** Privilege comes from process credentials (ADR 0003).
It is shown to scripts, but it never grants them anything (§11.5).
`terminal.send` is refused while the session runs as root, both in the
runtime and in the host, because typing into a root shell would hand the
extension root.

**Trusted provenance.** An extension's events must be named
`<extension>.*` and carry the source `extension.<name>`. Extension names
cannot be built-in namespaces (`terminal`, `session`, `gabriel`, …), so a
script cannot forge `session.privileged` or `gabriel.install.*`.
`animation.completed` is published by the runtime with source `animation`.

**Reactive properties reuse `[[bind]]` (ADR 0008).** `visual.bind`
produces exactly a `[[bind]]` table, which the host validates and adds to
the window's binding set. Bindings gained color targets
(`param = "color"`, with `colors` instead of `map`) and event-backed
sources (`terminal.last_exit_code`, `session.root`, …). The two §11.4
examples work the same from TOML and from Lua.

**Output at three levels (§8.4).** The session publishes:

* `terminal.output.raw`: the bytes.
* `terminal.output`: the printed text, collected by the terminal state
  while someone listens.
* `command.output`: the text printed while a command runs.

Semantic events stay as they were (`command.*`, `shell.*`). The costly
payloads are built only when `EventBus::wants` says someone subscribed.

**Live reload.** The runtime fingerprints each extension's Lua files and
manifest once a second when `live_reload` is on. It restarts only the
extension that changed. Reloading the configuration restarts all of them.

## Not done here

* WebAssembly extensions (ADR 0005), which are for third-party code from
  the Seraphim Gallery in Phase V.
* Custom panels and widgets drawn by scripts (Phase IV). Today scripts
  create background layers, notices, menu commands and shortcuts.
* Asynchronous handlers as coroutines that yield across callbacks; today
  callbacks are explicit.
