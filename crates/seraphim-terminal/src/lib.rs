//! # seraphim-terminal
//!
//! The Seraphim **Terminal Core** state machine (spec §3.1, §3.3).
//!
//! This crate turns a byte stream from a PTY into *structured* terminal state:
//! a character grid with attributes, cursor, scrollback, modes, and a queue of
//! semantic [`TerminalEvent`]s. It has no knowledge of rendering, the event
//! bus, extensions, or I/O — by design (spec §2, "Terminal Core must never
//! depend on the Visual Engine / plugins").
//!
//! Escape-sequence *tokenization* is delegated to the [`vte`] crate (see
//! `docs/adr/0002-vt-parser.md`); this crate owns all semantics and state.
//!
//! ```
//! use seraphim_terminal::Terminal;
//! let mut term = Terminal::new(20, 3);
//! term.advance(b"hello\r\n\x1b[1;31mworld\x1b[0m");
//! assert_eq!(term.screen().row(0).text(), "hello");
//! assert_eq!(term.screen().row(1).text(), "world");
//! ```

mod cell;
mod grid;
pub mod input;
mod selection;
mod state;

pub use cell::{Attrs, Cell, Color, Flags};
pub use grid::{Grid, Row};
pub use selection::{Point, Selection, SelectionKind, SelectionRange, WORD_SEPARATORS};
pub use state::{
    CursorShape, CursorState, Hyperlink, Modes, MouseEncoding, MouseMode, TerminalEvent,
};

use std::collections::VecDeque;

/// Default scrollback length in lines.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// A terminal emulator instance: parser + state.
pub struct Terminal {
    parser: vte::Parser,
    state: state::State,
}

impl Terminal {
    /// Create a terminal of `cols` x `rows` cells (each clamped to at least 1).
    pub fn new(cols: usize, rows: usize) -> Self {
        Self::with_scrollback(cols, rows, DEFAULT_SCROLLBACK)
    }

    pub fn with_scrollback(cols: usize, rows: usize, scrollback: usize) -> Self {
        Terminal {
            parser: vte::Parser::new(),
            state: state::State::new(cols.max(1), rows.max(1), scrollback),
        }
    }

    /// Feed raw bytes from the PTY.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.state, bytes);
    }

    /// Resize the terminal. Content is not reflowed (yet); lines pushed off
    /// the top of the primary screen move into scrollback.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.state.resize(cols.max(1), rows.max(1));
    }

    /// The currently displayed screen buffer (primary or alternate).
    pub fn screen(&self) -> &Grid {
        self.state.active_grid()
    }

    pub fn cols(&self) -> usize {
        self.state.cols
    }
    pub fn rows(&self) -> usize {
        self.state.rows
    }

    /// Lines that scrolled off the top of the primary screen, oldest first.
    pub fn scrollback(&self) -> &VecDeque<Row> {
        &self.state.scrollback
    }

    pub fn cursor(&self) -> CursorState {
        self.state.cursor_state()
    }

    pub fn modes(&self) -> &Modes {
        &self.state.modes
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.state.alt_active
    }

    pub fn title(&self) -> &str {
        &self.state.title
    }

    /// Working directory reported by the shell via OSC 7, if any.
    pub fn cwd(&self) -> Option<&str> {
        self.state.cwd.as_deref()
    }

    pub fn hyperlink(&self, id: u32) -> Option<&Hyperlink> {
        id.checked_sub(1).and_then(|i| self.state.hyperlinks.get(i as usize))
    }

    /// Drain semantic events produced since the last call.
    pub fn take_events(&mut self) -> Vec<TerminalEvent> {
        std::mem::take(&mut self.state.events)
    }

    /// Drain bytes the terminal must send back to the application
    /// (device status reports, DA responses, ...).
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.state.responses)
    }

    /// Indices of rows modified since the last call (dirty-region tracking
    /// for partial redraws, spec §4.4).
    pub fn take_damage(&mut self) -> Vec<usize> {
        self.state.take_damage()
    }

    // ---- viewport (scrollback viewing) --------------------------------

    /// How many lines the viewport is scrolled back into history (0 = live).
    pub fn display_offset(&self) -> usize {
        self.state.display_offset
    }

    /// Scroll the viewport by `delta` lines (positive = back into history).
    /// Clamped to the available scrollback; the alternate screen has none.
    pub fn scroll_display(&mut self, delta: i64) {
        self.state.scroll_display(delta);
    }

    /// Return the viewport to the live screen.
    pub fn scroll_to_bottom(&mut self) {
        let off = self.state.display_offset as i64;
        self.state.scroll_display(-off);
    }

    /// The row displayed at viewport row `i`, which may come from
    /// scrollback when [`Terminal::display_offset`] is non-zero. Its length
    /// may differ from [`Terminal::cols`] if the terminal was resized.
    pub fn visible_row(&self, i: usize) -> &Row {
        self.state.visible_row(i)
    }

    /// Absolute [`Point`] for a cell of the viewport.
    pub fn viewport_point(&self, row: usize, col: usize) -> Point {
        Point::new(row as i64 - self.state.display_offset as i64, col.min(self.cols() - 1))
    }

    // ---- selection ------------------------------------------------------

    pub fn start_selection(&mut self, kind: SelectionKind, at: Point) {
        self.state.selection = Some(Selection { kind, anchor: at, head: at });
    }

    /// Move the selection's free end (e.g. while dragging).
    pub fn update_selection(&mut self, to: Point) {
        if let Some(s) = &mut self.state.selection {
            s.head = to;
        }
    }

    pub fn clear_selection(&mut self) {
        self.state.selection = None;
    }

    pub fn selection(&self) -> Option<&Selection> {
        self.state.selection.as_ref()
    }

    /// The selection, normalized and expanded (words/lines), if any.
    pub fn selection_range(&self) -> Option<SelectionRange> {
        self.state.selection_range()
    }

    /// Selected text, with soft-wrapped lines joined.
    pub fn selection_text(&self) -> Option<String> {
        self.state.selection_text()
    }

    /// The whole visible screen as text, one line per row, trailing blanks
    /// trimmed. Mostly useful for tests and headless operation.
    pub fn screen_text(&self) -> String {
        let g = self.screen();
        let mut lines: Vec<String> = g.rows_iter().map(Row::text).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }
}
