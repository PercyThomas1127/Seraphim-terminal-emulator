//! Terminal state and escape-sequence semantics (implements [`vte::Perform`]).

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::cell::{Attrs, Cell, Color, Flags};
use crate::grid::{Grid, Row};

/// Semantic events surfaced to higher layers. The Terminal Core only
/// *reports* these; acting on them (e.g. touching the clipboard) is the
/// Session/Integration layer's responsibility, under permissions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    Bell,
    TitleChanged(String),
    /// OSC 7 working-directory report (decoded path).
    CwdChanged(String),
    /// OSC 133;A — shell is about to draw a prompt.
    PromptStarted,
    /// OSC 133;B — prompt drawn, user is typing a command.
    CommandInputStarted,
    /// OSC 133;C — command submitted, output begins. `command` is supplied
    /// by shell integration via `cmdline_url=` (percent-encoded) or
    /// `cmdline=` (Kitty's convention).
    CommandExecuted {
        command: Option<String>,
    },
    /// OSC 133;D[;exit] — command finished.
    CommandFinished {
        exit_code: Option<i32>,
    },
    /// OSC 52 store request. `data` is still base64-encoded.
    ClipboardStore {
        selection: String,
        data: String,
    },
    /// OSC 52 query (`?`). Answering requires an explicit permission.
    ClipboardLoad {
        selection: String,
    },
    AlternateScreen(bool),
    ModeChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    Off,
    /// X10 (9) / normal tracking (1000): button presses.
    Press,
    /// 1002: presses plus motion while a button is held.
    ButtonMotion,
    /// 1003: all motion.
    AnyMotion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Terminal modes that affect input encoding and behaviour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Modes {
    pub insert: bool,
    pub linefeed_newline: bool,
    pub origin: bool,
    pub autowrap: bool,
    pub cursor_visible: bool,
    pub cursor_blink: bool,
    pub cursor_shape: CursorShape,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
    pub synchronized_output: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            insert: false,
            linefeed_newline: false,
            origin: false,
            autowrap: true,
            cursor_visible: true,
            cursor_blink: false,
            cursor_shape: CursorShape::Block,
            app_cursor: false,
            app_keypad: false,
            bracketed_paste: false,
            focus_events: false,
            mouse: MouseMode::Off,
            mouse_encoding: MouseEncoding::Default,
            synchronized_output: false,
        }
    }
}

/// Public snapshot of the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorState {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
    pub shape: CursorShape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    pub id: Option<String>,
    pub uri: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Charset {
    #[default]
    Ascii,
    DecSpecial,
}

#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    row: usize,
    col: usize,
    attrs: Attrs,
    /// Set after printing in the last column; the next printable character
    /// wraps first (DEC "last column flag").
    pending_wrap: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct SavedCursor {
    cursor: Cursor,
    origin: bool,
    charsets: [Charset; 2],
    active_charset: usize,
}

pub(crate) struct State {
    pub cols: usize,
    pub rows: usize,
    primary: Grid,
    alternate: Grid,
    pub alt_active: bool,
    cursor: Cursor,
    saved_primary: SavedCursor,
    saved_alt: SavedCursor,
    scroll_top: usize,
    scroll_bottom: usize,
    tabs: Vec<bool>,
    charsets: [Charset; 2],
    active_charset: usize,
    pub modes: Modes,
    pub scrollback: VecDeque<Row>,
    scrollback_limit: usize,
    pub title: String,
    pub cwd: Option<String>,
    pub hyperlinks: Vec<Hyperlink>,
    pub events: Vec<TerminalEvent>,
    pub responses: Vec<u8>,
    damage: Vec<bool>,
    last_printed: Option<char>,
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|i| i % 8 == 0 && i != 0).collect()
}

impl State {
    pub fn new(cols: usize, rows: usize, scrollback_limit: usize) -> Self {
        State {
            cols,
            rows,
            primary: Grid::new(cols, rows),
            alternate: Grid::new(cols, rows),
            alt_active: false,
            cursor: Cursor::default(),
            saved_primary: SavedCursor::default(),
            saved_alt: SavedCursor::default(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            tabs: default_tabs(cols),
            charsets: [Charset::Ascii; 2],
            active_charset: 0,
            modes: Modes::default(),
            scrollback: VecDeque::new(),
            scrollback_limit,
            title: String::new(),
            cwd: None,
            hyperlinks: Vec::new(),
            events: Vec::new(),
            responses: Vec::new(),
            damage: vec![true; rows],
            last_printed: None,
        }
    }

    // ---- accessors -----------------------------------------------------

    pub fn active_grid(&self) -> &Grid {
        if self.alt_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn grid(&mut self) -> &mut Grid {
        if self.alt_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    pub fn cursor_state(&self) -> CursorState {
        CursorState {
            row: self.cursor.row,
            col: self.cursor.col,
            visible: self.modes.cursor_visible,
            shape: self.modes.cursor_shape,
        }
    }

    pub fn take_damage(&mut self) -> Vec<usize> {
        let out = self.damage.iter().enumerate().filter(|(_, d)| **d).map(|(i, _)| i).collect();
        self.damage.iter_mut().for_each(|d| *d = false);
        out
    }

    fn damage_row(&mut self, row: usize) {
        if let Some(d) = self.damage.get_mut(row) {
            *d = true;
        }
    }

    fn damage_range(&mut self, top: usize, bottom: usize) {
        for r in top..=bottom.min(self.rows - 1) {
            self.damage[r] = true;
        }
    }

    fn damage_all(&mut self) {
        self.damage.iter_mut().for_each(|d| *d = true);
    }

    // ---- resize --------------------------------------------------------

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        // Keep the cursor on screen when shrinking: drop lines from the top
        // of the primary screen into scrollback.
        // (Only the active grid is shifted; drop_top <= cursor.row always.)
        let drop_top = (self.cursor.row + 1).saturating_sub(rows);
        let (primary_drop, alt_drop) = if self.alt_active { (0, drop_top) } else { (drop_top, 0) };
        for row in self.primary.resize(cols, rows, primary_drop) {
            self.push_scrollback(row);
        }
        self.alternate.resize(cols, rows, alt_drop);
        self.cursor.row -= drop_top;
        self.cols = cols;
        self.rows = rows;
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.cursor.pending_wrap = false;
        for saved in [&mut self.saved_primary, &mut self.saved_alt] {
            saved.cursor.row = saved.cursor.row.min(rows - 1);
            saved.cursor.col = saved.cursor.col.min(cols - 1);
        }
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        let old = self.tabs.len();
        self.tabs.resize(cols, false);
        for i in old..cols {
            self.tabs[i] = i % 8 == 0 && i != 0;
        }
        self.damage = vec![true; rows];
    }

    fn push_scrollback(&mut self, row: Row) {
        if self.scrollback_limit == 0 {
            return;
        }
        if self.scrollback.len() >= self.scrollback_limit {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(row);
    }

    // ---- primitive operations -----------------------------------------

    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        let attrs = self.cursor.attrs;
        let removed = self.grid().scroll_up(top, bottom, n, &attrs);
        if top == 0 && !self.alt_active {
            for row in removed {
                self.push_scrollback(row);
            }
        }
        self.damage_range(top, bottom);
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        let attrs = self.cursor.attrs;
        self.grid().scroll_down(top, bottom, n, &attrs);
        self.damage_range(top, bottom);
    }

    fn linefeed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    /// Absolute cursor positioning, honouring origin mode.
    fn goto(&mut self, row: usize, col: usize) {
        let (min_row, max_row) = if self.modes.origin {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows - 1)
        };
        self.cursor.row = (min_row + row).min(max_row);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    fn goto_row(&mut self, row: usize) {
        let col = self.cursor.col;
        self.goto(row, col);
    }

    /// Relative vertical movement, stopping at the scroll margins when the
    /// cursor starts inside them (CUU/CUD semantics).
    fn move_up(&mut self, n: usize) {
        let limit = if self.cursor.row >= self.scroll_top { self.scroll_top } else { 0 };
        self.cursor.row = self.cursor.row.saturating_sub(n).max(limit);
        self.cursor.pending_wrap = false;
    }

    fn move_down(&mut self, n: usize) {
        let limit =
            if self.cursor.row <= self.scroll_bottom { self.scroll_bottom } else { self.rows - 1 };
        self.cursor.row = (self.cursor.row + n).min(limit);
        self.cursor.pending_wrap = false;
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let attrs = self.cursor.attrs;
        let to = to.min(self.cols);
        if from >= to {
            return;
        }
        let grid = self.grid();
        grid.split_wide(row, from);
        grid.split_wide(row, to - 1);
        for col in from..to {
            *grid.cell_mut(row, col) = Cell::blank(&attrs);
        }
        self.damage_row(row);
    }

    fn erase_rows(&mut self, from: usize, to: usize) {
        let attrs = self.cursor.attrs;
        let cols = self.cols;
        for r in from..to.min(self.rows) {
            *self.grid().row_mut(r) = Row::new(cols, &attrs);
            self.damage_row(r);
        }
    }

    fn insert_blank(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        let attrs = self.cursor.attrs;
        let grid = self.grid();
        grid.split_wide(row, col);
        let line = &mut grid.row_mut(row).cells;
        line.truncate(cols - n);
        for _ in 0..n {
            line.insert(col, Cell::blank(&attrs));
        }
        if line[cols - 1].is_wide() {
            line[cols - 1] = Cell::blank(&attrs);
        }
        self.cursor.pending_wrap = false;
        self.damage_row(row);
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        let attrs = self.cursor.attrs;
        let grid = self.grid();
        grid.split_wide(row, col);
        if col + n < cols {
            grid.split_wide(row, col + n);
        }
        let line = &mut grid.row_mut(row).cells;
        line.drain(col..col + n);
        line.resize(cols, Cell::blank(&attrs));
        self.cursor.pending_wrap = false;
        self.damage_row(row);
    }

    fn insert_lines(&mut self, n: usize) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        let top = self.scroll_top;
        self.scroll_top = self.cursor.row;
        self.scroll_down(n);
        self.scroll_top = top;
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn delete_lines(&mut self, n: usize) {
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        let top = self.scroll_top;
        self.scroll_top = self.cursor.row;
        // Lines deleted mid-screen never go to scrollback.
        let attrs = self.cursor.attrs;
        let (t, b) = (self.scroll_top, self.scroll_bottom);
        self.grid().scroll_up(t, b, n, &attrs);
        self.damage_range(t, b);
        self.scroll_top = top;
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn tab_forward(&mut self, n: usize) {
        for _ in 0..n {
            let mut col = self.cursor.col + 1;
            while col < self.cols - 1 && !self.tabs[col] {
                col += 1;
            }
            self.cursor.col = col.min(self.cols - 1);
        }
        self.cursor.pending_wrap = false;
    }

    fn tab_backward(&mut self, n: usize) {
        for _ in 0..n {
            let mut col = self.cursor.col;
            while col > 0 {
                col -= 1;
                if self.tabs[col] {
                    break;
                }
            }
            self.cursor.col = col;
        }
        self.cursor.pending_wrap = false;
    }

    fn save_cursor(&mut self) {
        let saved = SavedCursor {
            cursor: self.cursor,
            origin: self.modes.origin,
            charsets: self.charsets,
            active_charset: self.active_charset,
        };
        if self.alt_active {
            self.saved_alt = saved;
        } else {
            self.saved_primary = saved;
        }
    }

    fn restore_cursor(&mut self) {
        let saved = if self.alt_active { self.saved_alt } else { self.saved_primary };
        self.cursor = saved.cursor;
        self.cursor.row = self.cursor.row.min(self.rows - 1);
        self.cursor.col = self.cursor.col.min(self.cols - 1);
        self.modes.origin = saved.origin;
        self.charsets = saved.charsets;
        self.active_charset = saved.active_charset;
    }

    fn set_alternate(&mut self, enable: bool, clear: bool) {
        if enable == self.alt_active {
            return;
        }
        self.alt_active = enable;
        if enable && clear {
            let attrs = self.cursor.attrs;
            self.alternate.clear(&attrs);
        }
        self.damage_all();
        self.events.push(TerminalEvent::AlternateScreen(enable));
    }

    fn reset(&mut self) {
        let (cols, rows, limit) = (self.cols, self.rows, self.scrollback_limit);
        let scrollback = std::mem::take(&mut self.scrollback);
        let was_alt = self.alt_active;
        *self = State::new(cols, rows, limit);
        self.scrollback = scrollback;
        if was_alt {
            self.events.push(TerminalEvent::AlternateScreen(false));
        }
    }

    // ---- printing ------------------------------------------------------

    fn print_char(&mut self, c: char) {
        let c = match self.charsets[self.active_charset] {
            Charset::DecSpecial => dec_special(c),
            Charset::Ascii => c,
        };
        let width = match c.width() {
            Some(w) => w,
            None => return,
        };
        if width == 0 {
            self.attach_combining(c);
            return;
        }
        if width == 2 && self.cols < 2 {
            return;
        }

        if self.cursor.pending_wrap && self.modes.autowrap {
            self.wrap_line();
        }
        if width == 2 && self.cursor.col + 1 >= self.cols {
            if self.modes.autowrap {
                // Wide char does not fit: pad the last column and wrap.
                let (row, col) = (self.cursor.row, self.cursor.col);
                self.erase_cells(row, col, col + 1);
                self.wrap_line();
            } else {
                self.cursor.col = self.cols - 2;
            }
        }
        if self.modes.insert {
            self.insert_blank(width);
        }

        let (row, col) = (self.cursor.row, self.cursor.col);
        let mut attrs = self.cursor.attrs;
        attrs.flags.remove(Flags::WIDE | Flags::WIDE_SPACER);
        {
            let grid = self.grid();
            grid.split_wide(row, col);
            if width == 2 {
                grid.split_wide(row, col + 1);
            }
            let mut lead = attrs;
            if width == 2 {
                lead.flags.insert(Flags::WIDE);
            }
            *grid.cell_mut(row, col) = Cell { ch: c, combining: None, attrs: lead };
            if width == 2 {
                let mut spacer = attrs;
                spacer.flags.insert(Flags::WIDE_SPACER);
                *grid.cell_mut(row, col + 1) = Cell { ch: ' ', combining: None, attrs: spacer };
            }
        }
        self.damage_row(row);
        self.last_printed = Some(c);

        let next = col + width;
        if next >= self.cols {
            self.cursor.col = self.cols - 1;
            self.cursor.pending_wrap = self.modes.autowrap;
        } else {
            self.cursor.col = next;
        }
    }

    fn wrap_line(&mut self) {
        let row = self.cursor.row;
        self.grid().row_mut(row).wrapped = true;
        self.cursor.col = 0;
        self.linefeed();
    }

    fn attach_combining(&mut self, c: char) {
        let row = self.cursor.row;
        let mut col = if self.cursor.pending_wrap {
            self.cursor.col
        } else if self.cursor.col > 0 {
            self.cursor.col - 1
        } else {
            return;
        };
        if self.grid().cell(row, col).is_wide_spacer() && col > 0 {
            col -= 1;
        }
        self.grid().cell_mut(row, col).push_combining(c);
        self.damage_row(row);
    }

    // ---- SGR -----------------------------------------------------------

    fn sgr(&mut self, params: &Params) {
        let groups: Vec<&[u16]> = params.iter().collect();
        if groups.is_empty() {
            self.cursor.attrs = Attrs { link: self.cursor.attrs.link, ..Attrs::default() };
            return;
        }
        let a = &mut self.cursor.attrs;
        let mut i = 0;
        while i < groups.len() {
            let g = groups[i];
            let code = g[0];
            match code {
                0 => *a = Attrs { link: a.link, ..Attrs::default() },
                1 => a.flags.insert(Flags::BOLD),
                2 => a.flags.insert(Flags::DIM),
                3 => a.flags.insert(Flags::ITALIC),
                4 => {
                    a.flags.remove(Flags::ALL_UNDERLINES);
                    match g.get(1).copied() {
                        Some(0) => {}
                        Some(2) => a.flags.insert(Flags::DOUBLE_UNDERLINE),
                        Some(3..=5) => a.flags.insert(Flags::CURLY_UNDERLINE),
                        _ => a.flags.insert(Flags::UNDERLINE),
                    }
                }
                5 | 6 => a.flags.insert(Flags::BLINK),
                7 => a.flags.insert(Flags::INVERSE),
                8 => a.flags.insert(Flags::HIDDEN),
                9 => a.flags.insert(Flags::STRIKETHROUGH),
                21 => {
                    a.flags.remove(Flags::ALL_UNDERLINES);
                    a.flags.insert(Flags::DOUBLE_UNDERLINE);
                }
                22 => a.flags.remove(Flags::BOLD | Flags::DIM),
                23 => a.flags.remove(Flags::ITALIC),
                24 => a.flags.remove(Flags::ALL_UNDERLINES),
                25 => a.flags.remove(Flags::BLINK),
                27 => a.flags.remove(Flags::INVERSE),
                28 => a.flags.remove(Flags::HIDDEN),
                29 => a.flags.remove(Flags::STRIKETHROUGH),
                30..=37 => a.fg = Color::Indexed((code - 30) as u8),
                39 => a.fg = Color::Default,
                40..=47 => a.bg = Color::Indexed((code - 40) as u8),
                49 => a.bg = Color::Default,
                59 => a.underline_color = Color::Default,
                90..=97 => a.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => a.bg = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 | 58 => {
                    let (color, consumed) = parse_extended_color(&groups[i..]);
                    if let Some(color) = color {
                        match code {
                            38 => a.fg = color,
                            48 => a.bg = color,
                            _ => a.underline_color = color,
                        }
                    }
                    i += consumed;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }

    // ---- modes ---------------------------------------------------------

    fn set_dec_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.modes.app_cursor = on,
            6 => {
                self.modes.origin = on;
                self.goto(0, 0);
            }
            7 => self.modes.autowrap = on,
            12 => self.modes.cursor_blink = on,
            25 => {
                self.modes.cursor_visible = on;
                let row = self.cursor.row;
                self.damage_row(row);
            }
            47 => self.set_alternate(on, false),
            66 => self.modes.app_keypad = on,
            1047 => {
                if on {
                    self.set_alternate(true, true);
                } else {
                    self.set_alternate(false, false);
                }
            }
            1048 => {
                if on {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => {
                if on {
                    self.save_cursor();
                    self.set_alternate(true, true);
                } else {
                    self.set_alternate(false, false);
                    self.restore_cursor();
                }
            }
            9 | 1000 => self.modes.mouse = if on { MouseMode::Press } else { MouseMode::Off },
            1002 => self.modes.mouse = if on { MouseMode::ButtonMotion } else { MouseMode::Off },
            1003 => self.modes.mouse = if on { MouseMode::AnyMotion } else { MouseMode::Off },
            1004 => self.modes.focus_events = on,
            1005 => {
                self.modes.mouse_encoding =
                    if on { MouseEncoding::Utf8 } else { MouseEncoding::Default }
            }
            1006 => {
                self.modes.mouse_encoding =
                    if on { MouseEncoding::Sgr } else { MouseEncoding::Default }
            }
            2004 => self.modes.bracketed_paste = on,
            2026 => self.modes.synchronized_output = on,
            _ => return,
        }
        self.events.push(TerminalEvent::ModeChanged);
    }

    fn set_ansi_mode(&mut self, mode: u16, on: bool) {
        match mode {
            4 => self.modes.insert = on,
            20 => self.modes.linefeed_newline = on,
            _ => {}
        }
    }

    // ---- OSC -----------------------------------------------------------

    fn osc(&mut self, params: &[&[u8]]) {
        let Some(cmd) = params.first() else { return };
        let text = |i: usize| -> String {
            params.get(i).map(|p| String::from_utf8_lossy(p).into_owned()).unwrap_or_default()
        };
        match *cmd {
            b"0" | b"2" => {
                // Titles may legitimately contain ';', which vte splits on.
                let title = params[1..]
                    .iter()
                    .map(|p| String::from_utf8_lossy(p))
                    .collect::<Vec<_>>()
                    .join(";");
                let title: String = title.chars().filter(|c| !c.is_control()).take(1024).collect();
                if title != self.title {
                    self.title = title.clone();
                    self.events.push(TerminalEvent::TitleChanged(title));
                }
            }
            b"7" => {
                if let Some(path) = parse_osc7(&text(1)) {
                    if self.cwd.as_deref() != Some(&path) {
                        self.cwd = Some(path.clone());
                        self.events.push(TerminalEvent::CwdChanged(path));
                    }
                }
            }
            b"8" => {
                // OSC 8 ; params ; URI   (URI may contain ';')
                let uri = params.get(2..).map(|p| {
                    p.iter().map(|s| String::from_utf8_lossy(s)).collect::<Vec<_>>().join(";")
                });
                match uri {
                    Some(uri) if !uri.is_empty() => {
                        let id = text(1)
                            .split(':')
                            .find_map(|kv| kv.strip_prefix("id=").map(str::to_owned));
                        let link = Hyperlink { id, uri };
                        let existing = self.hyperlinks.iter().position(|h| {
                            *h == link && link.id.is_some()
                                || (link.id.is_none() && h.id.is_none() && h.uri == link.uri)
                        });
                        let idx = existing.unwrap_or_else(|| {
                            self.hyperlinks.push(link);
                            self.hyperlinks.len() - 1
                        });
                        self.cursor.attrs.link = idx as u32 + 1;
                    }
                    _ => self.cursor.attrs.link = 0,
                }
            }
            b"52" => {
                let selection = text(1);
                let data = text(2);
                let selection = if selection.is_empty() { "c".to_owned() } else { selection };
                if data == "?" {
                    self.events.push(TerminalEvent::ClipboardLoad { selection });
                } else {
                    self.events.push(TerminalEvent::ClipboardStore { selection, data });
                }
            }
            b"133" => match params.get(1).and_then(|p| p.first()) {
                Some(b'A') => self.events.push(TerminalEvent::PromptStarted),
                Some(b'B') => self.events.push(TerminalEvent::CommandInputStarted),
                Some(b'C') => {
                    let command = params[2.min(params.len())..].iter().find_map(|kv| {
                        let kv = String::from_utf8_lossy(kv);
                        if let Some(v) = kv.strip_prefix("cmdline_url=") {
                            percent_decode(v)
                        } else {
                            kv.strip_prefix("cmdline=").map(str::to_owned)
                        }
                    });
                    self.events.push(TerminalEvent::CommandExecuted { command });
                }
                Some(b'D') => {
                    let exit_code = text(2).parse().ok();
                    self.events.push(TerminalEvent::CommandFinished { exit_code });
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Parse 38/48/58 extended colors in either `38;5;n` / `38;2;r;g;b`
/// (semicolon) or `38:5:n` / `38:2:[cs]:r:g:b` (colon) form.
/// Returns the color and how many parameter groups were consumed.
fn parse_extended_color(groups: &[&[u16]]) -> (Option<Color>, usize) {
    let first = groups[0];
    let clamp = |v: u16| v.min(255) as u8;
    if first.len() > 1 {
        // Colon form: everything is in the first group.
        let color = match first.get(1) {
            Some(5) => first.get(2).map(|&n| Color::Indexed(clamp(n))),
            Some(2) => {
                // 38:2:r:g:b or 38:2:colorspace:r:g:b
                let rgb =
                    if first.len() >= 6 { &first[3..6] } else { first.get(2..5).unwrap_or(&[]) };
                (rgb.len() == 3).then(|| Color::Rgb(clamp(rgb[0]), clamp(rgb[1]), clamp(rgb[2])))
            }
            _ => None,
        };
        return (color, 1);
    }
    let get = |i: usize| groups.get(i).map(|g| g[0]);
    match get(1) {
        Some(5) => (get(2).map(|n| Color::Indexed(clamp(n))), 3.min(groups.len())),
        Some(2) => match (get(2), get(3), get(4)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(clamp(r), clamp(g), clamp(b))), 5),
            _ => (None, groups.len()),
        },
        _ => (None, 2.min(groups.len())),
    }
}

/// Decode an OSC 7 `file://host/path` URL into a path.
fn parse_osc7(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    percent_decode(&rest[rest.find('/')?..])
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// DEC Special Graphics (line drawing) character set.
fn dec_special(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => c,
    }
}

/// Parameter `i`, treating missing and zero as `default` (the VT convention
/// for counts and positions).
fn arg(params: &[u16], i: usize, default: u16) -> u16 {
    match params.get(i) {
        Some(&0) | None => default,
        Some(&v) => v,
    }
}

impl Perform for State {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.events.push(TerminalEvent::Bell),
            0x08 => {
                self.cursor.col = self.cursor.col.saturating_sub(1);
                self.cursor.pending_wrap = false;
            }
            0x09 => self.tab_forward(1),
            0x0A..=0x0C => {
                self.linefeed();
                if self.modes.linefeed_newline {
                    self.carriage_return();
                }
            }
            0x0D => self.carriage_return(),
            0x0E => self.active_charset = 1,
            0x0F => self.active_charset = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        if action == 'm' && intermediates.is_empty() {
            self.sgr(params);
            return;
        }
        let p: Vec<u16> = params.iter().map(|g| g[0]).collect();
        let n = |i: usize| arg(&p, i, 1) as usize;
        match (intermediates, action) {
            ([], '@') => self.insert_blank(n(0)),
            ([], 'A') => self.move_up(n(0)),
            ([], 'B') | ([], 'e') => self.move_down(n(0)),
            ([], 'C') | ([], 'a') => {
                self.cursor.col = (self.cursor.col + n(0)).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            ([], 'D') => {
                self.cursor.col = self.cursor.col.saturating_sub(n(0));
                self.cursor.pending_wrap = false;
            }
            ([], 'E') => {
                self.move_down(n(0));
                self.cursor.col = 0;
            }
            ([], 'F') => {
                self.move_up(n(0));
                self.cursor.col = 0;
            }
            ([], 'G') | ([], '`') => {
                self.cursor.col = (n(0) - 1).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            ([], 'H') | ([], 'f') => self.goto(n(0) - 1, n(1) - 1),
            ([], 'I') => self.tab_forward(n(0)),
            ([], 'Z') => self.tab_backward(n(0)),
            ([], 'J') | ([b'?'], 'J') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                match p.first().copied().unwrap_or(0) {
                    0 => {
                        self.erase_cells(row, col, self.cols);
                        self.erase_rows(row + 1, self.rows);
                    }
                    1 => {
                        self.erase_rows(0, row);
                        self.erase_cells(row, 0, col + 1);
                    }
                    2 => self.erase_rows(0, self.rows),
                    3 => self.scrollback.clear(),
                    _ => {}
                }
                self.cursor.pending_wrap = false;
            }
            ([], 'K') | ([b'?'], 'K') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                match p.first().copied().unwrap_or(0) {
                    0 => self.erase_cells(row, col, self.cols),
                    1 => self.erase_cells(row, 0, col + 1),
                    2 => self.erase_cells(row, 0, self.cols),
                    _ => {}
                }
                self.cursor.pending_wrap = false;
            }
            ([], 'L') => self.insert_lines(n(0)),
            ([], 'M') => self.delete_lines(n(0)),
            ([], 'P') => self.delete_chars(n(0)),
            ([], 'S') => self.scroll_up(n(0)),
            ([], 'T') if p.len() <= 1 => self.scroll_down(n(0)),
            ([], 'X') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                self.erase_cells(row, col, col + n(0));
                self.cursor.pending_wrap = false;
            }
            ([], 'b') => {
                if let Some(c) = self.last_printed {
                    for _ in 0..n(0).min(self.cols * self.rows) {
                        self.print_char(c);
                    }
                }
            }
            ([], 'c') if arg(&p, 0, 0) == 0 => {
                // Primary DA: VT220 with ANSI color.
                self.responses.extend_from_slice(b"\x1b[?62;22c");
            }
            ([b'>'], 'c') if arg(&p, 0, 0) == 0 => {
                self.responses.extend_from_slice(b"\x1b[>1;10;0c");
            }
            ([], 'd') => self.goto_row(n(0) - 1),
            ([], 'g') => match p.first().copied().unwrap_or(0) {
                0 => {
                    let col = self.cursor.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            ([], 'h') => p.iter().for_each(|&m| self.set_ansi_mode(m, true)),
            ([], 'l') => p.iter().for_each(|&m| self.set_ansi_mode(m, false)),
            ([b'?'], 'h') => p.iter().for_each(|&m| self.set_dec_mode(m, true)),
            ([b'?'], 'l') => p.iter().for_each(|&m| self.set_dec_mode(m, false)),
            ([], 'n') => match p.first().copied().unwrap_or(0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let row = if self.modes.origin {
                        self.cursor.row - self.scroll_top
                    } else {
                        self.cursor.row
                    };
                    let resp = format!("\x1b[{};{}R", row + 1, self.cursor.col + 1);
                    self.responses.extend_from_slice(resp.as_bytes());
                }
                _ => {}
            },
            ([], 'r') => {
                let top = n(0) - 1;
                let bottom = (arg(&p, 1, self.rows as u16) as usize).min(self.rows) - 1;
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.goto(0, 0);
                }
            }
            ([], 's') => self.save_cursor(),
            ([], 'u') => self.restore_cursor(),
            ([b' '], 'q') => {
                let (shape, blink) = match p.first().copied().unwrap_or(0) {
                    0 | 1 => (CursorShape::Block, true),
                    2 => (CursorShape::Block, false),
                    3 => (CursorShape::Underline, true),
                    4 => (CursorShape::Underline, false),
                    5 => (CursorShape::Bar, true),
                    6 => (CursorShape::Bar, false),
                    _ => return,
                };
                self.modes.cursor_shape = shape;
                self.modes.cursor_blink = blink;
                let row = self.cursor.row;
                self.damage_row(row);
            }
            ([b'!'], 'p') => {
                // DECSTR soft reset: modes and cursor, not screen content.
                let keep =
                    (self.modes.mouse, self.modes.mouse_encoding, self.modes.bracketed_paste);
                self.modes = Modes::default();
                (self.modes.mouse, self.modes.mouse_encoding, self.modes.bracketed_paste) = keep;
                self.cursor.attrs = Attrs::default();
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
                self.charsets = [Charset::Ascii; 2];
                self.active_charset = 0;
                self.saved_primary = SavedCursor::default();
                self.saved_alt = SavedCursor::default();
            }
            // CSI t (window manipulation) is deliberately ignored: it lets
            // programs move/resize windows and report titles, which is a
            // known attack surface.
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.linefeed();
                self.carriage_return();
            }
            ([], b'H') => {
                let col = self.cursor.col;
                self.tabs[col] = true;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.reset(),
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([b'#'], b'8') => {
                // DECALN screen alignment test.
                let cols = self.cols;
                for r in 0..self.rows {
                    let row = self.grid().row_mut(r);
                    row.cells = vec![Cell { ch: 'E', ..Cell::default() }; cols];
                    row.wrapped = false;
                }
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
                self.goto(0, 0);
                self.damage_all();
            }
            ([b'('], b'0') => self.charsets[0] = Charset::DecSpecial,
            ([b'('], _) => self.charsets[0] = Charset::Ascii,
            ([b')'], b'0') => self.charsets[1] = Charset::DecSpecial,
            ([b')'], _) => self.charsets[1] = Charset::Ascii,
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.osc(params);
    }
}
