# ADR 0010 — Tabs, panes, panels and layouts

**Status:** Accepted (Phase IV)  **Spec:** §7.3, §9, §18.3  **Builds on:** ADR 0004, ADR 0006, ADR 0009

## Decisions

**One renderer, many panes.** Each pane draws into its own viewport with
the same `Renderer`, which keeps per-pane *slots*: instance buffer,
uniforms, images, cursor effects and color overrides. The glyph atlas,
shaping cache, fonts and pipeline are shared. Selecting a slot swaps its
state into the renderer, so the single-pane code path is unchanged. One
renderer per pane was rejected: each would hold its own atlas texture and
shaping cache. Dividers and the veil over unfocused panes are plain
rectangles in a decoration slot. The visual engine stays one scene per
window, behind every pane.

**A workspace model without a window.** `workspace.rs` holds tabs, each a
binary split tree of pane ids, with ratios. It computes pixel rectangles
and dividers for an area, moves focus by direction (choosing the pane most
in line), resizes the nearest split along an axis, zooms, reorders, and
serializes. It is pure and tested. The window owns the sessions and maps
ids to terminals or panels.

**Terminal focus versus pane focus.** The focused pane can be a panel.
Typing, the clipboard, selection and scripts' `terminal.*` target the
terminal that last had focus (`term_focus`). Interaction rules, bindings
and the script host switch to that terminal's session and replay its state
events, so focusing a root pane turns the scene red at once. A window
closes when its last terminal does; panels alone don't keep it open.

**Panels are widget trees painted by Seraphim (ADR 0006).**

* A panel returns a list of widgets: heading, text, key-value, gauge,
  sparkline, list, separator.
* Seraphim paints them as ANSI text into a virtual terminal the size of
  the pane, and the text renderer draws that like any terminal. Fonts,
  themes, colors, accessibility and reduced motion stay Seraphim's.
* Extensions describe widgets with `ui.panel`. The runtime calls their
  `render` off the main thread, and the window validates the JSON against
  the widget types.
* Keys and selections go back to the extension as events, never as
  drawing calls.
* The built-in panels (system monitor, sessions, command history, package
  activity, settings) use the same widgets.

**Settings edit the file, not a copy (§7.3).** The settings panel changes
`seraphim.toml` with `toml_edit`, keeping comments and layout, and writes
it atomically. Live reload (or the same reload path when it is off) applies
the change, which gives the live preview. Profiles still override the base
file, as they do for hand edits.

**Layouts are configuration.** `[[layout]]` describes tabs as trees of
panes. A pane runs the shell, a command, or a panel, and can set its
directory. Layouts open from the Settings menu, `--layout`,
`startup_layout` or `ui.open_layout`. They are validated like the rest of
the configuration.

**All windows in one process.** New Window and detached tabs open in
the process that asked, each window with its own GPU surface, renderer,
event bus, rules and extensions. Only this way can a tab move to another
window with its shells still running: the PTY, the terminal state
(images included) and the child process stay where they are, and
`Session::move_to` points the session's events at the new window's bus,
announcing its directory and privilege there. Handing a PTY to another
process (`SCM_RIGHTS`) was rejected: the screen would have to be
serialized, and the shell would stay the old process's child. Pane ids
are unique in the process, so a pane keeps its id when it moves. The
cost: one crash closes every window, as in most terminals.

**Restoration saves arrangement and text, not processes.** Every 30
seconds while windows are open (so a crash loses little), and when the
last one closes, Seraphim saves the following to
`$XDG_STATE_HOME/seraphim/session.json` (mode 0600):

* each window: its tabs' trees, labels and profiles, its floating
  panels with their place and size;
* panels;
* every terminal's directory and its last 200 lines.

A `seraphim -e PROGRAM` window never replaces the saved session.

Restoring starts new shells in those directories and replays the text
dimmed, with control characters removed, above a "restored" marker.
Re-running programs was rejected: replaying a command is not safe in
general. Exiting the last shell ends the session, so nothing is restored.

**Tabs.**

* The bar is painted like the menu bar, into its own overlay slot.
* Labels come from a format string (`{title}`, `{program}`, `{cwd}`,
  `{index}`) unless renamed.
* Icons depend on the foreground program (Nerd Font).
* Tabs show marks for activity and bells in tabs out of sight, and
  progress from OSC 9;4 (ConEmu / Windows Terminal), filled in the tab.
* Root tabs use the alert color.
* Designs are configuration (`[tabs]`): a style (`classic`, `powerline`,
  `rounded`, `slanted` with Nerd Font ends, or `minimal`), the top or
  bottom of the window, widths, buttons and colors. The renderer stretches
  Powerline separators to fill their cell, so shapes join.
* A tab dropped outside the window (or Ctrl+Shift+L) gets a window of its
  own where it was dropped.
* A tab may carry its own profile; switching to it switches the theme and
  background with the scene transition.
* Shortcuts follow Konsole and GNOME Terminal. Alt+digit and Alt+arrow
  only act when there is more than one tab or pane, so a single shell
  keeps them.

**Floating panels.** A panel can float over the panes instead of
sitting in a tab: it belongs to the window, not a tab, and floating
panels stack. Each one's frame (shadow and border) is a rect slot of its
own, drawn just under it, so frames and panels interleave correctly.
Pressing one raises it and gives it the keyboard; tab and split actions
give the keyboard back. Floating panels keep whole cells in size and
stay inside the window.

**Window decorations.** `window.decorations = "seraphim"` draws the
title bar and its buttons with the overlay renderer. They go in the
first row already at the top (the pinned menu bar, or the tab bar), or
in a title row of their own. Moving and resizing go to the window
manager (`drag_window`, `drag_resize_window`), so they behave like the
desktop's own. The move starts on the first motion, not on the press,
so that a double-click (maximize) still arrives.

## Not done

* Moving a tab into another existing window (a tab can only get a new
  window).
* Floating terminals (only panels float).
