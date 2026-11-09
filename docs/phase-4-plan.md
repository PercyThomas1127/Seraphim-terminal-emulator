# Phase IV plan — Full Interface

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
- [ ] Renderer slots: per-pane draw state, shared atlas and pipeline
- [ ] Draw each pane in its viewport; dividers and a focus accent

### 2. Workspace model
- [ ] Tabs → split tree → panes; split, close, focus by direction, resize, swap, zoom
- [ ] Pixel and cell rectangles for a window size (padding, dividers, tab bar)
- [ ] Serializable (layouts, restoration)

### 3. Window on the workspace
- [ ] Many sessions per window; input, mouse, selection, scrolling, IME and clipboard go to the focused pane
- [ ] Rules, bindings and scripts follow the focused pane's session; events carry their session id
- [ ] A pane whose shell exits closes; the last one closes the window

### 4. Tabs (§9.2)
- [ ] Tab bar (shown with two or more tabs, or always/never by config), click, middle-click to close, drag to reorder
- [ ] New, close, next/previous, move, Ctrl+Shift+1…9 (Konsole-style shortcuts)
- [ ] Labels: title, working directory or program, with custom labels (rename) and a format string
- [ ] Activity and bell indicators for background tabs; a progress indicator (OSC 9;4); root tabs in the alert color
- [ ] Icons by foreground program (Nerd Font glyphs, with fallbacks)
- [ ] Per-tab profiles: switching tabs switches theme and background, with the scene transition

### 5. Split panes (§9.1)
- [ ] Split right/down, close pane, focus with Alt+arrows, resize with Ctrl+Alt+arrows, zoom a pane
- [ ] Click to focus; drag dividers to resize
- [ ] New panes start in the focused pane's directory

### 6. Panels (§9.3, §9.4)
- [ ] Widget tree and painter (text, list, gauge, sparkline, key-value, separator; styles from the theme)
- [ ] Built-in panels: system monitor, sessions, command history, package activity (Gabriel events)
- [ ] Panels as panes (open, close, keyboard and mouse)
- [ ] Extensions: `ui.panel(id, {title, render})`, `ui.refresh(id)`, `ui.open_panel(id)`, keys and clicks back to the extension

### 7. Layouts (§9.5)
- [ ] `[[layout]]` in configuration: named trees of terminals (command, directory, profile) and panels
- [ ] `--layout NAME`, `startup_layout`, a Layouts menu, `ui.open_layout` for extensions

### 8. Session restoration (§9.1)
- [ ] Save tabs, splits, directories, profiles, labels and recent screen text on exit (and periodically)
- [ ] Restore on start (`restore_session`), with the old screen text shown above the new prompt

### 9. GUI configuration (§7.3)
- [ ] Settings panel: theme, font, size, opacity, preset, transition, reduced motion, menu and tab bars, profiles, with live preview through the config file
- [ ] Extensions manager: enable or disable, permissions requested and granted
- [ ] Edits keep the file's comments and layout (`toml_edit`)

### 10. Close-out
- [ ] Detachable tabs: move a tab to a new window (the PTY is handed over) — stretch
- [ ] ADR 0010, README, docs, tests, real-window checks, `docs/progress.md`
