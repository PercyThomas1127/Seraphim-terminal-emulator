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
| [0004](0004-gpu-backend.md) | winit + wgpu, glyph atlas, CPU fallback | §18.4 | Proposed |
| [0005](0005-scripting.md) | Lua-first (mlua), off the render thread; Wasm for isolated extensions | §18.1, §8 | Proposed |
| [0006](0006-declarative-scenes-and-ui.md) | TOML declares scenes/bindings, Lua defines behaviour; UI API reserved early | §18.2, §18.3 | Proposed |
