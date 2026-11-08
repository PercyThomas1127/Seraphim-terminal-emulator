//! Configuration → window settings, used at startup and when switching
//! profiles at runtime (Ctrl+Shift+P / Ctrl+Shift+R).

use std::path::Path;

use seraphim_config::{parse_rgba, Config};
use seraphim_render::Theme;
use seraphim_visual::{Blend, Fit, LayerKind, LayerSpec};

use crate::window::WindowOptions;

/// Load the layered configuration (`profile` overrides the file's own).
pub fn load(
    config_path: Option<&Path>,
    profile: Option<&str>,
) -> Result<(WindowOptions, Vec<String>), String> {
    let Some(path) = config_path else {
        return Ok(options_from_config(&Config::default(), None, "default"));
    };
    let loaded = Config::load_layered(path, profile).map_err(|e| e.to_string())?;
    Ok(options_from_config(&loaded.config, path.parent(), &loaded.profile))
}

/// Window settings from a (layered) configuration. Returns warnings for
/// background layers that could not be prepared (e.g. unreadable shader
/// files); those layers are skipped.
pub fn options_from_config(
    c: &Config,
    config_dir: Option<&Path>,
    profile: &str,
) -> (WindowOptions, Vec<String>) {
    let mut theme = Theme::default();
    let k = &c.colors;
    theme.foreground = k.foreground;
    theme.background = k.background;
    theme.cursor = k.cursor;
    theme.cursor_text = k.cursor_text;
    theme.selection_bg = k.selection_background;
    theme.selection_fg = k.selection_foreground;
    theme.bold_is_bright = k.bold_is_bright;
    theme.ansi[..8].copy_from_slice(&k.normal);
    theme.ansi[8..].copy_from_slice(&k.bright);
    let (layers, warnings) = layers(c, config_dir);
    let o = WindowOptions {
        font_family: c.font.family.clone(),
        font_size: c.font.size,
        cols: c.window.columns,
        rows: c.window.rows,
        padding: c.window.padding,
        command: c.shell.program.as_ref().map(|p| {
            seraphim_session::Command::new(p.as_str()).args(c.shell.args.iter().map(|s| s.as_str()))
        }),
        allow_clipboard_read: c.clipboard.allow_read,
        theme,
        scrollback: c.terminal.scrollback,
        grapheme_clustering: c.terminal.grapheme_clustering,
        visual: c.visual.clone(),
        layers,
        profile: profile.to_owned(),
        config_path: None,
        cli_font_family: None,
        cli_font_size: None,
    };
    (o, warnings)
}

/// `[[background]]` entries as visual-engine layers.
fn layers(c: &Config, config_dir: Option<&Path>) -> (Vec<LayerSpec>, Vec<String>) {
    let dir = config_dir.map(Path::to_path_buf).unwrap_or_default();
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for (i, b) in c.background.iter().enumerate() {
        let id = b.id.clone().unwrap_or_else(|| format!("{}-{}", b.kind, i + 1));
        let kind = match b.kind.as_str() {
            "solid" => {
                LayerKind::Solid(b.color.as_deref().and_then(parse_rgba).unwrap_or([0.0; 4]))
            }
            "gradient" => LayerKind::Gradient {
                colors: b.colors.iter().filter_map(|c| parse_rgba(c)).collect(),
                angle: b.angle,
            },
            "image" => LayerKind::Image {
                path: b.resolve(b.path.as_deref().unwrap_or_default(), &dir),
                fit: Fit::parse(&b.fit).unwrap_or_default(),
            },
            _ => {
                let name = b.shader.clone().unwrap_or_default();
                if name.ends_with(".wgsl") {
                    let path = b.resolve(&name, &dir.join("shaders"));
                    let path = if path.exists() { path } else { b.resolve(&name, &dir) };
                    match std::fs::read_to_string(&path) {
                        Ok(src) => LayerKind::Shader { name, source: Some(src) },
                        Err(e) => {
                            warnings.push(format!("background {id:?}: {}: {e}", path.display()));
                            continue;
                        }
                    }
                } else {
                    LayerKind::Shader { name, source: None }
                }
            }
        };
        out.push(LayerSpec {
            id,
            kind,
            opacity: b.opacity,
            blend: Blend::parse(&b.blend).unwrap_or_default(),
            params: b.params.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        });
    }
    (out, warnings)
}
