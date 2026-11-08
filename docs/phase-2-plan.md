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
| Named shader colors (`palette`, `color(C_name)`), theme-relative | `seraphim-visual` |

See ADR 0007 for the design and measurements, and ADR 0008 for presets and rules.

## Next

1. **Graphics protocols** (spec §4.2): Kitty graphics (APC pre-filter,
   placement, z-index under/over text), then Sixel.
2. **Continuous bindings** (spec §5.3, §11.4): `[[bind]]` from metrics
   (CPU, battery) to layer parameters; event rules (`[[on]]`) are done.
3. **More background features:** JPEG/WebP, blur, animated images.
4. **Cursor and selection effects**, animated cursor movement (spec §6.3).
5. **Live config watching.**
6. **Measure animated scenes on real GPUs.**
