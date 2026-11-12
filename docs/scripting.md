# Scripting Seraphim with Lua

Extensions are small Lua 5.4 programs that react to what happens in the
terminal and change how it looks and behaves. Each one runs in its own
sandbox on a script thread, so a slow or broken script can never freeze
the terminal or crash it.

## A first extension

Create `~/.config/seraphim/extensions/hello/extension.toml`:

```toml
name = "hello"
version = "1.0.0"
description = "Flash when a command fails."
api = 1
permissions = ["visual", "interface"]
```

and `~/.config/seraphim/extensions/hello/init.lua` next to it:

```lua
visual.add_layer({ id = "fail", kind = "solid", color = "#ff2255", opacity = 0 })

animation.define("fail_flash", {
  { layer = "fail", param = "opacity", from = 0.4, to = 0, duration = 800 },
})

seraphim.on("command.completed", function(event)
  if event.exit_code ~= 0 then
    animation.play("fail_flash")
    ui.notify(event.command .. " failed with " .. event.exit_code)
  end
end)
```

Open a new Seraphim window (or press Ctrl+Shift+R to reload). Exit codes
come from shell integration (see the README).

* A one-file script goes in `~/.config/seraphim/scripts/NAME.lua`, without
  a manifest. It gets the default permissions only.
* `seraphim --extension PATH` loads an extension folder or `.lua` file from
  anywhere, which is handy while writing one.
* With `live_reload = true`, saving a file restarts that extension.
* `print` and `seraphim.log` write to Seraphim's standard error. Errors
  appear in the corner of the window with their file and line.

The examples in [`examples/`](../examples) are complete extensions: an
error flash (spec §8.4), package install reactions (§8.3), exit-code
colors and CPU-driven intensity (§11.4), and a one-file clock.

## The manifest

| Field | Meaning |
|---|---|
| `name` | Lowercase letters, digits, `-`, `_`. It can't be a built-in event namespace such as `terminal` or `package`. Default: the folder name. |
| `version`, `description`, `authors` | For people. |
| `api` | The scripting API version it was written for. This Seraphim provides `1`. |
| `permissions` | What it asks for (below). |
| `main` | Entry file inside the folder. Default: `init.lua`. |

## Permissions

An extension gets the permissions it **asks for** in its manifest **and**
that you **grant** in `seraphim.toml`. When you grant nothing, it gets the
harmless defaults: `visual`, `interface` and `metrics`. API tables for
permissions it doesn't hold don't exist in its Lua state. A root shell in
the terminal never gives an extension more permissions.

| Permission | Allows | Extra settings |
|---|---|---|
| `visual` | Layers, parameters, colors, animations, bindings, profile switches | |
| `interface` | Notices, menu commands, keyboard shortcuts, built-in actions | |
| `metrics` | CPU, memory, load, battery, time of day | |
| `terminal.read` | Screen text, cursor, title, directory, last command and exit code | |
| `terminal.write` | Typing into the terminal (refused while the session runs as root) | |
| `process.read` | The foreground process and the shell (pid, name, directory) | |
| `process.exec` | Running programs | `commands` |
| `filesystem` | Reading and writing files | `paths` |
| `network` | HTTP(S) requests (redirects are not followed) | `hosts` |
| `system` | Desktop notifications | |

```toml
[extensions.git-status]
grant = ["visual", "interface", "terminal.read", "process.exec"]
commands = ["git"]                 # names, or full paths; "*" allows any
settings = { show_branch = true }  # seraphim.settings in Lua

[extensions.weather]
grant = ["interface", "network"]
hosts = ["api.weather.example", "*.cdn.example"]

[extensions.notes]
grant = ["interface", "filesystem"]
paths = ["~/notes"]

[extensions.noisy]
disabled = true
```

When an extension asks for something you haven't granted, Seraphim says
so once at startup and runs it without that permission.

## Limits

* **Time.** A handler, timer or callback may run for `scripting.budget_ms`
  (default 50 ms), and the main file for four times that. Then it is
  stopped. `pcall` can't catch that error. After three overruns the
  extension is disabled until it is changed or the configuration is
  reloaded.
* **Memory.** `scripting.memory_mb` (default 64) per extension.
* **Sandbox.** No `io`, `debug`, `package`, `dofile`, `loadfile` or
  `string.dump`. `os` only has `time`, `clock`, `date` and `difftime`.
  `load` accepts source text only, never bytecode. `require("a.b")` loads
  `a/b.lua` from the extension's own folder.
* Long-running work such as programs and HTTP requests runs in the
  background and calls you back. Nothing blocks.

## Events

`seraphim.on(pattern, handler, options)` calls `handler(event)` for every
matching event. A pattern is an exact name, `prefix.*`, or `*`.

```lua
seraphim.on("terminal.output", function(event)
  -- event.name, event.timestamp, event.source, event.session, event.data
  -- Data fields are also readable directly: event.text == event.data.text
  -- (except name, timestamp, source and session, which are the event's own).
end, { priority = 10, throttle = 100 })
```

* `priority`: higher runs first, across all extensions. Default 0.
* Return `false` from a handler to **cancel**: handlers with a lower
  priority don't see that event. Cancelling only affects extensions;
  the terminal itself has already acted.
* `throttle = ms`: at most one call per window. The latest event that
  arrived in between is delivered when the window ends.

| Event | Data |
|---|---|
| `terminal.output` | `{bytes, text}`: printed text, without escape sequences, with `\n` for line feeds |
| `terminal.output.raw` | `{bytes, data}`: the raw bytes, as lossy UTF-8 |
| `command.output` | `{command, text}`: text printed while a command runs |
| `command.started` | `{command}` |
| `command.completed` | `{command, exit_code, duration_ms}` |
| `shell.prompt` | `{}` |
| `terminal.text.changed` | `{}`: the screen changed |
| `terminal.cursor.moved` | `{row, col}`: 0-based |
| `terminal.title.changed` | `{title}` |
| `terminal.bell` | `{}` |
| `terminal.notification` | `{title, body}`: the program asked for a desktop notification (OSC 9, OSC 777;notify, OSC 99) |
| `notification.sent` | `{title, body, reason, shown}`: Seraphim sent a desktop notification; `reason` is `program`, `command`, `package` or `bell` |
| `terminal.screen.alternate` | `{active}`: a full-screen app started or stopped |
| `session.created`, `session.exited`, `session.closed` | `{pid, cols, rows}`, `{exit_code, signal}`, `{}` |
| `session.privileged` | `{root}`: from process credentials, never from output |
| `session.cwd.changed` | `{cwd}` |
| `network.online`, `network.offline`, `network.changed` | `{online}`: the default route came or went |
| `interface.focus.changed` | `{focused}` |
| `interface.resized` | `{cols, rows, width, height}` |
| `interface.menu.toggled` | `{open}` |
| `interface.tab.changed` | `{tab, from, tabs}`: 0-based |
| `interface.pane.focused` | `{pane, tab}` |
| `terminal.progress` | `{state, percent}`: OSC 9;4 (0 clear, 1 normal, 2 error, 3 indeterminate, 4 paused) |
| `visual.profile.changed` | `{profile}` |
| `animation.completed` | `{name, extension}` |
| `package.<op>.started`, `.progress`, `.completed`, `.failed` | A package manager at work. `<op>` is `install`, `remove`, `upgrade` or `sync`. Data: `{manager, operation, packages, package, done, total, percent}`, plus `{exit_code, duration_ms}` at the end (`exit_code` is `nil` without shell integration). Managers: emerge (Portage), pacman, paru, yay, pikaur, apt, apt-get, aptitude, nala, dnf, yum, zypper, xbps, apk, eopkg, nix, flatpak, snap, brew |
| `<extension>.*` | Custom events from extensions |

The output and cursor events are only produced while someone listens for
them. Command and prompt events need shell integration.

## API reference

Each table exists only with the permission shown.

### `seraphim` (always)

| Function | |
|---|---|
| `seraphim.on(pattern, fn, {priority, throttle}) → id` | Subscribe. |
| `seraphim.off(id) → bool` | Unsubscribe. |
| `seraphim.emit(name, data)` | Publish a custom event. The name must start with `<extension name>.`; its source is `extension.<name>`. |
| `seraphim.after(ms, fn) → id` | Run once, later. |
| `seraphim.every(ms, fn) → id` | Run repeatedly (at least every 16 ms). |
| `seraphim.cancel(id) → bool` | Stop a timer. |
| `seraphim.log(...)` | Write to standard error. |
| `seraphim.has(permission) → bool` | Whether it holds a permission. |
| `seraphim.storage.get(key)`, `.set(key, value)` | Persistent JSON values (1 MiB), in `~/.local/share/seraphim/extensions/<name>/`. `nil` deletes. |
| `seraphim.name`, `.version`, `.api_version`, `.permissions`, `.settings` | Facts. |

### `visual` and `animation` (`visual`)

Options tables take `duration` (ms), `easing` (`linear`, `ease`,
`ease-in`, `ease-out`, `ease-in-out`, `ease-out-expo`, `ease-out-back`,
`spring`, `instant`) and, where it makes sense, `layer`, `from`, `name`.

| Function | |
|---|---|
| `visual.set_parameter(name, value, {layer, from, …})` | A shader parameter (or `opacity`). Without `layer`: every layer that has it. |
| `visual.set_opacity(layer, value, {from, …})` | |
| `visual.set_color(color, {layer, name, …})` | `#rrggbb[aa]` or a theme color (`theme.background`, `theme.bright.1`, …). `name`: a palette color of a shader, a gradient stop (`"0"`, `"1"`, …), or `color` for a solid layer. Defaults: the bottom layer's first color. |
| `visual.add_layer(table, {…})` | A layer with the fields of `[[background]]` (`id` required). It replaces the layer with the same id. Files must be inside the extension's folder. Script layers survive profile switches. |
| `visual.remove_layer(id, {…})` | |
| `visual.set_background(profile)` | Switch profile. |
| `visual.bind(target, spec) → id` | A reactive property, below. |
| `visual.unbind(id)` | |
| `animation.tween(layer, param, to, {…})` | One property. |
| `animation.define(name, steps)` | Each step has `layer`, `param` (or `color` and `name`), `to`, `from`, `delay`, `duration`, `easing`. |
| `animation.play(name)`, `animation.stop(name)` | Playing again restarts it. When it ends, `animation.completed` is published. |

Reactive properties update by themselves as their source changes (spec
§11.4):

```lua
visual.bind("sky.intensity", { source = "system.cpu", min = 0.1, max = 1.0 })

visual.bind("base.color", {
  source = "terminal.last_exit_code",
  mapping = { [0] = "#f5a6d0", ["default"] = "#ff5577" },
})

visual.bind("sky.alert", { event = "session.privileged", field = "root", mapping = { ["true"] = 1, ["default"] = 0 } })
```

* `target` is `layer.parameter`, `layer.opacity`, `layer.color` or
  `layer.color.<name>`.
* `source` can be `system.cpu`, `system.memory`, `system.load`,
  `system.gpu`, `system.gpu.memory`, `system.gpu.temp`, `system.temp`,
  `network.rx`, `network.tx`, `network.online`, `disk.read`,
  `disk.write`, `battery.level`, `battery.charging`, `time.hour`,
  `terminal.last_exit_code`, `session.root`, `session.cwd` or
  `terminal.alt_screen`. You can also pass `event` and `field` instead.
* The value maps with `input = {a, b}` onto `min`/`max`, or through
  `mapping` (numbers, or colors for a color target).
* `smoothing` (ms) eases each change. `interval` (ms) sets how often a
  metric is sampled.

These are the same bindings as `[[bind]]` in TOML.

### `terminal` (`terminal.read`, `terminal.write`)

| Function | |
|---|---|
| `terminal.text()`, `terminal.lines(first, last)`, `terminal.line(n)` | The visible screen (1-based rows, trailing blanks trimmed). |
| `terminal.cursor() → {row, col}`, `terminal.size() → {cols, rows}` | 1-based cursor. |
| `terminal.title()`, `terminal.cwd()`, `terminal.alt_screen()`, `terminal.selection()` | |
| `terminal.last_command()`, `terminal.last_exit_code()`, `terminal.running_command()` | Need shell integration. |
| `terminal.root()` | The session runs as root. |
| `terminal.send(text)` | Type into the terminal (`terminal.write`; refused when root). |

The screen is a snapshot, at most 50 ms old, never the live grid.

### `ui` (`interface`)

| Function | |
|---|---|
| `ui.notify(text)` | A notice in the corner of the window. |
| `ui.command(id, {title, run})` or `ui.command(id, fn)` | A command. It appears in the menu bar's **Extensions** menu. |
| `ui.key(chord, command_id or fn)` | A shortcut such as `ctrl+shift+e`, `alt+enter` or `f5`. Extension shortcuts take precedence over the built-in ones; a bare letter is refused. |
| `ui.action(name)` | A built-in action: `copy`, `paste`, `zoom-in`, `new-tab`, `split-right`, `close-pane`, `panel-system`, `settings`, … (the menu actions). |
| `ui.panel(id, {title, render, on_select, on_key, interval})` | A panel (below). It appears in the Extensions menu. |
| `ui.open_panel(id, {place})` | Show it: `place` is `right` (default), `down`, `tab` or `float` (over the panes; drag its title to move it, its corner to resize it). |
| `ui.refresh(id)` | Render an open panel again now. |
| `ui.open_layout(name)` | Open a `[[layout]]`. |

#### Panels

A panel is a pane that shows widgets your `render` function returns.
Seraphim lays them out and draws them with the terminal's fonts and theme;
extensions describe what to show and never draw. `render` is called when
the panel opens, after `on_select` and `on_key`, on `ui.refresh`, and every
`interval` ms (at least 100) while the panel is open.

```lua
ui.panel("builds", {
  title = "Builds",
  interval = 2000,
  render = function()
    return {
      { type = "heading", text = "CI" },
      { type = "gauge", label = "Progress", value = 0.4 },
      { type = "sparkline", label = "Load", values = { 1, 3, 2, 5 } },
      { type = "kv", items = { { "Branch", "main" }, { "Commit", "590c0fb" } } },
      { type = "separator" },
      { type = "list", items = {
          { text = "build", detail = "ok", style = "good" },
          { text = "test", detail = "failed", style = "alert" },
      } },
      { type = "text", text = "Enter re-runs the selected job.", style = "dim" },
    }
  end,
  on_select = function(n) --[[ n-th list item, 1-based ]] end,
  on_key = function(key, selected) --[[ "r", "left", "enter"… ]] end,
})
ui.open_panel("builds", { place = "down" })
```

Widgets: `heading {text}`, `text {text, style}`, `kv {items}`,
`gauge {label, value 0-1, text, style}`, `sparkline {label, values, max}`,
`list {items = {{text, detail, style}}}`, `separator`. Styles: `normal`,
`dim`, `bold`, `accent`, `good`, `warn`, `alert`. In a panel, Up and Down
move the selection, Enter and clicks choose, and Escape closes it.
[`examples/extensions/git-panel`](../examples/extensions/git-panel) is a
complete one.

### `system` (`metrics`, `system`)

`system.cpu()`, `system.memory()` (percent), `system.load()` (per core),
`system.battery() → {level, charging}` or `nil`, `system.hour()`,
`system.temperature()` (CPU, °C), `system.gpu() → {busy, memory,
temperature}` (percent, percent, °C; AMD and Intel from sysfs, NVIDIA from
`nvidia-smi`), `system.network() → {rx, tx, online}` (KiB/s) and
`system.disk() → {read, write}` (KiB/s). Readings the machine doesn't
have are `nil`. With
`system`: `system.notify(title, body)` shows a desktop notification
through `notify-send`.

### `process` (`process.read`, `process.exec`)

* `process.foreground()`, `process.shell() → {pid, name, cwd}`.
* `process.run({"git", "status"}, {timeout, cwd, input}, function(r) end)`:
  `r` is `{ok, code, stdout, stderr}` or `{ok = false, error}`. Output is
  capped at 1 MiB. There's no shell: pass arguments one by one.

### `fs` (`filesystem`)

`fs.read(path)`, `fs.write(path, text)`, `fs.append(path, text)`,
`fs.list(dir)`, `fs.exists(path)`, `fs.remove(path)`, `fs.data_dir`.
Paths are allowed under `paths` and the extension's data folder; the
extension's own folder is readable. Relative paths are relative to the
data folder. Symbolic links can't lead outside.

### `net` (`network`)

* `net.get(url, function(r) end)`, `net.post(url, body, fn)`.
* `net.request({url, method, headers, body, timeout}, fn)`.
* `r` is `{ok, status, headers, body}` or `{ok = false, error}`.
* Only `http`/`https` URLs to the granted `hosts` are allowed. Redirects
  come back as responses and are not followed.
