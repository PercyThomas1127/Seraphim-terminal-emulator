# Seraphim user guide

Everything Seraphim does or is meant to do, grouped by area, with how you
use each part. Everything here is built unless it is marked **Not built
yet** or **Planned** (see [section 8](#8-not-built-yet)).

Your settings live in `~/.config/seraphim/config.toml`. Run
`seraphim --print-default-config` for every setting with its
documentation. Changes apply when you save the file; **Ctrl+Shift+R**
reloads by hand.

## Contents

1. [Installing and starting](#1-installing-and-starting)
2. [The terminal itself](#2-the-terminal-itself)
3. [Images in the terminal](#3-images-in-the-terminal)
4. [Look: themes, backgrounds and presets](#4-look-themes-backgrounds-and-presets)
5. [Interface: menus, tabs, panes, panels, settings](#5-interface)
6. [Extensions (Lua)](#6-extensions-lua)
7. [Tools without a window](#7-tools-without-a-window)
8. [Not built yet](#8-not-built-yet)
9. [Keyboard shortcuts](#9-keyboard-shortcuts)

## 1. Installing and starting

```bash
cargo install --path crates/seraphim --profile dist --locked --root ~/.local
```

```bash
seraphim --install-desktop    # menu entry and icon
```

```bash
seraphim --install-terminfo   # 24-bit colour, undercurl and cursor shapes in tmux
```

`seraphim --uninstall-desktop` removes the menu entry and icon. Then
`seraphim` on its own opens a window running your shell.

### Command-line options

| Option | What it does |
|---|---|
| `--config PATH` | Use a different config file |
| `--profile NAME` | Start with a profile |
| `--preset NAME` | Start with a background preset |
| `--font FAMILY`, `--font-size PT` | Override the font |
| `--cols N`, `--rows N` | Override the window size |
| `--layout NAME` | Open a saved arrangement of tabs and panes |
| `--extension PATH` | Load an extension folder for this run only |
| `--allow-clipboard-read` | Let programs read your clipboard |
| `-e PROGRAM ARGS…` | Run that program instead of your shell |
| `--print-default-config` | Print every setting with its documentation |

## 2. The terminal itself

### Behaviour

- **Compatibility:** behaves like xterm (checked against 427 recorded
  xterm screens). vim, less, tmux and nano are tested with it.
- **Text:** 24-bit colour, ligatures and font shaping, emoji, Nerd Font
  icons, and every underline style (curly, dotted, coloured).
- **Resizing:** lines that wrapped are re-wrapped when you resize,
  history included.
- **Kitty keyboard protocol:** programs that ask for it can tell Ctrl+I
  from Tab and Esc from Alt.
- **Mouse:** mouse reporting for programs that use it. Hold **Shift** to
  select text anyway.
- **IME:** input methods for CJK languages.
- **TERM:** `terminal.term = "auto"` uses `xterm-seraphim` once the
  terminfo entry is installed. Hosts you ssh into need the entry too, or
  set `terminal.term = "xterm-256color"`.

### Selection and clipboard

- Drag to select, double-click for a word, triple-click for a line.
- Ctrl+drag selects a block. Shift+click extends the selection.
- **Ctrl+Shift+C** copies. **Ctrl+Shift+V** or **Shift+Insert** pastes.
  Middle-click pastes the current selection.
- Programs may set your clipboard (OSC 52). Reading it needs
  `clipboard.allow_read = true`.

### Scrolling and history

- Scroll with the wheel, **Shift+PgUp/PgDn**, **Shift+Home/End** or
  **Ctrl+Shift+Up/Down**.
- Each pane keeps 10,000 lines of history (`terminal.scrollback`).

### Shell integration

Add one line to your shell's rc file:

```bash
[[ $TERM_PROGRAM == seraphim ]] && source /path/to/seraphim/shell-integration/seraphim.bash
```

There are `.zsh` and `.fish` versions too in `shell-integration/`. It
lets Seraphim know when a prompt appears, when a command starts and
finishes (with exit code and duration), and when the directory changes.
The command history panel, reactions to failed commands and the Gabriel
effects all depend on it.

## 3. Images in the terminal

- **Protocols:** Kitty graphics and Sixel. Tools such as `kitten icat`,
  `chafa`, `img2sixel`, `timg`, yazi and gnuplot just work.
- **How images behave:** they scroll into history, can sit over or under
  the text, and Kitty animations play.
- **Inside tmux:** works once the terminfo entry is installed
  (`seraphim --install-terminfo`).

More in [image-tools.md](image-tools.md).

## 4. Look: themes, backgrounds and presets

Settings layer in this order, later ones winning:
**preset < theme < config.toml < active profile**.

### Themes

`theme = "celestial"`, `"monochrome"` or `"daylight"`, or your own
`themes/<name>.toml`. A `[colors]` section overrides individual colours.

### Background layers

Each `[[background]]` is a layer, listed bottom first:

| Kind | What it is |
|---|---|
| `solid` | A plain colour |
| `gradient` | A gradient, animated with `spin` and `flow` |
| `image` | PNG, JPEG, WebP or GIF. Animated GIF, WebP and APNG play. Supports blur, brightness, contrast and saturation |
| `video` | Anything ffmpeg can play, looped silently. Needs ffmpeg installed |
| `shader` | Built-in `aurora`, `starfield` or `heavens`, or your own WGSL file in `shaders/` |
| `blur` | Frosted glass over everything beneath it |

Every layer takes `opacity` and `blend` (`normal`, `add`, `multiply` or
`screen`).

```toml
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

### Presets

A preset bundles a scene with its reactions.

- `preset = "heavens"` is an animated sky. It turns red in a root shell
  and sends a wave of light when you install something.
- Its style is set on its `sky` layer: 1 aurora, 2 nebula, 3 starfield.

  ```toml
  preset = "heavens"

  [[background]]
  id = "sky"
  params = { style = 2 }   # the nebula
  ```

- Your own presets go in `presets/<name>.toml`: a file of
  `[[background]]`, `[[on]]` and `[visual]` keys.

### Reactions (`[[on]]`)

When an event happens, animate a layer property: pulse red when a
command fails, flash on the terminal bell. Easing can be `linear`,
`ease`, `ease-in`, `ease-out`, `ease-in-out`, `ease-out-expo`,
`ease-out-back`, `spring` or `instant`.

```toml
[[on]]
event = "command.completed"
when = { exit_code = 1 }
layer = "glow"
param = "pulse"
from = 1.0
to = 0.0
duration_ms = 600
easing = "ease-out"
```

### Bindings (`[[bind]]`)

A property follows a live value, eased in as it changes. Sources:

- CPU, memory, load and battery (`system.cpu`, `system.memory`,
  `system.load`, `battery.level`, `battery.charging`);
- the time of day (`time.hour`);
- the last exit code (`terminal.last_exit_code`);
- root, the current directory and whether a full-screen app is running
  (`session.root`, `session.cwd`, `terminal.alt_screen`);
- any field of an event (`event` plus `field`).

```toml
[[bind]]
source = "system.cpu"
layer = "sky"
param = "speed"
output = [0.6, 2.5]
```

### Transitions

`visual.transition` sets how scenes change: `fade`, `slide`, `wipe`,
`dissolve`, `zoom`, `blur`, `fade-through`, or your own `.wgsl` file.
`visual.transition_ms` sets the duration.

### Cursor (`[cursor]`)

The cursor glides between cells and fades when it blinks. Optional: a
glow, a trail and a ripple on every key press.

```toml
[cursor]
glide_ms = 80
glow = 0.6
ripple = true
trail = true
```

### Window and motion

- `visual.window_opacity` makes the window see-through (needs a
  compositor).
- `visual.reduced_motion = true` stops all animation.
- `visual.max_fps` caps the frame rate. Animation pauses when the window
  loses focus (`animate_unfocused` changes that).

### Profiles

- Profiles live in `profiles/<name>.toml` and can override any setting.
- Switch with `--profile`, **Ctrl+Shift+P** (the background crossfades),
  or the Profiles menu.
- `live_reload = false` turns off reloading on save; **Ctrl+Shift+R**
  reloads by hand.

## 5. Interface

### Menu bar

- Tap **Alt** to show it. It has File, Edit, View, Profiles, Settings
  and Help.
- **Ctrl+Shift+M** (or `window.menu_bar = true`) keeps it on screen.
- Add your own menus with `[[menu]]`. An item can run an action, type
  text into the shell, or switch profile:

  ```toml
  [[menu]]
  title = "&Tools"
  items = [
      { label = "&System Monitor", send = "htop\n" },
      { separator = true },
      { label = "&Work Profile", profile = "work" },
  ]
  ```

  A `[[menu]]` titled like a built-in one appends to it
  (`replace = true` replaces it).

### Tabs

| Action | Keys |
|---|---|
| New / close | Ctrl+Shift+T / Ctrl+Shift+W |
| Next / previous | Ctrl+PgDn / Ctrl+PgUp, Ctrl+Tab / Ctrl+Shift+Tab |
| Jump to a tab | Alt+1…9 |
| Move tab | Ctrl+Shift+PgUp / PgDn |
| Rename | Ctrl+Alt+S |
| Move to a new window | Ctrl+Shift+L, or drag the tab out of the window |

- Use the bar with the mouse: click to switch, middle-click or × to
  close, drag to reorder. Drop a tab outside the window and it gets a
  window of its own there, with its shells still running.
- Each tab shows the running program's icon.
- Marks for tabs out of sight: a dot for new output and `!` for a bell.
  Programs that report progress fill their tab.
- Root tabs are red.
- Choosing a profile while in a tab gives that tab its own theme and
  background.
- Settings in `[tabs]`: `bar = "auto"`, `"always"` or `"never"`;
  `icons`; and the `label` format (`{title}`, `{program}`, `{cwd}`,
  `{index}`).

#### Tab designs

| Setting | Values |
|---|---|
| `style` | `classic`, `powerline`, `rounded`, `slanted` (these three need a Nerd Font) or `minimal` (no backgrounds; the active tab is underlined) |
| `position` | `top` or `bottom` |
| `min_width`, `max_width` | Width of a tab in columns (default 6 and 32) |
| `close_button`, `new_tab_button` | Show the × and the + |
| `[tabs.colors]` | `bar`, `active_fg`, `active_bg`, `inactive_fg`, `inactive_bg`, `accent` as `"#rrggbb"`; unset ones follow the theme |

```toml
[tabs]
style = "powerline"
position = "bottom"

[tabs.colors]
active_bg = "#f5a6d0"
```

### Split panes

| Action | Keys |
|---|---|
| Split right / down | Ctrl+Shift+E / Ctrl+Shift+O |
| Move between panes | Alt+arrows |
| Resize | Ctrl+Alt+arrows |
| Zoom / close | Ctrl+Shift+Z / Ctrl+Shift+X |

- Drag dividers to resize and click a pane to focus it.
- Equal Pane Sizes is in the View menu.
- New panes open in the current directory, and panes without focus are
  dimmed (`[panes] divider`, `dim_inactive`).

### Panels

Panels open as panes from the View menu:

- **System Monitor:** CPU, memory and load.
- **Sessions:** your open terminals.
- **Command History:** Enter types the selected command again.
- **Package Activity:** Gabriel installs.

Extensions can add their own (see [section 6](#6-extensions-lua)). In a
panel, Up and Down move, Enter or a click chooses, and Escape closes it.

**Floating panels.** View → Float or Dock Panel lifts the focused panel
out of its tab and floats it over the panes (and puts a floating one
back). Extensions can open theirs floating with
`ui.open_panel(id, { place = "float" })`.

- Drag the title to move it and the ◢ corner to resize it. × closes it.
- Clicking a floating panel brings it to the front and gives it the
  keyboard. Clicking a terminal gives the keyboard back.
- Floating panels stay up when you switch tabs, and session restore
  brings them back where they were.

### Window decorations

`[window] decorations` sets who draws the window's frame. It takes
effect for new windows.

| Value | What you get |
|---|---|
| `system` (default) | The desktop's title bar and borders |
| `seraphim` | Seraphim's own title bar with minimize, maximize and close buttons. When the tab bar (or a pinned menu bar) is at the top, the buttons go at its right end and there is no separate title row |
| `none` | No title bar, for tiling window managers |

With `seraphim`, drag empty space in the title bar (or tab bar) to move
the window, and double-click it to maximize or restore. With `seraphim`
or `none`, drag the window's edges and corners to resize it.

### Windows

New Window (Ctrl+Shift+N) and detached tabs open in the same Seraphim
process, so tabs can move between windows with their programs running.

### Settings

**Ctrl+Shift+,** (or Settings → Configure Seraphim) opens the settings
panel.

- Change the theme, preset, font, size, opacity, transitions, bars,
  effects and profiles.
- Turn extensions on or off and see their permissions.
- Each change is written into your config file with its comments kept,
  and applies at once.

### Layouts

A layout is a named arrangement of tabs and panes. Each pane can run the
shell, run a command, start in a directory, or show a panel (`system`,
`sessions`, `history`, `packages`, `settings`, or an extension's
`name:id`).

```toml
[[layout]]
name = "dev"

[[layout.tab]]
label = "code"
pane = { split = "right", ratio = 0.6, first = { cwd = "~/src" }, second = { split = "down", first = { panel = "system" }, second = { panel = "history" } } }
```

Open one from the Settings menu, with `--layout NAME`, or at startup
with `startup_layout = "dev"` at the top of the config.

### Session restore

With `restore_session = true` (at the top of the config), Seraphim saves
every window's tabs, splits, directories, labels, floating panels and the
last 200 lines of each terminal. It saves every 30 seconds and when you
quit, so even a crash loses little. The next start reopens them all,
with the old text shown above a new prompt. Programs are not re-run, and
a `seraphim -e PROGRAM` window never replaces the saved session.

## 6. Extensions (Lua)

### Where they go

- A folder with `extension.toml` and `init.lua` goes in
  `~/.config/seraphim/extensions/`.
- A single file goes in `~/.config/seraphim/scripts/`.

```lua
-- scripts/fail.lua: tint the window red after a failed command.
visual.add_layer({ id = "fail", kind = "solid", color = "#ff2255", opacity = 0 })
seraphim.on("command.completed", function(event)
  local failed = (event.exit_code or 0) ~= 0
  visual.set_opacity("fail", failed and 0.25 or 0, { duration = 400 })
end)
```

### What extensions can do

| Area | Functions |
|---|---|
| Events | `seraphim.on(...)` for output, commands, prompts, focus, tabs, progress, Gabriel installs and custom events; timers; storage that persists; publishing their own events |
| Visuals | Add or remove layers; set parameters, colours and opacity; tweens and animations; bind properties to live values; switch profiles |
| Interface | Notices; commands in the Extensions menu; shortcuts (`ui.key`); built-in actions; panels (`ui.panel`); opening layouts |
| Terminal | Read the screen, cursor, title, directory and last command; type into the terminal (never into a root shell) |
| System | Metrics; desktop notifications; running programs (`process.run`); files (`fs`); HTTP requests (`net`) |

Panel widgets: heading, text, key-value rows, gauge, sparkline, list and
separator.

### Safety

- Each extension gets only the permissions it asks for *and* that you
  grant in `[extensions.NAME]`. By default those are `visual`,
  `interface` and `metrics`.
- Programs, file paths and network hosts each need an allow-list:

  ```toml
  [extensions.git-status]
  grant = ["visual", "interface", "terminal.read", "process.exec"]
  commands = ["git"]
  ```

- Each extension runs in a sandbox with time and memory limits
  (`[scripting] budget_ms`, `memory_mb`). One that misbehaves is
  disabled, and the terminal never waits for it.
- `disabled = true` in `[extensions.NAME]` turns one off.

Examples in [`examples/extensions/`](../examples/extensions): error-flash,
exit-color, cpu-glow, gabriel-glow and git-panel. The full reference is
[scripting.md](scripting.md).

## 7. Tools without a window

| Command | What it does |
|---|---|
| `seraphim run [--events] -- PROGRAM` | Runs a program without a window, then prints the final screen and every event |
| `seraphim replay FILE` | Replays a recorded byte stream, for reproducing display bugs |
| `seraphim bench [--json]` | Measures this machine's speed |
| `seraphim bench --window` | Measures in a real window, including startup time and keystroke latency |
| `seraphim bench --list-gpus` | Lists your GPUs |

Environment variables:

- `SERAPHIM_GPU=name` picks a GPU by any part of its name.
- `SERAPHIM_TIMING=1` logs startup time and keystroke latency during
  normal use.
- `SERAPHIM_DEBUG_INPUT=1` logs raw input events.
- `WGPU_BACKEND=gl` forces OpenGL instead of Vulkan.

Results on real hardware are in [benchmarks.md](benchmarks.md).

## 8. Not built yet

### Phase V: Divinity OS integration (planned)

- **Gabriel:** the effects react to `gabriel.install.*` events, but
  nothing connects to the real package manager yet.
- **System monitoring:** GPU and network monitoring are missing (CPU,
  memory, load and battery work).
- **Audio:** sound reactions and `audio.*` events. Not started.
- **Desktop integration:** notifications go through `notify-send`.
  Deeper desktop integration is not started.
- **Seraphim Gallery:** a store for sharing themes, presets and
  extensions. Not started.
- **Extension SDK:** packaging and publishing tools, and WebAssembly
  extensions so untrusted Gallery code can run safely. Not started.

### Other gaps

- Contrast checks for accessibility.
- Testing on macOS, Windows, Wayland and Intel or laptop GPUs.

## 9. Keyboard shortcuts

| Action | Keys |
|---|---|
| Copy / paste | Ctrl+Shift+C / Ctrl+Shift+V, Shift+Insert |
| Select all | Ctrl+Shift+A |
| Clear and reset | Ctrl+Shift+K |
| Scroll history | Shift+PgUp/PgDn, Shift+Home/End, Ctrl+Shift+Up/Down |
| Font size bigger / smaller / reset | Ctrl+Shift+= / Ctrl+Shift+− / Ctrl+Shift+0 |
| Fullscreen | F11 |
| Show the menu bar | Alt (tap); Ctrl+Shift+M keeps it shown |
| Next profile / reload config | Ctrl+Shift+P / Ctrl+Shift+R |
| Settings | Ctrl+Shift+, |
| New / close window | Ctrl+Shift+N / Ctrl+Shift+Q |
| New / close tab | Ctrl+Shift+T / Ctrl+Shift+W |
| Next / previous tab | Ctrl+PgDn / Ctrl+PgUp, Ctrl+Tab / Ctrl+Shift+Tab |
| Jump to tab | Alt+1…9 (with two or more tabs) |
| Move tab | Ctrl+Shift+PgUp / PgDn |
| Rename tab | Ctrl+Alt+S |
| Move tab to a new window | Ctrl+Shift+L (with two or more tabs) |
| Split right / down | Ctrl+Shift+E / Ctrl+Shift+O |
| Move between panes | Alt+arrows (with two or more panes) |
| Resize pane | Ctrl+Alt+arrows |
| Zoom / close pane | Ctrl+Shift+Z / Ctrl+Shift+X |

Scrolling shortcuts are passed to full-screen programs (vim, less) while
they run. Alt+digit and Alt+arrows go to the shell when there is only
one tab or pane. Extensions can add their own shortcuts with `ui.key`.
