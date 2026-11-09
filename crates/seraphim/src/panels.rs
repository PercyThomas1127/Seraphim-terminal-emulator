//! Built-in panels (spec §9.3) and extension panels (§9.4) as panes:
//! what each shows (widgets, see `panel.rs`), and what keys and clicks in
//! it do. The window supplies a [`Ctx`] with the facts the panels show.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

use seraphim_script::metrics::{Metric, Sampler};

use crate::panel::{Item, Style, Widget};
use crate::settings::Setting;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    System,
    Sessions,
    History,
    Packages,
    Settings,
    /// An extension's panel.
    Script {
        ext: String,
        id: String,
    },
}

impl Kind {
    /// `system`, …, or `extension:id`.
    pub fn parse(name: &str) -> Option<Kind> {
        Some(match name {
            "system" => Kind::System,
            "sessions" => Kind::Sessions,
            "history" => Kind::History,
            "packages" => Kind::Packages,
            "settings" => Kind::Settings,
            other => {
                let (ext, id) = other.split_once(':')?;
                Kind::Script { ext: ext.into(), id: id.into() }
            }
        })
    }

    pub fn name(&self) -> String {
        match self {
            Kind::System => "system".into(),
            Kind::Sessions => "sessions".into(),
            Kind::History => "history".into(),
            Kind::Packages => "packages".into(),
            Kind::Settings => "settings".into(),
            Kind::Script { ext, id } => format!("{ext}:{id}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub command: String,
    pub exit_code: Option<i64>,
    pub duration_ms: u64,
    pub at: SystemTime,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackageEvent {
    pub at: SystemTime,
    /// `install.started`, …
    pub what: String,
    pub package: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub pane: u64,
    pub tab: usize,
    pub label: String,
    pub program: String,
    pub cwd: String,
    pub root: bool,
    pub focused: bool,
}

/// What the window knows that panels show.
pub struct Ctx<'a> {
    pub sessions: &'a [SessionRow],
    pub history: &'a VecDeque<HistoryEntry>,
    pub packages: &'a VecDeque<PackageEvent>,
    pub settings: &'a [(Setting, String)],
    /// An extension panel's title and latest widgets.
    pub script: Option<(&'a str, &'a [Widget])>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Home,
    End,
    PageUp,
    PageDown,
    Escape,
    Char(char),
}

/// What a key or click asks the window to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    None,
    /// Repaint (the selection moved).
    Redraw,
    FocusPane(u64),
    /// Type into the terminal (not run: no newline).
    Type(String),
    /// Change setting `i` by one step in a direction.
    Setting(usize, i32),
    /// Tell the extension (`{"select": n}` or `{"key": "..."}`, 1-based).
    Script(serde_json::Value),
    Close,
}

/// A panel pane.
pub struct PanelState {
    pub kind: Kind,
    pub selected: Option<usize>,
    /// List items in the last paint (for moving the selection).
    pub items: usize,
    /// Rows holding items in the last paint (for clicks).
    pub rows: Vec<(usize, usize)>,
    cpu: VecDeque<f32>,
    mem: VecDeque<f32>,
    sampler: Sampler,
    next_sample: Instant,
}

const SPARK: usize = 120;

fn ago(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).unwrap_or_default().as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, ms / 1000 % 60)
    }
}

impl PanelState {
    pub fn new(kind: Kind) -> Self {
        let selected = matches!(kind, Kind::Sessions | Kind::History | Kind::Settings).then_some(0);
        PanelState {
            kind,
            selected,
            items: 0,
            rows: Vec::new(),
            cpu: VecDeque::new(),
            mem: VecDeque::new(),
            sampler: Sampler::default(),
            next_sample: Instant::now(),
        }
    }

    pub fn title(&self, ctx: &Ctx) -> String {
        match &self.kind {
            Kind::System => "System Monitor".into(),
            Kind::Sessions => "Sessions".into(),
            Kind::History => "Command History".into(),
            Kind::Packages => "Package Activity".into(),
            Kind::Settings => "Settings".into(),
            Kind::Script { id, .. } => ctx.script.map_or(id.clone(), |(t, _)| t.to_owned()),
        }
    }

    /// Sample metrics when due. Returns whether it sampled (repaint) and
    /// when to call again; `None` for panels that don't change by
    /// themselves.
    pub fn tick(&mut self, now: Instant) -> Option<(bool, Instant)> {
        if self.kind != Kind::System {
            return None;
        }
        let due = now >= self.next_sample;
        if due {
            self.next_sample = now + Duration::from_secs(1);
            for (m, buf) in [(Metric::Cpu, &mut self.cpu), (Metric::Memory, &mut self.mem)] {
                if let Some(v) = self.sampler.read(m, now) {
                    buf.push_back(v as f32);
                    if buf.len() > SPARK {
                        buf.pop_front();
                    }
                }
            }
        }
        Some((due, self.next_sample))
    }

    pub fn widgets(&mut self, ctx: &Ctx) -> Vec<Widget> {
        match &self.kind {
            Kind::System => {
                let now = Instant::now();
                let load = self.sampler.read(Metric::Load, now);
                let cpu = self.cpu.back().copied();
                let mem = self.mem.back().copied();
                let mut w = vec![
                    Widget::Gauge {
                        label: "CPU".into(),
                        value: cpu.unwrap_or(0.0) / 100.0,
                        text: Some(cpu.map_or("…".into(), |c| format!("{c:.0}%"))),
                        style: Style::Normal,
                    },
                    Widget::Sparkline {
                        label: "".into(),
                        values: self.cpu.iter().copied().collect(),
                        max: Some(100.0),
                    },
                    Widget::Gauge {
                        label: "Memory".into(),
                        value: mem.unwrap_or(0.0) / 100.0,
                        text: Some(mem.map_or("…".into(), |m| format!("{m:.0}%"))),
                        style: Style::Normal,
                    },
                    Widget::Sparkline {
                        label: "".into(),
                        values: self.mem.iter().copied().collect(),
                        max: Some(100.0),
                    },
                    Widget::Separator,
                ];
                let mut kv = Vec::new();
                if let Some(l) = load {
                    kv.push(("Load per core".to_owned(), format!("{l:.2}")));
                }
                if let Some(b) = self.sampler.read(Metric::BatteryLevel, now) {
                    let charging = self.sampler.read(Metric::BatteryCharging, now) == Some(1.0);
                    kv.push((
                        "Battery".into(),
                        format!("{b:.0}%{}", if charging { " (charging)" } else { "" }),
                    ));
                }
                if let Some(up) = std::fs::read_to_string("/proc/uptime")
                    .ok()
                    .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
                {
                    let up = up as u64;
                    kv.push((
                        "Uptime".into(),
                        format!("{}d {}h {}m", up / 86400, up / 3600 % 24, up / 60 % 60),
                    ));
                }
                kv.push(("Sessions".into(), ctx.sessions.len().to_string()));
                w.push(Widget::Keyvalue { items: kv });
                w
            }
            Kind::Sessions => {
                let mut w = Vec::new();
                let mut tab = usize::MAX;
                let mut items = Vec::new();
                for s in ctx.sessions {
                    if s.tab != tab {
                        tab = s.tab;
                    }
                    let text = format!("{} {}  {}", s.tab + 1, s.label, s.program);
                    let style = if s.root {
                        Style::Alert
                    } else if s.focused {
                        Style::Accent
                    } else {
                        Style::Normal
                    };
                    items.push(Item { text, detail: s.cwd.clone(), style });
                }
                if items.is_empty() {
                    w.push(Widget::Text { text: "No sessions.".into(), style: Style::Dim });
                } else {
                    w.push(Widget::List { items });
                    w.push(Widget::Text {
                        text: "Enter: go to the session".into(),
                        style: Style::Dim,
                    });
                }
                w
            }
            Kind::History => {
                if ctx.history.is_empty() {
                    return vec![Widget::Text {
                        text: "Commands appear here as they finish (needs shell integration)."
                            .into(),
                        style: Style::Dim,
                    }];
                }
                let items = ctx
                    .history
                    .iter()
                    .rev()
                    .map(|h| {
                        let style = match h.exit_code {
                            Some(0) | None => Style::Normal,
                            _ => Style::Alert,
                        };
                        let code = h.exit_code.map_or("".into(), |c| {
                            if c == 0 {
                                "".into()
                            } else {
                                format!("[{c}] ")
                            }
                        });
                        Item {
                            text: h.command.clone(),
                            detail: format!("{code}{} · {}", duration(h.duration_ms), ago(h.at)),
                            style,
                        }
                    })
                    .collect();
                vec![
                    Widget::List { items },
                    Widget::Text {
                        text: "Enter: type the command into the terminal".into(),
                        style: Style::Dim,
                    },
                ]
            }
            Kind::Packages => {
                if ctx.packages.is_empty() {
                    return vec![Widget::Text {
                        text: "No package activity yet (gabriel.* events).".into(),
                        style: Style::Dim,
                    }];
                }
                let items = ctx
                    .packages
                    .iter()
                    .rev()
                    .map(|p| {
                        let style = if p.what.ends_with("failed") {
                            Style::Alert
                        } else if p.what.ends_with("completed") {
                            Style::Good
                        } else {
                            Style::Accent
                        };
                        Item {
                            text: format!("{} {}", p.package, p.what),
                            detail: format!("{} {}", p.detail, ago(p.at)),
                            style,
                        }
                    })
                    .collect();
                vec![Widget::List { items }]
            }
            Kind::Settings => {
                let items = ctx
                    .settings
                    .iter()
                    .map(|(s, v)| Item {
                        text: s.label.clone(),
                        detail: v.clone(),
                        style: Style::Normal,
                    })
                    .collect();
                vec![
                    Widget::List { items },
                    Widget::Separator,
                    Widget::Text {
                        text: "←/→ change, Enter next value. Saved to the configuration file and applied at once.".into(),
                        style: Style::Dim,
                    },
                ]
            }
            Kind::Script { .. } => ctx.script.map(|(_, w)| w.to_vec()).unwrap_or_else(|| {
                vec![Widget::Text {
                    text: "Waiting for the extension…".into(), style: Style::Dim
                }]
            }),
        }
    }

    fn select_move(&mut self, delta: i64) -> Act {
        if self.items == 0 {
            return Act::None;
        }
        let last = self.items as i64 - 1;
        self.selected = Some(match self.selected {
            Some(cur) => (cur as i64 + delta).clamp(0, last) as usize,
            // Nothing selected yet: Down starts at the top, Up at the end.
            None if delta > 0 => 0,
            None => last as usize,
        });
        Act::Redraw
    }

    pub fn key(&mut self, key: Key, ctx: &Ctx) -> Act {
        match key {
            Key::Up => return self.select_move(-1),
            Key::Down => return self.select_move(1),
            Key::PageUp => return self.select_move(-10),
            Key::PageDown => return self.select_move(10),
            Key::Home => return self.select_move(i64::MIN / 2),
            Key::End => return self.select_move(i64::MAX / 2),
            Key::Escape => return Act::Close,
            _ => {}
        }
        let sel = self.selected;
        match (&self.kind, key, sel) {
            (Kind::Sessions, Key::Enter, Some(i)) => {
                ctx.sessions.get(i).map_or(Act::None, |s| Act::FocusPane(s.pane))
            }
            (Kind::History, Key::Enter, Some(i)) => {
                let n = ctx.history.len();
                ctx.history
                    .get(n.wrapping_sub(i + 1))
                    .map_or(Act::None, |h| Act::Type(h.command.clone()))
            }
            (Kind::Settings, Key::Enter | Key::Right | Key::Char('+' | ' '), Some(i)) => {
                Act::Setting(i, 1)
            }
            (Kind::Settings, Key::Left | Key::Char('-'), Some(i)) => Act::Setting(i, -1),
            (Kind::Script { .. }, Key::Enter, Some(i)) => {
                Act::Script(serde_json::json!({ "select": i + 1 }))
            }
            (Kind::Script { .. }, k, _) => {
                let name = match k {
                    Key::Enter => "enter".to_owned(),
                    Key::Left => "left".into(),
                    Key::Right => "right".into(),
                    Key::Char(c) => c.to_string(),
                    _ => return Act::None,
                };
                Act::Script(serde_json::json!({ "key": name, "selected": sel.map(|s| s + 1) }))
            }
            _ => Act::None,
        }
    }

    /// A click on grid row `row`: select that item and activate it.
    pub fn click(&mut self, row: usize, ctx: &Ctx) -> Act {
        let Some(&(_, i)) = self.rows.iter().find(|(r, _)| *r == row) else { return Act::None };
        self.selected = Some(i);
        match self.kind {
            Kind::Settings => Act::Setting(i, 1),
            _ => self.key(Key::Enter, ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        sessions: &'a [SessionRow],
        history: &'a VecDeque<HistoryEntry>,
        packages: &'a VecDeque<PackageEvent>,
    ) -> Ctx<'a> {
        Ctx { sessions, history, packages, settings: &[], script: None }
    }

    #[test]
    fn names() {
        for n in ["system", "sessions", "history", "packages", "settings", "glow:cpu"] {
            assert_eq!(Kind::parse(n).unwrap().name(), n);
        }
        assert_eq!(Kind::parse("clock"), None);
    }

    #[test]
    fn history_types_and_sessions_focus() {
        let mut h = VecDeque::new();
        for (c, code) in [("make", 0), ("cargo test", 101)] {
            h.push_back(HistoryEntry {
                command: c.into(),
                exit_code: Some(code),
                duration_ms: 1500,
                at: SystemTime::now(),
            });
        }
        let rows = vec![SessionRow {
            pane: 7,
            tab: 0,
            label: "bash".into(),
            program: "bash".into(),
            cwd: "/tmp".into(),
            root: false,
            focused: true,
        }];
        let pk = VecDeque::new();
        let c = ctx(&rows, &h, &pk);
        let mut p = PanelState::new(Kind::History);
        let w = p.widgets(&c);
        match &w[0] {
            Widget::List { items } => {
                assert_eq!(items[0].text, "cargo test", "newest first");
                assert_eq!(items[0].style, Style::Alert);
                assert!(items[0].detail.starts_with("[101] 1.5s"));
            }
            other => panic!("{other:?}"),
        }
        p.items = 2;
        assert_eq!(p.key(Key::Enter, &c), Act::Type("cargo test".into()));
        assert_eq!(p.key(Key::Down, &c), Act::Redraw);
        assert_eq!(p.key(Key::Enter, &c), Act::Type("make".into()));
        assert_eq!(p.key(Key::Down, &c), Act::Redraw, "stays on the last");
        assert_eq!(p.selected, Some(1));
        let mut s = PanelState::new(Kind::Sessions);
        s.items = 1;
        assert_eq!(s.key(Key::Enter, &c), Act::FocusPane(7));
        s.rows = vec![(1, 0)];
        assert_eq!(s.click(1, &c), Act::FocusPane(7));
        assert_eq!(s.click(5, &c), Act::None);
        assert_eq!(s.key(Key::Escape, &c), Act::Close);
    }

    #[test]
    fn system_samples_and_script_keys() {
        let (r, h, pk) = (vec![], VecDeque::new(), VecDeque::new());
        let c = ctx(&r, &h, &pk);
        let mut p = PanelState::new(Kind::System);
        let t = Instant::now();
        assert_eq!(p.tick(t).map(|(sampled, _)| sampled), Some(true));
        assert_eq!(
            p.tick(t).map(|(sampled, _)| sampled),
            Some(false),
            "not again within the second"
        );
        let w = p.widgets(&c);
        assert!(matches!(&w[0], Widget::Gauge { label, .. } if label == "CPU"));
        let mut s = PanelState::new(Kind::Script { ext: "e".into(), id: "x".into() });
        assert_eq!(
            s.key(Key::Char('r'), &c),
            Act::Script(serde_json::json!({ "key": "r", "selected": null }))
        );
        s.items = 3;
        s.key(Key::Down, &c);
        assert_eq!(s.key(Key::Enter, &c), Act::Script(serde_json::json!({ "select": 1 })));
    }
}
