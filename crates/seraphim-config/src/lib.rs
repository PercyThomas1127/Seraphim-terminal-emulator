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
//! A **preset** (`preset = "name"`: built-in, or `presets/<name>.toml`) goes
//! underneath all of that: a file of `[[background]]` layers, `[[on]]`
//! interaction rules and `[visual]` settings. Layers from the
//! configuration with the same `id` merge into the preset's; others stack
//! on top. Rules add up.
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

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Color theme: a built-in name ([`BUILTIN_THEMES`]) or a file in
    /// `themes/`.
    pub theme: Option<String>,
    /// Profile applied on top of this file at startup.
    pub profile: Option<String>,
    /// Apply changes to the configuration files (and shaders, images)
    /// as soon as they are saved.
    pub live_reload: bool,
    pub font: Font,
    pub window: Window,
    pub terminal: TerminalConfig,
    pub shell: Shell,
    pub clipboard: Clipboard,
    pub colors: Colors,
    pub visual: Visual,
    pub cursor: CursorFx,
    /// Background preset: a built-in name ([`BUILTIN_PRESETS`]) or a file
    /// in `presets/`; `"none"` for no preset.
    pub preset: Option<String>,
    /// Background layers, bottom first (spec §5.1, §5.4).
    pub background: Vec<Background>,
    /// Interaction rules: animate layers when events happen.
    pub on: Vec<Rule>,
    /// Continuous bindings: a layer parameter follows a live value.
    pub bind: Vec<Bind>,
    /// Menu bar additions ([`BUILTIN_MENU`] is the base).
    pub menu: Vec<Menu>,
}

/// The built-in menu bar, as TOML (`[[menu]]` entries).
pub const BUILTIN_MENU: &str = include_str!("menu.toml");

/// Menu actions an item can run (`action = "..."`).
pub const MENU_ACTIONS: &[&str] = &[
    "new-window",
    "open-file-manager",
    "close-window",
    "copy",
    "paste",
    "select-all",
    "clear-scrollback",
    "clear-and-reset",
    "toggle-menu-bar",
    "fullscreen",
    "zoom-in",
    "zoom-out",
    "zoom-reset",
    "scroll-top",
    "scroll-bottom",
    "profiles",
    "next-profile",
    "reload-config",
    "open-config-folder",
    "shortcuts",
    "about",
];

/// One menu of the menu bar.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Menu {
    /// `&` marks the access key, e.g. `"&Tools"`.
    pub title: String,
    pub items: Vec<MenuItem>,
    /// Replace a built-in menu of the same title instead of appending.
    pub replace: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MenuItem {
    pub label: Option<String>,
    /// One of [`MENU_ACTIONS`].
    pub action: Option<String>,
    /// Text typed into the shell, e.g. `"htop\n"`.
    pub send: Option<String>,
    /// Switch to this profile.
    pub profile: Option<String>,
    pub separator: bool,
}

impl Menu {
    fn validate(&self) -> Result<(), String> {
        if self.title.replace('&', "").trim().is_empty() {
            return Err("missing `title`".into());
        }
        for (i, it) in self.items.iter().enumerate() {
            let what = [it.action.is_some(), it.send.is_some(), it.profile.is_some()];
            let n = what.iter().filter(|b| **b).count();
            let err = |e: String| Err(format!("item {}: {e}", i + 1));
            if it.separator {
                if n > 0 || it.label.is_some() {
                    return err("a separator takes no label or command".into());
                }
                continue;
            }
            if n != 1 {
                return err("needs exactly one of `action`, `send` or `profile`".into());
            }
            if let Some(a) = &it.action {
                if !MENU_ACTIONS.contains(&a.as_str()) {
                    return err(format!("unknown action {a:?} ({})", MENU_ACTIONS.join(", ")));
                }
                if a == "profiles" {
                    continue; // expands to the profile list; no label
                }
            }
            if it.label.as_deref().is_none_or(|l| l.trim().is_empty()) {
                return err("missing `label`".into());
            }
        }
        Ok(())
    }
}

/// The built-in menus.
pub fn builtin_menu() -> Vec<Menu> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        menu: Vec<Menu>,
    }
    toml::from_str::<File>(BUILTIN_MENU).expect("valid built-in menu").menu
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: None,
            profile: None,
            live_reload: true,
            font: Default::default(),
            window: Default::default(),
            terminal: Default::default(),
            shell: Default::default(),
            clipboard: Default::default(),
            colors: Default::default(),
            visual: Default::default(),
            cursor: Default::default(),
            preset: None,
            background: Vec::new(),
            on: Vec::new(),
            bind: Vec::new(),
            menu: Vec::new(),
        }
    }
}

/// Built-in presets: (name, TOML).
pub const BUILTIN_PRESETS: &[(&str, &str)] = &[("heavens", include_str!("presets/heavens.toml"))];

/// "When this event happens, animate that layer property."
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    /// Event name: exact, `prefix.*` or `*`.
    pub event: String,
    /// Conditions on the event's data: key (dotted path) = value. Strings
    /// are globs (`*`, `?`); a list matches if any entry does.
    pub when: std::collections::BTreeMap<String, toml::Value>,
    /// Target layer id.
    pub layer: String,
    /// A shader parameter, or `opacity`.
    pub param: String,
    pub to: f32,
    /// Jump here first, then animate to `to` (restarts one-shot effects).
    pub from: Option<f32>,
    pub duration_ms: u32,
    /// An easing name, `spring` or `instant`.
    pub easing: String,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            event: String::new(),
            when: Default::default(),
            layer: String::new(),
            param: String::new(),
            to: 0.0,
            from: None,
            duration_ms: 300,
            easing: "ease-in-out".into(),
        }
    }
}

/// Metric sources a `[[bind]]` can follow, with their natural input range.
pub const BIND_SOURCES: &[(&str, [f32; 2])] = &[
    ("system.cpu", [0.0, 100.0]),
    ("system.memory", [0.0, 100.0]),
    ("system.load", [0.0, 1.0]),
    ("battery.level", [0.0, 100.0]),
    ("battery.charging", [0.0, 1.0]),
    ("time.hour", [0.0, 24.0]),
];

/// "This layer property follows that value" (spec 5.3, 11.4).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bind {
    /// A metric ([`BIND_SOURCES`]).
    pub source: Option<String>,
    /// Or the latest event with this name ...
    pub event: Option<String>,
    /// ... and this field of its data (dotted path).
    pub field: Option<String>,
    pub layer: String,
    /// A shader parameter, or `opacity`.
    pub param: String,
    /// Source values mapped to `output` (default: the source's range;
    /// 0-1 for events).
    pub input: Option<[f32; 2]>,
    pub output: [f32; 2],
    /// Exact values instead of a range: `{ "0" = 0.85, default = 0.0 }`.
    pub map: std::collections::BTreeMap<String, f32>,
    /// Ease to each new value over this many milliseconds.
    pub smoothing_ms: u32,
    /// How often metrics are sampled.
    pub interval_ms: u32,
}

impl Default for Bind {
    fn default() -> Self {
        Bind {
            source: None,
            event: None,
            field: None,
            layer: String::new(),
            param: String::new(),
            input: None,
            output: [0.0, 1.0],
            map: Default::default(),
            smoothing_ms: 600,
            interval_ms: 1000,
        }
    }
}

impl Bind {
    fn validate(&self) -> Result<(), String> {
        match (&self.source, &self.event) {
            (Some(s), None) => {
                if !BIND_SOURCES.iter().any(|(n, _)| n == s) {
                    let names: Vec<&str> = BIND_SOURCES.iter().map(|b| b.0).collect();
                    return Err(format!("unknown source {s:?} ({})", names.join(", ")));
                }
            }
            (None, Some(e)) => {
                if e.trim().is_empty() || self.field.as_deref().is_none_or(|f| f.trim().is_empty())
                {
                    return Err("an event binding needs `event` and `field`".into());
                }
            }
            _ => return Err("needs exactly one of `source` or `event`".into()),
        }
        if self.layer.trim().is_empty() || self.param.trim().is_empty() {
            return Err("needs `layer` and `param`".into());
        }
        if self.input.is_some_and(|[a, b]| a == b) {
            return Err("`input` must be two different values".into());
        }
        if !(200..=3_600_000).contains(&self.interval_ms) {
            return Err("interval_ms must be 200-3600000".into());
        }
        if self.smoothing_ms > 60_000 {
            return Err("smoothing_ms must be at most 60000".into());
        }
        Ok(())
    }
}

pub const EASINGS: &[&str] = &[
    "linear",
    "ease",
    "ease-in",
    "ease-out",
    "ease-in-out",
    "ease-out-expo",
    "ease-out-back",
    "spring",
    "instant",
];

impl Rule {
    fn validate(&self) -> Result<(), String> {
        if self.event.trim().is_empty() {
            return Err("missing `event` (e.g. \"session.privileged\")".into());
        }
        if self.layer.trim().is_empty() {
            return Err("missing `layer` (the id of a [[background]] layer)".into());
        }
        if self.param.trim().is_empty() {
            return Err("missing `param` (a shader parameter or \"opacity\")".into());
        }
        if !EASINGS.contains(&self.easing.as_str()) {
            return Err(format!("unknown easing {:?} ({})", self.easing, EASINGS.join(", ")));
        }
        if self.duration_ms > 600_000 {
            return Err("duration_ms must be at most 600000".into());
        }
        for (k, v) in &self.when {
            let ok = match v {
                toml::Value::String(_) | toml::Value::Boolean(_) => true,
                toml::Value::Integer(_) | toml::Value::Float(_) => true,
                toml::Value::Array(a) => a.iter().all(|x| x.is_str()),
                _ => false,
            };
            if !ok {
                return Err(format!(
                    "when.{k}: expected a string, number, boolean or list of strings"
                ));
            }
        }
        Ok(())
    }
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
    /// Duration of scene changes (profile switches), in milliseconds.
    pub transition_ms: u32,
    /// How scenes change: `fade`, `slide`, `wipe`, `dissolve`, `zoom`,
    /// `blur`, `fade-through`, or a `.wgsl` file implementing
    /// `fn transition(uv, p)` (in `transitions/` or the config directory).
    pub transition: String,
}

/// Cursor and selection effects (`[cursor]`). Reduced motion turns the
/// moving ones off.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CursorFx {
    /// Glide between cells over this many milliseconds (0: jump).
    pub glide_ms: u32,
    /// Leave a fading trail while gliding.
    pub trail: bool,
    /// Fade the cursor in and out when blinking (0: hard blink).
    pub blink_fade_ms: u32,
    /// Soft glow around the cursor, 0 to 1.
    pub glow: f32,
    /// A ring spreads from the cursor on each key press.
    pub ripple: bool,
    /// Fade a new selection in over this many milliseconds.
    pub selection_fade_ms: u32,
}

impl Default for CursorFx {
    fn default() -> Self {
        CursorFx {
            glide_ms: 80,
            trail: false,
            blink_fade_ms: 150,
            glow: 0.0,
            ripple: false,
            selection_fade_ms: 90,
        }
    }
}

impl Default for Visual {
    fn default() -> Self {
        Visual {
            reduced_motion: false,
            max_fps: 60,
            animate_unfocused: false,
            window_opacity: 1.0,
            transition_ms: 400,
            transition: "fade".into(),
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
    /// `image`: blur radius in image pixels.
    pub blur: f32,
    /// `shader`: a built-in name (`aurora`, `starfield`, `heavens`) or a `.wgsl` file.
    pub shader: Option<String>,
    pub opacity: f32,
    /// `normal`, `add`, `multiply` or `screen`.
    pub blend: String,
    /// Shader parameters.
    pub params: std::collections::BTreeMap<String, f32>,
    /// `shader`: named colors, `"#rrggbb[aa]"` or a theme color
    /// ([`is_color_ref`]).
    pub palette: std::collections::BTreeMap<String, String>,
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
            blur: 0.0,
            shader: None,
            opacity: 1.0,
            blend: "normal".into(),
            params: Default::default(),
            palette: Default::default(),
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
    /// Keep the menu bar on screen. Off by default: the bar appears when
    /// Alt/Option is tapped and hides when the menu closes (Ctrl+Shift+M
    /// toggles at runtime).
    pub menu_bar: bool,
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
        Window { columns: 100, rows: 30, padding: 6.0, menu_bar: false }
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
        Config::load_layered_with(path, profile, None)
    }

    /// [`Config::load_layered`] with `preset` overriding the files' own
    /// (e.g. from the command line).
    pub fn load_layered_with(
        path: &Path,
        profile: Option<&str>,
        preset: Option<&str>,
    ) -> Result<Loaded, ConfigError> {
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
            Config::parse_with(text, p, true)?;
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
        if let Some(p) = preset {
            merged.insert("preset".into(), toml::Value::String(p.to_owned()));
        }
        // The preset goes underneath everything loaded so far.
        let preset = merged.get("preset").and_then(|v| v.as_str()).map(str::to_owned);
        if let Some(name) = preset.filter(|p| p != "none") {
            check_name("preset", &name)?;
            let (text, origin) = match BUILTIN_PRESETS.iter().find(|(n, _)| *n == name) {
                Some((_, t)) => (t.to_string(), PathBuf::from(format!("<built-in preset {name}>"))),
                None => {
                    let p = dir.join("presets").join(format!("{name}.toml"));
                    let t = read(&p)?.ok_or_else(|| {
                        let names: Vec<&str> = BUILTIN_PRESETS.iter().map(|t| t.0).collect();
                        ConfigError::Invalid(
                            p.clone(),
                            format!("preset {name:?} not found (built-in: {})", names.join(", ")),
                        )
                    })?;
                    sources.push(p.clone());
                    (t, p)
                }
            };
            let mut base = parse_preset(&text, &origin)?;
            merge_over_preset(&mut base, merged);
            merged = base;
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
        config.validate(false).map_err(|e| ConfigError::Invalid(path.to_owned(), e))?;
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
        Config::parse_with(text, origin, false)
    }

    /// `partial`: one file of several, where a `[[background]]` with an
    /// `id` and no `kind` may modify a preset's layer.
    fn parse_with(text: &str, origin: &Path, partial: bool) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text)
            .map_err(|e| ConfigError::Invalid(origin.to_owned(), e.to_string()))?;
        config.validate(partial).map_err(|e| ConfigError::Invalid(origin.to_owned(), e))?;
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

    fn validate(&self, partial: bool) -> Result<(), String> {
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
        let c = &self.cursor;
        if !(0.0..=1.0).contains(&c.glow) {
            return Err(format!("cursor.glow must be 0-1 (got {})", c.glow));
        }
        if c.glide_ms > 1000 || c.blink_fade_ms > 1000 || c.selection_fade_ms > 1000 {
            return Err("cursor durations must be at most 1000 ms".into());
        }
        const TRANSITIONS: &[&str] =
            &["fade", "slide", "wipe", "dissolve", "zoom", "blur", "fade-through"];
        if !TRANSITIONS.contains(&v.transition.as_str()) && !v.transition.ends_with(".wgsl") {
            return Err(format!(
                "visual.transition must be {} or a .wgsl file (got {:?})",
                TRANSITIONS.join(", "),
                v.transition
            ));
        }
        if !(0.0..=1.0).contains(&v.window_opacity) {
            return Err(format!("visual.window_opacity must be 0-1 (got {})", v.window_opacity));
        }
        for (i, b) in self.background.iter().enumerate() {
            let name = b.id.clone().unwrap_or_else(|| (i + 1).to_string());
            if b.kind.is_empty() && b.id.is_some() {
                if partial {
                    continue;
                }
                return Err(format!(
                    "background {name:?}: missing `kind`, and no preset layer has this id"
                ));
            }
            b.validate().map_err(|e| format!("background {name:?} ({}): {e}", b.kind))?;
        }
        for m in &self.menu {
            m.validate().map_err(|e| format!("menu {:?}: {e}", m.title))?;
        }
        for (i, b) in self.bind.iter().enumerate() {
            let what = b.source.clone().or_else(|| b.event.clone()).unwrap_or_default();
            b.validate().map_err(|e| format!("bind {} ({what}): {e}", i + 1))?;
        }
        for (i, r) in self.on.iter().enumerate() {
            r.validate().map_err(|e| format!("on {} ({}): {e}", i + 1, r.event))?;
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
    fn menus() {
        let builtin = builtin_menu();
        let titles: Vec<String> = builtin.iter().map(|m| m.title.replace('&', "")).collect();
        assert_eq!(titles, ["File", "Edit", "View", "Profiles", "Help"]);
        for m in &builtin {
            m.validate().unwrap();
        }
        let c = parse("[[menu]]\ntitle = \"&Tools\"\nitems = [{ label = \"htop\", send = \"htop\\n\" }, { separator = true }, { label = \"Work\", profile = \"work\" }]\n").unwrap();
        assert_eq!(c.menu[0].items[0].send.as_deref(), Some("htop\n"));
        let e = parse("[[menu]]\ntitle = \"T\"\nitems = [{ label = \"x\" }]\n").unwrap_err();
        assert!(e.contains("exactly one"), "{e}");
        let e =
            parse("[[menu]]\ntitle = \"T\"\nitems = [{ label = \"x\", action = \"explode\" }]\n")
                .unwrap_err();
        assert!(e.contains("explode") && e.contains("new-window"), "{e}");
        let e = parse("[[menu]]\ntitle = \"T\"\nitems = [{ send = \"x\" }]\n").unwrap_err();
        assert!(e.contains("missing `label`"), "{e}");
    }

    #[test]
    fn builtin_presets_are_valid() {
        for (name, text) in BUILTIN_PRESETS {
            let t = parse_preset(text, Path::new(name)).unwrap();
            assert!(t.contains_key("background"), "{name}");
        }
    }

    #[test]
    fn preset_layers_merge_by_id_and_rules_add_up() {
        let cfg = dir_with(&[
            (
                "config.toml",
                "preset = \"heavens\"\n[[background]]\nid = \"sky\"\nparams = { style = 2 }\n\
                 [[background]]\nkind = \"solid\"\ncolor = \"#00000040\"\n\
                 [[on]]\nevent = \"terminal.bell\"\nlayer = \"sky\"\nparam = \"alert\"\nto = 1\n",
            ),
            ("profiles/still.toml", "[[background]]\nid = \"sky\"\nparams = { speed = 0 }\n"),
        ]);
        let c = Config::load_layered(&cfg, None).unwrap().config;
        assert_eq!(c.background.len(), 2, "override merged, new layer stacked on top");
        let sky = &c.background[0];
        assert_eq!(sky.shader.as_deref(), Some("heavens"));
        assert_eq!(sky.params["style"], 2.0, "config overrides the preset's param");
        assert_eq!(sky.params["intensity"], 0.6, "other params kept");
        assert_eq!(sky.palette["base"], "theme.background");
        assert_eq!(c.background[1].kind, "solid");
        assert_eq!(c.visual.max_fps, 30, "preset [visual] applies");
        assert_eq!(c.on.len(), 5, "4 preset rules + 1");
        assert_eq!(c.on[4].event, "terminal.bell");
        // A profile works the same way, on top of the preset.
        let c = Config::load_layered(&cfg, Some("still")).unwrap().config;
        assert_eq!(c.background.len(), 1);
        assert_eq!(c.background[0].params["speed"], 0.0);
        assert_eq!(c.background[0].params["style"], 1.0, "profile list replaced config's");
    }

    #[test]
    fn user_presets_and_errors() {
        let cfg = dir_with(&[
            ("config.toml", "preset = \"mine\"\n"),
            (
                "presets/mine.toml",
                "[[background]]\nid = \"a\"\nkind = \"solid\"\ncolor = \"#102030\"\n",
            ),
            ("presets/bad.toml", "[font]\nsize = 12.0\n"),
            ("profiles/bad.toml", "preset = \"bad\"\n"),
            ("profiles/missing.toml", "preset = \"nope\"\n"),
            ("profiles/orphan.toml", "[[background]]\nid = \"zzz\"\nopacity = 0.5\n"),
            ("profiles/none.toml", "preset = \"none\"\n"),
        ]);
        let l = Config::load_layered(&cfg, None).unwrap();
        assert_eq!(l.config.background[0].color.as_deref(), Some("#102030"));
        assert!(l.sources.iter().any(|p| p.ends_with("presets/mine.toml")));
        let e = Config::load_layered(&cfg, Some("bad")).unwrap_err().to_string();
        assert!(e.contains("may only set") && e.contains("font"), "{e}");
        let e = Config::load_layered(&cfg, Some("missing")).unwrap_err().to_string();
        assert!(e.contains("not found") && e.contains("heavens"), "{e}");
        let e = Config::load_layered(&cfg, Some("orphan")).unwrap_err().to_string();
        assert!(e.contains("zzz") && e.contains("no preset layer"), "{e}");
        assert!(Config::load_layered(&cfg, Some("none")).unwrap().config.background.is_empty());
    }

    #[test]
    fn bindings_are_validated() {
        let c = parse("[[bind]]\nsource = \"system.cpu\"\nlayer = \"sky\"\nparam = \"speed\"\noutput = [0.5, 2]\n\n[[bind]]\nevent = \"command.completed\"\nfield = \"exit_code\"\nlayer = \"sky\"\nparam = \"alert\"\nmap = { \"0\" = 0.0, default = 1.0 }\n").unwrap();
        assert_eq!(c.bind.len(), 2);
        assert_eq!(c.bind[0].smoothing_ms, 600);
        for (bad, want) in [
            ("source = \"system.gpu\"", "unknown source"),
            ("source = \"system.cpu\"\nevent = \"x\"", "exactly one"),
            ("event = \"x\"", "needs `event` and `field`"),
            ("source = \"system.cpu\"\ninput = [1, 1]", "two different"),
        ] {
            let e = parse(&format!("[[bind]]\nlayer = \"a\"\nparam = \"b\"\n{bad}\n")).unwrap_err();
            assert!(e.contains(want), "{bad}: {e}");
        }
    }

    #[test]
    fn rules_and_palettes_are_validated() {
        let e = parse("[[on]]\nevent = \"x\"\nlayer = \"a\"\nparam = \"p\"\neasing = \"bouncy\"\n")
            .unwrap_err();
        assert!(e.contains("bouncy") && e.contains("ease-out"), "{e}");
        let e = parse("[[on]]\nlayer = \"a\"\nparam = \"p\"\n").unwrap_err();
        assert!(e.contains("missing `event`"), "{e}");
        let e = parse(
            "[[on]]\nevent = \"x\"\nlayer = \"a\"\nparam = \"p\"\nwhen = { a = { b = 1 } }\n",
        )
        .unwrap_err();
        assert!(e.contains("when.a"), "{e}");
        let e = parse("[[background]]\nkind = \"shader\"\nshader = \"heavens\"\npalette = { base = \"theme.bright.9\" }\n")
            .unwrap_err();
        assert!(e.contains("palette.base"), "{e}");
        for ok in ["#123456", "#12345678", "theme.background", "theme.normal.0", "theme.bright.7"] {
            assert!(is_color_ref(ok), "{ok}");
        }
        assert!(!is_color_ref("theme.ansi.3") && !is_color_ref("red"));
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

/// Keys a preset file may set.
const PRESET_KEYS: &[&str] = &["background", "on", "bind", "visual"];

/// Validate a preset file and return its table.
pub fn parse_preset(text: &str, origin: &Path) -> Result<toml::Table, ConfigError> {
    let invalid = |e: String| ConfigError::Invalid(origin.to_owned(), e);
    Config::parse(text, origin)?;
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| invalid(e.to_string()))?;
    if let Some(k) = table.keys().find(|k| !PRESET_KEYS.contains(&k.as_str())) {
        return Err(invalid(format!(
            "a preset may only set {} (found `{k}`)",
            PRESET_KEYS.join(", ")
        )));
    }
    Ok(table)
}

/// Layer the configuration over a preset: `[[background]]` entries with a
/// matching `id` merge into the preset's layer, others are appended;
/// `[[on]]` rules are appended; everything else deep-merges.
fn merge_over_preset(base: &mut toml::Table, mut over: toml::Table) {
    if let Some(toml::Value::Array(layers)) = over.remove("background") {
        let toml::Value::Array(list) =
            base.entry("background").or_insert_with(|| toml::Value::Array(Vec::new()))
        else {
            unreachable!("validated preset")
        };
        for layer in layers {
            let id = layer.get("id").and_then(|v| v.as_str()).map(str::to_owned);
            let existing = id.and_then(|id| {
                list.iter_mut().find(|l| l.get("id").and_then(|v| v.as_str()) == Some(&id))
            });
            match (existing, layer) {
                (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
                (_, layer) => list.push(layer),
            }
        }
    }
    for key in ["on", "bind"] {
        if let Some(toml::Value::Array(rules)) = over.remove(key) {
            match base.entry(key).or_insert_with(|| toml::Value::Array(Vec::new())) {
                toml::Value::Array(list) => list.extend(rules),
                _ => unreachable!("validated preset"),
            }
        }
    }
    merge(base, over);
}

/// Whether `s` names a color: `#rrggbb`, `#rrggbbaa`, `theme.foreground`,
/// `theme.background`, `theme.cursor`, `theme.selection`,
/// `theme.normal.0`-`7` or `theme.bright.0`-`7`.
pub fn is_color_ref(s: &str) -> bool {
    if s.starts_with('#') {
        return parse_rgba(s).is_some();
    }
    let Some(r) = s.strip_prefix("theme.") else { return false };
    if ["foreground", "background", "cursor", "selection"].contains(&r) {
        return true;
    }
    let idx = r.strip_prefix("normal.").or_else(|| r.strip_prefix("bright."));
    idx.and_then(|i| i.parse::<u8>().ok()).is_some_and(|i| i < 8)
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
                if !(0.0..=200.0).contains(&self.blur) {
                    return Err(format!("blur must be 0-200 (got {})", self.blur));
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
                for (k, v) in &self.palette {
                    if !is_color_ref(v) {
                        return Err(format!(
                            "palette.{k}: invalid color {v:?} (\"#rrggbb\", \"#rrggbbaa\", \"theme.background\", \"theme.bright.5\", ...)"
                        ));
                    }
                }
            }
            "blur" => {}
            "" => return Err("missing `kind` (solid, gradient, image, blur or shader)".into()),
            k => {
                return Err(format!("unknown kind {k:?} (solid, gradient, image, blur or shader)"))
            }
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
