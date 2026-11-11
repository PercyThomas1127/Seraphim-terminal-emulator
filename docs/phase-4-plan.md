# Phase IV plan — Full Interface

**Status: complete**, including the items first left over: detachable
tabs, floating panels, window decorations, tab designs and periodic
session saves. As built: [ADR 0010](adr/0010-interface.md).

**Goal (spec §15):** build the complete terminal application.
**Milestone:** a complete terminal application.

Spec: §9 (windows, tabs, panes, panels, layouts), §7.3 (GUI
configuration), §18.3 (interface extensibility; ADR 0006 reserves
`ui.register_panel` with a retained widget tree).

## Shape

* **One renderer, many panes.** The text renderer keeps per-pane *slots*
  (instances, uniforms, images, cursor effects, theme overrides) and shares
  the glyph atlas, fonts and pipeline. Each pane draws into its own
  viewport. The visual engine stays one scene per window, behind all panes.
* **A workspace model** independent of the window (`workspace.rs`, pure and
  tested): tabs, each a binary split tree of panes. It handles rectangles,
  focus moves, resizing, zoom, and serialization for layouts and
  restoration.
* **Panes are terminals or panels.** A panel is a retained widget tree
  (text, list, gauge, sparkline, key-value, separator). Seraphim paints it
  into a cell grid and draws it with the same renderer, so it gets the same
  fonts, themes and reduced motion. Extensions describe widgets and never
  draw.
* **The configuration stays the source of truth** (§7.3). The settings
  panel edits `seraphim.toml` in place, keeping comments, and live reload
  applies the change, which is the live preview.

## Checklist

### 1. Multi-pane rendering
- [x] Renderer slots: per-pane draw state, shared atlas and pipeline
- [x] Draw each pane in its viewport; dividers and a focus accent

### 2. Workspace model
- [x] Tabs → split tree → panes; split, close, focus by direction, resize, swap, zoom
- [x] Pixel and cell rectangles for a window size (padding, dividers, tab bar)
- [x] Serializable (layouts, restoration)

### 3. Window on the workspace
- [x] Many sessions per window; input, mouse, selection, scrolling, IME and clipboard go to the focused pane
- [x] Rules, bindings and scripts follow the focused pane's session; events carry their session id
- [x] A pane whose shell exits closes; the last one closes the window

### 4. Tabs (§9.2)
- [x] Tab bar (shown with two or more tabs, or always/never by config), click, middle-click to close, drag to reorder
- [x] New, close, next/previous, move, Ctrl+Shift+1…9 (Konsole-style shortcuts)
- [x] Labels: title, working directory or program, with custom labels (rename) and a format string
- [x] Activity and bell indicators for background tabs; a progress indicator (OSC 9;4); root tabs in the alert color
- [x] Icons by foreground program (Nerd Font glyphs, with fallbacks)
- [x] Per-tab profiles: switching tabs switches theme and background, with the scene transition

### 5. Split panes (§9.1)
- [x] Split right/down, close pane, focus with Alt+arrows, resize with Ctrl+Alt+arrows, zoom a pane
- [x] Click to focus; drag dividers to resize
- [x] New panes start in the focused pane's directory

### 6. Panels (§9.3, §9.4)
- [x] Widget tree and painter (text, list, gauge, sparkline, key-value, separator; styles from the theme)
- [x] Built-in panels: system monitor, sessions, command history, package activity (Gabriel events)
- [x] Panels as panes (open, close, keyboard and mouse)
- [x] Extensions: `ui.panel(id, {title, render})`, `ui.refresh(id)`, `ui.open_panel(id)`, keys and clicks back to the extension

### 7. Layouts (§9.5)
- [x] `[[layout]]` in configuration: named trees of terminals (command, directory, profile) and panels
- [x] `--layout NAME`, `startup_layout`, a Layouts menu, `ui.open_layout` for extensions

### 8. Session restoration (§9.1)
- [x] Save tabs, splits, directories, profiles, labels and recent screen text on exit (and periodically)
- [x] Restore on start (`restore_session`), with the old screen text shown above the new prompt

### 9. GUI configuration (§7.3)
- [x] Settings panel: theme, font, size, opacity, preset, transition, reduced motion, menu and tab bars, profiles, with live preview through the config file
- [x] Extensions manager: enable or disable, permissions requested and granted
- [x] Edits keep the file's comments and layout (`toml_edit`)

### 10. Close-out
- [x] Detachable tabs: move a tab to a new window with its shells running (all windows share one process)
- [x] ADR 0010, README, docs, tests, real-window checks, `docs/progress.md`

### 11. Leftovers, done after the close-out
- [x] Floating panels: over the panes, moved by the title, resized by the corner, restored with the session
- [x] Window decorations: Seraphim's own title bar and buttons (or none), resizable edges
- [x] Tab designs: styles, position, sizes, buttons and colors
- [x] The session is saved every 30 seconds, so it survives a crash
