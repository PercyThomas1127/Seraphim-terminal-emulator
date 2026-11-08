# ADR 0004 — GPU backend and windowing

**Status:** Accepted for Phase I (spike built); criteria 1, 3, 4 still to be measured on real hardware  **Spec:** §4, §18.4

## Recommendation

* **Windowing:** `winit` (native Wayland and X11; IME; fractional scale).
* **GPU:** `wgpu` (Vulkan on Linux, GL fallback, Metal later for macOS).
  WGSL is native to wgpu, matching the shader-language choice (§14).
  GLSL user shaders can be accepted later via `naga`'s GLSL front-end.
* **Text:** `fontdb` for discovery and per-character fallback, `swash`
  for rasterization (hinted outlines, color emoji bitmaps/COLR), packed
  into one RGBA glyph atlas. Ligatures will need shaping over runs of
  same-attribute cells (`swash::shape`), not yet implemented.
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

## Spike results (implemented in `crates/seraphim-render`, `crates/seraphim/src/window.rs`)

Design as built: **one pipeline, one atlas, one instanced draw call.** Every
background run, glyph, decoration and cursor piece is a 64-byte instance;
the fragment shader selects solid / coverage-mask / color-glyph by a flat
`kind`. Colors blend in sRGB space on a non-sRGB surface (`Bgra8Unorm`),
matching how other terminals look. Glyphs land on whole pixels and are
sampled with nearest filtering (crisp, never blurred); only downscaled
color emoji use a linear sampler.

Measured in the development container (4 vCPU VM, **software** Vulkan via
Mesa llvmpipe, Xvfb — so GPU-side numbers are not representative):

| Criterion | Result |
|---|---|
| 5. Idle CPU | **0 ticks over 3 s** with a shell at the prompt: frames are rendered only when the session's generation counter changes. |
| 2. Throughput (parser) | `cargo run --release -p seraphim-terminal --example throughput`: ~115 MB/s ASCII / SGR / Unicode, **on par with `alacritty_terminal` 0.26 on the same corpus and machine** (116 / 126 / 121 MB/s). |
| 2. Throughput (frame build) | `cargo run --release -p seraphim-render --example frame_time`: **1.3 ms CPU** to build a completely full 200×50 frame (10,001 instances). |
| Correctness | Pixel tests through the real wgpu pipeline (`crates/seraphim-render/tests/render.rs`); window driven by `xdotool` under Xvfb: typing, colors, vim, resize, exit all verified; keyboard encoding verified byte-for-byte. |
| 1. Latency, 3. cold start, 4. Wayland fractional scaling | **Not measured yet** — need real hardware and a Wayland compositor. |

Findings from the spike:

* winit replays *synthetic* key presses for keys held when focus arrives;
  these must be ignored or the first keystroke is duplicated.
* The terminal hot path must not allocate or drop per cell: `Cell` is now
  `Copy` (combining marks live beside cells in `Row`), rows track their
  written prefix so blanking is proportional to content, and CSI/SGR
  parameters are collected on the stack.
* wgpu 30 API notes for future work: present via `Queue::present`, the
  instance needs a display handle (`OwnedDisplayHandle`) for GL/EGL, and
  `get_current_texture` returns a `CurrentSurfaceTexture` enum.

Not yet done: partial redraws using `take_damage()` (full rebuild is cheap
enough so far), cursor blinking, a CPU fallback renderer, procedural
box-drawing glyphs, ligatures.
