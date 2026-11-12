//! Finding extensions on disk.
//!
//! * `extensions/<name>/extension.toml` plus its main file (`init.lua`).
//! * `scripts/<name>.lua`: a one-file extension; it requests the default
//!   permissions only.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::perm::{Permission, DEFAULT_GRANT};

/// Scripting API version this build provides.
pub const API_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub authors: Vec<String>,
    /// Scripting API version the extension was written for.
    pub api: u32,
    pub permissions: Vec<String>,
    /// Entry point, relative to the extension folder.
    pub main: String,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            name: String::new(),
            version: "0.0.0".into(),
            description: String::new(),
            authors: Vec::new(),
            api: API_VERSION,
            permissions: Vec::new(),
            main: "init.lua".into(),
        }
    }
}

/// An extension found on disk, not yet running.
#[derive(Clone, Debug, PartialEq)]
pub struct Extension {
    pub name: String,
    /// Its folder (for a one-file script: the folder holding it).
    pub dir: PathBuf,
    pub main: PathBuf,
    pub manifest: Manifest,
    pub requested: BTreeSet<Permission>,
    /// A one-file script (no manifest).
    pub script: bool,
}

/// Event namespaces only Seraphim itself publishes into; an extension
/// cannot take one as its name (its events are `<name>.*`).
pub const RESERVED: &[&str] = &[
    "terminal",
    "shell",
    "command",
    "session",
    "interface",
    "animation",
    "visual",
    "system",
    "audio",
    "network",
    "package",
    "notification",
    "clipboard",
    "extension",
    "seraphim",
];

pub fn valid_name(name: &str) -> bool {
    !RESERVED.contains(&name)
        && !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl Extension {
    /// An extension folder.
    pub fn from_dir(dir: &Path) -> Result<Extension, String> {
        let file = dir.join("extension.toml");
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let mut manifest: Manifest =
            toml::from_str(&text).map_err(|e| format!("{}: {}", file.display(), e.message()))?;
        if manifest.name.is_empty() {
            manifest.name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        }
        if !valid_name(&manifest.name) {
            return Err(format!(
                "{}: name {:?} must be lowercase letters, digits, - or _, and not a built-in event namespace",
                file.display(),
                manifest.name
            ));
        }
        if manifest.api > API_VERSION || manifest.api == 0 {
            return Err(format!(
                "{}: written for scripting API {}, this Seraphim provides {API_VERSION}",
                manifest.name, manifest.api
            ));
        }
        let mut requested = BTreeSet::new();
        for p in &manifest.permissions {
            let perm = Permission::parse(p).ok_or_else(|| {
                format!(
                    "{}: unknown permission {p:?} (known: {})",
                    file.display(),
                    crate::perm::PERMISSIONS
                        .iter()
                        .map(|(_, n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            requested.insert(perm);
        }
        let main = dir.join(&manifest.main);
        if Path::new(&manifest.main)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(format!("{}: main must be a path inside the extension", file.display()));
        }
        if !main.is_file() {
            return Err(format!("{}: {} not found", manifest.name, main.display()));
        }
        Ok(Extension {
            name: manifest.name.clone(),
            dir: dir.to_owned(),
            main,
            manifest,
            requested,
            script: false,
        })
    }

    /// A one-file script.
    pub fn from_script(file: &Path) -> Result<Extension, String> {
        let name = file.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        if !valid_name(&name) {
            return Err(format!(
                "{}: script names must be lowercase letters, digits, - or _, and not a built-in event namespace",
                file.display()
            ));
        }
        let manifest = Manifest { name: name.clone(), ..Default::default() };
        Ok(Extension {
            name,
            dir: file.parent().unwrap_or(Path::new(".")).to_owned(),
            main: file.to_owned(),
            manifest,
            requested: DEFAULT_GRANT.iter().copied().collect(),
            script: true,
        })
    }

    /// A folder with a manifest, or a `.lua` file.
    pub fn from_path(path: &Path) -> Result<Extension, String> {
        if path.is_dir() {
            Extension::from_dir(path)
        } else {
            Extension::from_script(path)
        }
    }
}

/// Every extension under `<config>/extensions/` and `<config>/scripts/`,
/// then `extra` (command line). Problems are returned, not fatal; a
/// later extension with a name already taken is skipped.
pub fn discover(config_dir: &Path, extra: &[PathBuf]) -> (Vec<Extension>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    for (sub, want_dir) in [("extensions", true), ("scripts", false)] {
        let Ok(rd) = std::fs::read_dir(config_dir.join(sub)) else { continue };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let hidden =
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
                !hidden
                    && if want_dir { p.is_dir() } else { p.extension().is_some_and(|e| e == "lua") }
            })
            .collect();
        paths.sort();
        candidates.extend(paths);
    }
    candidates.extend(extra.iter().cloned());
    for p in candidates {
        match Extension::from_path(&p) {
            Ok(e) if found.iter().any(|f: &Extension| f.name == e.name) => errors.push(format!(
                "{}: another extension is already named {:?}",
                p.display(),
                e.name
            )),
            Ok(e) => found.push(e),
            Err(e) => errors.push(e),
        }
    }
    (found, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("seraphim-manifest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn discovers_folders_and_scripts() {
        let c = tmp("discover");
        let e = c.join("extensions/glow");
        std::fs::create_dir_all(&e).unwrap();
        std::fs::write(
            e.join("extension.toml"),
            "name = \"glow\"\nversion = \"1.0\"\npermissions = [\"visual\", \"terminal.read\"]\n",
        )
        .unwrap();
        std::fs::write(e.join("init.lua"), "").unwrap();
        std::fs::create_dir_all(c.join("scripts")).unwrap();
        std::fs::write(c.join("scripts/clock.lua"), "").unwrap();
        std::fs::write(c.join("scripts/notes.txt"), "").unwrap();
        let bad = c.join("extensions/bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("extension.toml"), "permissions = [\"root\"]\n").unwrap();
        let (found, errors) = discover(&c, &[]);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["glow", "clock"]);
        assert_eq!(found[0].requested, [Permission::Visual, Permission::TerminalRead].into());
        assert_eq!(found[1].requested, DEFAULT_GRANT.iter().copied().collect());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("unknown permission \"root\""), "{errors:?}");
    }

    #[test]
    fn rejects_newer_api_and_escaping_main() {
        let c = tmp("api");
        std::fs::write(c.join("extension.toml"), "name = \"x\"\napi = 99\n").unwrap();
        assert!(Extension::from_dir(&c).unwrap_err().contains("API 99"));
        std::fs::write(c.join("extension.toml"), "name = \"x\"\nmain = \"../evil.lua\"\n").unwrap();
        assert!(Extension::from_dir(&c).unwrap_err().contains("inside the extension"));
        std::fs::write(c.join("extension.toml"), "name = \"Bad Name\"\n").unwrap();
        assert!(Extension::from_dir(&c).unwrap_err().contains("lowercase"));
        std::fs::write(c.join("extension.toml"), "name = \"package\"\n").unwrap();
        assert!(
            Extension::from_dir(&c).unwrap_err().contains("namespace"),
            "cannot forge package.*"
        );
    }
}
