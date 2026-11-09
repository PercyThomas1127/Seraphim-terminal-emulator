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

**Restoration saves arrangement and text, not processes.** On close the
window saves the following to `$XDG_STATE_HOME/seraphim/session.json`
(mode 0600):

* each tab's tree, labels and profile;
* panels;
* every terminal's directory and its last 200 lines.

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
* A tab may carry its own profile; switching to it switches the theme and
  background with the scene transition.
* Shortcuts follow Konsole and GNOME Terminal. Alt+digit and Alt+arrow
  only act when there is more than one tab or pane, so a single shell
  keeps them.

## Not done

* Detachable tabs (moving a tab to another window). Windows are separate
  processes, so the PTY would need handing over, and the shell would stay
  the old process's child.
* Floating panels, custom window decorations and custom tab designs beyond
  the label format.
* Saving the session periodically, so it survives a crash.
