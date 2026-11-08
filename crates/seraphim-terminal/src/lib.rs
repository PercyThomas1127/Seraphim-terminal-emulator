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
pub mod graphics;
mod grid;
pub mod input;
pub mod palette;
mod prefilter;
mod reflow;
mod selection;
mod sixel;
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
    limiter: prefilter::OscLimiter,
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
            limiter: prefilter::OscLimiter::new(prefilter::DEFAULT_MAX_OSC),
        }
    }

    /// Feed raw bytes from the PTY.
    pub fn advance(&mut self, bytes: &[u8]) {
        let (parser, state) = (&mut self.parser, &mut self.state);
        self.limiter.filter(bytes, |part| match part {
            prefilter::Part::Bytes(b) => parser.advance(state, b),
            prefilter::Part::Apc(payload) => state.apc(&payload),
        });
    }

    /// Inline images (Kitty graphics protocol, Sixel) and their placements.
    pub fn graphics(&self) -> &graphics::Graphics {
        &self.state.graphics
    }

    /// Images to draw in the current viewport, lowest z first.
    pub fn visible_images(&self) -> Vec<graphics::VisiblePlacement<'_>> {
        let s = &self.state;
        s.graphics.visible(s.alt_active, s.rows, s.display_offset)
    }

    /// Cell size in pixels, used to size images given in pixels. Set by
    /// the front end whenever the font changes.
    pub fn set_cell_pixel_size(&mut self, width: f32, height: f32) {
        if width > 0.0 && height > 0.0 {
            self.state.graphics.cell_px = (width, height);
            self.state.graphics.cell_px_known = true;
        }
    }

    /// Maximum OSC payload in bytes; longer ones are discarded.
    pub fn set_max_osc_len(&mut self, max: usize) {
        self.limiter.max = max;
    }

    /// How many oversized OSC sequences were discarded.
    pub fn dropped_osc_count(&self) -> u64 {
        self.limiter.dropped
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

    /// Group grapheme clusters (emoji ZWJ sequences, flags, skin tones,
    /// keycaps, VS16 emoji, combining marks) into single cells with the
    /// cluster's width. On by default; applications can toggle it with
    /// mode 2027. Turning it off gives strict per-codepoint widths.
    pub fn set_grapheme_clustering(&mut self, on: bool) {
        self.state.grapheme_default = on;
        self.state.modes.grapheme_clusters = on;
    }

    // ---- colors -------------------------------------------------------

    /// Set the base colors (normally from the renderer's theme) used to
    /// answer OSC 4/10/11/12 queries.
    pub fn set_color_scheme(&mut self, scheme: palette::ColorScheme) {
        self.state.scheme = scheme;
    }

    pub fn color_scheme(&self) -> &palette::ColorScheme {
        &self.state.scheme
    }

    /// Colors changed by the application (OSC 4/10/11/12); renderers apply
    /// these on top of their theme.
    pub fn color_overrides(&self) -> &palette::ColorOverrides {
        &self.state.overrides
    }

    /// Verify internal consistency (used by fuzzing and tests).
    #[doc(hidden)]
    pub fn check_invariants(&self) -> Result<(), String> {
        self.state.check_invariants()
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
