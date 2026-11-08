# Phase II — Visual Identity: status and plan

**Goal (spec §15):** a beautiful, customizable terminal.

## Done (first slice)

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
| Named shader colors (`palette`, `color(C_name)`), theme-relative | `seraphim-visual` |

See ADR 0007 for the design and measurements, and ADR 0008 for presets and rules.

## Next

1. **Graphics follow-ups:** Kitty animation frames (`a=f/a/c`), Unicode
   placeholders (`U=1`, for tmux), shared-memory transfer (`t=s`),
   moving images with scroll regions and reflow.
2. **Continuous bindings** (spec §5.3, §11.4): `[[bind]]` from metrics
   (CPU, battery) to layer parameters; event rules (`[[on]]`) are done.
3. **More background features:** JPEG/WebP, blur, animated images.
6. **Measure animated scenes on real GPUs.**
