# ADR 0007 — Visual Engine, Animation Engine and profiles

**Status:** Accepted (first slice implemented in `crates/seraphim-visual`,
`crates/seraphim-config`, `crates/seraphim/src/window.rs`)
**Spec:** §5, §6, §7, ADR 0006

## Decisions

* **Every layer is a fullscreen fragment shader** over one shared uniform
  block (time, resolution, opacity, 16 parameters, 8 colors/stops, image
  info). Solid, gradient and image layers are built-in shaders; there is a
  single code path for compiling, blending and drawing. Layers draw into
  the same render pass as the text, *before* it, so text is always
  composited last and stays pixel-exact (tested).
* **User shaders** implement
  `fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32>` (straight RGBA,
  sRGB space). Parameters are named in configuration; the engine generates
  `const P_<name>: u32` so shaders read `p(P_speed)`.
* **Failure isolation:** shaders and pipelines are created inside a wgpu
  validation error scope. A broken layer is reported (with the naga
  message) and skipped; the rest of the scene still renders.
* **Blend modes** are fixed-function on premultiplied color: normal, add,
  multiply, screen.
* **Animation** is a pure function of an explicit clock: tweens (6 easings),
  springs integrated at a fixed 240 Hz step (identical results at any frame
  rate), keyframes with loop/ping-pong. Retargeting starts from the current
  value, so interrupting a transition never jumps.
* **Frames only when something moves.** `VisualEngine::animating()` is true
  only for time-dependent shaders (detected by `u.time` use) or active
  transitions; the window then draws at up to `visual.max_fps`. The clock
  pauses while unfocused (unless `animate_unfocused`) and freezes under
  `reduced_motion`, which also makes transitions instant. Resuming never
  makes time jump.
* **Window opacity** scales the clear color and every layer (premultiplied),
  while cell backgrounds and text stay opaque. Transparency must be
  requested at window creation, so turning it on needs a restart.
* **Configuration layering:** defaults < theme colors < `config.toml` <
  profile. Each file is validated alone first (exact file/line/column
  errors), then merged as TOML tables. A profile's `[[background]]` list
  replaces the base list. Profiles switch at runtime (Ctrl+Shift+P), files
  reload with Ctrl+Shift+R; scenes crossfade over `visual.transition_ms`. A
  broken profile is rejected and the current look kept.

## Measured (development container: software Vulkan, Xvfb)

| Scene | CPU |
|---|---|
| Static layers (gradient, solid, image) | 0 ticks / 3 s idle |
| Animated aurora + starfield at 30 fps, focused | ~29 % of one core — llvmpipe runs the fbm shader on the CPU; to be re-measured on a GPU |
| Same, window unfocused | 0 |
| Same, `reduced_motion` | ~0 |

## Not yet

* (Done since: Kitty graphics protocol and Sixel inline images; see the
  Phase II plan.)
* (Done since: JPEG/WebP/GIF, animated images, blur, color grading.)
  Still to do: video, and blur for non-image layers.
* (Done since: event rules and continuous bindings, ADR 0008.)
* (Done since: live reload, `seraphim/src/watch.rs`. Scene changes made
  while the window is unfocused apply instantly instead of freezing
  half-way.)
