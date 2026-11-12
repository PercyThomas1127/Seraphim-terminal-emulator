//! The tab bar: one row of cells painted as ANSI text (like the menu bar),
//! drawn by the overlay renderer. Pure: tabs in, bytes and click targets
//! out.

use unicode_width::UnicodeWidthStr;

use crate::menu::Palette;

/// A progress report from the application (OSC 9;4, as Windows Terminal
/// and ConEmu define it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Normal(u8),
    Error(u8),
    Indeterminate,
    Paused(u8),
}

impl Progress {
    /// `OSC 9 ; 4 ; state ; percent`. `None` clears it.
    pub fn parse(state: u8, percent: u8) -> Option<Progress> {
        let p = percent.min(100);
        match state {
            1 => Some(Progress::Normal(p)),
            2 => Some(Progress::Error(p)),
            3 => Some(Progress::Indeterminate),
            4 => Some(Progress::Paused(p)),
            _ => None,
        }
    }

    fn fraction(self) -> f32 {
        match self {
            Progress::Normal(p) | Progress::Error(p) | Progress::Paused(p) => p as f32 / 100.0,
            Progress::Indeterminate => 1.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabInfo {
    pub label: String,
    pub icon: Option<char>,
    pub active: bool,
    /// Output while in the background.
    pub activity: bool,
    pub bell: bool,
    /// Some pane runs as root.
    pub root: bool,
    pub progress: Option<Progress>,
    pub panes: usize,
    pub zoomed: bool,
}

/// Columns of each tab (`start..end`, with its close button at `close`)
/// and the new-tab button.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hits {
    pub tabs: Vec<(usize, usize, usize)>,
    pub new_tab: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    Close(usize),
    NewTab,
    Bar,
}

impl Hits {
    pub fn at(&self, col: usize) -> Hit {
        for (i, (s, e, c)) in self.tabs.iter().enumerate() {
            if col == *c {
                return Hit::Close(i);
            }
            if (*s..*e).contains(&col) {
                return Hit::Tab(i);
            }
        }
        if self.new_tab == Some(col) || self.new_tab.is_some_and(|n| col == n + 1) {
            return Hit::NewTab;
        }
        Hit::Bar
    }
}

/// An icon for the program running in the foreground (Nerd Font glyphs).
pub fn icon_for(program: &str) -> char {
    match program {
        "vim" | "nvim" | "vi" => '\u{e62b}',
        "git" | "lazygit" | "tig" => '\u{e702}',
        "python" | "python3" | "ipython" => '\u{e73c}',
        "node" | "npm" | "yarn" | "deno" | "bun" => '\u{e718}',
        "cargo" | "rustc" => '\u{e7a8}',
        "ssh" | "mosh" => '\u{f0c2}',
        "htop" | "top" | "btop" => '\u{f080}',
        "docker" | "podman" => '\u{f308}',
        "man" | "less" | "more" => '\u{f02d}',
        "make" | "cmake" | "ninja" => '\u{f0ad}',
        "emerge" | "pacman" | "paru" | "yay" | "pikaur" | "apt" | "apt-get" | "aptitude"
        | "nala" | "dnf" | "dnf5" | "yum" | "zypper" | "xbps-install" | "xbps-remove" | "apk"
        | "eopkg" | "nix-env" | "flatpak" | "snap" | "brew" => '\u{f187}',
        _ => '\u{f489}', // terminal
    }
}

/// How tabs are drawn (`[tabs]`): a style, sizes, buttons and colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Flat tabs with a thin gap.
    Classic,
    /// Arrow-shaped ends (Nerd Font / Powerline glyphs).
    Powerline,
    /// Round ends.
    Rounded,
    /// Slanted ends.
    Slanted,
    /// No backgrounds: the active tab is underlined in the accent color.
    Minimal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Design {
    pub style: Style,
    pub min_width: usize,
    pub max_width: usize,
    pub close_button: bool,
    pub new_tab_button: bool,
    pub bar: [u8; 3],
    pub active_fg: [u8; 3],
    pub active_bg: [u8; 3],
    pub inactive_fg: [u8; 3],
    pub inactive_bg: [u8; 3],
    pub accent: [u8; 3],
    pub dim: [u8; 3],
    pub border: [u8; 3],
}

fn mix(a: [u8; 3], b: [u8; 3], k: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * k).round() as u8)
}

impl Design {
    /// The configured design, with colors from the theme's palette where
    /// none are set.
    pub fn new(c: &seraphim_config::Tabs, p: &Palette) -> Design {
        let style = match c.style.as_str() {
            "powerline" => Style::Powerline,
            "rounded" => Style::Rounded,
            "slanted" => Style::Slanted,
            "minimal" => Style::Minimal,
            _ => Style::Classic,
        };
        let bar = c.colors.bar.unwrap_or(p.bar_bg);
        // Shaped tabs need a background of their own to show their shape.
        let inactive_bg = match style {
            Style::Classic | Style::Minimal => bar,
            _ => mix(bar, p.bar_fg, 0.14),
        };
        let accent = c.colors.accent.unwrap_or(p.accent);
        Design {
            style,
            min_width: c.min_width as usize,
            max_width: c.max_width.max(c.min_width) as usize,
            close_button: c.close_button,
            new_tab_button: c.new_tab_button,
            bar,
            active_fg: c.colors.active_fg.unwrap_or(match style {
                Style::Minimal => accent,
                _ => p.hi_fg,
            }),
            active_bg: c.colors.active_bg.unwrap_or(match style {
                Style::Minimal => bar,
                _ => p.hi_bg,
            }),
            inactive_fg: c.colors.inactive_fg.unwrap_or(match style {
                Style::Minimal => p.dim,
                _ => p.bar_fg,
            }),
            inactive_bg: c.colors.inactive_bg.unwrap_or(inactive_bg),
            accent,
            dim: p.dim,
            border: p.border,
        }
    }

    /// Columns before and after each tab's text.
    fn caps(&self) -> (usize, usize) {
        match self.style {
            Style::Rounded => (1, 1),
            _ => (0, 1),
        }
    }
}

impl Default for Design {
    fn default() -> Self {
        let p = Palette::from_theme(&seraphim_render::Theme::default());
        Design::new(&seraphim_config::Tabs::default(), &p)
    }
}

fn sgr(out: &mut String, fg: [u8; 3], bg: [u8; 3], bold: bool) {
    let b = if bold { "1;" } else { "" };
    out.push_str(&format!(
        "\x1b[0;{b}38;2;{};{};{};48;2;{};{};{}m",
        fg[0], fg[1], fg[2], bg[0], bg[1], bg[2]
    ));
}

/// Cut `s` to `w` columns, with an ellipsis.
fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_owned();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if out.width() + cw + 1 > w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Paint `tabs` into one row of `cols` cells. `alert` colors root tabs;
/// `editing` shows the active tab's label being renamed.
pub fn paint(
    tabs: &[TabInfo],
    cols: usize,
    alert: [u8; 3],
    editing: Option<&str>,
    d: &Design,
) -> (Vec<u8>, Hits) {
    let mut out = String::from("\x1b[H\x1b[?7l");
    let mut hits = Hits::default();
    let reserve = if d.new_tab_button { 3 } else { 0 }; // " + "
    let (left, right) = d.caps();
    let n = tabs.len().max(1);
    let width =
        (cols.saturating_sub(reserve + n * (left + right)) / n).clamp(d.min_width, d.max_width);
    // The last columns of a tab's text: "× " or a space.
    let tail = if d.close_button { 2 } else { 1 };
    let mut col = 0;
    for (i, t) in tabs.iter().enumerate() {
        if col + width + left + right > cols.saturating_sub(reserve) && i > 0 {
            // No room: the rest are summarized.
            sgr(&mut out, d.dim, d.bar, false);
            let more = format!(" +{} ", tabs.len() - i);
            out.push_str(&more);
            col += more.width();
            break;
        }
        let (fg, bg) =
            if t.active { (d.active_fg, d.active_bg) } else { (d.inactive_fg, d.inactive_bg) };
        let fg = if t.root { alert } else { fg };
        // Marks: bell, activity, zoom, pane count.
        let mut marks = String::new();
        if t.bell {
            marks.push('!');
        } else if t.activity {
            marks.push('•');
        }
        if t.zoomed {
            marks.push('⤢');
        } else if t.panes > 1 {
            marks.push_str(&format!("{}", t.panes.min(9)));
        }
        let label = match (t.active, editing) {
            (true, Some(e)) => format!("{e}▏"),
            _ => t.label.clone(),
        };
        let icon = t.icon.map(|c| format!("{c} ")).unwrap_or_default();
        // " icon label marks" then the tail.
        let room = width.saturating_sub(
            2 + tail + icon.width() + marks.width() + usize::from(!marks.is_empty()),
        );
        let mut text = format!(" {icon}{}", fit(&label, room));
        if !marks.is_empty() {
            text.push(' ');
            text.push_str(&marks);
        }
        let pad = width.saturating_sub(text.width() + tail);
        text.push_str(&" ".repeat(pad));
        let start = col;
        if left > 0 {
            sgr(&mut out, bg, d.bar, false);
            out.push('\u{e0b6}');
            col += left;
        }
        // Progress fills the tab's background from the left.
        let filled =
            t.progress.map_or(0, |pr| ((width - 1) as f32 * pr.fraction()).round() as usize);
        let fill_bg = match t.progress {
            Some(Progress::Error(_)) => alert,
            Some(Progress::Paused(_)) => d.dim,
            _ => d.accent,
        };
        let underline = t.active && d.style == Style::Minimal;
        for (k, ch) in text.chars().enumerate() {
            let in_fill = k < filled;
            let (f, b) = if in_fill { (d.bar, fill_bg) } else { (fg, bg) };
            sgr(&mut out, f, b, t.active);
            if underline && k > 0 && ch != ' ' {
                let a = d.accent;
                out.push_str(&format!("\x1b[4;58;2;{};{};{}m", a[0], a[1], a[2]));
            }
            out.push(ch);
        }
        col += text.width();
        if d.close_button {
            sgr(&mut out, if t.active { fg } else { d.dim }, bg, false);
            out.push('×');
            out.push(' ');
            col += 2;
        } else {
            sgr(&mut out, fg, bg, false);
            out.push(' ');
            col += 1;
        }
        let close = if d.close_button { col - 2 } else { usize::MAX };
        // The tab's end: a gap or a shape.
        match d.style {
            Style::Classic => {
                sgr(&mut out, d.border, d.bar, false);
                out.push('▏');
            }
            Style::Minimal => {
                sgr(&mut out, d.border, d.bar, false);
                out.push(' ');
            }
            Style::Powerline => {
                sgr(&mut out, bg, d.bar, false);
                out.push('\u{e0b0}');
            }
            Style::Rounded => {
                sgr(&mut out, bg, d.bar, false);
                out.push('\u{e0b4}');
            }
            Style::Slanted => {
                sgr(&mut out, bg, d.bar, false);
                out.push('\u{e0bc}');
            }
        }
        col += right;
        hits.tabs.push((start, col - right, close));
    }
    sgr(&mut out, d.inactive_fg, d.bar, true);
    if d.new_tab_button && col + 3 <= cols {
        out.push_str(" + ");
        hits.new_tab = Some(col + 1);
        col += 3;
    }
    sgr(&mut out, d.inactive_fg, d.bar, false);
    out.push_str(&" ".repeat(cols.saturating_sub(col)));
    out.push_str("\x1b[0m");
    (out.into_bytes(), hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::from_theme(&seraphim_render::Theme::default())
    }

    fn screen(bytes: &[u8], cols: usize) -> String {
        let mut t = seraphim_terminal::Terminal::new(cols, 1);
        t.advance(bytes);
        t.screen_text()
    }

    #[test]
    fn tabs_labels_marks_and_hits() {
        let tabs = vec![
            TabInfo { label: "bash".into(), active: true, panes: 1, ..Default::default() },
            TabInfo {
                label: "a very long title that will not fit".into(),
                activity: true,
                panes: 2,
                ..Default::default()
            },
        ];
        let (bytes, hits) = paint(&tabs, 60, [255, 0, 0], None, &Design::default());
        let s = screen(&bytes, 60);
        assert!(s.starts_with(" bash"), "{s:?}");
        assert!(s.contains("…"), "long labels are cut: {s:?}");
        assert!(s.contains("•2 "), "activity and pane count: {s:?}");
        assert!(s.contains("+"));
        assert_eq!(hits.tabs.len(), 2);
        let (s0, e0, c0) = hits.tabs[0];
        assert_eq!(hits.at(s0), Hit::Tab(0));
        assert_eq!(hits.at(c0), Hit::Close(0));
        assert_eq!(hits.at(hits.tabs[1].0 + 2), Hit::Tab(1));
        assert_eq!(hits.at(hits.new_tab.unwrap()), Hit::NewTab);
        assert_eq!(hits.at(59), Hit::Bar);
        assert!(e0 <= hits.tabs[1].0);
    }

    #[test]
    fn many_tabs_are_summarized_and_editing_shows() {
        let tabs: Vec<_> = (0..20)
            .map(|i| TabInfo { label: format!("t{i}"), active: i == 0, ..Default::default() })
            .collect();
        let (bytes, hits) = paint(&tabs, 40, [255, 0, 0], None, &Design::default());
        let s = screen(&bytes, 40);
        assert!(hits.tabs.len() < 20);
        assert!(s.contains(&format!("+{}", 20 - hits.tabs.len())), "{s:?}");
        let (bytes, _) = paint(&tabs[..2], 40, [255, 0, 0], Some("new nam"), &Design::default());
        assert!(screen(&bytes, 40).contains("new nam▏"));
    }

    #[test]
    fn styles_shapes_and_buttons() {
        let tabs = vec![
            TabInfo { label: "one".into(), active: true, ..Default::default() },
            TabInfo { label: "two".into(), ..Default::default() },
        ];
        let p = palette();
        let design = |style: &str, close: bool, new_tab: bool| {
            let c = seraphim_config::Tabs {
                style: style.into(),
                close_button: close,
                new_tab_button: new_tab,
                ..Default::default()
            };
            Design::new(&c, &p)
        };
        let (b, _) = paint(&tabs, 60, [255, 0, 0], None, &design("powerline", true, true));
        assert_eq!(screen(&b, 60).matches('\u{e0b0}').count(), 2);
        let (b, h) = paint(&tabs, 60, [255, 0, 0], None, &design("rounded", true, true));
        let s = screen(&b, 60);
        assert!(s.starts_with('\u{e0b6}') && s.contains('\u{e0b4}'), "{s:?}");
        // The cap is part of the tab.
        assert_eq!(h.at(0), Hit::Tab(0));
        let (b, h) = paint(&tabs, 60, [255, 0, 0], None, &design("minimal", false, false));
        let s = screen(&b, 60);
        assert!(!s.contains('×') && !s.contains('+'), "{s:?}");
        assert!(String::from_utf8(b).unwrap().contains("\x1b[4;58;2;"), "underlined");
        assert!(h.new_tab.is_none());
        assert!((0..60).all(|c| !matches!(h.at(c), Hit::Close(_))));
        // Sizes.
        let c = seraphim_config::Tabs { min_width: 10, max_width: 12, ..Default::default() };
        let (_, h) = paint(&tabs, 200, [255, 0, 0], None, &Design::new(&c, &p));
        assert_eq!(h.tabs[0].1 - h.tabs[0].0, 12);
        // Colors from the configuration.
        let mut c = seraphim_config::Tabs::default();
        c.colors.active_bg = Some([1, 2, 3]);
        let (b, _) = paint(&tabs, 60, [255, 0, 0], None, &Design::new(&c, &p));
        assert!(String::from_utf8(b).unwrap().contains("48;2;1;2;3m"));
    }

    #[test]
    fn progress_states() {
        assert_eq!(Progress::parse(1, 50), Some(Progress::Normal(50)));
        assert_eq!(Progress::parse(2, 250), Some(Progress::Error(100)));
        assert_eq!(Progress::parse(0, 0), None);
        assert_eq!(Progress::parse(3, 0), Some(Progress::Indeterminate));
        let tabs = vec![TabInfo {
            label: "build".into(),
            progress: Some(Progress::Normal(50)),
            ..Default::default()
        }];
        let (bytes, _) = paint(&tabs, 40, [255, 0, 0], None, &Design::default());
        assert!(String::from_utf8(bytes).unwrap().contains(&format!(
            "{};{};{}m",
            palette().accent[0],
            palette().accent[1],
            palette().accent[2]
        )));
        assert_eq!(icon_for("nvim"), '\u{e62b}');
        assert_eq!(icon_for("zsh"), '\u{f489}');
    }
}
