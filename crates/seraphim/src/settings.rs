//! The settings panel's model: which settings it shows, their current
//! values, and editing `seraphim.toml` in place. The file stays the source
//! of truth (spec §7.3); edits keep its comments and layout (`toml_edit`),
//! and live reload (or an explicit reload) applies them, which is the live
//! preview.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Toggle,
    Choice(Vec<String>),
    Number {
        step: f64,
        min: f64,
        max: f64,
    },
    /// The inverse of a boolean key (`extensions.x.disabled` shown as
    /// "enabled").
    Enabled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub label: String,
    /// Dotted key in the file.
    pub key: String,
    pub kind: Kind,
    /// Shown when the file and the documented defaults don't say.
    pub fallback: String,
}

fn s(label: &str, key: &str, kind: Kind) -> Setting {
    Setting { label: label.into(), key: key.into(), kind, fallback: String::new() }
}

/// Names of `dir/*.toml`.
fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension()? == "toml")
                        .then(|| p.file_stem()?.to_str().map(str::to_owned))?
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The settings shown, for a configuration in `dir` with these
/// extensions installed (name, what it asks for).
pub fn all(dir: &Path, extensions: &[(String, String)]) -> Vec<Setting> {
    let mut themes: Vec<String> =
        seraphim_config::BUILTIN_THEMES.iter().map(|(n, _)| n.to_string()).collect();
    themes.extend(names_in(&dir.join("themes")));
    themes.dedup();
    let mut presets = vec!["none".to_owned()];
    presets.extend(seraphim_config::BUILTIN_PRESETS.iter().map(|(n, _)| n.to_string()));
    presets.extend(names_in(&dir.join("presets")));
    let mut transitions = vec!["fade".to_owned()];
    transitions.extend(seraphim_visual::BUILTIN_TRANSITIONS.iter().map(|(n, _)| n.to_string()));
    let choice = |v: &[&str]| Kind::Choice(v.iter().map(|s| s.to_string()).collect());
    let mut out = vec![
        Setting { fallback: "built-in".into(), ..s("Theme", "theme", Kind::Choice(themes)) },
        Setting {
            fallback: "none".into(),
            ..s("Background preset", "preset", Kind::Choice(presets))
        },
        s("Font size", "font.size", Kind::Number { step: 1.0, min: 6.0, max: 72.0 }),
        s(
            "Window opacity (on restart)",
            "visual.window_opacity",
            Kind::Number { step: 0.05, min: 0.2, max: 1.0 },
        ),
        s("Scene transition", "visual.transition", Kind::Choice(transitions)),
        s("Reduced motion", "visual.reduced_motion", Kind::Toggle),
        s("Show the menu bar", "window.menu_bar", Kind::Toggle),
        s(
            "Window decorations (new windows)",
            "window.decorations",
            choice(&["system", "seraphim", "none"]),
        ),
        s("Tab bar", "tabs.bar", choice(&["auto", "always", "never"])),
        s("Tab style", "tabs.style", choice(seraphim_config::TAB_STYLES)),
        s("Tab bar position", "tabs.position", choice(&["top", "bottom"])),
        s("Tab icons", "tabs.icons", Kind::Toggle),
        s("Close buttons on tabs", "tabs.close_button", Kind::Toggle),
        s(
            "Dim panes without focus",
            "panes.dim_inactive",
            Kind::Number { step: 0.05, min: 0.0, max: 0.8 },
        ),
        s("Cursor glow", "cursor.glow", Kind::Number { step: 0.1, min: 0.0, max: 1.0 }),
        s("Cursor ripple on keys", "cursor.ripple", Kind::Toggle),
        s("Apply changes when files are saved", "live_reload", Kind::Toggle),
        s("Restore tabs at startup", "restore_session", Kind::Toggle),
    ];
    for (name, asks) in extensions {
        out.push(Setting {
            fallback: asks.clone(),
            ..s(
                &format!("Extension: {name}"),
                &format!("extensions.{name}.disabled"),
                Kind::Enabled,
            )
        });
    }
    out
}

/// The documented defaults, to show values the file doesn't set.
fn defaults() -> DocumentMut {
    seraphim_config::DEFAULT_TOML.parse().unwrap_or_default()
}

fn lookup<'a>(doc: &'a DocumentMut, key: &str) -> Option<&'a Item> {
    let mut item = doc.as_item();
    for part in key.split('.') {
        item = item.get(part)?;
    }
    Some(item)
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.value().clone(),
        Value::Float(f) => {
            let f = *f.value();
            if f.fract() == 0.0 {
                format!("{f:.0}")
            } else {
                format!("{f:.2}")
            }
        }
        Value::Integer(i) => i.value().to_string(),
        Value::Boolean(b) => if *b.value() { "on" } else { "off" }.into(),
        other => other.to_string().trim().to_owned(),
    }
}

/// The current value of each setting, as text.
pub fn values(file: &Path, settings: &[Setting]) -> Vec<String> {
    let doc: DocumentMut =
        std::fs::read_to_string(file).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let def = defaults();
    settings
        .iter()
        .map(|st| {
            let v =
                lookup(&doc, &st.key).or_else(|| lookup(&def, &st.key)).and_then(Item::as_value);
            match (&st.kind, v) {
                (Kind::Enabled, v) => {
                    let off = v.and_then(Value::as_bool).unwrap_or(false);
                    format!("{}  ({})", if off { "off" } else { "on" }, st.fallback)
                }
                (_, Some(v)) => show(v),
                (_, None) => st.fallback.clone(),
            }
        })
        .collect()
}

/// The value after one step in `dir` (+1 / -1) from `current`.
pub fn step(st: &Setting, current: &str, dir: i32) -> Value {
    match &st.kind {
        Kind::Toggle => Value::from(current != "on"),
        Kind::Enabled => Value::from(current.starts_with("on")),
        Kind::Choice(options) => {
            let i = options.iter().position(|o| o == current);
            let n = options.len().max(1) as i32;
            let next = match i {
                Some(i) => (i as i32 + dir).rem_euclid(n),
                None => 0,
            };
            Value::from(options.get(next as usize).cloned().unwrap_or_default())
        }
        Kind::Number { step, min, max } => {
            let v: f64 = current.parse().unwrap_or(*min);
            let next = ((v + step * dir as f64) / step).round() * step;
            let next = (next.clamp(*min, *max) * 1000.0).round() / 1000.0;
            Value::from(next)
        }
    }
}

/// Set `key` in `file` (created if missing), keeping everything else. A
/// preset of "none" and a theme are plain top-level keys.
pub fn set(file: &Path, key: &str, value: Value) -> Result<(), String> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{}: {e}", file.display()))?;
    let parts: Vec<&str> = key.split('.').collect();
    let (last, tables) = parts.split_last().ok_or("empty key")?;
    let mut table = doc.as_table_mut();
    for t in tables {
        if !table.contains_key(t) {
            let mut new = toml_edit::Table::new();
            new.set_implicit(true);
            table.insert(t, Item::Table(new));
        }
        table = table
            .get_mut(t)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| format!("{key}: {t} is not a table in {}", file.display()))?;
    }
    table.insert(last, Item::Value(value));
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    // Write next to it, then rename: a crash never leaves half a file.
    let tmp: PathBuf = file.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("seraphim-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    #[test]
    fn edits_keep_comments_and_validate() {
        let f = tmp("edit");
        std::fs::write(
            &f,
            "# my config\ntheme = \"celestial\" # favourite\n\n[font]\nsize = 12.0\n",
        )
        .unwrap();
        set(&f, "font.size", Value::from(14.0)).unwrap();
        set(&f, "tabs.bar", Value::from("always")).unwrap();
        set(&f, "extensions.noisy.disabled", Value::from(true)).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("# my config\ntheme = \"celestial\" # favourite\n"), "{text}");
        assert!(text.contains("size = 14.0"), "{text}");
        assert!(text.contains("[tabs]\nbar = \"always\""), "{text}");
        assert!(text.contains("[extensions.noisy]\ndisabled = true"), "{text}");
        // Still a valid Seraphim configuration.
        let c = seraphim_config::Config::parse(&text, &f).unwrap();
        assert_eq!(c.font.size, 14.0);
        assert!(c.extensions["noisy"].disabled);
    }

    #[test]
    fn values_steps_and_defaults() {
        let f = tmp("values");
        std::fs::write(&f, "[visual]\nreduced_motion = true\n").unwrap();
        let st = all(f.parent().unwrap(), &[("glow".into(), "visual".into())]);
        let v = values(&f, &st);
        let get = |k: &str| v[st.iter().position(|s| s.key == k).unwrap()].clone();
        assert_eq!(get("visual.reduced_motion"), "on");
        assert_eq!(get("tabs.bar"), "auto", "from the documented defaults");
        assert_eq!(get("theme"), "built-in");
        assert_eq!(get("extensions.glow.disabled"), "on  (visual)");
        let size = &st[st.iter().position(|s| s.key == "font.size").unwrap()];
        assert_eq!(step(size, "12", 1).as_float(), Some(13.0));
        assert_eq!(step(size, "72", 1).as_float(), Some(72.0), "clamped");
        let bar = &st[st.iter().position(|s| s.key == "tabs.bar").unwrap()];
        assert_eq!(step(bar, "never", 1).as_str(), Some("auto"), "wraps");
        assert_eq!(step(bar, "auto", -1).as_str(), Some("never"));
        let rm = &st[st.iter().position(|s| s.key == "visual.reduced_motion").unwrap()];
        assert_eq!(step(rm, "on", 1).as_bool(), Some(false));
        let ext = st.last().unwrap();
        assert_eq!(
            step(ext, "on  (visual)", 1).as_bool(),
            Some(true),
            "disabled = true turns it off"
        );
        let op = &st[st.iter().position(|s| s.key == "visual.window_opacity").unwrap()];
        assert_eq!(step(op, "1", -1).as_float(), Some(0.95));
    }
}
