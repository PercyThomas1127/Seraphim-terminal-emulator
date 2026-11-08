# ADR 0005 — Scripting architecture

**Status:** Accepted for Lua, as built in [ADR 0009](0009-scripting-runtime.md); WebAssembly still proposed  **Spec:** §8, §18.1

## Recommendation

**Lua-first, WebAssembly for isolated extensions, no other languages in
v1.** External processes can integrate in any language via the IPC socket
(Integration Layer) instead of being embedded.

* **Runtime:** Lua 5.4 via `mlua`. (LuaJIT is faster but stuck at 5.1
  semantics; revisit only if profiling demands it.)
* **Threading:** scripts run on a dedicated script thread, never on the
  render or I/O threads. They receive events through bus subscriptions
  (ADR 0003) and act by emitting *commands* (set property, play
  animation, create layer) into a queue the visual engine applies at
  frame start. A script cannot hold a lock the renderer needs.
* **Budgets:** an instruction-count hook aborts handlers that exceed a
  time budget; repeat offenders are disabled with a notification.
* **Permissions (§8.5):** each extension declares permissions in its
  manifest. The Lua environment is built per-extension and only contains
  the API tables that were granted (no `io`, `os`, `require` of native
  modules unless granted). Terminal-read access is a read-only snapshot
  API, never the live grid.
* **Wasm:** `wasmtime` with WASI capabilities mapped from the same
  permission model, for third-party extensions from the Seraphim Gallery.

## Why not multi-language now

Python and JavaScript runtimes are large, hard to sandbox in-process and
would multiply the API surface we must keep stable (§13 "stable,
versioned APIs"). The IPC socket covers "write an integration in my
language" without that cost.
