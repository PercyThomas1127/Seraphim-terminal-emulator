# Architecture Decision Records

Each record settles one open question from the Seraphim spec v0.1 §18
(or a decision forced by implementation). Status is one of
**Accepted** (in force, implemented or binding), **Proposed** (recommended,
to be confirmed when the subsystem is built), or **Superseded**.

| ADR | Decision | Spec | Status |
|-----|----------|------|--------|
| [0001](0001-workspace-architecture.md) | Rust, one crate per subsystem, dependency rules enforced by Cargo | §2, §14 | Accepted |
| [0002](0002-vt-parser.md) | `vte` for tokenizing, Seraphim owns all terminal semantics/state | §18.5 | Accepted |
| [0003](0003-event-bus.md) | Non-blocking bounded bus, drop-on-full, trusted sources | §11 | Accepted |
| [0004](0004-gpu-backend.md) | winit + wgpu, one instanced pipeline + glyph atlas | §18.4 | Accepted for Phase I (hardware measurements pending) |
| [0005](0005-scripting.md) | Lua-first (mlua), off the render thread; Wasm for isolated extensions | §18.1, §8 | Accepted for Lua (see 0009); Wasm proposed |
| [0006](0006-declarative-scenes-and-ui.md) | TOML declares scenes/bindings, Lua defines behaviour; UI API reserved early | §18.2, §18.3 | Proposed |
| [0007](0007-visual-engine.md) | Fullscreen shader layers behind the text, time-based animation, crossfades | §5, §6 | Accepted |
| [0008](0008-presets-and-interactions.md) | Presets as data; `[[on]]` event rules; Lua and CSS compile to the same layers and rules | §5.3, §11.4, §18.2 | Accepted |
| [0009](0009-scripting-runtime.md) | One script thread, a sandboxed state per extension, commands not calls, budgets, requested ∩ granted permissions | §8, §11, §13 | Accepted |
