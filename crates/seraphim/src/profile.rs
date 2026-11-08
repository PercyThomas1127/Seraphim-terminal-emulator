//! Configuration → window settings, used at startup and when switching
//! profiles at runtime (Ctrl+Shift+P / Ctrl+Shift+R).

use std::path::Path;

use seraphim_config::{parse_rgba, Config};
use seraphim_render::Theme;
use seraphim_visual::{Blend, Fit, LayerKind, LayerSpec};

use crate::window::WindowOptions;

/// Load the layered configuration (`profile` and `preset` override the
/// files' own). Without a configuration path, built-in defaults (plus the
/// preset, if any).
pub fn load(
    config_path: Option<&Path>,
    profile: Option<&str>,
    preset: Option<&str>,
) -> Result<(WindowOptions, Vec<String>), String> {
    let Some(path) = config_path else {
        let Some(preset) = preset else {
            return Ok(options_from_config(&Config::default(), None, "default"));
        };
        // No file: a path that does not exist layers just the preset.
        let none = Path::new("/nonexistent/seraphim/config.toml");
        let loaded =
            Config::load_layered_with(none, None, Some(preset)).map_err(|e| e.to_string())?;
        return Ok(options_from_config(&loaded.config, None, &loaded.profile));
    };
    let loaded = Config::load_layered_with(path, profile, preset).map_err(|e| e.to_string())?;
    let (mut o, warnings) = options_from_config(&loaded.config, path.parent(), &loaded.profile);
    // Live reload watches every file that went in, and the config file
    // even before it exists.
    let mut watch = vec![path.to_path_buf()];
    watch.extend(loaded.sources);
    watch.append(&mut o.watch);
    watch.dedup();
    o.watch = watch;
    Ok((o, warnings))
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
    let (layers, warnings, files) = layers(c, config_dir, &theme);
    let binds =
        c.bind.iter().filter_map(|b| crate::bindings::Bind::from_config(b, &theme)).collect();
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
        rules: c.on.iter().map(crate::interaction::Rule::from_config).collect(),
        binds,
        watch: files,
        live_reload: c.live_reload,
        notice: None,
        transition: transition_style(&c.visual.transition, config_dir),
        cursor: c.cursor.clone(),
        menu_bar: c.window.menu_bar,
        menus: crate::menu::merge(&seraphim_config::builtin_menu(), &c.menu),
        profile: profile.to_owned(),
        config_path: None,
        cli_font_family: None,
        cli_font_size: None,
        cli_preset: None,
        scripting: c.scripting.clone(),
        extensions: c.extensions.clone(),
        cli_extensions: Vec::new(),
    };
    (o, warnings)
}

/// `[[background]]` entries as visual-engine layers.
fn layers(
    c: &Config,
    config_dir: Option<&Path>,
    theme: &Theme,
) -> (Vec<LayerSpec>, Vec<String>, Vec<std::path::PathBuf>) {
    let dir = config_dir.map(Path::to_path_buf).unwrap_or_default();
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let mut files = Vec::new();
    for (i, b) in c.background.iter().enumerate() {
        match layer_spec(b, i, &dir, theme, &mut files) {
            Ok(l) => out.push(l),
            Err(e) => warnings.push(e),
        }
    }
    (out, warnings, files)
}

/// One `[[background]]` entry (the `index`-th) as a layer. Relative paths
/// resolve against `dir`; files read go into `files` (for live reload).
pub fn layer_spec(
    b: &seraphim_config::Background,
    index: usize,
    dir: &Path,
    theme: &Theme,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<LayerSpec, String> {
    let id = b.id.clone().unwrap_or_else(|| format!("{}-{}", b.kind, index + 1));
    let kind = match b.kind.as_str() {
        "solid" => LayerKind::Solid(
            b.color.as_deref().and_then(|c| resolve_color(c, theme)).unwrap_or([0.0; 4]),
        ),
        "gradient" => LayerKind::Gradient {
            colors: b.colors.iter().filter_map(|c| resolve_color(c, theme)).collect(),
            angle: b.angle,
        },
        "image" => {
            let path = b.resolve(b.path.as_deref().unwrap_or_default(), dir);
            files.push(path.clone());
            LayerKind::Image { path, fit: Fit::parse(&b.fit).unwrap_or_default(), blur: b.blur }
        }
        "blur" => LayerKind::Blur,
        "video" => {
            let path = b.resolve(b.path.as_deref().unwrap_or_default(), dir);
            files.push(path.clone());
            LayerKind::Video { path, fit: Fit::parse(&b.fit).unwrap_or_default() }
        }
        _ => {
            let name = b.shader.clone().unwrap_or_default();
            if name.ends_with(".wgsl") {
                let path = b.resolve(&name, &dir.join("shaders"));
                let path = if path.exists() { path } else { b.resolve(&name, dir) };
                files.push(path.clone());
                match std::fs::read_to_string(&path) {
                    Ok(src) => LayerKind::Shader { name, source: Some(src) },
                    Err(e) => return Err(format!("background {id:?}: {}: {e}", path.display())),
                }
            } else {
                LayerKind::Shader { name, source: None }
            }
        }
    };
    Ok(LayerSpec {
        id,
        kind,
        opacity: b.opacity,
        blend: Blend::parse(&b.blend).unwrap_or_default(),
        params: b.params.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        colors: b
            .palette
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), resolve_color(v, theme)?)))
            .collect(),
    })
}

/// `visual.transition` as a style: a built-in name, or a WGSL file from
/// `transitions/` or the config directory. Unreadable files fall back to
/// fade (with a message).
pub fn transition_style(
    name: &str,
    config_dir: Option<&Path>,
) -> Result<seraphim_visual::TransitionStyle, String> {
    if let Some(s) = seraphim_visual::TransitionStyle::named(name) {
        return Ok(s);
    }
    let dir = config_dir.map(Path::to_path_buf).unwrap_or_default();
    let candidates = [dir.join("transitions").join(name), dir.join(name)];
    let path = candidates.iter().find(|p| p.exists()).unwrap_or(&candidates[1]);
    std::fs::read_to_string(path)
        .map(|source| seraphim_visual::TransitionStyle::Custom { name: name.to_owned(), source })
        .map_err(|e| format!("transition {}: {e}", path.display()))
}

/// A palette color: `#rrggbb[aa]` or a theme color such as
/// `theme.background` or `theme.bright.5` (see
/// [`seraphim_config::is_color_ref`]).
pub fn resolve_color(s: &str, theme: &Theme) -> Option<[f32; 4]> {
    if s.starts_with('#') {
        return parse_rgba(s);
    }
    let r = s.strip_prefix("theme.")?;
    let rgb = match r {
        "foreground" => theme.foreground,
        "background" => theme.background,
        "cursor" => theme.cursor,
        "selection" => theme.selection_bg,
        _ => {
            let (base, i) = if let Some(i) = r.strip_prefix("normal.") {
                (0, i)
            } else {
                (8, r.strip_prefix("bright.")?)
            };
            let i: usize = i.parse().ok().filter(|i| *i < 8)?;
            theme.ansi[base + i]
        }
    };
    Some([rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, 1.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_follows_the_theme() {
        let t = Theme::default();
        let f = |c: u8| c as f32 / 255.0;
        let bg = t.background;
        assert_eq!(
            resolve_color("theme.background", &t),
            Some([f(bg[0]), f(bg[1]), f(bg[2]), 1.0])
        );
        let b5 = t.ansi[13];
        assert_eq!(resolve_color("theme.bright.5", &t), Some([f(b5[0]), f(b5[1]), f(b5[2]), 1.0]));
        assert_eq!(resolve_color("#ff000080", &t), Some([1.0, 0.0, 0.0, 128.0 / 255.0]));
        assert_eq!(resolve_color("theme.bright.8", &t), None);
    }

    #[test]
    fn heavens_preset_becomes_a_layer() {
        let dir = std::env::temp_dir().join(format!("seraphim-preset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "preset = \"heavens\"\ntheme = \"daylight\"\n").unwrap();
        let (o, warnings) = load(Some(&path), None, None).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(o.layers.len(), 1);
        let sky = &o.layers[0];
        assert_eq!(sky.id, "sky");
        let base = sky.colors.iter().find(|(n, _)| n == "base").unwrap().1;
        assert_eq!(Some(base), resolve_color("theme.background", &o.theme), "theme-relative");
        assert_eq!(o.rules.len(), 4);
        assert_eq!(o.visual.max_fps, 30);
    }
}
