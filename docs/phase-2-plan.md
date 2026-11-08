# Phase II — Visual Identity: complete

**Goal (spec §15):** a beautiful, customizable terminal.

Every Phase II task in the spec is done: Visual Engine, background
management, GPU graphics, transparency, shader support, Animation Engine,
themes and profiles. The table lists what was built and where.

## Done

| Spec item | Where |
|---|---|
| Visual Engine: layered scene of solid, gradient (8 stops, any angle), PNG image (cover/contain/stretch/tile) and WGSL shader layers, with opacity and normal/add/multiply/screen blending | `seraphim-visual` |
| Built-in shaders `aurora` and `starfield`; user `.wgsl` shaders with named parameters | `seraphim-visual/src/shaders` |
| Animation Engine: tweens (6 easings), springs (frame-rate independent), keyframes (loop, ping-pong), retargeting without jumps | `seraphim-visual/src/animation.rs` |
| Crossfading scene changes | `VisualEngine::set_layers` |
| Themes: built-in `celestial`, `monochrome`, `daylight`; `themes/<name>.toml` | `seraphim-config` |
| Profiles: `profiles/<name>.toml`, `--profile`, runtime switching (Ctrl+Shift+P), reload (Ctrl+Shift+R), `visual.profile.changed` event | config + window |
| Reduced motion, frame-rate cap, pause when unfocused, window background opacity | `[visual]` |
| Shader failures reported, never fatal | error scopes |
| Presets: `preset = "name"` / `--preset`, built-in `heavens` (merged from the `animated-background` branch), user `presets/<name>.toml` | config + `heavens.wgsl` |
| Interaction rules `[[on]]`: bus events animate layer parameters or opacity (globs, conditions, easings, springs) | `seraphim/src/interaction.rs` |
| Menu bar (File, Edit, View, Profiles, Help, after Konsole): Alt tap, mouse, access keys, check marks, user `[[menu]]` entries; zoom, full screen, new window, select all, clear and reset | `seraphim/src/menu.rs`, `menu.toml` |
| Inline images: Kitty graphics protocol (transmit/put/delete/query, chunked, zlib, PNG/RGB/RGBA, direct and file/temp-file transfer, source rects, cell sizes, offsets, z-index under backgrounds / under text / over text, C=1, replies with q) and Sixel (RGB/HLS palettes, raster attributes, repeats); images scroll with the text into history; `CSI 14t/16t` and the PTY's pixel size report the cell size | `seraphim-terminal/src/graphics.rs`, `sixel.rs`, `seraphim-render/src/images.rs` |
| Live reload: config, profile, theme, preset, shader and image files are watched (polled, no extra dependency) and applied on save; in-window notices for reloads and errors, including at startup | `seraphim/src/watch.rs` |
| Cursor and selection effects: glide, trail, blink fade, glow, typing ripples, selection fade-in (`[cursor]`), time-based, off under reduced motion | `seraphim-render/src/cursorfx.rs` |
| Continuous bindings `[[bind]]`: CPU, memory, load, battery, time of day, or an event field, mapped by range or table, eased; values survive reloads | `seraphim/src/bindings.rs` |
| Background images in PNG, JPEG, WebP and GIF; animated GIF/WebP/APNG; blur (at load); color grading (brightness, contrast, saturation as animatable parameters) | `seraphim-visual/src/picture.rs` |
| Graphics follow-ups: images move with scroll regions (and IL/DL/SD/RI); shared-memory transfer (`t=s`); Unicode placeholders (`U=1`, the 297 row/column diacritics, inferred rows/columns, aspect-fit) for tmux; animation frames and playback (`a=f` with base/background/blend/replace/gaps, `a=a` stop/run/loading/loops/current) | `graphics.rs`, `images.rs` |
| Offscreen compositing (only when needed): backdrop blur layers (`kind = "blur"`, separable Gaussian, downsampled for large radii) and styled scene transitions (spec §6.3): fade, slide, wipe, dissolve, zoom, blur, fade-through, user WGSL `fn transition(uv, p)` | `seraphim-visual/src/compose.rs` |
| Animated gradients (`spin`, `flow`); frame pacing (animated images wake the loop at their frame boundaries, not at the fps cap) | `seraphim-visual` |
| Video backgrounds (`kind = "video"`): decoded by an ffmpeg subprocess into a bounded frame queue, paced by the engine clock, looped, graded like images; ffmpeg stops with Seraphim (PDEATHSIG on Linux) | `seraphim-visual/src/video.rs` |
| Kitty `a=c` (compose frames); images follow their text through resize reflow | `graphics.rs`, `state.rs` |
| Named shader colors (`palette`, `color(C_name)`), theme-relative | `seraphim-visual` |

See ADR 0007 for the design and measurements, and ADR 0008 for presets and rules.

## Spec tasks → status

| Phase II task (spec §15) | Status |
|---|---|
| Visual Engine | Done: layered scenes, blend modes, compositing (blur layers, transitions) |
| Background management | Done: solid, gradient (animated), image (PNG/JPEG/WebP/GIF, animated, blur, grading), video, shaders, presets |
| GPU graphics | Done: wgpu renderer, Kitty graphics and Sixel inline images |
| Transparency | Done: `visual.window_opacity`; verified under a compositor (xcompmgr): a 50 % window blends the terminal background with what's behind it exactly |
| Basic shader support | Done: built-in and user WGSL layers and transitions, compiled safely |
| Animation Engine | Done: tweens, springs, keyframes, event rules, bindings, transitions, cursor effects; time-based and paced |
| Theme and profile system | Done: themes, profiles, presets, live reload, menu-bar profile switching |

## Not done (deliberately, or needs hardware)

* **Measuring on real GPUs.** Every number so far is from software
  Vulkan (llvmpipe) in a container.
* **Particle systems** (spec §5.4 lists them as a layer type): a shader
  layer can draw particles today; a dedicated particle layer with
  emitters is better done with Phase III scripting.
* **Audio-reactive visuals** (spec §5.3): needs an audio capture source
  for `[[bind]]`.
