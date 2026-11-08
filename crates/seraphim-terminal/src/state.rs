//! Terminal state and escape-sequence semantics (implements [`vte::Perform`]).

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::cell::{Attrs, Cell, Color, Flags};
use crate::grid::{Grid, Row};
use crate::palette::{format_color, parse_color, xterm_256, ColorOverrides, ColorScheme, Rgb};
use crate::selection::{Lines, Selection, SelectionRange};

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
    /// Mode 1007: on the alternate screen without mouse reporting, the
    /// mouse wheel sends cursor keys (scrolls in less, man, vim...).
    pub alternate_scroll: bool,
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
            alternate_scroll: true,
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
    /// Start column of the immediately preceding printed character, if it
    /// was wide (xterm replaces it with a wide char that doesn't fit when
    /// autowrap is off).
    last_wide_at: Option<usize>,
    /// Insert mode off and the active charset is ASCII: printable ASCII can
    /// take the fast path. Recomputed after every control/CSI/ESC dispatch,
    /// the only places those settings change.
    fast_print: bool,
    /// Lines scrolled back into history in the viewport (0 = bottom).
    pub display_offset: usize,
    pub selection: Option<Selection>,
    pub scheme: ColorScheme,
    pub overrides: ColorOverrides,
    /// DECRQSS request being collected (`DCS $ q ... ST`), capped length.
    dcs: Option<Vec<u8>>,
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
            last_wide_at: None,
            fast_print: true,
            display_offset: 0,
            selection: None,
            scheme: ColorScheme::default(),
            overrides: ColorOverrides::default(),
            dcs: None,
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

    /// Mark a row for redraw (no content change, e.g. the cursor moved).
    fn damage_row(&mut self, row: usize) {
        if let Some(d) = self.damage.get_mut(row) {
            *d = true;
        }
    }

    /// Mark a row whose *text* changed: redraw it and drop a selection
    /// that covered it.
    fn content_changed(&mut self, row: usize) {
        self.damage_row(row);
        if self.selection.is_some() {
            self.invalidate_selection(row as i64, row as i64);
        }
    }

    /// Drop the selection if it covers any of screen lines `from..=to`
    /// (their content is being changed under it).
    fn invalidate_selection(&mut self, from: i64, to: i64) {
        if let Some(range) = self.selection_range() {
            if range.intersects_lines(from, to) {
                self.selection = None;
            }
        }
    }

    pub fn selection_range(&self) -> Option<SelectionRange> {
        self.selection.map(|s| s.range(self))
    }

    /// The row shown at viewport row `i` (accounts for scrollback viewing).
    pub fn visible_row(&self, i: usize) -> &Row {
        let line = i as i64 - self.display_offset as i64;
        self.row(line).unwrap_or_else(|| self.active_grid().row(i.min(self.rows - 1)))
    }

    pub fn scroll_display(&mut self, delta: i64) {
        let max = if self.alt_active { 0 } else { self.scrollback.len() as i64 };
        let new = (self.display_offset as i64 + delta).clamp(0, max) as usize;
        if new != self.display_offset {
            self.display_offset = new;
            self.damage_all();
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
        self.selection = None;
        self.display_offset = 0;
        self.last_printed = None;
        self.last_wide_at = None;
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
        let keep = top == 0 && !self.alt_active && self.scrollback_limit > 0;
        let limit = self.scrollback_limit;
        let scrollback = &mut self.scrollback;
        let grid = if self.alt_active { &mut self.alternate } else { &mut self.primary };
        let moved = n.min(bottom + 1 - top);
        grid.scroll_up(top, bottom, n, &attrs, |row| {
            if keep {
                // Recycle the oldest scrollback line's buffer once full.
                let buf = if scrollback.len() >= limit {
                    scrollback.pop_front().unwrap_or_else(|| Row::empty(0))
                } else {
                    Row::empty(row.cells.len())
                };
                scrollback.push_back(std::mem::replace(row, buf));
            }
        });
        if keep {
            // Content moved up into history: keep the viewport and the
            // selection on the same text.
            if self.display_offset > 0 {
                self.display_offset = (self.display_offset + moved).min(self.scrollback.len());
            }
            if let Some(sel) = &mut self.selection {
                sel.anchor.line -= moved as i64;
                sel.head.line -= moved as i64;
                let oldest = -(self.scrollback.len() as i64);
                if sel.anchor.line.min(sel.head.line) < oldest {
                    self.selection = None;
                }
            }
        } else if self.selection.is_some() {
            self.invalidate_selection(top as i64, bottom as i64);
        }
        self.damage_range(top, bottom);
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.scroll_top, self.scroll_bottom);
        let attrs = self.cursor.attrs;
        self.grid().scroll_down(top, bottom, n, &attrs);
        if self.selection.is_some() {
            self.invalidate_selection(top as i64, bottom as i64);
        }
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
        if attrs.bg == Color::Default {
            // Writing default blanks never breaks the `occ` invariant.
            grid.clear_combining(row, from, to);
            for col in from..to {
                *grid.cell_mut_untracked(row, col) = Cell::default();
            }
        } else {
            let r = grid.row_mut(row);
            r.cells[from..to].fill(Cell::blank(&attrs));
            r.clear_combining(from, to);
        }
        self.content_changed(row);
    }

    fn erase_rows(&mut self, from: usize, to: usize) {
        let attrs = self.cursor.attrs;
        let cols = self.cols;
        for r in from..to.min(self.rows) {
            *self.grid().row_mut(r) = Row::new(cols, &attrs);
            self.content_changed(r);
        }
    }

    fn insert_blank(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        let attrs = self.cursor.attrs;
        let grid = self.grid();
        grid.split_wide_boundary(row, col);
        let r = grid.row_mut(row);
        r.shift_combining(col, n as isize, cols);
        let line = &mut r.cells;
        line[col..].rotate_right(n);
        line[col..col + n].fill(Cell::blank(&attrs));
        if line[cols - 1].is_wide() {
            line[cols - 1] = Cell::blank(&attrs);
            r.clear_combining(cols - 1, cols);
        }
        self.cursor.pending_wrap = false;
        self.content_changed(row);
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        let attrs = self.cursor.attrs;
        let grid = self.grid();
        grid.split_wide_boundary(row, col);
        if col + n < cols {
            grid.split_wide_boundary(row, col + n);
        }
        let r = grid.row_mut(row);
        r.clear_combining(col, col + n);
        r.shift_combining(col + n, -(n as isize), cols);
        let line = &mut r.cells;
        line[col..].rotate_left(n);
        line[cols - n..].fill(Cell::blank(&attrs));
        self.cursor.pending_wrap = false;
        self.content_changed(row);
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
        self.grid().scroll_up(t, b, n, &attrs, |_| {});
        self.damage_range(t, b);
        self.scroll_top = top;
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn tab_forward(&mut self, n: usize) {
        if self.cursor.col + 1 >= self.cols {
            // Already at the right margin: nothing moves, and (as in xterm)
            // a pending wrap stays pending.
            return;
        }
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

    fn clear_alternate(&mut self) {
        let attrs = self.cursor.attrs;
        self.alternate.clear(&attrs);
        self.selection = None;
        self.damage_all();
    }

    fn set_alternate(&mut self, enable: bool, clear: bool) {
        if enable == self.alt_active {
            return;
        }
        self.alt_active = enable;
        self.selection = None;
        self.display_offset = 0;
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
        let scheme = std::mem::take(&mut self.scheme);
        let was_alt = self.alt_active;
        *self = State::new(cols, rows, limit);
        self.scrollback = scrollback;
        self.scheme = scheme;
        if was_alt {
            self.events.push(TerminalEvent::AlternateScreen(false));
        }
    }

    // ---- printing ------------------------------------------------------

    // Kept out of line so the ASCII fast path in `print` stays small.
    #[inline(never)]
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
                // Wide char does not fit: wrap, leaving the last column as
                // it is (xterm keeps whatever was there).
                self.wrap_line();
            } else if let Some(start) = self.last_wide_at.filter(|&s| s + 2 <= self.cols) {
                // xterm: replace the wide character printed just before.
                self.cursor.col = start;
            } else {
                // xterm: no room and no autowrap, so the character is dropped.
                return;
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
            *grid.cell_mut(row, col) = Cell { ch: c, attrs: lead };
            grid.clear_combining(row, col, col + width);
            if width == 2 {
                let mut spacer = attrs;
                spacer.flags.insert(Flags::WIDE_SPACER);
                *grid.cell_mut(row, col + 1) = Cell { ch: ' ', attrs: spacer };
            }
        }
        self.content_changed(row);
        self.last_printed = Some(c);
        self.last_wide_at = (width == 2).then_some(col);

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
        self.grid().row_mut(row).push_combining(col, c);
        self.content_changed(row);
    }

    // ---- SGR -----------------------------------------------------------

    fn sgr(&mut self, params: &Params) {
        // vte caps parameters at 32; keep them on the stack (no allocation
        // per SGR sequence).
        let mut storage: [&[u16]; 32] = [&[]; 32];
        let mut len = 0;
        for g in params.iter().take(32) {
            storage[len] = g;
            len += 1;
        }
        let groups = &storage[..len];
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
            // xterm: 1047h switches without clearing; 1047l clears the
            // alternate screen on the way out.
            1047 => {
                if on {
                    self.set_alternate(true, false);
                } else {
                    if self.alt_active {
                        self.clear_alternate();
                    }
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
            // xterm: 1049h always clears, even when already on the
            // alternate screen.
            1049 => {
                if on {
                    self.save_cursor();
                    if self.alt_active {
                        self.clear_alternate();
                    } else {
                        self.set_alternate(true, true);
                    }
                } else {
                    self.set_alternate(false, false);
                    self.restore_cursor();
                }
            }
            9 | 1000 => self.modes.mouse = if on { MouseMode::Press } else { MouseMode::Off },
            1002 => self.modes.mouse = if on { MouseMode::ButtonMotion } else { MouseMode::Off },
            1003 => self.modes.mouse = if on { MouseMode::AnyMotion } else { MouseMode::Off },
            1004 => self.modes.focus_events = on,
            1007 => self.modes.alternate_scroll = on,
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

    fn osc(&mut self, params: &[&[u8]], bell: bool) {
        let Some(cmd) = params.first() else { return };
        // Replies use the same terminator as the request (xterm behaviour).
        let st: &str = if bell { "\x07" } else { "\x1b\\" };
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
            b"4" => {
                // OSC 4 ; index ; spec|? [; index ; spec|? ...]
                for pair in params[1..].chunks(2) {
                    let (Some(idx), Some(spec)) = (pair.first(), pair.get(1)) else { break };
                    let Some(i) = std::str::from_utf8(idx).ok().and_then(|s| s.parse::<u8>().ok())
                    else {
                        continue;
                    };
                    if *spec == b"?" {
                        let c = self.indexed_color(i);
                        let reply = format!("\x1b]4;{i};{}{st}", format_color(c));
                        self.responses.extend_from_slice(reply.as_bytes());
                    } else if let Some(c) = std::str::from_utf8(spec).ok().and_then(parse_color) {
                        self.overrides.indexed[i as usize] = Some(c);
                        self.damage_all();
                    }
                }
            }
            b"10" | b"11" | b"12" => {
                // Each parameter addresses the next dynamic color in turn
                // (OSC 10;?;? queries foreground then background).
                let first: u8 = if *cmd == b"10" {
                    10
                } else if *cmd == b"11" {
                    11
                } else {
                    12
                };
                for (k, spec) in params[1..].iter().enumerate() {
                    let which = first + k as u8;
                    if which > 12 {
                        break;
                    }
                    if *spec == b"?" {
                        let c = self.dynamic_color(which);
                        let reply = format!("\x1b]{which};{}{st}", format_color(c));
                        self.responses.extend_from_slice(reply.as_bytes());
                    } else if let Some(c) = std::str::from_utf8(spec).ok().and_then(parse_color) {
                        *self.dynamic_override(which) = Some(c);
                        self.damage_all();
                    }
                }
            }
            b"104" => {
                if params.len() <= 1 || params[1].is_empty() {
                    *self.overrides.indexed = [None; 256];
                } else {
                    for p in &params[1..] {
                        if let Some(i) =
                            std::str::from_utf8(p).ok().and_then(|s| s.parse::<u8>().ok())
                        {
                            self.overrides.indexed[i as usize] = None;
                        }
                    }
                }
                self.damage_all();
            }
            b"110" | b"111" | b"112" => {
                let which = match *cmd {
                    b"110" => 10,
                    b"111" => 11,
                    _ => 12,
                };
                *self.dynamic_override(which) = None;
                self.damage_all();
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
    #[inline]
    fn print(&mut self, c: char) {
        // Fast path: printable ASCII in the ASCII charset, no insert mode,
        // no pending wrap, onto a narrow cell. This is nearly all output.
        if self.fast_print && matches!(c, ' '..='~') && !self.cursor.pending_wrap {
            let (row, col) = (self.cursor.row, self.cursor.col);
            let cell = Cell { ch: c, attrs: self.cursor.attrs };
            let grid = if self.alt_active { &mut self.alternate } else { &mut self.primary };
            if grid.put_narrow(row, col, cell) {
                self.damage[row] = true;
                if self.selection.is_some() {
                    self.invalidate_selection(row as i64, row as i64);
                }
                self.last_printed = Some(c);
                self.last_wide_at = None;
                if col + 1 >= self.cols {
                    self.cursor.pending_wrap = self.modes.autowrap;
                } else {
                    self.cursor.col = col + 1;
                }
                return;
            }
        }
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        self.execute_inner(byte);
        self.update_fast_print();
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.csi_dispatch_inner(params, intermediates, ignore, action);
        self.update_fast_print();
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.esc_dispatch_inner(intermediates, ignore, byte);
        self.update_fast_print();
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.osc(params, bell_terminated);
    }

    fn hook(&mut self, _params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // DECRQSS: DCS $ q <setting> ST
        self.dcs = (!ignore && intermediates == b"$" && action == 'q').then(Vec::new);
    }

    fn put(&mut self, byte: u8) {
        if let Some(buf) = &mut self.dcs {
            if buf.len() < 64 {
                buf.push(byte);
            }
        }
    }

    fn unhook(&mut self) {
        if let Some(buf) = self.dcs.take() {
            self.decrqss(&buf);
        }
    }
}

impl State {
    fn update_fast_print(&mut self) {
        self.fast_print =
            !self.modes.insert && self.charsets[self.active_charset] == Charset::Ascii;
    }

    fn execute_inner(&mut self, byte: u8) {
        // REP only repeats a graphic character output immediately before it.
        self.last_printed = None;
        self.last_wide_at = None;
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

    fn csi_dispatch_inner(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        // REP repeats a graphic character output *immediately* before it;
        // any CSI (including REP itself, per xterm) ends that.
        let last_printed = self.last_printed.take();
        self.last_wide_at = None;
        if ignore {
            return;
        }
        if action == 'm' && intermediates.is_empty() {
            self.sgr(params);
            return;
        }
        let mut storage = [0u16; 32];
        let mut len = 0;
        for g in params.iter().take(32) {
            storage[len] = g[0];
            len += 1;
        }
        let p = &storage[..len];
        let n = |i: usize| arg(p, i, 1) as usize;
        match (intermediates, action) {
            ([], '@') => self.insert_blank(n(0)),
            ([], 'A') => self.move_up(n(0)),
            ([], 'B') => self.move_down(n(0)),
            ([], 'e') => {
                // VPR ignores the scroll margins (unlike CUD), as in xterm;
                // origin mode still confines it to the region.
                let limit = if self.modes.origin { self.scroll_bottom } else { self.rows - 1 };
                self.cursor.row = (self.cursor.row + n(0)).min(limit);
                self.cursor.pending_wrap = false;
            }
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
                    3 => {
                        self.scrollback.clear();
                        self.display_offset = 0;
                        if self.selection_range().is_some_and(|r| r.start.line < 0) {
                            self.selection = None;
                        }
                    }
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
            // SD. Note: xterm treats an *explicit* `CSI 0 T` as a no-op, but
            // vte reports `CSI T` and `CSI 0 T` identically, so both scroll one
            // line here (see seraphim-conformance KNOWN_DIFFERENCES).
            ([], 'T') if p.len() <= 1 => self.scroll_down(n(0)),
            ([], 'X') => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                self.erase_cells(row, col, col + n(0));
                self.cursor.pending_wrap = false;
            }
            ([], 'b') => {
                if let Some(c) = last_printed {
                    for _ in 0..n(0).min(self.cols * self.rows) {
                        self.print_char(c);
                    }
                    self.last_printed = None;
                }
            }
            ([], 'c') if arg(p, 0, 0) == 0 => {
                // Primary DA: VT220 with ANSI color.
                self.responses.extend_from_slice(b"\x1b[?62;22c");
            }
            ([b'>'], 'c') if arg(p, 0, 0) == 0 => {
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
            ([b'?', b'$'], 'p') => {
                // DECRQM (DEC private): CSI ? Ps ; Pm $ y
                let mode = p.first().copied().unwrap_or(0);
                let state = self.dec_mode_state(mode);
                self.responses.extend_from_slice(format!("\x1b[?{mode};{state}$y").as_bytes());
            }
            ([b'$'], 'p') => {
                let mode = p.first().copied().unwrap_or(0);
                let state = match mode {
                    4 => 2 - self.modes.insert as u8,
                    20 => 2 - self.modes.linefeed_newline as u8,
                    _ => 0,
                };
                self.responses.extend_from_slice(format!("\x1b[{mode};{state}$y").as_bytes());
            }
            ([b'>'], 'q') if arg(p, 0, 0) == 0 => {
                // XTVERSION
                let reply = format!("\x1bP>|Seraphim {}\x1b\\", env!("CARGO_PKG_VERSION"));
                self.responses.extend_from_slice(reply.as_bytes());
            }
            ([], 't') if p.first() == Some(&18) => {
                // Report text area size in characters. (Pixel reports and
                // window manipulation stay unsupported.)
                let reply = format!("\x1b[8;{};{}t", self.rows, self.cols);
                self.responses.extend_from_slice(reply.as_bytes());
            }
            ([], 'n') => match p.first().copied().unwrap_or(0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let row = if self.modes.origin {
                        self.cursor.row.saturating_sub(self.scroll_top)
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
                let bottom = (arg(p, 1, self.rows as u16) as usize).min(self.rows) - 1;
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

    fn esc_dispatch_inner(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.last_printed = None;
        self.last_wide_at = None;
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
                    row.reset(cols, &Attrs::default());
                    row.cells.fill(Cell { ch: 'E', ..Cell::default() });
                    row.touch(cols);
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
}

impl Lines for State {
    fn row(&self, line: i64) -> Option<&Row> {
        if line >= 0 {
            let g = self.active_grid();
            (line < g.rows() as i64).then(|| g.row(line as usize))
        } else if self.alt_active {
            None
        } else {
            let idx = self.scrollback.len() as i64 + line;
            (idx >= 0).then(|| &self.scrollback[idx as usize])
        }
    }
    fn first_line(&self) -> i64 {
        if self.alt_active {
            0
        } else {
            -(self.scrollback.len() as i64)
        }
    }
    fn last_line(&self) -> i64 {
        self.rows as i64 - 1
    }
    fn cols(&self) -> usize {
        self.cols
    }
}

impl State {
    /// Structural invariants, for fuzzing and tests.
    pub fn check_invariants(&self) -> Result<(), String> {
        let (rows, cols) = (self.rows, self.cols);
        if self.cursor.row >= rows || self.cursor.col >= cols {
            return Err(format!(
                "cursor {:?} outside {cols}x{rows}",
                (self.cursor.row, self.cursor.col)
            ));
        }
        if self.scroll_top > self.scroll_bottom || self.scroll_bottom >= rows {
            return Err(format!("bad scroll region {}..={}", self.scroll_top, self.scroll_bottom));
        }
        if self.scrollback.len() > self.scrollback_limit {
            return Err("scrollback over limit".into());
        }
        if self.damage.len() != rows || self.tabs.len() != cols {
            return Err("damage/tab vectors out of sync with size".into());
        }
        for (name, grid) in [("primary", &self.primary), ("alternate", &self.alternate)] {
            if grid.rows() != rows || grid.cols() != cols {
                return Err(format!("{name} grid is {}x{}", grid.cols(), grid.rows()));
            }
            for (r, row) in grid.rows_iter().enumerate() {
                if row.cells.len() != cols {
                    return Err(format!("{name} row {r} has {} cells", row.cells.len()));
                }
                row.check_invariants().map_err(|e| format!("{name} row {r}: {e}"))?;
            }
        }
        Ok(())
    }

    fn indexed_color(&self, i: u8) -> Rgb {
        self.overrides.indexed[i as usize]
            .or_else(|| xterm_256(i))
            .unwrap_or_else(|| self.scheme.ansi[i as usize])
    }

    fn dynamic_override(&mut self, which: u8) -> &mut Option<Rgb> {
        match which {
            10 => &mut self.overrides.foreground,
            11 => &mut self.overrides.background,
            _ => &mut self.overrides.cursor,
        }
    }

    fn dynamic_color(&self, which: u8) -> Rgb {
        match which {
            10 => self.overrides.foreground.unwrap_or(self.scheme.foreground),
            11 => self.overrides.background.unwrap_or(self.scheme.background),
            _ => self.overrides.cursor.unwrap_or(self.scheme.cursor),
        }
    }

    /// DECRQM reply value: 1 set, 2 reset, 0 unrecognized.
    fn dec_mode_state(&self, mode: u16) -> u8 {
        let m = &self.modes;
        let on = match mode {
            1 => m.app_cursor,
            6 => m.origin,
            7 => m.autowrap,
            12 => m.cursor_blink,
            25 => m.cursor_visible,
            47 | 1047 | 1049 => self.alt_active,
            66 => m.app_keypad,
            9 | 1000 => m.mouse == MouseMode::Press,
            1002 => m.mouse == MouseMode::ButtonMotion,
            1003 => m.mouse == MouseMode::AnyMotion,
            1004 => m.focus_events,
            1005 => m.mouse_encoding == MouseEncoding::Utf8,
            1006 => m.mouse_encoding == MouseEncoding::Sgr,
            1007 => m.alternate_scroll,
            2004 => m.bracketed_paste,
            2026 => m.synchronized_output,
            _ => return 0,
        };
        if on {
            1
        } else {
            2
        }
    }

    /// Answer a DECRQSS request: `DCS 1 $ r <value> ST` or `DCS 0 $ r ST`.
    fn decrqss(&mut self, req: &[u8]) {
        let value = match req {
            b"m" => Some(format!("{}m", sgr_string(&self.cursor.attrs))),
            b" q" => {
                let blink = self.modes.cursor_blink;
                let n = match self.modes.cursor_shape {
                    CursorShape::Block => 1,
                    CursorShape::Underline => 3,
                    CursorShape::Bar => 5,
                } + if blink { 0 } else { 1 };
                Some(format!("{n} q"))
            }
            b"r" => Some(format!("{};{}r", self.scroll_top + 1, self.scroll_bottom + 1)),
            _ => None,
        };
        let reply = match value {
            Some(v) => format!("\x1bP1$r{v}\x1b\\"),
            None => "\x1bP0$r\x1b\\".to_owned(),
        };
        self.responses.extend_from_slice(reply.as_bytes());
    }

    pub fn selection_text(&self) -> Option<String> {
        let range = self.selection_range()?;
        Some(crate::selection::text(self, &range))
    }
}

/// SGR parameters that reproduce `a` (for DECRQSS).
fn sgr_string(a: &Attrs) -> String {
    let mut parts = vec!["0".to_owned()];
    let f = a.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "21"),
        (Flags::CURLY_UNDERLINE, "4:3"),
        (Flags::BLINK, "5"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKETHROUGH, "9"),
    ] {
        if f.contains(flag) {
            parts.push(code.to_owned());
        }
    }
    let color = |c: Color, base: u8, ext: u8| -> Option<String> {
        match c {
            Color::Default => None,
            Color::Indexed(i) if i < 8 => Some(format!("{}", base + i)),
            Color::Indexed(i) if i < 16 => Some(format!("{}", base + 60 + i - 8)),
            Color::Indexed(i) => Some(format!("{ext};5;{i}")),
            Color::Rgb(r, g, b) => Some(format!("{ext};2;{r};{g};{b}")),
        }
    };
    parts.extend(color(a.fg, 30, 38));
    parts.extend(color(a.bg, 40, 48));
    if let Color::Rgb(r, g, b) = a.underline_color {
        parts.push(format!("58:2::{r}:{g}:{b}"));
    } else if let Color::Indexed(i) = a.underline_color {
        parts.push(format!("58:5:{i}"));
    }
    parts.join(";")
}
