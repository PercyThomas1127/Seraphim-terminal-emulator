//! # seraphim-config
//!
//! The Configuration System (spec §7): loads and validates
//! `~/.config/seraphim/config.toml`. Unknown keys are errors, so typos are
//! reported instead of silently ignored.
//!
//! Layering (spec §7.1–7.2), lowest to highest priority:
//! 1. built-in defaults;
//! 2. the theme's colors (`theme = "name"`: built-in, or `themes/<name>.toml`);
//! 3. `config.toml`;
//! 4. the active profile, `profiles/<name>.toml` (any subset of the same
//!    keys; tables merge key by key, a profile's `[[background]]` list
//!    replaces the base one).
//!
//! Every file is validated on its own first, so errors point at the right
//! file, line and column.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer};
use serde::Deserialize;

/// The documented default configuration file.
pub const DEFAULT_TOML: &str = include_str!("default.toml");

pub type Rgb = [u8; 3];

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Color theme: a built-in name ([`BUILTIN_THEMES`]) or a file in
    /// `themes/`.
    pub theme: Option<String>,
    /// Profile applied on top of this file at startup.
    pub profile: Option<String>,
    pub font: Font,
    pub window: Window,
    pub terminal: TerminalConfig,
    pub shell: Shell,
    pub clipboard: Clipboard,
    pub colors: Colors,
    pub visual: Visual,
    /// Background layers, bottom first (spec §5.1, §5.4).
    pub background: Vec<Background>,
}

/// Built-in themes: (name, colors as TOML).
pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("celestial", include_str!("themes/celestial.toml")),
    ("monochrome", include_str!("themes/monochrome.toml")),
    ("daylight", include_str!("themes/daylight.toml")),
];

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Visual {
    /// Freeze all animation and make transitions instant (accessibility).
    pub reduced_motion: bool,
    /// Frame-rate cap while something animates.
    pub max_fps: u32,
    /// Keep animating while the window is unfocused.
    pub animate_unfocused: bool,
    /// Opacity of the window background (needs a compositor). Text and
    /// cell colors stay opaque.
    pub window_opacity: f32,
    /// Duration of crossfades when switching profiles, in milliseconds.
    pub transition_ms: u32,
}

impl Default for Visual {
    fn default() -> Self {
        Visual {
            reduced_motion: false,
            max_fps: 60,
            animate_unfocused: false,
            window_opacity: 1.0,
            transition_ms: 400,
        }
    }
}

/// One background layer.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Background {
    /// Name used to address the layer (defaults to its position).
    pub id: Option<String>,
    /// `solid`, `gradient`, `image` or `shader`.
    pub kind: String,
    /// `solid`: `"#rrggbb"` or `"#rrggbbaa"`.
    pub color: Option<String>,
    /// `gradient`: 1-8 colors, evenly spaced.
    pub colors: Vec<String>,
    /// `gradient`: direction in degrees (0 = left to right, 90 = top to bottom).
    pub angle: f32,
    /// `image`: a PNG file (`~` and paths relative to the config directory work).
    pub path: Option<String>,
    /// `image`: `cover`, `contain`, `stretch` or `tile`.
    pub fit: String,
    /// `shader`: a built-in name (`aurora`, `starfield`) or a `.wgsl` file.
    pub shader: Option<String>,
    pub opacity: f32,
    /// `normal`, `add`, `multiply` or `screen`.
    pub blend: String,
    /// Shader parameters.
    pub params: std::collections::BTreeMap<String, f32>,
}

impl Default for Background {
    fn default() -> Self {
        Background {
            id: None,
            kind: String::new(),
            color: None,
            colors: Vec::new(),
            angle: 90.0,
            path: None,
            fit: "cover".into(),
            shader: None,
            opacity: 1.0,
            blend: "normal".into(),
            params: Default::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    pub family: Option<String>,
    /// Points.
    pub size: f32,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    pub columns: u16,
    pub rows: u16,
    pub padding: f32,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    pub scrollback: usize,
    /// Group emoji sequences and combining marks into single cells.
    pub grapheme_clustering: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    pub program: Option<String>,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clipboard {
    pub allow_read: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    #[serde(deserialize_with = "color")]
    pub foreground: Rgb,
    #[serde(deserialize_with = "color")]
    pub background: Rgb,
    #[serde(deserialize_with = "color")]
    pub cursor: Rgb,
    #[serde(deserialize_with = "color")]
    pub cursor_text: Rgb,
    #[serde(deserialize_with = "color")]
    pub selection_background: Rgb,
    #[serde(deserialize_with = "opt_color")]
    pub selection_foreground: Option<Rgb>,
    pub bold_is_bright: bool,
    #[serde(deserialize_with = "colors8")]
    pub normal: [Rgb; 8],
    #[serde(deserialize_with = "colors8")]
    pub bright: [Rgb; 8],
}

impl Default for Font {
    fn default() -> Self {
        Font { family: None, size: 11.0 }
    }
}

impl Default for Window {
    fn default() -> Self {
        Window { columns: 100, rows: 30, padding: 6.0 }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        TerminalConfig { scrollback: 10_000, grapheme_clustering: true }
    }
}

impl Default for Colors {
    fn default() -> Self {
        let h = |s: &str| parse_hex(s).expect("valid built-in color");
        Colors {
            foreground: h("#e6e1f0"),
            background: h("#14121f"),
            cursor: h("#f5a6d0"),
            cursor_text: h("#14121f"),
            selection_background: h("#4a3b6b"),
            selection_foreground: None,
            bold_is_bright: false,
            normal: [
                "#2a273a", "#ff5577", "#8be9a8", "#f9d98b", "#89a8ff", "#c89bff", "#7fe0e8",
                "#d8d3e6",
            ]
            .map(h),
            bright: [
                "#5c5775", "#ff7a96", "#a9f5bf", "#fce6a9", "#a8bfff", "#f5a6d0", "#a3eef2",
                "#ffffff",
            ]
            .map(h),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(PathBuf, std::io::Error),
    /// Parse or validation error; the message includes line and column.
    Invalid(PathBuf, String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(p, e) => write!(f, "{}: {e}", p.display()),
            ConfigError::Invalid(p, e) => write!(f, "{}: {}", p.display(), e.trim_end()),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// `$XDG_CONFIG_HOME/seraphim/config.toml`, else
    /// `~/.config/seraphim/config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("seraphim").join("config.toml"))
    }

    /// The configuration with `profile` (or the file's own `profile` key)
    /// and its theme applied. A missing `path` means built-in defaults plus
    /// the profile, if any.
    pub fn load_layered(path: &Path, profile: Option<&str>) -> Result<Loaded, ConfigError> {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut sources = Vec::new();
        let read = |p: &Path| -> Result<Option<String>, ConfigError> {
            match std::fs::read_to_string(p) {
                Ok(t) => Ok(Some(t)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(ConfigError::Io(p.to_owned(), e)),
            }
        };
        let table = |text: &str, p: &Path| -> Result<toml::Table, ConfigError> {
            // Full validation of this file alone (line/column errors) ...
            Config::parse(text, p)?;
            // ... then keep its raw keys for merging.
            text.parse::<toml::Table>()
                .map_err(|e| ConfigError::Invalid(p.to_owned(), e.to_string()))
        };
        let mut merged = toml::Table::new();
        if let Some(text) = read(path)? {
            merged = table(&text, path)?;
            sources.push(path.to_owned());
        }
        let profile = profile
            .map(str::to_owned)
            .or_else(|| merged.get("profile").and_then(|v| v.as_str()).map(str::to_owned))
            .filter(|p| p != "default");
        if let Some(name) = &profile {
            check_name("profile", name)?;
            let p = dir.join("profiles").join(format!("{name}.toml"));
            let text = read(&p)?.ok_or_else(|| {
                ConfigError::Invalid(p.clone(), format!("profile {name:?} not found"))
            })?;
            let over = table(&text, &p)?;
            merge(&mut merged, over);
            sources.push(p);
        }
        // Theme colors go underneath the explicit [colors] table.
        if let Some(theme) = merged.get("theme").and_then(|v| v.as_str()).map(str::to_owned) {
            check_name("theme", &theme)?;
            let (text, origin) = match BUILTIN_THEMES.iter().find(|(n, _)| *n == theme) {
                Some((_, t)) => (t.to_string(), PathBuf::from(format!("<built-in theme {theme}>"))),
                None => {
                    let p = dir.join("themes").join(format!("{theme}.toml"));
                    let t = read(&p)?.ok_or_else(|| {
                        let names: Vec<&str> = BUILTIN_THEMES.iter().map(|t| t.0).collect();
                        ConfigError::Invalid(
                            p.clone(),
                            format!("theme {theme:?} not found (built-in: {})", names.join(", ")),
                        )
                    })?;
                    sources.push(p.clone());
                    (t, p)
                }
            };
            let colors: toml::Table = text.parse().map_err(|e: toml::de::Error| {
                ConfigError::Invalid(origin.clone(), e.to_string())
            })?;
            toml::Value::Table(colors.clone())
                .try_into::<Colors>()
                .map_err(|e| ConfigError::Invalid(origin.clone(), e.to_string()))?;
            let mut base = colors;
            if let Some(toml::Value::Table(explicit)) = merged.remove("colors") {
                merge(&mut base, explicit);
            }
            merged.insert("colors".into(), toml::Value::Table(base));
        }
        let config: Config = toml::Value::Table(merged)
            .try_into()
            .map_err(|e| ConfigError::Invalid(path.to_owned(), e.to_string()))?;
        config.validate().map_err(|e| ConfigError::Invalid(path.to_owned(), e))?;
        Ok(Loaded { config, profile: profile.unwrap_or_else(|| "default".into()), sources })
    }

    /// `"default"` plus the names of `profiles/*.toml` next to `path`,
    /// sorted.
    pub fn profiles(path: &Path) -> Vec<String> {
        let dir = path.parent().map(|d| d.join("profiles")).unwrap_or_default();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        (p.extension()? == "toml")
                            .then(|| p.file_stem()?.to_str().map(str::to_owned))?
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names.retain(|n| n != "default");
        names.insert(0, "default".into());
        names
    }

    pub fn parse(text: &str, origin: &Path) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text)
            .map_err(|e| ConfigError::Invalid(origin.to_owned(), e.to_string()))?;
        config.validate().map_err(|e| ConfigError::Invalid(origin.to_owned(), e))?;
        Ok(config)
    }

    /// Load `path`. A missing file yields the defaults (`Ok(None)` source).
    pub fn load(path: &Path) -> Result<Option<Config>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text, path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigError::Io(path.to_owned(), e)),
        }
    }

    fn validate(&self) -> Result<(), String> {
        let f = &self.font;
        if !(4.0..=200.0).contains(&f.size) {
            return Err(format!("font.size must be between 4 and 200 points (got {})", f.size));
        }
        if f.family.as_deref().is_some_and(|s| s.trim().is_empty()) {
            return Err("font.family must not be empty (remove it to use the default)".into());
        }
        let w = &self.window;
        if !(10..=1000).contains(&w.columns) || !(3..=1000).contains(&w.rows) {
            return Err(format!(
                "window.columns must be 10-1000 and window.rows 3-1000 (got {}x{})",
                w.columns, w.rows
            ));
        }
        if !(0.0..=200.0).contains(&w.padding) {
            return Err(format!("window.padding must be between 0 and 200 (got {})", w.padding));
        }
        if self.terminal.scrollback > 1_000_000 {
            return Err("terminal.scrollback must be at most 1000000 lines".into());
        }
        let v = &self.visual;
        if !(1..=1000).contains(&v.max_fps) {
            return Err(format!("visual.max_fps must be 1-1000 (got {})", v.max_fps));
        }
        if !(0.0..=1.0).contains(&v.window_opacity) {
            return Err(format!("visual.window_opacity must be 0-1 (got {})", v.window_opacity));
        }
        for (i, b) in self.background.iter().enumerate() {
            b.validate().map_err(|e| format!("background {} ({}): {e}", i + 1, b.kind))?;
        }
        if self.shell.program.as_deref().is_some_and(|s| s.trim().is_empty()) {
            return Err("shell.program must not be empty (remove it to use $SHELL)".into());
        }
        Ok(())
    }
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn color<'de, D: Deserializer<'de>>(d: D) -> Result<Rgb, D::Error> {
    let s = String::deserialize(d)?;
    parse_hex(&s)
        .ok_or_else(|| de::Error::custom(format!("invalid color {s:?}, expected \"#rrggbb\"")))
}

fn opt_color<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Rgb>, D::Error> {
    color(d).map(Some)
}

fn colors8<'de, D: Deserializer<'de>>(d: D) -> Result<[Rgb; 8], D::Error> {
    let v = Vec::<String>::deserialize(d)?;
    if v.len() != 8 {
        return Err(de::Error::custom(format!("expected 8 colors, got {}", v.len())));
    }
    let mut out = [[0; 3]; 8];
    for (i, s) in v.iter().enumerate() {
        out[i] = parse_hex(s).ok_or_else(|| {
            de::Error::custom(format!("invalid color {s:?}, expected \"#rrggbb\""))
        })?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Config, String> {
        Config::parse(s, Path::new("test.toml")).map_err(|e| e.to_string())
    }

    #[test]
    fn documented_default_file_matches_built_in_defaults() {
        assert_eq!(parse(DEFAULT_TOML).unwrap(), Config::default());
    }

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_override() {
        let c =
            parse("[font]\nfamily = \"Fira Code\"\n[colors]\nbackground = \"#000000\"\n").unwrap();
        assert_eq!(c.font.family.as_deref(), Some("Fira Code"));
        assert_eq!(c.font.size, 11.0, "unset keys keep defaults");
        assert_eq!(c.colors.background, [0, 0, 0]);
        assert_eq!(c.colors.foreground, Colors::default().foreground);
    }

    #[test]
    fn typos_are_errors_with_location() {
        let e = parse("[font]\nsise = 12\n").unwrap_err();
        assert!(e.contains("sise") && e.contains("line 2"), "{e}");
        let e = parse("[colour]\n").unwrap_err();
        assert!(e.contains("colour"), "{e}");
    }

    #[test]
    fn bad_values_are_explained() {
        let e = parse("[colors]\nforeground = \"red\"\n").unwrap_err();
        assert!(e.contains("#rrggbb"), "{e}");
        let e = parse("[colors]\nnormal = [\"#000000\"]\n").unwrap_err();
        assert!(e.contains("expected 8 colors"), "{e}");
        let e = parse("[font]\nsize = 1000.0\n").unwrap_err();
        assert!(e.contains("font.size"), "{e}");
        let e = parse("[window]\ncolumns = 2\n").unwrap_err();
        assert!(e.contains("window.columns"), "{e}");
        let e = parse("[font]\nsize = \"big\"\n").unwrap_err();
        assert!(e.contains("line 2"), "{e}");
    }

    fn dir_with(files: &[(&str, &str)]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "seraphim-cfg-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        for (name, text) in files {
            let p = d.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        d.join("config.toml")
    }

    #[test]
    fn builtin_themes_parse_and_celestial_is_the_default() {
        for (name, text) in BUILTIN_THEMES {
            let colors: Colors = toml::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            if *name == "celestial" {
                assert_eq!(colors, Colors::default());
            }
        }
    }

    #[test]
    fn theme_then_file_then_profile() {
        let cfg = dir_with(&[
            (
                "config.toml",
                "theme = \"monochrome\"\n[colors]\ncursor = \"#ff0000\"\n[font]\nsize = 12.0\n",
            ),
            (
                "profiles/work.toml",
                "[font]\nfamily = \"Fira Code\"\n[colors]\nbackground = \"#000000\"\n",
            ),
        ]);
        let l = Config::load_layered(&cfg, None).unwrap();
        let mono: Colors = toml::from_str(BUILTIN_THEMES[1].1).unwrap();
        assert_eq!(l.config.colors.foreground, mono.foreground, "theme colors apply");
        assert_eq!(l.config.colors.cursor, [255, 0, 0], "explicit colors beat the theme");
        assert_eq!(l.profile, "default");
        let l = Config::load_layered(&cfg, Some("work")).unwrap();
        assert_eq!(l.config.font.family.as_deref(), Some("Fira Code"));
        assert_eq!(l.config.font.size, 12.0, "tables merge key by key");
        assert_eq!(l.config.colors.background, [0, 0, 0]);
        assert_eq!(
            l.config.colors.foreground, mono.foreground,
            "profile colors layer over the theme"
        );
        assert_eq!(l.sources.len(), 2);
        assert_eq!(Config::profiles(&cfg), ["default", "work"]);
    }

    #[test]
    fn profile_backgrounds_replace_the_base_list() {
        let cfg = dir_with(&[
            ("config.toml", "[[background]]\nkind = \"solid\"\ncolor = \"#112233\"\n[[background]]\nkind = \"shader\"\nshader = \"aurora\"\n"),
            ("profiles/calm.toml", "[[background]]\nkind = \"solid\"\ncolor = \"#000000cc\"\n"),
        ]);
        assert_eq!(Config::load_layered(&cfg, None).unwrap().config.background.len(), 2);
        let bg = Config::load_layered(&cfg, Some("calm")).unwrap().config.background;
        assert_eq!(bg.len(), 1);
        assert_eq!(parse_rgba(bg[0].color.as_deref().unwrap()).unwrap()[3], 0.8);
    }

    #[test]
    fn layered_errors_name_the_right_file() {
        let cfg = dir_with(&[
            ("config.toml", "profile = \"broken\"\n"),
            ("profiles/broken.toml", "[font]\nsise = 3\n"),
        ]);
        let e = Config::load_layered(&cfg, None).unwrap_err().to_string();
        assert!(
            e.contains("profiles/broken.toml") && e.contains("line 2") && e.contains("sise"),
            "{e}"
        );
        let e = Config::load_layered(&cfg, Some("missing")).unwrap_err().to_string();
        assert!(e.contains("not found"), "{e}");
        let cfg = dir_with(&[("config.toml", "theme = \"neon\"\n")]);
        let e = Config::load_layered(&cfg, None).unwrap_err().to_string();
        assert!(e.contains("celestial"), "lists built-in themes: {e}");
        let e = Config::load_layered(&cfg, Some("../etc")).unwrap_err().to_string();
        assert!(e.contains("invalid profile name"), "{e}");
    }

    #[test]
    fn custom_theme_file() {
        let cfg = dir_with(&[
            ("config.toml", "theme = \"mine\"\n"),
            ("themes/mine.toml", "background = \"#123456\"\n"),
        ]);
        assert_eq!(
            Config::load_layered(&cfg, None).unwrap().config.colors.background,
            [0x12, 0x34, 0x56]
        );
    }

    #[test]
    fn background_validation() {
        let bad = [
            ("[[background]]\nkind = \"solid\"\n", "needs `color`"),
            ("[[background]]\nkind = \"plasma\"\n", "unknown kind"),
            ("[[background]]\nkind = \"gradient\"\ncolors = []\n", "1-8"),
            ("[[background]]\nkind = \"solid\"\ncolor = \"red\"\n", "#rrggbbaa"),
            ("[[background]]\nkind = \"image\"\npath = \"x.png\"\nfit = \"zoom\"\n", "fit"),
            ("[[background]]\nkind = \"shader\"\nshader = \"aurora\"\nopacity = 2.0\n", "opacity"),
            ("[[background]]\nkind = \"solid\"\ncolor = \"#000000\"\nblend = \"dodge\"\n", "blend"),
            ("[visual]\nwindow_opacity = 1.5\n", "window_opacity"),
            ("[[background]]\nkind = \"shader\"\nshadr = \"aurora\"\n", "shadr"),
        ];
        for (text, want) in bad {
            let e = parse(text).unwrap_err();
            assert!(e.contains(want), "{text:?}: {e}");
        }
        let ok = parse(
            "[[background]]\nkind = \"shader\"\nshader = \"aurora\"\nparams = { speed = 0.5 }\n",
        )
        .unwrap();
        assert_eq!(ok.background[0].params["speed"], 0.5);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        assert!(Config::load(Path::new("/nonexistent/seraphim.toml")).unwrap().is_none());
    }

    #[test]
    fn default_path_follows_xdg() {
        // Only checks the shape; environment is shared by tests.
        if let Some(p) = Config::default_path() {
            assert!(p.ends_with("seraphim/config.toml"));
        }
    }
}

/// The result of [`Config::load_layered`].
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub config: Config,
    /// Active profile (`"default"` when none).
    pub profile: String,
    /// Files that contributed, in order.
    pub sources: Vec<PathBuf>,
}

fn check_name(what: &str, name: &str) -> Result<(), ConfigError> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(ConfigError::Invalid(
            PathBuf::from(what),
            format!("invalid {what} name {name:?}"),
        ));
    }
    Ok(())
}

/// Deep-merge `over` into `base`: tables merge, everything else replaces.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

/// `#rrggbb` or `#rrggbbaa` to straight RGBA 0..1.
pub fn parse_rgba(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|b| b as f32 / 255.0);
    Some([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 1.0 }])
}

impl Background {
    fn validate(&self) -> Result<(), String> {
        let color = |c: &str| {
            parse_rgba(c).ok_or_else(|| {
                format!("invalid color {c:?}, expected \"#rrggbb\" or \"#rrggbbaa\"")
            })
        };
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(format!("opacity must be 0-1 (got {})", self.opacity));
        }
        if !["normal", "add", "multiply", "screen"].contains(&self.blend.as_str()) {
            return Err(format!(
                "blend must be normal, add, multiply or screen (got {:?})",
                self.blend
            ));
        }
        match self.kind.as_str() {
            "solid" => {
                color(self.color.as_deref().ok_or("a solid layer needs `color`")?)?;
            }
            "gradient" => {
                if self.colors.is_empty() || self.colors.len() > 8 {
                    return Err("a gradient needs 1-8 `colors`".into());
                }
                for c in &self.colors {
                    color(c)?;
                }
            }
            "image" => {
                if self.path.as_deref().is_none_or(|p| p.trim().is_empty()) {
                    return Err("an image layer needs `path`".into());
                }
                if !["cover", "contain", "stretch", "tile"].contains(&self.fit.as_str()) {
                    return Err(format!(
                        "fit must be cover, contain, stretch or tile (got {:?})",
                        self.fit
                    ));
                }
            }
            "shader" => {
                if self.shader.as_deref().is_none_or(|p| p.trim().is_empty()) {
                    return Err(
                        "a shader layer needs `shader` (a built-in name or a .wgsl file)".into()
                    );
                }
            }
            "" => return Err("missing `kind` (solid, gradient, image or shader)".into()),
            k => return Err(format!("unknown kind {k:?} (solid, gradient, image or shader)")),
        }
        Ok(())
    }

    /// Resolve `path`/`shader` file paths: `~` and paths relative to the
    /// configuration directory.
    pub fn resolve(&self, file: &str, config_dir: &Path) -> PathBuf {
        if let Some(rest) = file.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
        let p = PathBuf::from(file);
        if p.is_absolute() {
            p
        } else {
            config_dir.join(p)
        }
    }
}
