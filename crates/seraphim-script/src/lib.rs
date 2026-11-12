//! # seraphim-script
//!
//! The scripting runtime (spec §8, §11; ADR 0005, ADR 0009): Lua 5.4
//! extensions, each in its own sandboxed state, run on one script thread
//! that is never the render or I/O thread.
//!
//! * Extensions **receive** events from the bus (ADR 0003) and a
//!   read-only [`TerminalSnapshot`] the window publishes.
//! * They **act** by sending [`Command`]s that the window applies at the
//!   start of its next frame. A script never touches the renderer or the
//!   terminal directly, so it cannot block or crash either.
//! * Each handler runs under a time budget; an extension that keeps
//!   overrunning it is disabled.
//! * What an extension may do is decided by its [`perm::Grant`].

pub mod audio;
pub mod desktop;
pub mod manifest;
pub mod metrics;
pub mod perm;

mod api;
mod runtime;
mod sandbox;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use manifest::{discover, Extension, Manifest, API_VERSION};
pub use perm::{Grant, Permission, DEFAULT_GRANT, PERMISSIONS};
pub use runtime::{PanelEvent, Runtime};

/// How a change animates: over `duration_ms` with an easing name
/// (`linear`, `ease-in-out`, `spring`, `instant`, …; validated by the host).
#[derive(Clone, Debug, PartialEq)]
pub struct Motion {
    pub duration_ms: u32,
    pub easing: String,
}

impl Default for Motion {
    fn default() -> Self {
        Motion { duration_ms: 0, easing: "ease-in-out".into() }
    }
}

/// What a script asks the host (the window) to do. Every command names the
/// extension that sent it, for error messages.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `layer`: `None` means every layer that has the parameter.
    SetParam {
        ext: String,
        layer: Option<String>,
        param: String,
        value: f32,
        from: Option<f32>,
        motion: Motion,
    },
    SetOpacity {
        ext: String,
        layer: String,
        value: f32,
        from: Option<f32>,
        motion: Motion,
    },
    /// `color`: `#rrggbb[aa]` or a theme reference. `name`: a palette
    /// color, a gradient stop index, or `color` (solid layers); `None`
    /// means the layer's first color. `layer`: `None` means the bottom one.
    SetColor {
        ext: String,
        layer: Option<String>,
        name: Option<String>,
        color: String,
        motion: Motion,
    },
    /// A layer table with the fields of `[[background]]`; replaces the
    /// layer with the same id. Script layers survive profile switches.
    AddLayer {
        ext: String,
        layer: serde_json::Value,
        motion: Motion,
    },
    RemoveLayer {
        ext: String,
        id: String,
        motion: Motion,
    },
    /// Switch profile (or `"default"`).
    SetProfile {
        ext: String,
        name: String,
    },
    /// A reactive binding with the fields of `[[bind]]` (plus `colors`).
    Bind {
        ext: String,
        id: u64,
        spec: serde_json::Value,
    },
    Unbind {
        ext: String,
        id: u64,
    },
    Notify {
        ext: String,
        text: String,
        error: bool,
    },
    /// Text to type into the terminal.
    Send {
        ext: String,
        text: String,
    },
    /// A command shown in the menu bar's Extensions menu.
    RegisterCommand {
        ext: String,
        id: String,
        title: String,
    },
    /// A keyboard shortcut (`ctrl+shift+e`) running a registered command.
    RegisterKey {
        ext: String,
        chord: String,
        command: String,
    },
    /// A built-in action (menu action names: `copy`, `zoom-in`, …).
    Action {
        ext: String,
        name: String,
    },
    /// The extension stopped or restarted: drop its layers, bindings,
    /// commands and shortcuts.
    Reset {
        ext: String,
    },
    /// A panel the extension provides (spec §9.4).
    RegisterPanel {
        ext: String,
        id: String,
        title: String,
    },
    /// A panel's widgets (a list of widget tables), after it rendered.
    PanelContent {
        ext: String,
        id: String,
        widgets: serde_json::Value,
    },
    /// Show a panel: `place` is `right`, `down` or `tab`.
    OpenPanel {
        ext: String,
        id: String,
        place: String,
    },
    /// Open a `[[layout]]` by name.
    OpenLayout {
        ext: String,
        name: String,
    },
}

impl Command {
    pub fn ext(&self) -> &str {
        match self {
            Command::SetParam { ext, .. }
            | Command::SetOpacity { ext, .. }
            | Command::SetColor { ext, .. }
            | Command::AddLayer { ext, .. }
            | Command::RemoveLayer { ext, .. }
            | Command::SetProfile { ext, .. }
            | Command::Bind { ext, .. }
            | Command::Unbind { ext, .. }
            | Command::Notify { ext, .. }
            | Command::Send { ext, .. }
            | Command::RegisterCommand { ext, .. }
            | Command::RegisterKey { ext, .. }
            | Command::Action { ext, .. }
            | Command::Reset { ext }
            | Command::RegisterPanel { ext, .. }
            | Command::PanelContent { ext, .. }
            | Command::OpenPanel { ext, .. }
            | Command::OpenLayout { ext, .. } => ext,
        }
    }
}

/// A process, as extensions with `process.read` see it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub cwd: Option<String>,
}

/// Read-only view of the terminal, refreshed by the window when it
/// changes (only while some extension holds `terminal.read`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalSnapshot {
    pub cols: usize,
    pub rows: usize,
    /// Visible lines, top first, trailing blanks trimmed.
    pub lines: Vec<String>,
    /// 0-based (row, column).
    pub cursor: (usize, usize),
    pub title: String,
    pub alt_screen: bool,
    pub selection: Option<String>,
    /// The session runs as root (from process credentials).
    pub root: bool,
    pub foreground: Option<Process>,
    pub shell: Option<Process>,
}

/// Runtime settings.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Longest a single handler may run.
    pub budget: Duration,
    /// Budget overruns before an extension is disabled.
    pub max_faults: u32,
    /// Memory limit per extension.
    pub memory: usize,
    /// Where extensions keep their data (`<data>/<name>/`).
    pub data_dir: PathBuf,
    /// Restart an extension when its files change.
    pub live_reload: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(std::env::temp_dir)
            .join("seraphim/extensions");
        Settings {
            budget: Duration::from_millis(50),
            max_faults: 3,
            memory: 64 << 20,
            data_dir: data,
            live_reload: false,
        }
    }
}

/// Called by the script thread when it has queued commands, so the window
/// wakes up to apply them.
pub type Waker = Arc<dyn Fn() + Send + Sync>;
