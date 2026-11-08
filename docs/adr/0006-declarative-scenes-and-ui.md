# ADR 0006 — Declarative scenes, reactive bindings, and interface extensibility

**Status:** Proposed  **Spec:** §5.4, §11.4, §18.2, §18.3

## Recommendation

**TOML declares what the environment looks like; Lua defines how it
behaves.** Both compile to the same internal scene graph, so anything
declarable is also scriptable.

```toml
# profiles/celestial.toml
[[layer]]
id = "sky"
kind = "shader"
shader = "shaders/aurora.wgsl"
params = { intensity = 0.2, hue = 0.85 }

[[bind]]                       # spec §11.4 reactive property, no script
target = "sky.params.intensity"
source = "system.cpu.usage"
range = [0.1, 1.0]
smoothing_ms = 400

[[bind]]
target = "sky.params.hue"
source = "command.completed.exit_code"
map = { 0 = 0.85, default = 0.0 }

[on."session.privileged"]      # declarative reaction
when = { root = true }
profile = "root"
```

Bindings are evaluated by the visual engine (not Lua), so the common
reactive cases cost no script time.

## Interface extensibility (§18.3)

Make it part of the API from Phase III even though panels arrive in
Phase IV: reserve the `interface.*` event namespace and the
`ui.register_panel{ id, title, render = fn }` Lua entry point now, with a
retained widget tree (text, gauge, list, sparkline, image) rendered by
Seraphim. Extensions describe widgets; they never draw directly. This
keeps rendering in Seraphim's control (performance, accessibility,
reduced-motion) and makes layouts serializable for session restoration.
