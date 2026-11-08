//! Extension permissions (spec §8.5).
//!
//! An extension *requests* permissions in its manifest; the user *grants*
//! them in `seraphim.toml`. The extension gets the intersection. API
//! tables for permissions it does not hold are simply absent from its Lua
//! state. Nothing here depends on the terminal session's privileges: a
//! root shell never widens what an extension may do (spec §11.5).

use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Permission {
    /// Change layers, parameters, colors; play animations; bind properties.
    Visual,
    /// Notices, commands, keyboard shortcuts, built-in actions.
    Interface,
    /// CPU, memory, load, battery, time of day.
    Metrics,
    /// Read the screen, cursor, title, working directory, last command.
    TerminalRead,
    /// Type into the terminal (never into a root session).
    TerminalWrite,
    /// The foreground process's pid, name and directory.
    ProcessRead,
    /// Run the allowed commands (`commands`).
    ProcessExec,
    /// Read and write under the approved `paths`.
    Filesystem,
    /// HTTP requests to the approved `hosts`.
    Network,
    /// Desktop integration (desktop notifications).
    System,
}

/// (permission, name in manifests and config, description).
pub const PERMISSIONS: &[(Permission, &str, &str)] = &[
    (Permission::Visual, "visual", "modify visual effects"),
    (Permission::Interface, "interface", "show notices, add commands and shortcuts"),
    (Permission::Metrics, "metrics", "read CPU, memory, load and battery levels"),
    (Permission::TerminalRead, "terminal.read", "read the terminal's contents"),
    (Permission::TerminalWrite, "terminal.write", "type into the terminal"),
    (Permission::ProcessRead, "process.read", "see the foreground process"),
    (Permission::ProcessExec, "process.exec", "run approved programs"),
    (Permission::Filesystem, "filesystem", "read and write approved paths"),
    (Permission::Network, "network", "make requests to approved hosts"),
    (Permission::System, "system", "use desktop integration (notifications)"),
];

/// Granted to an extension unless its `[extensions.<name>]` entry says
/// otherwise: they change only what Seraphim shows.
pub const DEFAULT_GRANT: &[Permission] =
    &[Permission::Visual, Permission::Interface, Permission::Metrics];

impl Permission {
    pub fn parse(s: &str) -> Option<Permission> {
        PERMISSIONS.iter().find(|(_, n, _)| *n == s).map(|(p, _, _)| *p)
    }

    pub fn name(self) -> &'static str {
        PERMISSIONS.iter().find(|(p, _, _)| *p == self).map_or("?", |(_, n, _)| n)
    }

    pub fn describe(self) -> &'static str {
        PERMISSIONS.iter().find(|(p, _, _)| *p == self).map_or("?", |(_, _, d)| d)
    }
}

/// What the user allows one extension.
#[derive(Clone, Debug, PartialEq)]
pub struct Grant {
    pub permissions: BTreeSet<Permission>,
    /// `filesystem`: folders (and everything under them).
    pub paths: Vec<PathBuf>,
    /// `network`: host names; `*.example.com` matches subdomains.
    pub hosts: Vec<String>,
    /// `process.exec`: program names or paths; `"*"` allows any.
    pub commands: Vec<String>,
    /// Free-form settings, readable as `seraphim.settings`.
    pub settings: serde_json::Value,
}

impl Default for Grant {
    fn default() -> Self {
        Grant {
            permissions: DEFAULT_GRANT.iter().copied().collect(),
            paths: Vec::new(),
            hosts: Vec::new(),
            commands: Vec::new(),
            settings: serde_json::Value::Object(Default::default()),
        }
    }
}

impl Grant {
    /// Requested ∩ granted.
    pub fn effective(&self, requested: &BTreeSet<Permission>) -> Grant {
        Grant {
            permissions: self.permissions.intersection(requested).copied().collect(),
            ..self.clone()
        }
    }

    pub fn has(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    pub fn host_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.hosts.iter().any(|h| {
            let h = h.to_ascii_lowercase();
            match h.strip_prefix("*.") {
                Some(base) => host.ends_with(&format!(".{base}")),
                None => h == host,
            }
        })
    }

    /// `program` as given to `process.run` (a name or a path).
    pub fn command_allowed(&self, program: &str) -> bool {
        let base =
            std::path::Path::new(program).file_name().and_then(|s| s.to_str()).unwrap_or(program);
        self.commands.iter().any(|c| c == "*" || c == program || (!c.contains('/') && c == base))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for (p, n, _) in PERMISSIONS {
            assert_eq!(Permission::parse(n), Some(*p));
            assert_eq!(p.name(), *n);
        }
        assert_eq!(Permission::parse("root"), None);
    }

    #[test]
    fn effective_is_the_intersection() {
        let g = Grant {
            permissions: [Permission::Visual, Permission::Network].into(),
            ..Default::default()
        };
        let asked = [Permission::Visual, Permission::TerminalRead].into();
        assert_eq!(g.effective(&asked).permissions, [Permission::Visual].into());
    }

    #[test]
    fn hosts_and_commands() {
        let g = Grant {
            hosts: vec!["api.example.com".into(), "*.cdn.net".into()],
            commands: vec!["git".into(), "/usr/bin/uptime".into()],
            ..Default::default()
        };
        assert!(g.host_allowed("API.example.com"));
        assert!(!g.host_allowed("evil.example.com"));
        assert!(g.host_allowed("a.cdn.net"));
        assert!(!g.host_allowed("cdn.net"), "*.x matches subdomains only");
        assert!(!g.host_allowed("xcdn.net"));
        assert!(g.command_allowed("git"));
        assert!(g.command_allowed("/usr/bin/git"), "a bare name allows it anywhere");
        assert!(g.command_allowed("/usr/bin/uptime"));
        assert!(!g.command_allowed("uptime"), "a path allows only that path");
        assert!(!g.command_allowed("sh"));
    }
}
