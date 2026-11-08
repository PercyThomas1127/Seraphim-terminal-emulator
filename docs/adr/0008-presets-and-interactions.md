# ADR 0008 — Presets and interaction rules

**Status:** Accepted  **Spec:** §5.3, §5.4, §11.4, §18.2  **Builds on:** ADR 0006, ADR 0007

## Context

The `animated-background` branch added an animated sky with two moods:
root shells turn it red, and package installs send a wave of light up
the window. It was a second, parallel renderer. It had a fixed
`[background] style = …` table, and the moods were hard-wired in Rust.
That conflicts with the goal that users author their own backgrounds and
interactions.

## Decision

The animated sky is just **one preset**, `heavens`. It is built from the
same pieces a user has.

* **Shader.** The branch's WGSL is ported to the layer contract (`fn shade`):
  `crates/seraphim-visual/src/shaders/heavens.wgsl`.
  * Its inputs are named parameters: `style`, `intensity`, `speed`,
    `alert` and `install`.
  * Its colours are named shader colours: `base`, `accent0`–`accent2`,
    `alert0`, `alert1` and `surge`.
  * Shader layers gained a `palette` for these colours. They are read in
    WGSL as `color(C_<name>)`. Values are `#rrggbb[aa]` or theme references
    (`theme.background`, `theme.bright.5`, …), so a preset follows the
    active theme.
* **Preset file.** The preset is a TOML file, `seraphim-config/src/presets/heavens.toml`,
  holding `[[background]]` layers, `[[on]]` rules and `[visual]` settings.
  * User presets live in `presets/<name>.toml` and are written the same way.
  * Select one with `preset = "name"` or `--preset NAME`.
  * Layering is: preset < theme < config < profile.
  * A config or profile `[[background]]` with a preset layer's `id` merges
    into that layer, e.g. `params = { style = 2 }`. Other layers stack on
    top. Rules add up.
* **Interaction rules.** The moods are `[[on]]` rules:
  * Each rule names an event, conditions on its data (globs, lists, bools
    and numbers) and a layer property (shader parameter or `opacity`).
  * It also gives `to`, an optional `from` for one-shot effects, a
    duration and an easing (`spring`, `instant` and the six curves).
  * The window subscribes to the bus *before* spawning the session, which
    reports privilege straight away.
  * Rules act only on events for their own session.
  * The latest state events (`session.privileged`,
    `terminal.screen.alternate`) are replayed into new rules on profile
    switch and reload.
  * Privilege comes from process credentials (ADR 0003), never from
    terminal output.

The branch's `[background]` table, `Renderer::background` and
`--background-preview` were removed in the merge. Its tests were ported
to `seraphim-visual/tests/visual.rs` (`heavens::*`).

## The authoring path: Lua and CSS-like styles

Everything compiles to the same two primitives: **layers** (`LayerSpec`)
and **event → animation** (`interaction::Rule`). New front ends add
syntax, not engines.

* **Lua (Phase III).**
  * `seraphim.on("command.completed", function(e) … end)` handlers call
    `visual.animate{ layer = "sky", param = "alert", to = 1, ms = 900 }`.
    That is the same `Rule::apply` path, with logic that TOML can't
    express.
  * `visual.set_layers{…}` builds scenes in code.
  * Scripts run in the sandboxed Lua runtime (ADR 0005) and never touch the GPU.
* **CSS-like styles (later).**
  * A stylesheet maps selectors to layer properties and transitions, e.g.
    `session[root] #sky { --alert: 1; transition: --alert 900ms ease; }`.
  * It compiles to rules and layer overrides.
  * It is attractive for interface panels (ADR 0006 §18.3), where the
    retained widget tree needs style rules anyway.
* **Shaders** stay WGSL with the `shade` contract. A future
  `[[background]] kind = "shader"` can point at a file next to a preset.

## Consequences

* A preset is data. It can be shared as one file (plus shaders) and diffed.
* The visual engine stays the only renderer, so reduced motion, pausing,
  frame-rate caps, crossfades and shader error reporting apply to presets
  automatically.
* Parameters are animated with `Animated`, so mood changes retarget
  smoothly. A root shell that exits mid-fade turns back without a jump.
* Not yet:
  * (done since: continuous bindings, `[[bind]]`, in
    `seraphim/src/bindings.rs`);
  * rules that switch profiles;
  * stopping redraws for a shader that is still (the branch did this
    when `speed = 0`).
