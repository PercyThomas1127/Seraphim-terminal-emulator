# ADR 0003 — Event Bus delivery semantics

**Status:** Accepted (implemented in `crates/seraphim-events`)  **Spec:** §11, §13

## Decision

* **Publishing never blocks.** Every subscription has a bounded queue.
  When it is full, the event is dropped *for that subscriber only* and a
  per-subscription `dropped()` counter is incremented. The terminal I/O
  thread therefore can never be stalled by a slow script (§13: "Scripts
  cannot block rendering").
* **Filtering at publish time** with patterns: `*`, `prefix.*`, or an
  exact name. Subscribers never receive events they did not ask for, so
  high-rate events (`terminal.output`) cost nothing for others.
* **Trusted provenance.** `Event.source` is fixed by the `Publisher`
  handle a subsystem is given, never by payload. Extensions will receive
  publishers scoped to their own namespace, so they cannot forge
  `session.privileged` or `package.*`.
* Events are `Arc`-shared; fan-out does not clone payloads.
* Payloads are `serde_json::Value`: schemaless, maps 1:1 onto Lua tables.

## Not in the core bus (deliberately)

Priorities, cancellation and async handlers (§11.3) need a dispatcher that
runs handlers. They will live in the scripting runtime (ADR 0005), built on
top of this bus, so the core path stays lock-light and allocation-light.
Cancellation in particular only makes sense for a small, explicit set of
"before" events (e.g. `clipboard.requested`), never for terminal output.

## Event names in use

See the table in `crates/seraphim-session/src/lib.rs`. Names follow
spec §11.2 categories.
