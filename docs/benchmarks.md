# Performance measurements

## How to measure

```bash
cargo build --release
./target/release/seraphim bench          # report
./target/release/seraphim bench --json   # the same, to save or compare
SERAPHIM_TIMING=1 ./target/release/seraphim   # startup and input latency (on exit)
```

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

`SERAPHIM_TIMING=1` logs, in a real window:

* the time from process start to the first frame presented;
* when Seraphim exits, key-press-to-echo latency: from Seraphim receiving
  a key to presenting the frame with the shell's response, so it includes
  the shell. Keyboard, compositor and display latency come on top.

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
when unfocused, and `reduced_motion` keep them in check. A GPU is expected
to run the shader scenes orders of magnitude faster, but **no GPU numbers
have been measured yet**.

### Real GPUs

Not measured yet. To add a row, run `seraphim bench --json` on the machine
and note the GPU, the driver, the display server, and the
`SERAPHIM_TIMING` startup and latency lines.

| GPU | Driver | Text frame | Aurora | Heavens | Blur | First frame | Key → echo |
|---|---|---|---|---|---|---|---|
| — | — | — | — | — | — | — | — |
