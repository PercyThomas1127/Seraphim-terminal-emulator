//! # seraphim-config
//!
//! The Configuration System (spec §7): loads and validates
//! `~/.config/seraphim/config.toml`. Unknown keys are errors, so typos are
//! reported instead of silently ignored. Profiles, themes directories and
//! live reload (spec §7.1–7.2) build on this in Phase II.

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
    pub font: Font,
    pub window: Window,
    pub terminal: TerminalConfig,
    pub shell: Shell,
    pub clipboard: Clipboard,
    pub colors: Colors,
    pub background: Background,
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

/// The animated background layer drawn behind the text.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Background {
    pub style: BackgroundStyle,
    /// Strength of the effects over `colors.background`, 0..1.
    pub intensity: f32,
    /// Animation speed; 0 draws a still image and stops redrawing.
    pub speed: f32,
    /// Frame rate cap while animating.
    pub fps: u32,
    /// Three scene colors; when unset, taken from the palette.
    #[serde(deserialize_with = "opt_colors3")]
    pub colors: Option<[Rgb; 3]>,
    /// The two colors the scene turns to in a root session.
    #[serde(deserialize_with = "colors2")]
    pub privileged_colors: [Rgb; 2],
    /// Color of the wave shown when a package is installed.
    #[serde(deserialize_with = "color")]
    pub install_color: Rgb,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundStyle {
    #[default]
    None,
    Aurora,
    Nebula,
    Starfield,
}

impl Default for Background {
    fn default() -> Self {
        let h = |s: &str| parse_hex(s).expect("valid built-in color");
        Background {
            style: BackgroundStyle::None,
            intensity: 0.6,
            speed: 1.0,
            fps: 30,
            colors: None,
            privileged_colors: [h("#ff3b4e"), h("#ff9a3c")],
            install_color: h("#ffd98a"),
        }
    }
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
        let b = &self.background;
        if !(0.0..=1.0).contains(&b.intensity) {
            return Err(format!(
                "background.intensity must be between 0 and 1 (got {})",
                b.intensity
            ));
        }
        if !(0.0..=10.0).contains(&b.speed) {
            return Err(format!("background.speed must be between 0 and 10 (got {})", b.speed));
        }
        if !(1..=240).contains(&b.fps) {
            return Err(format!("background.fps must be between 1 and 240 (got {})", b.fps));
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

fn opt_colors3<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[Rgb; 3]>, D::Error> {
    colors_n::<D, 3>(d).map(Some)
}

fn colors2<'de, D: Deserializer<'de>>(d: D) -> Result<[Rgb; 2], D::Error> {
    colors_n::<D, 2>(d)
}

fn colors_n<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<[Rgb; N], D::Error> {
    let v = Vec::<String>::deserialize(d)?;
    if v.len() != N {
        return Err(de::Error::custom(format!("expected {N} colors, got {}", v.len())));
    }
    let mut out = [[0; 3]; N];
    for (i, s) in v.iter().enumerate() {
        out[i] = parse_hex(s).ok_or_else(|| {
            de::Error::custom(format!("invalid color {s:?}, expected \"#rrggbb\""))
        })?;
    }
    Ok(out)
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

    #[test]
    fn background_section() {
        let c = parse(
            "[background]\nstyle = \"aurora\"\nintensity = 0.4\ncolors = [\"#ff0000\", \"#00ff00\", \"#0000ff\"]\n",
        )
        .unwrap();
        assert_eq!(c.background.style, BackgroundStyle::Aurora);
        assert_eq!(c.background.intensity, 0.4);
        assert_eq!(c.background.colors, Some([[255, 0, 0], [0, 255, 0], [0, 0, 255]]));
        assert_eq!(c.background.fps, 30, "unset keys keep defaults");
        let e = parse("[background]\nstyle = \"lava\"\n").unwrap_err();
        assert!(e.contains("lava") && e.contains("aurora"), "{e}");
        let e = parse("[background]\nintensity = 2.0\n").unwrap_err();
        assert!(e.contains("background.intensity"), "{e}");
        let e = parse("[background]\nprivileged_colors = [\"#ff0000\"]\n").unwrap_err();
        assert!(e.contains("expected 2 colors"), "{e}");
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
