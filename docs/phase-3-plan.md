# Phase III plan — Programmability

**Status: complete.** As built: [ADR 0009](adr/0009-scripting-runtime.md);
user guide and API reference: [scripting.md](scripting.md).

**Goal (spec §15):** make Seraphim programmable. **Milestone:** a
programmable terminal environment.

Builds on ADR 0003 (event bus), ADR 0005 (scripting) and ADR 0008 (the
declarative `[[on]]`/`[[bind]]` layer that Lua reuses).

## Shape

* A new crate, `seraphim-script`, embeds **Lua 5.4** (`mlua`, vendored).
* **Extensions** are folders: `extensions/<name>/extension.toml` (name,
  version, `api = 1`, requested permissions) plus `init.lua`. A single
  `scripts/*.lua` file is a one-file extension with no permissions beyond
  the defaults.
* **One script thread**, never the render or I/O thread. Each extension
  gets its **own Lua state** (no shared globals) with only the API tables it
  was granted. The thread subscribes to the bus directly and sends
  *commands* (set parameter, add layer, notify, write to the terminal…)
  back to the window, which applies them at frame start.
* **Budgets:** an instruction hook aborts a handler after 50 ms; three
  aborts disable the extension with a notice. A memory limit per state.
* **Permissions:** effective = requested (manifest) ∩ granted (user
  config). Nothing is granted automatically, and a root session never
  widens them (spec §8.5, §11.5).

## Checklist

### 1. Runtime
- [x] `seraphim-script` crate: script thread, one sandboxed Lua state per extension
- [x] Sandbox: no `io`, `os`, `debug`, native `require`; `require` loads only Lua files from the extension's folder; safe `os.time`/`os.clock`/`os.date`
- [x] Instruction-count budget, memory limit, disable after repeated faults
- [x] Discovery (`extensions/`, `scripts/`), manifests, `api` version check
- [x] Errors reported as notices with file:line; one broken extension never affects the others
- [x] Live reload of a changed extension; full restart on config reload

### 2. Scripting API (spec §8.2, §8.3, §8.4, §11.4)
- [x] `seraphim.on(pattern, fn, {priority, throttle_ms})`, `seraphim.off`, `seraphim.emit` (own namespace only), `seraphim.after`/`every` timers, `seraphim.log`, `seraphim.api_version`
- [x] Event tables `{name, timestamp, source, session, data}`; data fields also readable directly (`event.text`)
- [x] `visual.*`: `set_parameter`, `set_opacity`, `set_color`, `add_layer`/`remove_layer` (custom visual layers, same fields as `[[background]]`), `set_background` (profile or preset), `bind` (reactive properties)
- [x] `animation.*`: `tween`, `define`/`play` of named sequences, `animation.completed` events
- [x] `terminal.*` (terminal.read): read-only snapshot — lines, cursor, title, cwd, last exit code, alt screen, selection; `terminal.send` (terminal.write)
- [x] `ui.*`: `notify`, `command` (register named commands), `key` (register shortcuts), menu items under an Extensions menu
- [x] `system.*`: CPU, memory, load, battery, time (shared with `[[bind]]`)
- [x] `process.*`: foreground process metadata (process.read); `process.run` asynchronous with callback (process.exec, allow-listed commands)
- [x] `fs.*`: read, write, list within approved paths (filesystem); the extension's own data folder always allowed
- [x] `net.*`: HTTP GET/POST to approved hosts (network)

### 3. Events (spec §11.2, §11.3)
- [x] New events: `command.output` (parsed text between OSC 133 C and D), `terminal.text.changed`, `terminal.cursor.moved`, `animation.completed`, `interface.focus.changed`, `interface.resized`, `interface.menu.toggled`
- [x] `terminal.output` carries text (parsed, printable), `terminal.output.raw` the bytes — the three levels spec §8.4 asks for (raw, parsed, semantic `command.*`/`shell.*`)
- [x] Priorities and cancellation within the dispatcher (a handler returning `false` stops lower-priority handlers)
- [x] Rate limiting per handler, coalescing of high-rate events
- [x] Custom events, with the extension name as the trusted source

### 4. Reactive properties
- [x] `visual.bind(target, {source, min, max, mapping})` on the same engine as `[[bind]]`
- [x] Bindings to colors (`mapping` to `#rrggbb`), e.g. `terminal.last_exit_code` → background color (spec §11.4 example)
- [x] New sources: `terminal.last_exit_code`, `session.root`, any event field

### 5. Permissions (spec §8.5)
- [x] Manifest `permissions = [...]`; user grants in `[extensions.<name>]` (`grant`, `paths`, `hosts`, `commands`)
- [x] Ungranted API tables are absent; a call that needs a missing permission fails with a clear error
- [x] Visual, terminal read, terminal write, process read, filesystem, network, process execution, system integration, interface
- [x] A notice lists what an extension requested but was not granted

### 6. Close-out
- [x] Example extensions: the spec's Gabriel example, an error-flash on `ERROR` output, an exit-code color binding, a clock/command example
- [x] ADR 0009 (scripting runtime as built), README scripting guide, API reference
- [x] Tests: unit tests per API, sandbox escape attempts, budget/abort, permission denial, end-to-end in a real window
- [x] Update `docs/progress.md`

## Notes from building it

* Cancellation stops delivery to other scripts only; the terminal has
  already acted on the event. Truly vetoable "before" events (e.g. a
  paste) need a synchronous round trip and are left for when a concrete
  use appears.
* `system` currently means desktop notifications; other "approved system
  APIs" (D-Bus, audio) come with Phase V's integrations.
* A single C call (`string.rep`, pathological patterns) can outrun the
  instruction hook; memory limits bound the first, and either only stalls
  the script thread.
