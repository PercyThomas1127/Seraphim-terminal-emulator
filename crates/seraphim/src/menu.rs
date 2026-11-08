//! The menu bar (File, Edit, View, Profiles, Help, plus user menus).
//!
//! Menus are data: the built-in layout is `seraphim-config/src/menu.toml`
//! (after Konsole's), and `[[menu]]` entries in the configuration add to
//! it. This module is pure — model, layout, keyboard and mouse navigation,
//! and painting into ANSI bytes for a cell-grid overlay — so it can be
//! tested without a window. The overlay is drawn by a second text renderer
//! on top of the terminal; see `window.rs`.

use crate::actions::Action;

/// What an item does.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Action(Action),
    /// Type this text into the shell.
    Send(String),
    Profile(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Entry {
        label: String,
        /// Char index of the access key in `label`.
        key: Option<usize>,
        shortcut: String,
        command: Command,
        /// A check mark (current profile, menu bar shown, ...).
        checked: bool,
    },
    Separator,
}

impl Item {
    fn access_key(&self) -> Option<char> {
        match self {
            Item::Entry { label, key: Some(k), .. } => label.chars().nth(*k),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub title: String,
    pub key: Option<usize>,
    pub items: Vec<Item>,
}

/// Facts the menus show (check marks, the profile list).
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub profiles: Vec<String>,
    pub profile: String,
    pub menu_bar: bool,
    pub fullscreen: bool,
}

/// `"&File"` → ("File", Some(0)); `"&&"` is a literal `&`. Without a
/// marker, the first alphanumeric character is the access key.
pub fn parse_label(s: &str) -> (String, Option<usize>) {
    let mut out = String::new();
    let mut key = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.next() {
                Some('&') => out.push('&'),
                Some(n) => {
                    key.get_or_insert(out.chars().count());
                    out.push(n);
                }
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    let key = key.or_else(|| out.chars().position(|c| c.is_alphanumeric()));
    (out, key)
}

fn plain(title: &str) -> String {
    parse_label(title).0.to_lowercase()
}

/// Built-in menus with the user's `[[menu]]` entries merged in: same
/// title appends (or replaces), a new title goes before Help.
pub fn merge(
    builtin: &[seraphim_config::Menu],
    user: &[seraphim_config::Menu],
) -> Vec<seraphim_config::Menu> {
    let mut menus = builtin.to_vec();
    for u in user {
        match menus.iter_mut().find(|m| plain(&m.title) == plain(&u.title)) {
            Some(m) if u.replace => m.items = u.items.clone(),
            Some(m) => m.items.extend(u.items.iter().cloned()),
            None => {
                let at =
                    menus.iter().position(|m| plain(&m.title) == "help").unwrap_or(menus.len());
                menus.insert(at, u.clone());
            }
        }
    }
    menus
}

/// Resolve configuration into menus for the current [`Context`].
pub fn build(menus: &[seraphim_config::Menu], ctx: &Context) -> Vec<Menu> {
    menus
        .iter()
        .map(|m| {
            let (title, key) = parse_label(&m.title);
            let mut items = Vec::new();
            for it in &m.items {
                if it.separator {
                    // No leading or doubled separators.
                    if items.last().is_some_and(|l| *l != Item::Separator) {
                        items.push(Item::Separator);
                    }
                    continue;
                }
                if it.action.as_deref() == Some("profiles") {
                    for p in &ctx.profiles {
                        items.push(Item::Entry {
                            label: p.clone(),
                            key: None,
                            shortcut: String::new(),
                            command: Command::Profile(p.clone()),
                            checked: *p == ctx.profile,
                        });
                    }
                    continue;
                }
                let (command, shortcut, checked) = if let Some(a) = &it.action {
                    let Some(action) = Action::from_name(a) else { continue };
                    let checked = match action {
                        Action::ToggleMenuBar => ctx.menu_bar,
                        Action::Fullscreen => ctx.fullscreen,
                        _ => false,
                    };
                    (Command::Action(action), action.shortcut_label().to_owned(), checked)
                } else if let Some(text) = &it.send {
                    (Command::Send(text.clone()), String::new(), false)
                } else if let Some(p) = &it.profile {
                    (Command::Profile(p.clone()), String::new(), *p == ctx.profile)
                } else {
                    continue;
                };
                let (label, key) = parse_label(it.label.as_deref().unwrap_or_default());
                items.push(Item::Entry { label, key, shortcut, command, checked });
            }
            while items.last() == Some(&Item::Separator) {
                items.pop();
            }
            Menu { title, key, items }
        })
        .collect()
}

/// A message box (About, Keyboard Shortcuts).
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub title: String,
    pub lines: Vec<String>,
}

/// Keys the menu understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Escape,
    Char(char),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Handled; redraw.
    Stay,
    /// Close the menu.
    Close,
    /// Close the menu and run this.
    Run(Command),
}

/// Where a cell of the overlay is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Title(usize),
    Item(usize),
    /// Inside the open dropdown but not on an item (border, separator).
    Dropdown,
    Panel,
    /// The bar row, between titles.
    Bar,
    Outside,
}

/// Overlay geometry, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub cols: usize,
}

impl Rect {
    fn contains(&self, r: usize, c: usize) -> bool {
        (self.row..self.row + self.rows).contains(&r)
            && (self.col..self.col + self.cols).contains(&c)
    }
}

/// Open/closed state plus the menus themselves.
#[derive(Clone, Debug, Default)]
pub struct MenuBar {
    pub menus: Vec<Menu>,
    /// The open dropdown.
    pub open: Option<usize>,
    /// The highlighted item of the open dropdown.
    pub hover: Option<usize>,
    pub panel: Option<Panel>,
}

const GAP: usize = 4; // between label and shortcut

fn width(s: &str) -> usize {
    s.chars().count()
}

impl MenuBar {
    /// Menus or a panel are taking input.
    pub fn engaged(&self) -> bool {
        self.open.is_some() || self.panel.is_some()
    }

    pub fn close(&mut self) {
        self.open = None;
        self.hover = None;
        self.panel = None;
    }

    pub fn open_menu(&mut self, i: usize) {
        if i < self.menus.len() {
            self.open = Some(i);
            self.hover = self.next_entry(i, None, true);
        }
    }

    /// Column ranges of the titles on the bar: " File " " Edit " ...
    pub fn title_spans(&self) -> Vec<(usize, usize)> {
        let mut col = 1;
        self.menus
            .iter()
            .map(|m| {
                let span = (col, col + width(&m.title) + 2);
                col = span.1;
                span
            })
            .collect()
    }

    /// The open dropdown's box (with its border), clamped to `cols`.
    pub fn dropdown(&self, cols: usize) -> Option<Rect> {
        let i = self.open?;
        let m = &self.menus[i];
        let inner = m
            .items
            .iter()
            .map(|it| match it {
                Item::Entry { label, shortcut, .. } => {
                    // "✓ " + label + gap + shortcut, padded by one space.
                    2 + width(label) + if shortcut.is_empty() { 0 } else { GAP + width(shortcut) }
                }
                Item::Separator => 0,
            })
            .max()
            .unwrap_or(0)
            .max(14);
        let w = inner + 4; // borders + one space each side
        let col = self.title_spans()[i].0.min(cols.saturating_sub(w));
        Some(Rect { row: 1, col, rows: m.items.len() + 2, cols: w.min(cols) })
    }

    pub fn panel_rect(&self, cols: usize, rows: usize) -> Option<Rect> {
        let p = self.panel.as_ref()?;
        let inner = p.lines.iter().map(|l| width(l)).chain([width(&p.title)]).max().unwrap_or(0);
        let w = (inner + 6).min(cols);
        let h = (p.lines.len() + 4).min(rows);
        Some(Rect {
            row: rows.saturating_sub(h) / 2,
            col: cols.saturating_sub(w) / 2,
            rows: h,
            cols: w,
        })
    }

    pub fn hit(&self, row: usize, col: usize, cols: usize, rows: usize) -> Hit {
        if let Some(r) = self.panel_rect(cols, rows) {
            return if r.contains(row, col) { Hit::Panel } else { Hit::Outside };
        }
        if row == 0 {
            return match self.title_spans().iter().position(|&(a, b)| (a..b).contains(&col)) {
                Some(i) => Hit::Title(i),
                None => Hit::Bar,
            };
        }
        if let Some(r) = self.dropdown(cols) {
            if r.contains(row, col) {
                let i = row as isize - r.row as isize - 1;
                let items = &self.menus[self.open.unwrap()].items;
                return match usize::try_from(i).ok().and_then(|i| items.get(i).map(|it| (i, it))) {
                    Some((i, Item::Entry { .. })) if col > r.col && col + 1 < r.col + r.cols => {
                        Hit::Item(i)
                    }
                    _ => Hit::Dropdown,
                };
            }
        }
        Hit::Outside
    }

    /// Next selectable item after `from` (or the first), wrapping.
    fn next_entry(&self, menu: usize, from: Option<usize>, forward: bool) -> Option<usize> {
        let items = &self.menus[menu].items;
        let n = items.len();
        if n == 0 {
            return None;
        }
        let start = match (from, forward) {
            (Some(i), true) => i + 1,
            (Some(i), false) => i + n - 1,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        (0..n)
            .map(|k| if forward { (start + k) % n } else { (start + n - k) % n })
            .find(|&i| matches!(items[i], Item::Entry { .. }))
    }

    pub fn command(&self, item: usize) -> Option<Command> {
        match self.menus.get(self.open?)?.items.get(item)? {
            Item::Entry { command, .. } => Some(command.clone()),
            Item::Separator => None,
        }
    }

    pub fn key(&mut self, k: MenuKey) -> Outcome {
        if self.panel.is_some() {
            return Outcome::Close;
        }
        let Some(open) = self.open else { return Outcome::Close };
        let n = self.menus.len();
        match k {
            MenuKey::Escape => Outcome::Close,
            MenuKey::Left => {
                self.open_menu((open + n - 1) % n);
                Outcome::Stay
            }
            MenuKey::Right => {
                self.open_menu((open + 1) % n);
                Outcome::Stay
            }
            MenuKey::Down | MenuKey::Up => {
                self.hover = self.next_entry(open, self.hover, k == MenuKey::Down);
                Outcome::Stay
            }
            MenuKey::Enter => match self.hover.and_then(|i| self.command(i)) {
                Some(c) => Outcome::Run(c),
                None => Outcome::Stay,
            },
            MenuKey::Char(c) => {
                let c = c.to_lowercase().next().unwrap_or(c);
                let lower = |x: Option<char>| x.and_then(|x| x.to_lowercase().next());
                let items = &self.menus[open].items;
                if let Some(i) = items.iter().position(|it| lower(it.access_key()) == Some(c)) {
                    return Outcome::Run(self.command(i).expect("entries have commands"));
                }
                let title_key = |m: &Menu| lower(m.key.and_then(|k| m.title.chars().nth(k)));
                if let Some(i) = self.menus.iter().position(|m| title_key(m) == Some(c)) {
                    self.open_menu(i);
                }
                Outcome::Stay
            }
        }
    }

    /// Paint the bar (if `show_bar` or engaged), the open dropdown and any
    /// panel as ANSI bytes for a fresh `cols` x `rows` terminal.
    pub fn paint(&self, cols: usize, rows: usize, show_bar: bool, p: &Palette) -> Vec<u8> {
        let mut c = Canvas::new(cols, rows);
        if show_bar || self.engaged() {
            c.fill(0, 0, cols, p.bar_bg);
            for (i, (m, (start, _))) in self.menus.iter().zip(self.title_spans()).enumerate() {
                let on = self.open == Some(i);
                let (fg, bg) = if on { (p.hi_fg, p.hi_bg) } else { (p.bar_fg, p.bar_bg) };
                let st = Style { fg, bg, underline: false };
                c.text(0, start, " ", st);
                c.label(0, start + 1, &m.title, m.key, st, self.engaged());
                c.text(0, start + 1 + width(&m.title), " ", st);
            }
        }
        if let (Some(r), Some(open)) = (self.dropdown(cols), self.open) {
            let border = Style { fg: p.border, bg: p.menu_bg, underline: false };
            c.frame(r, border);
            for (i, it) in self.menus[open].items.iter().enumerate() {
                let row = r.row + 1 + i;
                match it {
                    Item::Separator => {
                        c.text(row, r.col, "├", border);
                        c.text(row, r.col + 1, &"─".repeat(r.cols.saturating_sub(2)), border);
                        c.text(row, r.col + r.cols - 1, "┤", border);
                    }
                    Item::Entry { label, key, shortcut, checked, .. } => {
                        let on = self.hover == Some(i);
                        let (fg, bg) = if on { (p.hi_fg, p.hi_bg) } else { (p.menu_fg, p.menu_bg) };
                        let st = Style { fg, bg, underline: false };
                        c.fill(row, r.col + 1, r.cols.saturating_sub(2), bg);
                        if *checked {
                            let mark = Style { fg: if on { fg } else { p.accent }, ..st };
                            c.text(row, r.col + 2, "✓", mark);
                        }
                        c.label(row, r.col + 4, label, *key, st, true);
                        let dim = Style { fg: if on { fg } else { p.dim }, ..st };
                        let sc = r.col + r.cols - 2 - width(shortcut);
                        c.text(row, sc, shortcut, dim);
                    }
                }
            }
        }
        if let (Some(r), Some(panel)) = (self.panel_rect(cols, rows), &self.panel) {
            let border = Style { fg: p.border, bg: p.menu_bg, underline: false };
            let st = Style { fg: p.menu_fg, bg: p.menu_bg, underline: false };
            c.frame(r, border);
            let title = format!(" {} ", panel.title);
            let tc = r.col + (r.cols.saturating_sub(width(&title))) / 2;
            c.text(r.row, tc, &title, Style { fg: p.accent, ..border });
            for (i, line) in panel.lines.iter().enumerate() {
                let row = r.row + 2 + i;
                if row + 1 < r.row + r.rows {
                    c.fill(row, r.col + 1, r.cols - 2, p.menu_bg);
                    c.text(row, r.col + 3, line, st);
                }
            }
            for row in [r.row + 1, r.row + r.rows.saturating_sub(2)] {
                c.fill(row, r.col + 1, r.cols.saturating_sub(2), p.menu_bg);
            }
        }
        c.to_ansi()
    }
}

/// Colors for the overlay (RGB).
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bar_bg: [u8; 3],
    pub bar_fg: [u8; 3],
    pub menu_bg: [u8; 3],
    pub menu_fg: [u8; 3],
    pub dim: [u8; 3],
    pub border: [u8; 3],
    pub hi_bg: [u8; 3],
    pub hi_fg: [u8; 3],
    pub accent: [u8; 3],
}

impl Palette {
    pub fn from_theme(t: &seraphim_render::Theme) -> Palette {
        let mix = |a: [u8; 3], b: [u8; 3], k: f32| {
            [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * k).round() as u8)
        };
        Palette {
            bar_bg: mix(t.background, t.foreground, 0.08),
            bar_fg: t.foreground,
            menu_bg: mix(t.background, t.foreground, 0.12),
            menu_fg: t.foreground,
            dim: mix(t.foreground, t.background, 0.45),
            border: mix(t.background, t.foreground, 0.30),
            hi_bg: t.selection_bg,
            hi_fg: t.selection_fg.unwrap_or(t.foreground),
            accent: t.ansi[13],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Style {
    fg: [u8; 3],
    bg: [u8; 3],
    underline: bool,
}

struct Canvas {
    cols: usize,
    cells: Vec<Vec<Option<(char, Style)>>>,
}

impl Canvas {
    fn new(cols: usize, rows: usize) -> Self {
        Canvas { cols, cells: vec![vec![None; cols]; rows] }
    }

    fn put(&mut self, row: usize, col: usize, ch: char, st: Style) {
        if let Some(cell) = self.cells.get_mut(row).and_then(|r| r.get_mut(col)) {
            *cell = Some((ch, st));
        }
    }

    fn text(&mut self, row: usize, col: usize, s: &str, st: Style) {
        for (i, ch) in s.chars().enumerate() {
            self.put(row, col + i, ch, st);
        }
    }

    /// Text with its access key underlined (when `keys`).
    fn label(
        &mut self,
        row: usize,
        col: usize,
        s: &str,
        key: Option<usize>,
        st: Style,
        keys: bool,
    ) {
        for (i, ch) in s.chars().enumerate() {
            let underline = keys && key == Some(i);
            self.put(row, col + i, ch, Style { underline, ..st });
        }
    }

    fn fill(&mut self, row: usize, col: usize, n: usize, bg: [u8; 3]) {
        let st = Style { fg: bg, bg, underline: false };
        for c in col..(col + n).min(self.cols) {
            self.put(row, c, ' ', st);
        }
    }

    fn frame(&mut self, r: Rect, st: Style) {
        if r.rows < 2 || r.cols < 2 {
            return;
        }
        let (bottom, right) = (r.row + r.rows - 1, r.col + r.cols - 1);
        let line = "─".repeat(r.cols - 2);
        self.text(r.row, r.col, &format!("┌{line}┐"), st);
        self.text(bottom, r.col, &format!("└{line}┘"), st);
        for row in r.row + 1..bottom {
            self.put(row, r.col, '│', st);
            self.put(row, right, '│', st);
            self.fill(row, r.col + 1, r.cols - 2, st.bg);
        }
    }

    fn to_ansi(&self) -> Vec<u8> {
        let mut out = String::from("\x1b[?25l\x1b[?7l");
        for (r, row) in self.cells.iter().enumerate() {
            let mut last: Option<Style> = None;
            let mut at: Option<usize> = None;
            for (c, cell) in row.iter().enumerate() {
                let Some((ch, st)) = cell else { continue };
                if at != Some(c) {
                    out.push_str(&format!("\x1b[{};{}H", r + 1, c + 1));
                }
                if last != Some(*st) {
                    let [fr, fg, fb] = st.fg;
                    let [br, bg, bb] = st.bg;
                    let ul = if st.underline { 4 } else { 24 };
                    out.push_str(&format!("\x1b[0;{ul};38;2;{fr};{fg};{fb};48;2;{br};{bg};{bb}m"));
                    last = Some(*st);
                }
                out.push(*ch);
                at = Some(c + 1);
            }
        }
        out.push_str("\x1b[0m");
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            profiles: vec!["default".into(), "work".into()],
            profile: "work".into(),
            menu_bar: true,
            fullscreen: false,
        }
    }

    fn bar() -> MenuBar {
        let menus = merge(&seraphim_config::builtin_menu(), &[]);
        MenuBar { menus: build(&menus, &ctx()), ..Default::default() }
    }

    fn entry(b: &MenuBar, menu: usize, i: usize) -> (String, String, bool) {
        match &b.menus[menu].items[i] {
            Item::Entry { label, shortcut, checked, .. } => {
                (label.clone(), shortcut.clone(), *checked)
            }
            Item::Separator => ("---".into(), String::new(), false),
        }
    }

    #[test]
    fn labels() {
        assert_eq!(parse_label("&File"), ("File".into(), Some(0)));
        assert_eq!(parse_label("Select &All"), ("Select All".into(), Some(7)));
        assert_eq!(parse_label("Tom && Jerry"), ("Tom & Jerry".into(), Some(0)));
        assert_eq!(parse_label("  42"), ("  42".into(), Some(2)));
    }

    #[test]
    fn builtin_layout_and_context() {
        let b = bar();
        let titles: Vec<&str> = b.menus.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["File", "Edit", "View", "Profiles", "Help"]);
        assert_eq!(entry(&b, 1, 0), ("Copy".into(), "Ctrl+Shift+C".into(), false));
        assert_eq!(entry(&b, 2, 0), ("Show Menubar".into(), "Ctrl+Shift+M".into(), true));
        // Profiles: the list (current checked), then the rest.
        assert_eq!(entry(&b, 3, 0), ("default".into(), String::new(), false));
        assert_eq!(entry(&b, 3, 1), ("work".into(), String::new(), true));
        assert_eq!(b.menus[3].items[2], Item::Separator);
    }

    #[test]
    fn user_menus_merge() {
        let user = seraphim_config::Config::parse(
            "[[menu]]\ntitle = \"&Tools\"\nitems = [{ label = \"&htop\", send = \"htop\\n\" }]\n\
             [[menu]]\ntitle = \"edit\"\nitems = [{ separator = true }, { label = \"Work\", profile = \"work\" }]\n\
             [[menu]]\ntitle = \"&Help\"\nreplace = true\nitems = [{ label = \"About\", action = \"about\" }]\n",
            std::path::Path::new("t.toml"),
        )
        .unwrap()
        .menu;
        let menus = build(&merge(&seraphim_config::builtin_menu(), &user), &ctx());
        let titles: Vec<&str> = menus.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["File", "Edit", "View", "Profiles", "Tools", "Help"]);
        assert_eq!(menus[5].items.len(), 1, "replaced");
        let last = menus[1].items.last().unwrap();
        assert!(
            matches!(last, Item::Entry { command: Command::Profile(p), checked: true, .. } if p == "work")
        );
        assert!(
            matches!(&menus[4].items[0], Item::Entry { command: Command::Send(s), .. } if s == "htop\n")
        );
    }

    #[test]
    fn keyboard_navigation() {
        let mut b = bar();
        b.open_menu(0);
        assert_eq!(b.hover, Some(0), "first entry");
        assert_eq!(b.key(MenuKey::Down), Outcome::Stay);
        assert_eq!(b.hover, Some(2), "skips the separator");
        assert_eq!(b.key(MenuKey::Up), Outcome::Stay);
        assert_eq!(b.key(MenuKey::Up), Outcome::Stay);
        assert_eq!(b.hover, Some(4), "wraps to Close Window");
        assert_eq!(b.key(MenuKey::Enter), Outcome::Run(Command::Action(Action::CloseWindow)));
        assert_eq!(b.key(MenuKey::Left), Outcome::Stay);
        assert_eq!(b.open, Some(4), "Left from File wraps to Help");
        assert_eq!(b.key(MenuKey::Char('e')), Outcome::Stay);
        assert_eq!(b.open, Some(1), "title access key opens Edit");
        assert_eq!(b.key(MenuKey::Char('A')), Outcome::Run(Command::Action(Action::SelectAll)));
        assert_eq!(b.key(MenuKey::Escape), Outcome::Close);
    }

    #[test]
    fn mouse_hits() {
        let mut b = bar();
        let spans = b.title_spans();
        assert_eq!(spans[0], (1, 7), "\" File \"");
        assert_eq!(b.hit(0, 3, 80, 24), Hit::Title(0));
        assert_eq!(b.hit(0, 70, 80, 24), Hit::Bar);
        assert_eq!(b.hit(5, 5, 80, 24), Hit::Outside);
        b.open_menu(1);
        let r = b.dropdown(80).unwrap();
        assert_eq!(b.hit(r.row, r.col + 2, 80, 24), Hit::Dropdown, "top border");
        assert_eq!(b.hit(r.row + 1, r.col + 2, 80, 24), Hit::Item(0));
        assert_eq!(b.hit(r.row + 3, r.col + 2, 80, 24), Hit::Dropdown, "separator");
        assert_eq!(b.command(0), Some(Command::Action(Action::Copy)));
        // A dropdown near the right edge stays on screen.
        b.open_menu(4);
        let r = b.dropdown(30).unwrap();
        assert!(r.col + r.cols <= 30, "{r:?}");
    }

    #[test]
    fn paints_into_a_terminal() {
        let mut b = bar();
        let p = Palette::from_theme(&seraphim_render::Theme::default());
        let mut t = seraphim_terminal::Terminal::new(80, 20);
        t.advance(&b.paint(80, 20, true, &p));
        assert!(t
            .screen_text()
            .lines()
            .next()
            .unwrap()
            .starts_with("  File  Edit  View  Profiles  Help"));
        b.open_menu(1);
        let mut t = seraphim_terminal::Terminal::new(80, 20);
        t.advance(&b.paint(80, 20, false, &p));
        let text = t.screen_text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].contains('┌'), "{text}");
        assert!(lines[2].contains("Copy") && lines[2].contains("Ctrl+Shift+C"), "{text}");
        assert!(lines[3].contains("Paste"));
        assert!(!t.cursor().visible);
        // Hidden bar, nothing open: nothing drawn.
        b.close();
        let mut t = seraphim_terminal::Terminal::new(80, 20);
        t.advance(&b.paint(80, 20, false, &p));
        assert!(t.screen_text().trim().is_empty());
        // Panel.
        b.panel = Some(Panel { title: "About".into(), lines: vec!["Seraphim".into()] });
        let mut t = seraphim_terminal::Terminal::new(80, 20);
        t.advance(&b.paint(80, 20, false, &p));
        assert!(t.screen_text().contains("Seraphim") && t.screen_text().contains(" About "));
        assert_eq!(b.key(MenuKey::Char('x')), Outcome::Close, "any key closes a panel");
    }
}
