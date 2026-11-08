# ADR 0004 — GPU backend and windowing

**Status:** Proposed  **Spec:** §4, §18.4

## Recommendation

* **Windowing:** `winit` (native Wayland and X11; IME; fractional scale).
* **GPU:** `wgpu` (Vulkan on Linux, GL fallback, Metal later for macOS).
  WGSL is native to wgpu, matching the shader-language choice (§14).
  GLSL user shaders can be accepted later via `naga`'s GLSL front-end.
* **Text:** shaping and fallback with `cosmic-text` (or `swash` +
  `fontdb` if we need tighter control), rasterized into a glyph atlas
  texture; one instanced draw for cell backgrounds and one for glyphs,
  per damaged region. Ligatures need shaping over runs of same-attribute
  cells.
* **Fallback:** when no adapter is available, render the same draw list on
  the CPU (`tiny-skia`) into a `softbuffer` surface. Text must stay
  crisp regardless of background effects (§4.4), so text is always
  composited last, above all visual layers, with no post-processing
  applied to it by default.

## Acceptance criteria before accepting this ADR

Build a spike (window + atlas + grid draw) and measure on Wayland and X11:

1. Keypress-to-photon latency comparable to Alacritty/foot (measure with
   `typometer`-style tooling).
2. Throughput: `cat` of a 100 MB file not slower than 2x Alacritty.
3. Cold start < 150 ms to first frame.
4. Correct fractional scaling on Wayland; no blurry text.
5. Idle CPU ~0% (no frames rendered without damage or active animation).

If wgpu's startup or GL-fallback quality fails these, evaluate a direct
OpenGL (glow) renderer as Alacritty does.
