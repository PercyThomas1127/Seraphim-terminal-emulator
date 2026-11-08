# ADR 0001 — Workspace architecture

**Status:** Accepted  **Spec:** §2 (architectural principles), §14

## Decision

Seraphim is a Rust Cargo workspace with **one crate per subsystem**. The
architectural rules in spec §2 are enforced by the crate dependency graph
rather than by convention:

```text
seraphim-terminal   (Terminal Core: parser semantics + state)   deps: vte, unicode-width
seraphim-pty        (PTY management)                              deps: nix, libc
seraphim-events     (Event Bus)                                   deps: serde_json
seraphim-session    (Session Manager)  -> terminal, pty, events
seraphim            (application)      -> session, terminal, events
--- planned ---
seraphim-render     (Rendering Engine) -> terminal (read-only)
seraphim-visual     (Visual + Animation Engines) -> events
seraphim-config     (TOML profiles)
seraphim-script     (Lua runtime, permissions) -> events
```

* `seraphim-terminal` depends on nothing Seraphim-specific. It cannot
  depend on the visual engine or plugins because those crates are not in
  its dependency list — a violation fails to compile.
* `seraphim-terminal` performs **no I/O**. It consumes bytes and produces
  state, response bytes and `TerminalEvent`s. This makes it fully
  deterministic and testable (and fuzzable).
* The terminal reports security-sensitive requests (OSC 52 clipboard)
  as events; it never acts on them. Policy lives above it.

## Consequences

* A renderer crash or a disabled visual subsystem cannot take a session
  down: sessions run on their own I/O thread and only share an
  `Arc<Mutex<Terminal>>` that the renderer reads.
* The Terminal mutex is the main contention point between the I/O thread
  and the renderer. If profiling shows contention, switch to
  double-buffered snapshots (I/O thread publishes damaged rows into a
  render-side copy). The `take_damage()` API was designed so that change
  is local.
