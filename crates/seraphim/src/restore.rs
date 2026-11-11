//! Session restoration (spec §9.1): what Seraphim saves while its windows
//! are open (every half minute, so it survives a crash) and when the last
//! one closes: each window's tabs, splits, directories, labels, profiles,
//! panels, and the last screenful of each terminal. And reading it back. Programs are not
//! restarted; each terminal pane gets a new shell in its old directory,
//! with its old text shown dimmed above the new prompt.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::workspace::Node;

/// Lines of text kept per terminal.
pub const LINES: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedPane {
    Terminal {
        cwd: Option<String>,
        #[serde(default)]
        text: Vec<String>,
    },
    Panel {
        panel: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub root: Node,
    pub focus: u64,
    pub label: Option<String>,
    pub profile: Option<String>,
    /// By the pane ids used in `root`.
    pub panes: BTreeMap<u64, SavedPane>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub version: u32,
    pub active: usize,
    pub tabs: Vec<SavedTab>,
    /// Floating panels, bottom first.
    #[serde(default)]
    pub floats: Vec<SavedFloat>,
}

/// A floating panel: which, and where, as fractions of the pane area.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedFloat {
    pub panel: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Every window (version 2; version 1 files hold one [`Saved`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedSession {
    pub version: u32,
    pub windows: Vec<Saved>,
}

/// `$XDG_STATE_HOME/seraphim/session.json` (or `~/.local/state/...`).
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("seraphim/session.json"))
}

/// Save these windows. Windows without tabs are left out; with none
/// left, the next start opens fresh.
pub fn save(windows: &[Saved]) -> Result<(), String> {
    let s = SavedSession {
        version: 2,
        windows: windows.iter().filter(|w| !w.tabs.is_empty()).cloned().collect(),
    };
    let p = path().ok_or("no home directory")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string(&s).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    // Screen text can be private: only the user reads it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

/// The saved windows (none: start fresh).
pub fn load() -> Option<Vec<Saved>> {
    let text = std::fs::read_to_string(path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let windows = match v.get("version")?.as_u64()? {
        1 => vec![serde_json::from_value::<Saved>(v).ok()?],
        2 => serde_json::from_value::<SavedSession>(v).ok()?.windows,
        _ => return None,
    };
    let windows: Vec<Saved> = windows.into_iter().filter(|w| !w.tabs.is_empty()).collect();
    (!windows.is_empty()).then_some(windows)
}

/// Old text as terminal output: dimmed, then a marker line.
pub fn replay(text: &[String], when: &str) -> Vec<u8> {
    let mut out = String::from("\x1b[2m");
    for l in text {
        // Control characters could be escape sequences; drop them.
        let clean: String = l.chars().filter(|c| !c.is_control()).collect();
        out.push_str(&clean);
        out.push_str("\r\n");
    }
    out.push_str(&format!("── restored {when} ──\x1b[0m\r\n"));
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Split;

    #[test]
    fn round_trip_and_replay() {
        let dir = std::env::temp_dir().join(format!("seraphim-restore-{}", std::process::id()));
        std::env::set_var("XDG_STATE_HOME", &dir);
        let mut panes = BTreeMap::new();
        panes
            .insert(1, SavedPane::Terminal { cwd: Some("/tmp".into()), text: vec!["$ ls".into()] });
        panes.insert(2, SavedPane::Panel { panel: "system".into() });
        let s = Saved {
            version: 1,
            active: 0,
            tabs: vec![SavedTab {
                root: Node::Split {
                    split: Split::Right,
                    ratio: 0.6,
                    first: Box::new(Node::Pane(1)),
                    second: Box::new(Node::Pane(2)),
                },
                focus: 1,
                label: Some("work".into()),
                profile: None,
                panes,
            }],
            floats: vec![SavedFloat { panel: "history".into(), x: 0.1, y: 0.2, w: 0.5, h: 0.5 }],
        };
        save(&[s.clone(), Saved { tabs: vec![], ..s.clone() }]).unwrap();
        // Windows without tabs are dropped.
        assert_eq!(load(), Some(vec![s.clone()]));
        // Version 1: one window.
        std::fs::write(path().unwrap(), serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(load(), Some(vec![s.clone()]));
        save(&[Saved { tabs: vec![], ..s.clone() }]).unwrap();
        assert_eq!(load(), None);
        let r = String::from_utf8(replay(&["a\x1b]52;c;evil\x07b".into()], "today")).unwrap();
        assert!(r.contains("a]52;c;evilb"), "escapes dropped: {r:?}");
        assert!(r.ends_with("── restored today ──\x1b[0m\r\n"));
        std::fs::write(path().unwrap(), "{\"version\": 9, \"active\": 0, \"tabs\": []}").unwrap();
        assert_eq!(load(), None);
    }
}
