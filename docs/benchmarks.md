# Performance measurements

## How to measure

```bash
cargo build --release
./target/release/seraphim bench --list-gpus        # which GPUs Seraphim sees
./target/release/seraphim bench --gpu 2060         # report, on that GPU
./target/release/seraphim bench --gpu 2060 --json  # the same, to save or compare
./target/release/seraphim bench --window --gpu 2060  # a real window (below)
SERAPHIM_TIMING=1 ./target/release/seraphim        # timings of normal use (on exit)
```

On a machine with more than one GPU, say which to measure: `--gpu` (or
`SERAPHIM_GPU`, which Seraphim itself also reads) takes any part of the
GPU's name. Without it, wgpu picks the low-power adapter, which on a
desktop with two cards may not be the one you meant.

`seraphim bench` needs no window. It measures:

* **Parser throughput:** terminal output (colored listings, progress
  lines, plain text) processed per second, with history on.
* **Frame preparation (CPU):** building and uploading a frame when
  nothing changed, when one line changed, and after a scroll, at 80×24 and
  200×60. Rows that didn't change are reused (partial redraw, spec §4.4).
* **GPU frame time:** a full 200×60 frame at 16 px, with text alone, over
  a gradient, over the `aurora` and `heavens` shaders, and with a backdrop
  blur. This is wall time from recording to the GPU finishing, so it
  includes the driver.

`seraphim bench --window [--keys N] [--preset NAME]` opens a window with
the built-in defaults, running `cat`. It types N keys (200 by default)
into it through the same path a key press takes, one at a time, at uneven
intervals so the samples land at different points of the display's
refresh. Then it closes and reports:

* startup phases and the time from process start to the first frame;
* key to echo: from Seraphim handling the key to presenting the frame
  that shows the echo. With `cat`, the kernel does the echo, so this is
  the terminal's own share. **Waiting for vsync (up to one refresh) and
  the compositor (usually about one more) come on top**, and so do the
  keyboard and the display;
* frames presented, and the time to draw and present one.

No input is synthesized for the desktop, so nothing outside the window
can receive it. `SERAPHIM_TIMING=1` logs the same timings during normal
use (key to echo then includes your shell).

## Results

### llvmpipe (software rendering), development container

No GPU is available in the development container: these are the reference
numbers for software rendering (Mesa llvmpipe, LLVM 20, 256-bit, Vulkan),
release build.

| Measurement | Result |
|---|---|
| Parser throughput | 87 MiB/s |
| Prepare 80×24: nothing changed / one line / scroll | 10 / 14 / 20 µs |
| Prepare 200×60: nothing changed / one line / scroll | 36 / 41 / 60 µs |
| Frame 2008×1268, text only | 2.3 ms (~430 fps) |
| … over a gradient | 6.4 ms (~155 fps) |
| … over the aurora shader | 34 ms (~29 fps) |
| … over the heavens shader | 51 ms (~20 fps) |
| … aurora with backdrop blur | 72 ms (~14 fps) |
| First frame after start (Xvfb) | 136 ms |
| Key press to echo (Xvfb, bash) | median 1.2 ms, p95 1.7 ms |

Before partial redraw, preparing a 200×60 frame took 865 µs whether or not
anything changed; it now takes 36 µs for an unchanged frame and uploads
nothing.

On software rendering, text stays fast. Full-window animated shaders are
expensive, because each pixel runs on the CPU. `visual.max_fps`, pausing
when unfocused, and `reduced_motion` keep them in check. A GPU runs them 25 to 50 times faster (below).

### RTX 2060 Super and Radeon Pro WX 7100, KDE Plasma on X11

Measured November 2026, release build.

* **Machine:** Ryzen 5 3600X, 32 GB, Gentoo, Linux 6.18.
* **GPUs:** a GeForce RTX 2060 Super (NVIDIA 595.104.02) and a Radeon Pro
  WX 7100 (Mesa 26.1.8, RADV).
* **Display:** one 1920×1080 monitor at 75 Hz, on the Radeon. The 2060
  Super has no monitor attached, so its windows reach the screen through
  PRIME render offload.

The frame size is 2008×1148 here, rather than 1268 high as above, because
this machine's default font has a shorter line height.

| Measurement | RTX 2060 Super | Radeon Pro WX 7100 |
|---|---|---|
| Frame 2008×1148, text only | 0.05 ms | 0.11 ms |
| … over a gradient | 0.10 ms | 0.19 ms |
| … over the aurora shader | 1.05 ms (~950 fps) | 1.70 ms (p95 5.1) |
| … over the heavens shader | 1.37 ms (~730 fps) | 2.67 ms (p95 5.8) |
| … aurora with backdrop blur | 1.37 ms (~730 fps) | 2.70 ms (p95 7.6) |
| First frame after start | 329 ms | 205 ms |
| Key to echo frame presented, median / p95 | 0.3 / 0.4 ms | 0.3 / 0.5 ms |
| Drawing and presenting a frame (window) | 0.18 ms; 0.25 with heavens | 0.22 ms |

* **Repeatability:** three 300-frame runs on the 2060 Super agreed within
  1 %. Its frame p95 is within 3 % of the median; the Radeon's is 2–3×
  its median.
* **CPU-side numbers** are the same on both GPUs. The parser does
  55 MiB/s on this CPU (87 on the development container's). Preparing a
  frame takes 10/15/18 µs at 80×24 and 36/44/46 µs at 200×60 (unchanged
  frame, one line changed, scroll).
* **Rates in the window:** 64 frames a second while typing (each echo,
  plus the cursor gliding to its new place). With `heavens`, 39: the
  preset caps animation at 30 fps, and each echo is drawn at once, outside
  that cap.
* **What a key costs on screen:** Seraphim's share is under half a
  millisecond on either GPU. At 75 Hz, vsync adds up to 13.3 ms, and
  KWin's compositing adds about one more refresh.

**Startup** to the first frame, by phase (medians of 5 runs):

| Phase | 2060 Super | Radeon |
|---|---|---|
| Event loop running | 12 ms | 13 ms |
| Fonts loaded (scanning system fonts) | +54 ms | +55 ms |
| Window created | +0.4 ms | +0.8 ms |
| GPU ready (Vulkan, adapter, device) | +187 ms | +71 ms |
| Renderers and shaders built | +56 ms | +55 ms |
| Shell started, scene built | +8 ms | +4 ms |
| First frame presented | +11 ms, **329 ms in all** | +7 ms, **205 ms in all** |

Most of the 2060 Super's extra startup time is NVIDIA's driver creating
the device, about 150 ms, which Seraphim can't change. Scanning system
fonts (about 55 ms) and building shader pipelines (about 55 ms) are the
next largest costs, and both could be cached between runs.

Found while measuring here, and fixed:

* **Seraphim used the Radeon by default.** wgpu's low-power choice between
  two discrete cards is the first one, so the 2060 Super was never used.
  `SERAPHIM_GPU` (any part of a GPU's name) now chooses, for both windows
  and the benchmark, and `--list-gpus` shows the names.
* **OpenGL was started on every launch**, as well as Vulkan, and listing
  GPUs brought it up on both cards. Vulkan is now tried first and OpenGL
  only when no Vulkan GPU fits. The first frame comes about 100 ms sooner
  on the 2060 Super (424 → 329 ms) and 30 ms sooner on the Radeon
  (235 → 205 ms).
* **Seraphim crashed in an OpenGL window.** The text shader read the glyph
  atlas through two samplers, which OpenGL can't do. It now has a binding
  per sampler, and the pixel tests pass under OpenGL (Mesa, Radeon).
* **Color emoji could come out black and white.** With "Noto Emoji"
  (monochrome) installed next to "Noto Color Emoji", either could be
  chosen. Fonts with color glyph tables (COLR, CBDT, sbix, SVG) now come
  first.

Known: windowless OpenGL on the 2060 Super fails ("parent device is lost"
from NVIDIA's EGL). Windows are unaffected: an OpenGL window draws on the
screen's GPU, and Vulkan is the default anyway. To run the benchmark under
OpenGL on Mesa: `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json WGPU_BACKEND=gl`.

### Adding a machine

Run `seraphim bench --json --gpu NAME` and `seraphim bench --window --gpu
NAME`, and note the GPU, driver, display server, monitor refresh rate and
the CPU.

| GPU | Driver | Display | Text frame | Aurora | Heavens | Blur | First frame | Key → echo frame |
|---|---|---|---|---|---|---|---|---|
| RTX 2060 Super | NVIDIA 595.104.02 | X11 (KWin), PRIME, 75 Hz | 0.05 ms | 1.05 ms | 1.37 ms | 1.37 ms | 329 ms | 0.3 ms |
| Radeon Pro WX 7100 | Mesa 26.1.8 RADV | X11 (KWin), 75 Hz | 0.11 ms | 1.70 ms | 2.67 ms | 2.70 ms | 205 ms | 0.3 ms |
| llvmpipe (software) | Mesa 25.2.8 | Xvfb | 2.3 ms | 34 ms | 51 ms | 72 ms | 136 ms | 1.2 ms (bash) |
