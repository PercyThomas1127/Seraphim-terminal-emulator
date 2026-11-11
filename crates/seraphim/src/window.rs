//! The windowed front end: winit window + wgpu surface + one session.
//!
//! Threading: the event loop thread owns the window, surface and renderer.
//! The session's I/O thread never touches them; it only pokes the loop via
//! an `EventLoopProxy`, and the loop redraws (coalesced, vsync-paced) when
//! it gets around to it. No frames are rendered while nothing changes; the
//! only timer is cursor blinking, and only when the application asked for a
//! blinking cursor.

use std::sync::Arc;
use std::time::{Duration, Instant};

use seraphim_events::EventBus;

use crate::interaction::Interactions;
use seraphim_render::{FontConfig, Fonts, Renderer, Theme, View};
use seraphim_session::{ClipboardRequest, Command, Notifier, Session, SessionConfig};
use seraphim_terminal::input::{
    encode_focus, encode_key, encode_key_input, encode_mouse, encode_paste, mouse_reporting, Key,
    KeyInput, KeyKind, ModifierKey, Mods, MouseAction, MouseButton,
};
use seraphim_terminal::{MouseMode, Point};
use seraphim_visual::{Transition, VisualEngine};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton as WButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::actions::{self, Action, ClickTracker, ShortcutKey};
use crate::workspace::{Closed, Direction, Divider, Node, Rect, Split, Workspace};

/// The focused pane's session. A macro, not a method: it borrows only the
/// `panes` field, so other fields stay usable while the terminal is locked.
macro_rules! focused {
    ($a:expr) => {
        $a.panes.get(&$a.term_focus).expect("a terminal pane").session
    };
}

/// One terminal pane.
struct Pane {
    session: Session,
    /// Terminal generation last drawn.
    last_generation: u64,
    /// Output arrived while the pane was out of sight.
    activity: bool,
    bell: bool,
    progress: Option<crate::tabbar::Progress>,
    root: bool,
    /// Where it was last laid out (pixels).
    rect: Rect,
}

/// What new panes run.
#[derive(Clone)]
struct SpawnConfig {
    command: Command,
    /// `terminal.term`.
    term: String,
    scrollback: usize,
    grapheme_clustering: bool,
}

/// `SERAPHIM_TIMING=1`: time to first frame, and key press to echo.
#[derive(Default)]
struct Timing {
    first_frame: bool,
    /// A key went to the shell and its echo hasn't been drawn yet.
    key_at: Option<Instant>,
    /// Output arrived after that key press.
    echoed: bool,
    /// Key → frame showing the response, in milliseconds.
    samples: Vec<f64>,
    /// Time spent drawing and presenting each frame, in milliseconds.
    draw_ms: Vec<f64>,
    /// When the first and the latest frame were presented.
    span: Option<(Instant, Instant)>,
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)]
}

impl Timing {
    fn report(&self) {
        let mut v = self.samples.clone();
        if !v.is_empty() {
            v.sort_by(|a, b| a.total_cmp(b));
            eprintln!(
                "seraphim: timing: key press to the frame with its echo presented, over {} keys: median {:.1} ms, p95 {:.1} ms, max {:.1} ms (includes the program's echo; waiting for vsync and the compositor come on top)",
                v.len(),
                quantile(&v, 0.5),
                quantile(&v, 0.95),
                v[v.len() - 1]
            );
        }
        let mut d = self.draw_ms.clone();
        if let (false, Some((first, last))) = (d.is_empty(), self.span) {
            d.sort_by(|a, b| a.total_cmp(b));
            let secs = (last - first).as_secs_f64();
            let rate = if secs > 0.0 { (d.len() - 1) as f64 / secs } else { 0.0 };
            eprintln!(
                "seraphim: timing: {} frames over {:.1} s ({:.0} per second); drawing and presenting a frame: median {:.2} ms, p95 {:.2} ms, max {:.2} ms",
                d.len(),
                secs,
                rate,
                quantile(&d, 0.5),
                quantile(&d, 0.95),
                d[d.len() - 1]
            );
        }
    }
}

/// `seraphim bench --window`: Seraphim types into its own terminal, as a
/// key press would, and measures how long the echo takes to reach the
/// screen. No input is synthesized for the desktop, so nothing outside the
/// window can receive it.
struct Probe {
    /// Keys still to send.
    left: usize,
    sent: usize,
    next: Option<Instant>,
}

/// When the process started (set by `main`).
pub static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// With timing on, log how far startup has got (`SERAPHIM_TIMING`).
fn phase(on: bool, what: &str) {
    if let (true, Some(s)) = (on, STARTED.get()) {
        eprintln!("seraphim: timing: {what} at {:.1} ms", s.elapsed().as_secs_f64() * 1e3);
    }
}

/// How long a live resize must pause before the terminals are resized.
/// Each terminal resize rewraps lines and tells the shell, which redraws
/// its prompt; while the window edge is dragged, resizes arrive faster
/// than a shell redraws, and redraws overlapping later resizes leave
/// pieces of prompts behind. The window itself repaints at once.
const RESIZE_SETTLE: Duration = Duration::from_millis(80);

/// Renderer slot for pane dividers and dimming.
const DECOR_SLOT: u64 = u64::MAX;
/// Overlay slot for the tab bar (slot 0 is the menu bar).
const TAB_BAR_SLOT: u64 = 1;
/// Seraphim's title bar (`window.decorations = "seraphim"`).
const TITLE_SLOT: u64 = 2;
use crate::menu::{self, Command as MenuCommand, Hit, MenuBar, MenuKey, Outcome};

#[derive(Clone, Debug)]
pub struct WindowOptions {
    pub font_family: Option<String>,
    /// Font size in points.
    pub font_size: f32,
    pub cols: u16,
    pub rows: u16,
    /// Program to run; `None` runs the user's shell.
    pub command: Option<Command>,
    /// Logical padding in pixels.
    pub padding: f32,
    /// Let applications read the clipboard via OSC 52 (off by default:
    /// any program, including one on a remote host, could read it).
    pub allow_clipboard_read: bool,
    pub theme: Theme,
    pub scrollback: usize,
    pub grapheme_clustering: bool,
    pub visual: seraphim_config::Visual,
    /// Background layers, bottom first.
    pub layers: Vec<seraphim_visual::LayerSpec>,
    /// `[[on]]` interaction rules driving the layers from bus events.
    pub rules: Vec<crate::interaction::Rule>,
    /// `[[bind]]` continuous bindings.
    pub binds: Vec<crate::bindings::Bind>,
    /// Active profile name ("default" for none).
    pub profile: String,
    pub config_path: Option<std::path::PathBuf>,
    /// Command-line overrides, re-applied on reload and profile switches.
    pub cli_font_family: Option<String>,
    pub cli_font_size: Option<f32>,
    /// `--preset`, kept across profile switches.
    pub cli_preset: Option<String>,
    /// Show the menu bar.
    pub menu_bar: bool,
    /// `system`, `seraphim` or `none` (`window.decorations`).
    pub decorations: String,
    /// Menu bar contents (built-in merged with `[[menu]]`).
    pub menus: Vec<seraphim_config::Menu>,
    /// Files the configuration came from (watched for live reload).
    pub watch: Vec<std::path::PathBuf>,
    pub live_reload: bool,
    /// Shown in the window at startup (e.g. a config error).
    pub notice: Option<String>,
    pub cursor: seraphim_config::CursorFx,
    /// How scenes change (or why the configured transition failed).
    pub transition: Result<seraphim_visual::TransitionStyle, String>,
    /// `[scripting]` and `[extensions.*]`.
    pub scripting: seraphim_config::Scripting,
    pub extensions: std::collections::BTreeMap<String, seraphim_config::ExtensionConfig>,
    /// `--extension` paths.
    pub cli_extensions: Vec<std::path::PathBuf>,
    /// `-e PROGRAM`: a one-off window that neither restores nor saves the
    /// session.
    pub cli_command: bool,
    /// With `-e`: the configured shell, for New Window.
    pub config_command: Option<Command>,
    pub tabs: seraphim_config::Tabs,
    pub panes: seraphim_config::Panes,
    /// `terminal.term`.
    pub term: String,
    pub layouts: Vec<seraphim_config::Layout>,
    pub startup_layout: Option<String>,
    pub restore_session: bool,
    /// `--layout`: open this instead of the startup layout or session.
    pub cli_layout: Option<String>,
    /// `seraphim bench --window`: type this many keys, then close.
    pub probe: Option<usize>,
}

impl WindowOptions {
    /// What the scripting runtime is started from.
    fn script_sources(&self) -> crate::scripting::Sources {
        crate::scripting::Sources {
            scripting: self.scripting.clone(),
            extensions: self.extensions.clone(),
            config_dir: self
                .config_path
                .as_deref()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf()),
            cli: self.cli_extensions.clone(),
            live_reload: self.live_reload,
        }
    }
}

impl WindowOptions {
    /// Cursor effects; reduced motion keeps only the static glow.
    fn cursor_effects(&self) -> seraphim_render::cursorfx::CursorEffects {
        let c = &self.cursor;
        let fx = seraphim_render::cursorfx::CursorEffects {
            glide_ms: c.glide_ms,
            trail: c.trail,
            blink_fade_ms: c.blink_fade_ms,
            glow: c.glow,
            ripple: c.ripple,
            selection_fade_ms: c.selection_fade_ms,
        };
        if self.visual.reduced_motion {
            fx.still()
        } else {
            fx
        }
    }
}

impl Default for WindowOptions {
    fn default() -> Self {
        WindowOptions {
            font_family: None,
            font_size: 11.0,
            cols: 100,
            rows: 30,
            command: None,
            padding: 6.0,
            allow_clipboard_read: false,
            theme: Theme::default(),
            scrollback: seraphim_terminal::DEFAULT_SCROLLBACK,
            grapheme_clustering: true,
            visual: Default::default(),
            layers: Vec::new(),
            rules: Vec::new(),
            binds: Vec::new(),
            profile: "default".into(),
            config_path: None,
            cli_font_family: None,
            cli_font_size: None,
            cli_preset: None,
            menu_bar: false,
            decorations: "system".into(),
            menus: seraphim_config::builtin_menu(),
            watch: Vec::new(),
            live_reload: true,
            notice: None,
            cursor: Default::default(),
            transition: Ok(Default::default()),
            scripting: Default::default(),
            extensions: Default::default(),
            cli_extensions: Vec::new(),
            cli_command: false,
            config_command: None,
            tabs: Default::default(),
            panes: Default::default(),
            term: "auto".into(),
            layouts: Vec::new(),
            startup_layout: None,
            restore_session: false,
            cli_layout: None,
            probe: None,
        }
    }
}

const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Longest we hold back frames while an application has synchronized
/// output (mode 2026) enabled, in case it never turns it off.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);
/// Lines scrolled per wheel notch when scrolling history.
const WHEEL_LINES: i64 = 3;

#[derive(Debug)]
enum UserEvent {
    /// Terminal state changed (output, exit, clipboard request).
    Wake,
    /// A watched configuration file changed on disk (window key).
    ConfigChanged(u64, Vec<std::path::PathBuf>),
    /// A window's scripts queued commands.
    Script(u64),
}

/// Pane ids are unique in the process, so a pane keeps its id (and its
/// session id) when its tab moves to another window.
static NEXT_PANE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_pane_id() -> u64 {
    NEXT_PANE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// What a new window opens with.
enum Seed {
    /// The first window: `--layout`, a restored window, the startup
    /// layout, or one shell.
    First(Option<crate::restore::Saved>),
    /// Another restored window.
    Restore(crate::restore::Saved),
    /// New Window: one shell, in this directory.
    Shell(Option<std::path::PathBuf>),
    /// A tab detached from another window, with its running sessions.
    Tab(Detached),
}

/// A tab on its way to a new window: its arrangement and its panes,
/// sessions still running.
struct Detached {
    tab: crate::workspace::Tab,
    panes: Vec<(u64, Pane)>,
    panels: Vec<(u64, crate::panels::PanelState)>,
    size: PhysicalSize<u32>,
    position: Option<winit::dpi::PhysicalPosition<i32>>,
}

/// How long a notice stays in the corner of the window.
const TOAST_TIME: Duration = Duration::from_secs(4);

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

/// System clipboards. Missing or failing clipboards degrade to no-ops with
/// a message, never a crash.
struct Clipboards {
    inner: Option<arboard::Clipboard>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Board {
    Clipboard,
    /// The X11/Wayland primary selection (select to copy, middle-click paste).
    Primary,
}

impl Clipboards {
    fn new() -> Self {
        match arboard::Clipboard::new() {
            Ok(c) => Clipboards { inner: Some(c) },
            Err(e) => {
                eprintln!("seraphim: clipboard unavailable: {e}");
                Clipboards { inner: None }
            }
        }
    }

    fn set(&mut self, board: Board, text: String) {
        let Some(c) = &mut self.inner else { return };
        let r = match board {
            Board::Clipboard => c.set_text(text),
            Board::Primary => primary_set(c, text),
        };
        if let Err(e) = r {
            eprintln!("seraphim: clipboard write failed: {e}");
        }
    }

    fn get(&mut self, board: Board) -> Option<String> {
        let c = self.inner.as_mut()?;
        match board {
            Board::Clipboard => c.get_text().ok(),
            Board::Primary => primary_get(c),
        }
    }
}

#[cfg(target_os = "linux")]
fn primary_set(c: &mut arboard::Clipboard, text: String) -> Result<(), arboard::Error> {
    use arboard::{LinuxClipboardKind, SetExtLinux};
    c.set().clipboard(LinuxClipboardKind::Primary).text(text)
}
#[cfg(not(target_os = "linux"))]
fn primary_set(_: &mut arboard::Clipboard, _: String) -> Result<(), arboard::Error> {
    Ok(())
}
#[cfg(target_os = "linux")]
fn primary_get(c: &mut arboard::Clipboard) -> Option<String> {
    use arboard::{GetExtLinux, LinuxClipboardKind};
    c.get().clipboard(LinuxClipboardKind::Primary).text().ok()
}
#[cfg(not(target_os = "linux"))]
fn primary_get(_: &mut arboard::Clipboard) -> Option<String> {
    None
}

#[derive(Default)]
struct MouseState {
    /// Last pointer position in physical pixels.
    pos: (f64, f64),
    /// Last reported cell, to avoid duplicate motion reports.
    cell: Option<(usize, usize)>,
    /// Buttons currently held (for drag selection and motion reports).
    left: bool,
    middle: bool,
    right: bool,
    clicks: ClickTracker,
    /// Press location of a simple selection not started yet: a plain click
    /// must not select (and overwrite PRIMARY with) a single character.
    pending: Option<(Point, bool)>,
    /// Fractional wheel scrolling from touchpads.
    wheel_accum: f64,
    reporting: bool,
}

impl MouseState {
    fn held(&self) -> MouseButton {
        if self.left {
            MouseButton::Left
        } else if self.middle {
            MouseButton::Middle
        } else if self.right {
            MouseButton::Right
        } else {
            MouseButton::None
        }
    }
}

struct Active {
    window: Arc<Window>,
    gpu: Gpu,
    renderer: Renderer,
    /// Terminal panes by id; `ws` arranges them (and panels) into tabs
    /// and splits.
    panes: std::collections::BTreeMap<u64, Pane>,
    /// Panel panes by id.
    panels: std::collections::BTreeMap<u64, crate::panels::PanelState>,
    /// The terminal that has (or last had) focus: typing goes there.
    term_focus: u64,
    /// Finished commands and Gabriel events, for their panels.
    history: std::collections::VecDeque<crate::panels::HistoryEntry>,
    packages: std::collections::VecDeque<crate::panels::PackageEvent>,
    layouts: Vec<seraphim_config::Layout>,
    config_path: Option<std::path::PathBuf>,
    /// Extensions installed (name, permissions asked), for settings.
    extension_list: Vec<(String, String)>,
    ws: Workspace,
    /// The pane that had focus last (to notice changes).
    last_focus: u64,
    spawn: SpawnConfig,
    notifier: Notifier,
    pad: f32,
    tabs_cfg: seraphim_config::Tabs,
    panes_cfg: seraphim_config::Panes,
    /// The window's own profile (tabs without one use it).
    base_profile: String,
    /// Pane rectangles drawn this frame.
    drawn: Vec<(u64, Rect)>,
    timing: Option<Timing>,
    probe: Option<Probe>,
    /// Click targets in the tab bar.
    tab_hits: crate::tabbar::Hits,
    /// A divider being dragged.
    drag: Option<Divider>,
    /// Floating panels over the panes, bottom first. They belong to the
    /// window, not a tab.
    floats: Vec<Float>,
    /// A floating panel has the keyboard.
    float_focus: Option<u64>,
    /// A floating panel being moved or resized.
    float_drag: Option<FloatDrag>,
    /// A tab being dragged to a new place.
    tab_drag: Option<usize>,
    /// Rename Tab: the label being typed.
    renaming: Option<String>,
    /// Tab bar indicators (bells, progress, root) from the bus.
    pane_events: seraphim_events::Subscription,
    bus: EventBus,
    clipboards: Clipboards,
    modifiers: ModifiersState,
    mouse: MouseState,
    focused: bool,
    scale: f64,
    title: String,
    blink_on: bool,
    blink_deadline: Instant,
    warned_clipboard_read: bool,
    /// When the app turned on synchronized output with a frame pending.
    sync_since: Option<Instant>,
    visual: VisualEngine,
    visual_cfg: seraphim_config::Visual,
    last_frame: Instant,
    /// A profile switch or reload requested by a shortcut; applied by `App`,
    /// which owns the configuration.
    pending: Option<Request>,
    /// Draws the menu bar overlay (a separate text renderer on top).
    overlay: Renderer,
    menu: MenuBar,
    /// The bar is pinned (reserves a row); otherwise it only appears
    /// while a menu is open.
    menu_pinned: bool,
    /// `window.decorations`, as the window was created.
    decorations: String,
    /// The title bar button under the pointer.
    decor_hover: Option<crate::decor::Button>,
    /// The pointer shows a resize arrow (on a window edge).
    edge_cursor: bool,
    /// When the title bar was last pressed (a double-click maximizes).
    title_click: Option<Instant>,
    /// The title bar is pressed: moving the pointer moves the window.
    title_drag: bool,
    /// Alt went down with nothing else pressed since: a release opens the
    /// menu.
    alt_tap: bool,
    /// A short notice in the corner (reloads, config errors), until the
    /// deadline.
    toast: Option<(String, Instant)>,
    /// Live reload; `None` when disabled.
    watcher: Option<crate::watch::Watcher>,
    /// What the menus are built from: configuration, config path (for
    /// the profile list) and the active profile.
    menu_src: (Vec<seraphim_config::Menu>, Option<std::path::PathBuf>, String),
    /// `[[on]]` rules, fed from the bus.
    interactions: Interactions,
    bindings: crate::bindings::Bindings,
    /// The compositor says no part of the window is visible.
    occluded: bool,
    /// When the background next changes (animated images); cleared by
    /// each redraw.
    visual_due: Option<Instant>,
    /// A live resize is under way: the grids and shells get the new size
    /// once no resize has arrived for [`RESIZE_SETTLE`].
    resize_due: Option<Instant>,
    /// Lua extensions and what they registered.
    scripts: crate::scripting::ScriptHost,
    /// `[[bind]]` from the configuration (script bindings are added).
    config_binds: Vec<crate::bindings::Bind>,
}

impl Active {
    /// The animation clock runs only while someone can see it.
    fn update_pause(&mut self) {
        let unfocused = !self.focused && !self.visual_cfg.animate_unfocused;
        self.visual.set_paused(unfocused || self.occluded);
    }
}

/// A floating panel and where it is (pixels in the window).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Float {
    id: u64,
    rect: Rect,
}

#[derive(Clone, Copy, Debug)]
struct FloatDrag {
    id: u64,
    /// Resizing by the corner rather than moving by the title.
    resize: bool,
    from: (f64, f64),
    start: Rect,
}

/// Rect slots of floating panels' frames (one per panel, so that each
/// frame is drawn under its own panel and over the ones beneath).
fn frame_slot(id: u64) -> u64 {
    DECOR_SLOT - 1 - id
}

/// Where Seraphim's window buttons are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonsRow {
    Menu,
    Tabs,
    Title,
}

/// Work for `App`, which owns the configuration and the event loop.
#[derive(Clone, Debug, PartialEq)]
enum Request {
    NextProfile,
    Reload,
    /// Live reload: these watched files changed.
    Files(Vec<std::path::PathBuf>),
    Profile(String),
    /// Switching to a tab with its own profile (quietly).
    TabProfile(String),
    /// Font size steps: +1, -1, or 0 to reset.
    Zoom(i8),
    ToggleMenuBar,
    NewWindow,
    /// Move the active tab to a new window (at this screen position).
    DetachTab(Option<winit::dpi::PhysicalPosition<i32>>),
    OpenConfigFolder,
    Quit,
}

/// One window: its configuration and its state.
struct Win {
    options: WindowOptions,
    proxy: EventLoopProxy<UserEvent>,
    active: Option<Active>,
    error: Option<String>,
    exit_code: i32,
    /// Points added to the configured font size (View → Enlarge Font).
    zoom: f32,
    /// Tells this window's events apart from other windows'.
    key: u64,
    /// What it opens with (taken by `start`).
    seed: Option<Seed>,
    /// The window asked to close.
    closed: bool,
    /// A window this one asked for (New Window, a detached tab).
    outgoing: Option<Seed>,
}

/// The windows of this process. A tab can move between them with its
/// sessions running, which separate processes couldn't do.
struct App {
    proxy: EventLoopProxy<UserEvent>,
    options: WindowOptions,
    wins: Vec<Win>,
    next_key: u64,
    started: bool,
    error: Option<String>,
    exit_code: i32,
    /// When the session was last saved (restore_session).
    saved_at: Instant,
}

/// How often the session is saved while windows are open, so that it
/// survives a crash.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

pub fn run(options: WindowOptions) -> Result<i32, String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("cannot open a window: {e}"))?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        options,
        wins: Vec::new(),
        next_key: 0,
        started: false,
        error: None,
        exit_code: 0,
        saved_at: Instant::now(),
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(app.exit_code),
    }
}

impl App {
    /// Open a window. Its configuration and font zoom come from the window
    /// that asked for it (or the command line).
    fn open(
        &mut self,
        event_loop: &ActiveEventLoop,
        options: WindowOptions,
        zoom: f32,
        seed: Seed,
    ) {
        self.next_key += 1;
        let mut w = Win {
            options,
            proxy: self.proxy.clone(),
            active: None,
            error: None,
            exit_code: 0,
            zoom,
            key: self.next_key,
            seed: Some(seed),
            closed: false,
            outgoing: None,
        };
        w.resumed(event_loop);
        if let Some(r) = w.active.as_mut().and_then(|a| a.pending.take()) {
            w.handle(r, event_loop);
        }
        match w.error.take() {
            Some(e) if self.wins.is_empty() => self.error = Some(e),
            Some(e) => eprintln!("seraphim: new window: {e}"),
            None => self.wins.push(w),
        }
    }

    fn win(&mut self, id: WindowId) -> Option<&mut Win> {
        self.wins.iter_mut().find(|w| w.active.as_ref().is_some_and(|a| a.window.id() == id))
    }

    /// After each event: open the windows asked for, close the ones that
    /// are done, save the session now and then, and quit with the last
    /// window.
    fn settle(&mut self, event_loop: &ActiveEventLoop) {
        let wanted: Vec<_> = self
            .wins
            .iter_mut()
            .filter_map(|w| w.outgoing.take().map(|s| (w.options.clone(), w.zoom, s)))
            .collect();
        for (mut options, zoom, seed) in wanted {
            if matches!(seed, Seed::Shell(_)) && options.cli_command {
                // New Window from `seraphim -e PROGRAM` opens a shell.
                options.command = options.config_command.take();
                options.cli_command = false;
            }
            self.open(event_loop, options, zoom, seed);
        }
        if self.wins.iter().any(|w| w.closed) {
            // Closing the last window saves what it had (nothing, if its
            // last shell ended); closing another one forgets it.
            let last = self.wins.iter().all(|w| w.closed);
            if last {
                self.save();
            }
            let (closed, open): (Vec<Win>, Vec<Win>) =
                std::mem::take(&mut self.wins).into_iter().partition(|w| w.closed);
            self.wins = open;
            for mut w in closed {
                self.exit_code = w.exit_code;
                if let Some(e) = w.error.take() {
                    eprintln!("seraphim: {e}");
                }
                w.exiting();
            }
        }
        if self.wins.is_empty() && self.started {
            event_loop.exit();
        }
    }

    fn save(&mut self) {
        self.saved_at = Instant::now();
        // `seraphim -e PROGRAM` is a one-off: it never replaces the
        // session you'd come back to.
        if !self.options.restore_session {
            return;
        }
        let windows: Vec<_> = self
            .wins
            .iter()
            .filter(|w| !w.options.cli_command)
            .filter_map(|w| w.active.as_ref().map(|a| a.saved_session()))
            .collect();
        if windows.is_empty() {
            return;
        }
        if let Err(e) = crate::restore::save(&windows) {
            eprintln!("seraphim: saving the session: {e}");
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        let o = &self.options;
        let mut saved = (o.restore_session && !o.cli_command && o.cli_layout.is_none())
            .then(crate::restore::load)
            .flatten()
            .unwrap_or_default()
            .into_iter();
        let first = saved.next();
        self.open(event_loop, self.options.clone(), 0.0, Seed::First(first));
        self.started = true;
        if self.error.is_some() {
            event_loop.exit();
            return;
        }
        for s in saved {
            self.open(event_loop, self.options.clone(), 0.0, Seed::Restore(s));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match &event {
            UserEvent::ConfigChanged(key, _) | UserEvent::Script(key) => {
                let key = *key;
                if let Some(w) = self.wins.iter_mut().find(|w| w.key == key) {
                    w.user_event(event_loop, event);
                }
            }
            // Session output: any window may own the session.
            UserEvent::Wake => {
                for w in &mut self.wins {
                    w.user_event(event_loop, UserEvent::Wake);
                }
            }
        }
        self.settle(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(w) = self.win(id) {
            w.window_event(event_loop, id, event);
        }
        self.settle(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut wake: Option<Instant> = None;
        for w in &mut self.wins {
            if let Some(t) = w.about_to_wait() {
                wake = Some(wake.map_or(t, |w| w.min(t)));
            }
        }
        if self.options.restore_session {
            let due = self.saved_at + SAVE_INTERVAL;
            if Instant::now() >= due {
                self.save();
            }
            let due = self.saved_at + SAVE_INTERVAL;
            wake = Some(wake.map_or(due, |w| w.min(due)));
        }
        self.settle(event_loop);
        event_loop.set_control_flow(match wake {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if !self.wins.is_empty() {
            self.save();
        }
        for w in &mut self.wins {
            w.exiting();
        }
        self.wins.clear();
    }
}

fn font_px(points: f32, scale: f64) -> f32 {
    // 1pt = 1/72in at 96 dpi logical.
    (points * 96.0 / 72.0 * scale as f32).round()
}

impl Win {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Active, String> {
        let seed = self.seed.take().unwrap_or(Seed::Shell(None));
        let o = &self.options;
        let timed = o.probe.is_some() || std::env::var_os("SERAPHIM_TIMING").is_some();
        phase(timed, "event loop running");
        let scale = event_loop.primary_monitor().map(|m| m.scale_factor()).unwrap_or(1.0);
        let fonts = Fonts::new(&FontConfig {
            family: o.font_family.clone(),
            size_px: font_px(o.font_size, scale),
        })
        .map_err(|e| e.to_string())?;
        let m = fonts.metrics();
        let pad = (o.padding as f64 * scale).round() as f32;
        let bar = if o.menu_bar { m.height.ceil() } else { 0.0 }
            + if o.decorations == "seraphim" { m.height.ceil() } else { 0.0 };
        let mut size = PhysicalSize::new(
            (o.cols as f32 * m.width + 2.0 * pad) as u32,
            (o.rows as f32 * m.height + 2.0 * pad + bar) as u32,
        );
        let mut position = None;
        if let Seed::Tab(d) = &seed {
            // A detached tab keeps its size, and opens where it was dropped.
            size = d.size;
            position = d.position;
        }
        let attrs = Window::default_attributes()
            .with_title("Seraphim")
            // Transparency must be requested when the window is created.
            .with_transparent(o.visual.window_opacity < 1.0)
            .with_inner_size(size)
            .with_min_inner_size(PhysicalSize::new(
                (m.width * 10.0) as u32,
                (m.height * 3.0) as u32,
            ))
            .with_window_icon(crate::desktop::window_icon())
            .with_decorations(o.decorations == "system");
        // WM_CLASS / app id: desktops match the window to seraphim.desktop
        // (its menu entry and icon). Both X11 and Wayland read this name.
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
        let attrs = winit::platform::x11::WindowAttributesExtX11::with_name(
            attrs,
            crate::desktop::APP_ID,
            crate::desktop::APP_ID,
        );
        let attrs = match position {
            Some(p) => attrs.with_position(p),
            None => attrs,
        };
        phase(timed, "fonts loaded");
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        set_window_background(&window, o.theme.background, o.visual.window_opacity < 1.0);
        phase(timed, "window created");
        window.set_ime_allowed(true);
        window.set_cursor(CursorIcon::Text);
        let scale = window.scale_factor();

        let gpu = init_gpu(event_loop, window.clone(), o.visual.window_opacity < 1.0)?;
        phase(timed, "GPU device ready");
        let mut renderer = Renderer::new(&gpu.device, gpu.config.format, fonts, o.theme.clone());
        if (font_px(o.font_size, scale) - renderer.fonts().size_px()).abs() > 0.1 {
            renderer.set_fonts(self.fonts_for(scale)?);
        }
        renderer.set_cursor_effects(o.cursor_effects());
        let overlay =
            Renderer::new(&gpu.device, gpu.config.format, self.fonts_for(scale)?, o.theme.clone());
        let inner = window.inner_size();
        let pad = (o.padding as f64 * scale).round() as f32;
        let bar = if o.menu_bar { renderer.cell_metrics().height.ceil() as u32 } else { 0 };
        let (cols, rows) =
            renderer.grid_size(inner.width, inner.height.saturating_sub(bar).max(1), pad);

        phase(timed, "renderers built");
        let bus = EventBus::new();
        let proxy = self.proxy.clone();
        let notify = Notifier(Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Wake);
        }));
        let command = o.command.clone().unwrap_or_else(Command::default_shell);
        // Subscribe before the first session starts: it reports privilege
        // at once.
        // (The first shell gets the next pane id; a detached tab brings its
        // panes.)
        let first_pane = match &seed {
            Seed::Tab(d) => d.tab.focus,
            _ => NEXT_PANE.load(std::sync::atomic::Ordering::Relaxed) + 1,
        };
        let interactions = Interactions::new(o.rules.clone(), &bus, first_pane);
        let bindings = crate::bindings::Bindings::new(o.binds.clone(), &bus, first_pane);
        let pane_events = bus.subscribe(
            seraphim_events::Pattern::Any(vec![
                "terminal.bell".into(),
                "terminal.progress".into(),
                "session.privileged".into(),
                "command.completed".into(),
                "gabriel.*".into(),
            ]),
            1024,
        );
        let _ = (cols, rows);

        let mut visual = VisualEngine::new(&gpu.device, &gpu.queue, gpu.config.format);
        visual.set_reduced_motion(o.visual.reduced_motion);
        visual.set_global_opacity(o.visual.window_opacity);
        if let Err(e) = o.transition.clone().and_then(|t| visual.set_transition_style(t)) {
            eprintln!("seraphim: {e}; using fade");
        }
        for e in visual.set_layers(&o.layers, Transition::Instant) {
            eprintln!("seraphim: {e}");
        }

        window.request_redraw();
        let mut active = Active {
            window,
            gpu,
            renderer,
            panes: Default::default(),
            panels: Default::default(),
            term_focus: first_pane,
            history: Default::default(),
            packages: Default::default(),
            layouts: o.layouts.clone(),
            config_path: o.config_path.clone(),
            extension_list: Vec::new(),
            ws: Workspace::new(),
            last_focus: first_pane,
            spawn: SpawnConfig {
                command,
                term: o.term.clone(),
                scrollback: o.scrollback,
                grapheme_clustering: o.grapheme_clustering,
            },
            notifier: notify,
            pad,
            tabs_cfg: o.tabs.clone(),
            panes_cfg: o.panes.clone(),
            base_profile: o.profile.clone(),
            drawn: Vec::new(),
            timing: (std::env::var_os("SERAPHIM_TIMING").is_some() || o.probe.is_some())
                .then(Timing::default),
            probe: o.probe.map(|left| Probe { left, sent: 0, next: None }),
            tab_hits: Default::default(),
            drag: None,
            floats: Vec::new(),
            float_focus: None,
            float_drag: None,
            tab_drag: None,
            renaming: None,
            pane_events,
            bus,
            clipboards: Clipboards::new(),
            modifiers: ModifiersState::empty(),
            mouse: MouseState::default(),
            focused: true,
            scale,
            title: String::new(),
            blink_on: true,
            blink_deadline: Instant::now() + BLINK_INTERVAL,
            warned_clipboard_read: false,
            sync_since: None,
            visual,
            visual_cfg: o.visual.clone(),
            last_frame: Instant::now(),
            pending: None,
            overlay,
            menu: MenuBar::default(),
            menu_pinned: o.menu_bar,
            decorations: o.decorations.clone(),
            decor_hover: None,
            edge_cursor: false,
            title_click: None,
            title_drag: false,
            alt_tap: false,
            menu_src: (o.menus.clone(), o.config_path.clone(), o.profile.clone()),
            toast: o.notice.clone().map(|m| (m, Instant::now() + TOAST_TIME * 2)),
            watcher: o.live_reload.then(|| {
                let proxy = self.proxy.clone();
                let key = self.key;
                crate::watch::Watcher::spawn(o.watch.clone(), move |changed| {
                    proxy.send_event(UserEvent::ConfigChanged(key, changed)).is_ok()
                })
            }),
            interactions,
            bindings,
            occluded: false,
            visual_due: None,
            resize_due: None,
            scripts: crate::scripting::ScriptHost::default(),
            config_binds: o.binds.clone(),
        };
        // What the window opens with: --layout, a restored session, the
        // startup layout, or one shell (the first window); a detached tab;
        // or one shell.
        let mut cwd = None;
        match seed {
            Seed::First(saved) => {
                let layout =
                    o.cli_layout.clone().or(o.startup_layout.clone()).filter(|_| saved.is_none());
                let restored = saved.is_some_and(|s| active.restore(s));
                if !restored {
                    if let Some(name) = layout {
                        active.open_layout(&name);
                    }
                }
            }
            Seed::Restore(saved) => {
                active.restore(saved);
            }
            Seed::Shell(dir) => cwd = dir,
            Seed::Tab(d) => {
                for (id, p) in d.panes {
                    // Its events go to this window's bus (and rules) now.
                    p.session.move_to(&active.bus);
                    active.panes.insert(id, p);
                }
                for (id, p) in d.panels {
                    active.panels.insert(id, p);
                }
                let i = active.ws.add_tab(d.tab.root.clone(), d.tab.profile.clone());
                let t = &mut active.ws.tabs[i];
                t.focus = d.tab.focus;
                t.zoomed = d.tab.zoomed;
                t.label = d.tab.label;
            }
        }
        if active.ws.tabs.is_empty() || active.panes.is_empty() {
            let first = active.spawn_pane(cwd, None)?;
            active.ws.add_tab(Node::Pane(first), None);
        }
        active.term_focus = active
            .ws
            .tab()
            .map(|t| t.focus)
            .filter(|f| active.panes.contains_key(f))
            .unwrap_or_else(|| *active.panes.keys().next().expect("a terminal"));
        active.last_focus = active.term_focus;
        active.apply_layout();
        let proxy = self.proxy.clone();
        let key = self.key;
        let wake: seraphim_script::Waker = Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Script(key));
        });
        let (warnings, _) =
            active.scripts.configure(o.script_sources(), &active.bus, wake, &mut active.visual);
        for w in &warnings {
            eprintln!("seraphim: {w}");
        }
        if let Some(w) = warnings.into_iter().next() {
            active.notify(w);
        }
        active.extension_list = active.scripts.installed.clone();
        active.rebuild_menu();
        // Extension panels the window opened with (restored, or in a
        // detached tab) render once their extension hears they are open.
        let script_panels: Vec<_> = active
            .panels
            .values()
            .filter_map(|p| match &p.kind {
                crate::panels::Kind::Script { ext, id } => Some((ext.clone(), id.clone())),
                _ => None,
            })
            .collect();
        for (ext, id) in script_panels {
            active.scripts.panel_event(&ext, &id, seraphim_script::PanelEvent::Open(true));
        }
        // Rules, bindings and scripts follow the focused terminal; a
        // detached tab may bring its own profile.
        let id = active.term_focus;
        active.interactions.set_session(id, &mut active.visual);
        active.bindings.set_session(id, &mut active.visual);
        active.scripts.set_session(id);
        active.check_tab_profile();
        phase(timed, "shell started, scene built");
        Ok(active)
    }

    /// Switch profile (or reload the current one) without restarting the
    /// shell: theme, fonts, background layers and visual settings change;
    /// the background crossfades.
    /// Handle a [`Request`] from the window.
    fn handle(&mut self, request: Request, _event_loop: &ActiveEventLoop) {
        match request {
            Request::Quit => self.closed = true,
            Request::NextProfile
            | Request::Reload
            | Request::Profile(_)
            | Request::TabProfile(_)
            | Request::Files(_) => self.apply_profile(request),
            Request::Zoom(step) => {
                self.zoom = match step {
                    0 => 0.0,
                    s => {
                        let size =
                            (self.options.font_size + self.zoom + s as f32).clamp(4.0, 200.0);
                        size - self.options.font_size
                    }
                };
                self.refresh_fonts();
            }
            Request::ToggleMenuBar => {
                if let Some(a) = &mut self.active {
                    a.menu_pinned = !a.menu_pinned;
                }
                self.resize_grid();
            }
            Request::NewWindow => {
                let cwd = self.active.as_ref().and_then(|a| a.focused_cwd());
                self.outgoing = Some(Seed::Shell(cwd));
            }
            Request::DetachTab(position) => {
                let Some(a) = &mut self.active else { return };
                let i = a.ws.active;
                // The window keeps a terminal (panels alone don't keep
                // one open).
                let others = a.ws.tabs.iter().enumerate().filter(|(j, _)| *j != i);
                let keeps =
                    others.flat_map(|(_, t)| t.root.panes()).any(|p| a.panes.contains_key(&p));
                if !keeps {
                    a.notify("This tab is the window's only terminal".to_owned());
                    return;
                }
                self.outgoing = Some(Seed::Tab(a.detach_tab(i, position)));
            }
            Request::OpenConfigFolder => {
                let dir = self
                    .options
                    .config_path
                    .as_deref()
                    .and_then(|p| p.parent())
                    .map(std::path::Path::to_path_buf);
                match dir {
                    Some(d) => {
                        let _ = std::fs::create_dir_all(&d);
                        open_path(&d);
                    }
                    None => eprintln!("seraphim: no configuration directory (HOME unset?)"),
                }
            }
        }
    }

    /// Rebuild fonts at the configured size plus zoom, keeping the window.
    fn refresh_fonts(&mut self) {
        let Some(scale) = self.active.as_ref().map(|a| a.scale) else { return };
        match (self.fonts_for(scale), self.fonts_for(scale)) {
            (Ok(f), Ok(g)) => {
                let a = self.active.as_mut().unwrap();
                a.renderer.set_fonts(f);
                a.overlay.set_fonts(g);
                let m = a.renderer.cell_metrics();
                for p in a.panes.values() {
                    let _ = p.session.set_cell_pixels(m.width, m.height);
                }
                self.resize_grid();
            }
            (Err(e), _) | (_, Err(e)) => eprintln!("seraphim: {e}"),
        }
    }

    /// Fit the grid to the window (after a resize, font or menu bar change).
    fn resize_grid(&mut self) {
        let Some(scale) = self.active.as_ref().map(|a| a.scale) else { return };
        let pad = self.padding(scale);
        let a = self.active.as_mut().unwrap();
        a.pad = pad;
        a.apply_layout();
    }

    fn apply_profile(&mut self, request: Request) {
        if self.active.is_none() {
            return;
        }
        let request_kind = request.clone();
        let path = self.options.config_path.clone();
        let current = self.options.profile.clone();
        let name = match request {
            Request::Profile(name) | Request::TabProfile(name) => name,
            Request::NextProfile => {
                let all = path
                    .as_deref()
                    .map(seraphim_config::Config::profiles)
                    .unwrap_or_else(|| vec!["default".into()]);
                let i = all.iter().position(|p| *p == current).map_or(0, |i| (i + 1) % all.len());
                all[i].clone()
            }
            _ => current.clone(),
        };
        let (mut o, warnings) = match crate::profile::load(
            path.as_deref(),
            Some(&name),
            self.options.cli_preset.as_deref(),
        ) {
            Ok(r) => r,
            Err(e) => {
                // Keep the current look; say why.
                if let Some(a) = &mut self.active {
                    a.notify(format!("Configuration not applied: {e}"));
                }
                return;
            }
        };
        let live = matches!(request_kind, Request::Files(_));
        let first_warning = warnings.first().cloned();
        for w in warnings {
            eprintln!("seraphim: {w}");
        }
        // Keep what isn't part of a profile, and command-line overrides.
        o.cols = self.options.cols;
        o.rows = self.options.rows;
        o.command = self.options.command.clone();
        o.config_path = path;
        o.cli_font_family = self.options.cli_font_family.clone();
        o.cli_font_size = self.options.cli_font_size;
        o.cli_preset = self.options.cli_preset.clone();
        o.cli_extensions = self.options.cli_extensions.clone();
        o.cli_command = self.options.cli_command;
        o.config_command = self.options.config_command.clone();
        if o.cli_font_family.is_some() {
            o.font_family = o.cli_font_family.clone();
        }
        if let Some(size) = o.cli_font_size {
            o.font_size = size;
        }
        let font_changed = o.font_family != self.options.font_family
            || o.font_size != self.options.font_size
            || o.padding != self.options.padding;
        let fade = Transition::Tween {
            duration: o.visual.transition_ms as f64 / 1000.0,
            easing: seraphim_visual::Easing::EaseInOut,
        };
        let a = self.active.as_mut().unwrap();
        a.renderer.set_theme(o.theme.clone());
        set_window_background(
            &a.window,
            o.theme.background,
            self.options.visual.window_opacity < 1.0,
        );
        a.renderer.set_cursor_effects(o.cursor_effects());
        a.overlay.set_theme(o.theme.clone());
        a.menu.close();
        a.menu_src = (o.menus.clone(), o.config_path.clone(), o.profile.clone());
        let bar_changed = o.menu_bar != self.options.menu_bar && o.menu_bar != a.menu_pinned;
        if bar_changed {
            a.menu_pinned = o.menu_bar;
        }
        a.rebuild_menu();
        {
            for p in a.panes.values() {
                let mut t = p.session.terminal();
                t.set_color_scheme(o.theme.scheme());
                t.set_grapheme_clustering(o.grapheme_clustering);
            }
            a.spawn.scrollback = o.scrollback;
            a.spawn.term = o.term.clone();
            a.spawn.grapheme_clustering = o.grapheme_clustering;
            a.tabs_cfg = o.tabs.clone();
            a.panes_cfg = o.panes.clone();
            // A profile chosen here becomes the tab's; a tab switch
            // (TabProfile) only applies the tab's own.
            if matches!(request_kind, Request::Profile(_) | Request::NextProfile) {
                let name = o.profile.clone();
                if let Some(t) = a.ws.tab_mut() {
                    t.profile = Some(name);
                }
            }
        }
        a.visual.set_reduced_motion(o.visual.reduced_motion);
        a.visual.set_global_opacity(o.visual.window_opacity);
        // Rebuild the scene only if it changed (or a shader/image file did).
        let files_changed = matches!(&request_kind, Request::Files(f)
            if f.iter().any(|p| p.extension().is_none_or(|e| e != "toml")));
        let mut layer_error = None;
        let mut transition_error = None;
        let style = o.transition.clone().and_then(|t| a.visual.set_transition_style(t));
        if let Err(e) = style {
            let _ = a.visual.set_transition_style(Default::default());
            transition_error = Some(format!("{e}; using fade"));
        }
        let rebuilt = o.layers != self.options.layers || files_changed || !live;
        if rebuilt {
            for e in a.visual.set_layers(&o.layers, fade) {
                eprintln!("seraphim: {e}");
                layer_error.get_or_insert(e);
            }
        }
        if let Some(w) = &a.watcher {
            w.set(o.watch.clone());
        }
        if o.visual.window_opacity < 1.0 && self.options.visual.window_opacity >= 1.0 {
            eprintln!("seraphim: window_opacity takes effect after a restart (transparency is set when the window opens)");
        }
        a.visual_cfg = o.visual.clone();
        a.update_pause();
        if rebuilt {
            a.scripts.reapply_layers(&mut a.visual);
        }
        let proxy = self.proxy.clone();
        let key = self.key;
        let wake: seraphim_script::Waker = Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Script(key));
        });
        if request_kind == Request::Reload {
            a.scripts.invalidate();
        }
        let (script_warnings, fx) =
            a.scripts.configure(o.script_sources(), &a.bus, wake, &mut a.visual);
        for w in &script_warnings {
            eprintln!("seraphim: {w}");
        }
        a.extension_list = a.scripts.installed.clone();
        a.layouts = o.layouts.clone();
        a.interactions.replace(o.rules.clone(), &a.bus, &mut a.visual);
        a.config_binds = o.binds.clone();
        let all: Vec<_> = a.config_binds.iter().chain(a.scripts.binds()).cloned().collect();
        a.bindings.replace(all, &a.bus);
        if rebuilt {
            a.bindings.reapply(&mut a.visual);
        }
        a.script_effects(fx);
        let scale = a.scale;
        self.options = o;
        let _ = scale;
        if font_changed {
            self.refresh_fonts();
        } else if bar_changed {
            self.resize_grid();
        }
        let a = self.active.as_mut().unwrap();
        a.bus.publisher("visual").publish(
            "visual.profile.changed",
            Some(focused!(a).id()),
            seraphim_events::json!({ "profile": self.options.profile }),
        );
        let msg = match (
            &request_kind,
            layer_error
                .or(transition_error)
                .or(first_warning)
                .or(script_warnings.into_iter().next()),
        ) {
            (_, Some(problem)) => problem,
            (Request::Files(_), None) => "Configuration reloaded".to_owned(),
            (Request::Reload, None) => "Configuration reloaded".to_owned(),
            (Request::TabProfile(_), None) => String::new(),
            _ => format!("Profile: {}", self.options.profile),
        };
        if !msg.is_empty() {
            a.notify(msg);
        }
        // The tab bar may have appeared or gone (tabs.bar).
        a.apply_layout();
        a.window.request_redraw();
    }

    fn fonts_for(&self, scale: f64) -> Result<Fonts, String> {
        Fonts::new(&FontConfig {
            family: self.options.font_family.clone(),
            size_px: font_px(self.options.font_size + self.zoom, scale),
        })
        .map_err(|e| e.to_string())
    }

    fn padding(&self, scale: f64) -> f32 {
        (self.options.padding as f64 * scale).round() as f32
    }
}

/// On X11, give the window a background color: X paints it into any part
/// of the window that has nothing drawn yet, such as the area a resize
/// uncovers before Seraphim's next frame. Without one (winit sets none),
/// that area shows leftover memory: random noise while resizing. Skipped
/// for transparent windows, whose 32-bit visual takes pixels differently.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
fn set_window_background(window: &Window, rgb: [u8; 3], transparent: bool) {
    use winit::raw_window_handle::{
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    };
    if transparent {
        return;
    }
    let (Ok(w), Ok(d)) = (window.window_handle(), window.display_handle()) else { return };
    let (RawWindowHandle::Xlib(w), RawDisplayHandle::Xlib(d)) = (w.as_raw(), d.as_raw()) else {
        return; // Wayland: the compositor never shows undrawn areas.
    };
    let Some(display) = d.display else { return };
    let Ok(xlib) = x11_dl::xlib::Xlib::open() else { return };
    // A 24-bit TrueColor visual: the pixel is 0xRRGGBB.
    let pixel = (rgb[0] as u64) << 16 | (rgb[1] as u64) << 8 | rgb[2] as u64;
    // SAFETY: winit's live Xlib display and window, on the event loop's
    // thread; Xlib copies the values.
    unsafe {
        let dpy = display.as_ptr() as *mut x11_dl::xlib::Display;
        (xlib.XSetWindowBackground)(dpy, w.window, pixel as _);
        (xlib.XFlush)(dpy);
    }
}

#[cfg(not(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android")))))]
fn set_window_background(_window: &Window, _rgb: [u8; 3], _transparent: bool) {}

fn init_gpu(
    event_loop: &ActiveEventLoop,
    window: Arc<Window>,
    transparent: bool,
) -> Result<Gpu, String> {
    use seraphim_render::gpu;
    // Native APIs first, OpenGL only if none fits (gpu::tiers).
    let open = |wanted: Option<&str>| {
        gpu::first_tier(|backends| {
            let mut desc = wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
                event_loop.owned_display_handle(),
            ));
            desc.backends = backends;
            let instance = wgpu::Instance::new(desc);
            let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
            let adapter = gpu::choose(&instance, Some(&surface), wanted)?;
            Ok((surface, adapter))
        })
    };
    // SERAPHIM_GPU picks a GPU on machines with several; if it names one
    // that can't draw here, say so and let wgpu choose.
    let wanted = gpu::from_env();
    let (surface, adapter) = match open(wanted.as_deref()) {
        Ok(r) => r,
        Err(e) if wanted.is_some() => {
            eprintln!("seraphim: {e}; using the default GPU");
            open(None)?
        }
        Err(e) => return Err(e),
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("seraphim"),
        required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
    // A GPU error must not take the shells down with it (spec §13: visual
    // crashes must not crash terminal sessions). wgpu's default panics;
    // log instead. The frame it happened in may look wrong; the next one
    // starts afresh.
    device.on_uncaptured_error(Arc::new(|e: wgpu::Error| {
        static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The first few in full, then a reminder now and then.
        if n < 5 || n.is_power_of_two() {
            let first = e
                .to_string()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(4)
                .collect::<Vec<_>>()
                .join(" / ");
            eprintln!("seraphim: GPU error #{} (ignored, Seraphim keeps running): {first}", n + 1);
        }
    }));
    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width.max(1), size.height.max(1))
        .ok_or("surface is not supported by this adapter")?;
    // Colors are blended in sRGB space (see shader.wgsl): prefer a non-sRGB
    // format, falling back to whatever the surface offers.
    let caps = surface.get_capabilities(&adapter);
    if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
        config.format = *f;
    }
    config.present_mode = wgpu::PresentMode::AutoVsync;
    if transparent {
        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            config.alpha_mode = wgpu::CompositeAlphaMode::PreMultiplied;
        } else {
            eprintln!("seraphim: this display does not support transparent windows; window_opacity ignored");
        }
    }
    surface.configure(&device, &config);
    eprintln!(
        "seraphim: {} ({:?}), surface {:?}{}",
        adapter.get_info().name,
        adapter.get_info().backend,
        config.format,
        if transparent {
            format!(", alpha {:?} (offered {:?})", config.alpha_mode, caps.alpha_modes)
        } else {
            String::new()
        }
    );
    Ok(Gpu { surface, device, queue, config })
}

impl Active {
    fn mods(&self) -> Mods {
        Mods {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            ctrl: self.modifiers.control_key(),
            super_key: self.modifiers.super_key(),
        }
    }

    /// Restart the blink cycle with the cursor visible (after input/output).
    fn reset_blink(&mut self) {
        self.blink_on = true;
        self.blink_deadline = Instant::now() + BLINK_INTERVAL;
    }

    fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Typing returns the view to the live screen.
        {
            let mut t = focused!(self).terminal();
            if t.display_offset() > 0 {
                t.scroll_to_bottom();
                self.window.request_redraw();
            }
        }
        self.reset_blink();
        let _ = focused!(self).write(bytes);
    }

    fn paste(&mut self, board: Board) {
        let Some(text) = self.clipboards.get(board) else { return };
        if text.is_empty() {
            return;
        }
        let bytes = encode_paste(&text, focused!(self).terminal().modes());
        self.write(&bytes);
    }

    fn copy_selection(&mut self, board: Board) {
        let text = focused!(self).terminal().selection_text();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.clipboards.set(board, text);
        }
    }

    fn run_action(&mut self, action: Action) {
        // Moving around the tabs and splits takes the keyboard from a
        // floating panel.
        use Action as A;
        if matches!(
            action,
            A::NewTab
                | A::CloseTab
                | A::NextTab
                | A::PrevTab
                | A::GotoTab(_)
                | A::MoveTabLeft
                | A::MoveTabRight
                | A::RenameTab
                | A::DetachTab
                | A::SplitRight
                | A::SplitDown
                | A::FocusLeft
                | A::FocusRight
                | A::FocusUp
                | A::FocusDown
                | A::ResizeLeft
                | A::ResizeRight
                | A::ResizeUp
                | A::ResizeDown
                | A::ZoomPane
                | A::EqualizePanes
        ) && self.float_focus.take().is_some()
        {
            self.focus_changed();
        }
        let rows = focused!(self).terminal().rows() as i64;
        let scroll = |a: &mut Active, delta: i64| {
            focused!(a).terminal().scroll_display(delta);
            a.window.request_redraw();
        };
        match action {
            Action::NextProfile => self.pending = Some(Request::NextProfile),
            Action::ReloadConfig => self.pending = Some(Request::Reload),
            Action::NewWindow => self.pending = Some(Request::NewWindow),
            Action::CloseWindow => self.pending = Some(Request::Quit),
            Action::ToggleMenuBar => self.pending = Some(Request::ToggleMenuBar),
            Action::ZoomIn => self.pending = Some(Request::Zoom(1)),
            Action::ZoomOut => self.pending = Some(Request::Zoom(-1)),
            Action::ZoomReset => self.pending = Some(Request::Zoom(0)),
            Action::OpenConfigFolder => self.pending = Some(Request::OpenConfigFolder),
            Action::OpenFileManager => {
                let cwd = focused!(self).terminal().cwd().map(std::path::PathBuf::from);
                let dir = cwd
                    .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
                    .unwrap_or_else(|| "/".into());
                open_path(&dir);
            }
            Action::SelectAll => {
                let mut t = focused!(self).terminal();
                let history = t.scrollback().len() as i64;
                let (rows, cols) = (t.rows() as i64, t.cols());
                t.start_selection(
                    seraphim_terminal::SelectionKind::Simple,
                    Point::new(-history, 0),
                );
                t.update_selection(Point::new(rows - 1, cols.saturating_sub(1)));
                drop(t);
                self.copy_selection(Board::Primary);
                self.window.request_redraw();
            }
            Action::ClearScrollback => {
                focused!(self).terminal().advance(b"\x1b[3J");
                self.window.request_redraw();
            }
            Action::ClearAndReset => {
                // Konsole's "Clear Scrollback and Reset": RIS, then drop
                // the history.
                focused!(self).terminal().advance(b"\x1bc\x1b[3J");
                self.window.request_redraw();
            }
            Action::Fullscreen => {
                let full = self.window.fullscreen().is_some();
                self.window
                    .set_fullscreen((!full).then_some(winit::window::Fullscreen::Borderless(None)));
            }
            Action::Shortcuts => {
                let lines = actions::SHORTCUT_HELP
                    .iter()
                    .map(|a| format!("{:<28}{}", a.description(), a.shortcut_label()))
                    .collect();
                self.menu.panel = Some(menu::Panel { title: "Keyboard Shortcuts".into(), lines });
                self.window.request_redraw();
            }
            Action::About => {
                self.menu.panel = Some(menu::Panel {
                    title: "About Seraphim".into(),
                    lines: vec![
                        format!("Seraphim {}", env!("CARGO_PKG_VERSION")),
                        "A programmable terminal for Divinity OS.".into(),
                        String::new(),
                        "Press any key to close.".into(),
                    ],
                });
                self.window.request_redraw();
            }
            Action::OpenMenu => self.toggle_menu(),
            Action::Copy => self.copy_selection(Board::Clipboard),
            Action::Paste => self.paste(Board::Clipboard),
            Action::ScrollPageUp => scroll(self, rows - 1),
            Action::ScrollPageDown => scroll(self, -(rows - 1)),
            Action::ScrollLineUp => scroll(self, 1),
            Action::ScrollLineDown => scroll(self, -1),
            Action::ScrollTop => scroll(self, i64::MAX / 2),
            Action::ScrollBottom => scroll(self, -(i64::MAX / 2)),
            Action::NewTab => self.new_tab(),
            Action::CloseTab => {
                if self.close_tab(self.ws.active) {
                    self.pending = Some(Request::Quit);
                }
            }
            Action::NextTab => self.switch_tab(|w| w.cycle(1)),
            Action::PrevTab => self.switch_tab(|w| w.cycle(-1)),
            Action::MoveTabLeft => self.switch_tab(|w| w.move_tab(-1)),
            Action::MoveTabRight => self.switch_tab(|w| w.move_tab(1)),
            Action::GotoTab(n) => {
                let last = self.ws.tabs.len().saturating_sub(1);
                let i = if n >= 9 { last } else { (n as usize).saturating_sub(1).min(last) };
                self.switch_tab(|w| {
                    w.goto(i);
                });
            }
            Action::RenameTab => {
                let current = self.ws.tab().and_then(|t| t.label.clone()).unwrap_or_default();
                self.renaming = Some(current);
                self.apply_layout();
            }
            Action::DetachTab => self.pending = Some(Request::DetachTab(None)),
            Action::FloatPanel => self.toggle_float(),
            Action::SplitRight => self.split(Split::Right),
            Action::SplitDown => self.split(Split::Down),
            Action::ClosePane => {
                if self.close_pane(self.focus_id()) {
                    self.pending = Some(Request::Quit);
                }
            }
            Action::FocusLeft | Action::FocusRight | Action::FocusUp | Action::FocusDown => {
                let dir = match action {
                    Action::FocusLeft => Direction::Left,
                    Action::FocusRight => Direction::Right,
                    Action::FocusUp => Direction::Up,
                    _ => Direction::Down,
                };
                let (area, gap) = (self.pane_area(), self.gap());
                if self.ws.focus_toward(dir, area, gap) {
                    self.focus_changed();
                }
            }
            Action::ResizeLeft | Action::ResizeRight | Action::ResizeUp | Action::ResizeDown => {
                let dir = match action {
                    Action::ResizeLeft => Direction::Left,
                    Action::ResizeRight => Direction::Right,
                    Action::ResizeUp => Direction::Up,
                    _ => Direction::Down,
                };
                if self.ws.resize(dir, 0.05) {
                    self.apply_layout();
                }
            }
            Action::ZoomPane => {
                self.ws.toggle_zoom();
                self.apply_layout();
            }
            Action::EqualizePanes => {
                self.ws.equalize();
                self.apply_layout();
            }
            Action::OpenPanel(p) => {
                let kind = crate::panels::Kind::parse(p.name()).expect("built-in panel");
                // Settings get a tab of their own; the others sit beside.
                let place = if p == actions::PanelId::Settings { "tab" } else { "right" };
                self.toggle_panel(kind, place);
            }
        }
    }

    /// Pixels at the top for the pinned menu bar.
    fn menu_height(&self) -> u32 {
        if self.menu_pinned {
            self.renderer.cell_metrics().height.ceil() as u32
        } else {
            0
        }
    }

    fn tab_bar_visible(&self) -> bool {
        if self.renaming.is_some() {
            return true;
        }
        match self.tabs_cfg.bar.as_str() {
            "always" => true,
            "never" => false,
            _ => self.ws.tabs.len() > 1,
        }
    }

    fn tab_bar_height(&self) -> u32 {
        if self.tab_bar_visible() {
            self.overlay.cell_metrics().height.ceil() as u32
        } else {
            0
        }
    }

    /// Which row holds Seraphim's window buttons: the pinned menu bar, the
    /// tab bar at the top, or a title bar of its own. `None` with system
    /// (or no) decorations.
    fn buttons_row(&self) -> Option<ButtonsRow> {
        if self.decorations != "seraphim" {
            return None;
        }
        Some(if self.menu_pinned {
            ButtonsRow::Menu
        } else if self.tab_bar_visible() && !self.tabs_at_bottom() {
            ButtonsRow::Tabs
        } else {
            ButtonsRow::Title
        })
    }

    fn title_bar_height(&self) -> u32 {
        match self.buttons_row() {
            Some(ButtonsRow::Title) => self.overlay.cell_metrics().height.ceil() as u32,
            _ => 0,
        }
    }

    /// The top and height of the row with the window buttons.
    fn buttons_row_span(&self) -> Option<(ButtonsRow, f64, f64)> {
        let row = self.buttons_row()?;
        let (top, h) = match row {
            ButtonsRow::Menu => (0, self.menu_height()),
            ButtonsRow::Tabs => (self.tab_bar_top(), self.tab_bar_height()),
            ButtonsRow::Title => (0, self.title_bar_height()),
        };
        Some((row, top as f64, h as f64))
    }

    fn decor_colors(&self) -> crate::decor::Colors {
        let theme = self.renderer.theme();
        let p = menu::Palette::from_theme(theme);
        let d = crate::tabbar::Design::new(&self.tabs_cfg, &p);
        crate::decor::Colors {
            bar: d.bar,
            fg: p.bar_fg,
            dim: p.dim,
            hover: p.border,
            alert: theme.ansi[1],
        }
    }

    fn tabs_at_bottom(&self) -> bool {
        self.tabs_cfg.position == "bottom"
    }

    /// Where the tab bar starts (pixels from the top).
    fn tab_bar_top(&self) -> u32 {
        if self.tabs_at_bottom() {
            self.window.inner_size().height.saturating_sub(self.tab_bar_height())
        } else {
            self.menu_height()
        }
    }

    /// Where panes go: below the menu bar, and above or below the tab bar.
    fn pane_area(&self) -> Rect {
        let s = self.window.inner_size();
        let tabs = self.tab_bar_height();
        let top = self.title_bar_height()
            + self.menu_height()
            + if self.tabs_at_bottom() { 0 } else { tabs };
        let top = top.min(s.height.saturating_sub(1));
        let h = s.height.saturating_sub(top + if self.tabs_at_bottom() { tabs } else { 0 }).max(1);
        Rect::new(0.0, top as f32, s.width as f32, h as f32)
    }

    /// Divider width in pixels.
    fn gap(&self) -> f32 {
        (self.panes_cfg.divider * self.scale as f32).round().max(1.0)
    }

    /// The pane with the keyboard: a floating panel, or the tab's focus.
    fn focus_id(&self) -> u64 {
        self.float_focus
            .filter(|f| self.floats.iter().any(|x| x.id == *f))
            .unwrap_or_else(|| self.ws.focus().unwrap_or(0))
    }

    fn float_rect(&self, id: u64) -> Option<Rect> {
        self.floats.iter().find(|f| f.id == id).map(|f| f.rect)
    }

    /// The topmost floating panel under a point.
    fn float_at(&self, x: f32, y: f32) -> Option<u64> {
        self.floats.iter().rev().find(|f| f.rect.contains(x, y)).map(|f| f.id)
    }

    fn raise_float(&mut self, id: u64) {
        if let Some(i) = self.floats.iter().position(|f| f.id == id) {
            let f = self.floats.remove(i);
            self.floats.push(f);
        }
    }

    /// Show panel `id` floating (it must not be in a tab), focused, in the
    /// middle of the panes (cascaded when there are others).
    fn add_float(&mut self, id: u64) {
        let area = self.pane_area();
        let m = self.renderer.cell_metrics();
        let n = self.floats.len() as f32;
        let w = ((area.w * 0.45).max(m.width * 40.0).min(area.w) / m.width).floor() * m.width;
        let h = ((area.h * 0.6).max(m.height * 10.0).min(area.h) / m.height).floor() * m.height;
        let step = m.height * 1.5 * (n % 5.0);
        let x = (area.x + (area.w - w) / 2.0 + step).round();
        let y = (area.y + (area.h - h) / 3.0 + step).round();
        self.floats.push(Float { id, rect: Rect::new(x, y, w, h) });
        self.clamp_floats();
        self.float_focus = Some(id);
        self.focus_changed();
        self.window.request_redraw();
    }

    /// Keep floating panels inside the window, at least a few cells big
    /// and whole cells in size.
    fn clamp_floats(&mut self) {
        let area = self.pane_area();
        let m = self.renderer.cell_metrics();
        for f in &mut self.floats {
            let r = &mut f.rect;
            r.w = ((r.w.min(area.w).max(m.width * 16.0)) / m.width).floor() * m.width;
            r.h = ((r.h.min(area.h).max(m.height * 4.0)) / m.height).floor() * m.height;
            r.x = r.x.min(area.x + area.w - r.w).max(area.x);
            r.y = r.y.min(area.y + area.h - r.h).max(area.y);
        }
    }

    /// View → Float Panel: a panel in a tab floats; a floating one goes
    /// back into the tab, beside the terminal.
    fn toggle_float(&mut self) {
        let id = self.focus_id();
        if !self.panels.contains_key(&id) {
            self.notify("Float Panel works on panels (focus one first)");
            return;
        }
        if self.float_rect(id).is_some() {
            self.floats.retain(|f| f.id != id);
            self.float_focus = None;
            self.ws.split_with(id, Split::Right, 0.62);
            self.renderer.drop_slot(frame_slot(id));
            self.apply_layout();
            self.focus_changed();
            return;
        }
        // Leave the tab; never the last terminal's.
        if self.ws.tab().is_some_and(|t| t.root.panes().len() == 1) && self.ws.tabs.len() == 1 {
            self.notify("The window needs its terminal: open the panel beside one first");
            return;
        }
        let closed = self.ws.close(id);
        if closed == Closed::Last {
            return;
        }
        self.fix_term_focus();
        self.apply_layout();
        self.add_float(id);
    }

    /// Start a shell (or `command`) in a new pane; it isn't placed yet.
    fn spawn_pane(
        &mut self,
        cwd: Option<std::path::PathBuf>,
        command: Option<Command>,
    ) -> Result<u64, String> {
        let id = next_pane_id();
        let area = self.pane_area();
        let (cols, rows) = self.renderer.grid_size(area.w as u32, area.h as u32, self.pad);
        let mut command = command.unwrap_or_else(|| self.spawn.command.clone());
        if self.spawn.term != "auto" && !self.spawn.term.is_empty() {
            command = command.env("TERM", self.spawn.term.clone());
        }
        if let Some(d) = cwd.filter(|d| d.is_dir()) {
            command = command.cwd(d);
        }
        let m = self.renderer.cell_metrics();
        let session = Session::spawn(
            id,
            SessionConfig {
                command,
                cols,
                rows,
                notify: Some(self.notifier.clone()),
                color_scheme: Some(self.renderer.theme().scheme()),
                scrollback: self.spawn.scrollback,
                grapheme_clustering: self.spawn.grapheme_clustering,
                cell_pixels: Some((m.width, m.height)),
            },
            &self.bus,
        )
        .map_err(|e| format!("cannot start shell: {e}"))?;
        self.panes.insert(
            id,
            Pane {
                session,
                last_generation: u64::MAX,
                activity: false,
                bell: false,
                progress: None,
                root: false,
                rect: Rect::default(),
            },
        );
        Ok(id)
    }

    /// The focused pane's working directory (OSC 7, else its process's).
    fn focused_cwd(&self) -> Option<std::path::PathBuf> {
        let p = self.panes.get(&self.focus_id())?;
        let osc7 = p.session.terminal().cwd().map(std::path::PathBuf::from);
        osc7.or_else(|| p.session.foreground_process().and_then(|i| i.cwd))
    }

    /// Size every visible pane's terminal to its rectangle.
    fn apply_layout(&mut self) {
        self.clamp_floats();
        let (rects, _) = self.ws.layout(self.pane_area(), self.gap());
        for (id, r) in rects {
            let Some(p) = self.panes.get_mut(&id) else { continue };
            p.rect = r;
            let (cols, rows) =
                self.renderer.grid_size(r.w.max(1.0) as u32, r.h.max(1.0) as u32, self.pad);
            let same = {
                let t = p.session.terminal();
                (t.cols(), t.rows()) == (cols as usize, rows as usize)
            };
            if !same {
                let _ = p.session.resize(cols, rows);
            }
        }
        self.window.request_redraw();
    }

    /// After anything that may move the focus: tell the panes, follow the
    /// new session, switch to the tab's profile.
    fn focus_changed(&mut self) {
        let id = self.focus_id();
        if id == self.last_focus {
            return;
        }
        if self.panels.contains_key(&id) {
            // A panel has the keyboard; typing targets stay with the
            // terminal that had focus.
            self.last_focus = id;
            self.check_tab_profile();
            self.window.request_redraw();
            return;
        }
        if !self.panes.contains_key(&id) {
            return;
        }
        let previous = self.term_focus;
        self.term_focus = id;
        if self.focused && previous != id {
            if let Some(old) = self.panes.get(&previous) {
                let bytes = encode_focus(false, old.session.terminal().modes());
                let _ = old.session.write(&bytes);
            }
            let new = &self.panes[&id];
            let bytes = encode_focus(true, new.session.terminal().modes());
            let _ = new.session.write(&bytes);
        }
        self.last_focus = id;
        self.interactions.set_session(id, &mut self.visual);
        self.bindings.set_session(id, &mut self.visual);
        self.scripts.set_session(id);
        if let Some(t) = self.ws.tab() {
            for p in t.root.panes() {
                if let Some(p) = self.panes.get_mut(&p) {
                    p.activity = false;
                    p.bell = false;
                }
            }
        }
        self.title.clear();
        self.mouse.pending = None;
        self.bus.publisher("interface").publish(
            "interface.pane.focused",
            Some(id),
            seraphim_events::json!({ "pane": id, "tab": self.ws.active }),
        );
        self.check_tab_profile();
        self.window.request_redraw();
    }

    /// Switch to the active tab's profile if it has its own.
    fn check_tab_profile(&mut self) {
        let want =
            self.ws.tab().and_then(|t| t.profile.clone()).unwrap_or(self.base_profile.clone());
        if want != self.menu_src.2 {
            self.pending = Some(Request::TabProfile(want));
        }
    }

    /// Open a panel (`place`: right, down or tab), or close it if one of
    /// that kind is open.
    fn toggle_panel(&mut self, kind: crate::panels::Kind, place: &str) {
        if let Some((&id, _)) = self.panels.iter().find(|(_, p)| p.kind == kind) {
            if self.close_pane(id) {
                self.pending = Some(Request::Quit);
            }
            return;
        }
        self.open_panel(kind, place);
    }

    fn open_panel_in_tab(&mut self, id: u64, place: &str) {
        match place {
            "tab" => {
                self.ws.add_tab(Node::Pane(id), None);
            }
            "down" => {
                self.ws.split_with(id, Split::Down, 0.65);
            }
            _ => {
                self.ws.split_with(id, Split::Right, 0.62);
            }
        }
        self.apply_layout();
        self.focus_changed();
    }

    fn open_panel(&mut self, kind: crate::panels::Kind, place: &str) -> u64 {
        let id = next_pane_id();
        if let crate::panels::Kind::Script { ext, id: pid } = &kind {
            self.scripts.panel_event(ext, pid, seraphim_script::PanelEvent::Open(true));
        }
        self.panels.insert(id, crate::panels::PanelState::new(kind));
        match place {
            "float" => {
                self.add_float(id);
                return id;
            }
            _ => {
                // A panel in the tab takes the keyboard.
                self.float_focus = None;
                self.open_panel_in_tab(id, place);
            }
        }
        id
    }

    /// Spawn the panes of a layout tree; returns its workspace node.
    fn build_layout(&mut self, p: &seraphim_config::LayoutPane) -> Result<Node, String> {
        if let (Some(split), Some(a), Some(b)) = (&p.split, &p.first, &p.second) {
            let first = self.build_layout(a)?;
            let second = self.build_layout(b)?;
            let split = if split == "down" { Split::Down } else { Split::Right };
            return Ok(Node::Split {
                split,
                ratio: p.ratio,
                first: Box::new(first),
                second: Box::new(second),
            });
        }
        if let Some(name) = &p.panel {
            let kind =
                crate::panels::Kind::parse(name).ok_or_else(|| format!("no panel {name:?}"))?;
            let id = next_pane_id();
            if let crate::panels::Kind::Script { ext, id: pid } = &kind {
                self.scripts.panel_event(ext, pid, seraphim_script::PanelEvent::Open(true));
            }
            self.panels.insert(id, crate::panels::PanelState::new(kind));
            return Ok(Node::Pane(id));
        }
        let cwd = p.cwd.as_deref().map(|c| match c.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join(rest),
            None => std::path::PathBuf::from(c),
        });
        let command = p.command.as_ref().map(|c| {
            let shell = seraphim_pty::default_shell_path();
            Command::new(shell).args(["-c", c.as_str()])
        });
        let cwd = cwd.or_else(|| self.focused_cwd());
        Ok(Node::Pane(self.spawn_pane(cwd, command)?))
    }

    /// Everything restoration needs, from the current tabs.
    fn saved_session(&self) -> crate::restore::Saved {
        use crate::restore::{SavedPane, SavedTab, LINES};
        let tabs = self
            .ws
            .tabs
            .iter()
            .map(|t| {
                let mut panes = std::collections::BTreeMap::new();
                for id in t.root.panes() {
                    if let Some(p) = self.panes.get(&id) {
                        let (cwd, text) = {
                            let term = p.session.terminal();
                            let mut lines: Vec<String> =
                                term.scrollback().iter().map(|r| r.text()).collect();
                            lines.extend(term.screen().rows_iter().map(|r| r.text()));
                            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                                lines.pop();
                            }
                            let skip = lines.len().saturating_sub(LINES);
                            (term.cwd().map(str::to_owned), lines.split_off(skip))
                        };
                        let cwd = cwd.or_else(|| {
                            p.session
                                .foreground_process()
                                .and_then(|i| i.cwd)
                                .map(|c| c.display().to_string())
                        });
                        panes.insert(id, SavedPane::Terminal { cwd, text });
                    } else if let Some(p) = self.panels.get(&id) {
                        panes.insert(id, SavedPane::Panel { panel: p.kind.name() });
                    }
                }
                SavedTab {
                    root: t.root.clone(),
                    focus: t.focus,
                    label: t.label.clone(),
                    profile: t.profile.clone(),
                    panes,
                }
            })
            .collect();
        let area = self.pane_area();
        let floats = self
            .floats
            .iter()
            .filter_map(|f| {
                let p = self.panels.get(&f.id)?;
                Some(crate::restore::SavedFloat {
                    panel: p.kind.name(),
                    x: (f.rect.x - area.x) / area.w.max(1.0),
                    y: (f.rect.y - area.y) / area.h.max(1.0),
                    w: f.rect.w / area.w.max(1.0),
                    h: f.rect.h / area.h.max(1.0),
                })
            })
            .collect();
        crate::restore::Saved { version: 1, active: self.ws.active, tabs, floats }
    }

    /// Rebuild saved tabs. Returns false if nothing could be restored.
    fn restore(&mut self, saved: crate::restore::Saved) -> bool {
        use crate::restore::SavedPane;
        let when = {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()) as libc::time_t;
            // SAFETY: localtime_r writes into the zeroed struct we own.
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&secs, &mut tm) };
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min
            )
        };
        let mut any = false;
        for t in saved.tabs {
            // New ids for the saved ones.
            let mut map = std::collections::HashMap::new();
            for (old, p) in &t.panes {
                let new = match p {
                    SavedPane::Terminal { cwd, text } => {
                        match self.spawn_pane(cwd.as_ref().map(std::path::PathBuf::from), None) {
                            Ok(id) => {
                                if !text.is_empty() {
                                    let bytes = crate::restore::replay(text, &when);
                                    self.panes[&id].session.terminal().advance(&bytes);
                                }
                                id
                            }
                            Err(_) => continue,
                        }
                    }
                    SavedPane::Panel { panel } => {
                        let Some(kind) = crate::panels::Kind::parse(panel) else { continue };
                        let id = next_pane_id();
                        self.panels.insert(id, crate::panels::PanelState::new(kind));
                        id
                    }
                };
                map.insert(*old, new);
            }
            fn remap(n: &Node, map: &std::collections::HashMap<u64, u64>) -> Option<Node> {
                match n {
                    Node::Pane(p) => map.get(p).map(|q| Node::Pane(*q)),
                    Node::Split { split, ratio, first, second } => {
                        match (remap(first, map), remap(second, map)) {
                            (Some(a), Some(b)) => Some(Node::Split {
                                split: *split,
                                ratio: *ratio,
                                first: Box::new(a),
                                second: Box::new(b),
                            }),
                            (a, b) => a.or(b),
                        }
                    }
                }
            }
            let Some(root) = remap(&t.root, &map) else { continue };
            let i = self.ws.add_tab(root, t.profile.clone());
            self.ws.tabs[i].label = t.label.clone();
            if let Some(f) = map.get(&t.focus) {
                self.ws.tabs[i].focus = *f;
            }
            any = true;
        }
        if any {
            self.ws.active = saved.active.min(self.ws.tabs.len() - 1);
            let area = self.pane_area();
            for f in &saved.floats {
                let Some(kind) = crate::panels::Kind::parse(&f.panel) else { continue };
                let id = next_pane_id();
                self.panels.insert(id, crate::panels::PanelState::new(kind));
                let rect = Rect::new(
                    area.x + f.x * area.w,
                    area.y + f.y * area.h,
                    f.w * area.w,
                    f.h * area.h,
                );
                self.floats.push(Float { id, rect });
            }
            self.clamp_floats();
        }
        any && !self.panes.is_empty()
    }

    /// Open a `[[layout]]`'s tabs after the current ones.
    fn open_layout(&mut self, name: &str) {
        let Some(layout) = self.layouts.iter().find(|l| l.name == name).cloned() else {
            return self.notify(format!("No layout {name:?}"));
        };
        let mut first = None;
        for t in &layout.tab {
            match self.build_layout(&t.pane) {
                Ok(root) => {
                    let i = self.ws.add_tab(root, t.profile.clone());
                    self.ws.tabs[i].label = t.label.clone();
                    first.get_or_insert(i);
                }
                Err(e) => self.notify(format!("Layout {name}: {e}")),
            }
        }
        if let Some(i) = first {
            self.ws.active = i;
        }
        self.apply_layout();
        self.focus_changed();
    }

    fn new_tab(&mut self) {
        let cwd = self.focused_cwd();
        match self.spawn_pane(cwd, None) {
            Ok(id) => {
                let before = self.ws.active;
                self.ws.add_tab(Node::Pane(id), None);
                self.bus.publisher("interface").publish(
                    "interface.tab.changed",
                    Some(id),
                    seraphim_events::json!({ "tab": self.ws.active, "from": before, "tabs": self.ws.tabs.len() }),
                );
                self.apply_layout();
                self.focus_changed();
            }
            Err(e) => self.notify(e),
        }
    }

    fn split(&mut self, how: Split) {
        let cwd = self.focused_cwd();
        match self.spawn_pane(cwd, None) {
            Ok(id) => {
                self.ws.split(id, how);
                self.apply_layout();
                self.focus_changed();
            }
            Err(e) => self.notify(e),
        }
    }

    /// Close a pane (hanging up its shell). Returns true when it was the
    /// window's last.
    fn close_pane(&mut self, id: u64) -> bool {
        if self.float_rect(id).is_some() {
            self.floats.retain(|f| f.id != id);
            self.renderer.drop_slot(frame_slot(id));
            if self.float_focus == Some(id) {
                self.float_focus = None;
            }
        }
        let closed = self.ws.close(id);
        if let Some(p) = self.panels.remove(&id) {
            if let crate::panels::Kind::Script { ext, id: pid } = &p.kind {
                self.scripts.panel_event(ext, pid, seraphim_script::PanelEvent::Open(false));
            }
        }
        self.panes.remove(&id);
        self.renderer.drop_slot(id);
        if closed == Closed::Last || self.panes.is_empty() {
            // Panels alone don't keep a window open.
            return true;
        }
        self.fix_term_focus();
        self.apply_layout();
        self.focus_changed();
        false
    }

    /// Keep `term_focus` on a live terminal: the focused pane if it is
    /// one, else one in the active tab, else any.
    fn fix_term_focus(&mut self) {
        if self.panes.contains_key(&self.term_focus) {
            return;
        }
        let focus = self.focus_id();
        let in_tab = self.ws.tab().map(|t| t.root.panes()).unwrap_or_default();
        self.term_focus = if self.panes.contains_key(&focus) {
            focus
        } else {
            in_tab
                .into_iter()
                .chain(self.ws.panes())
                .find(|p| self.panes.contains_key(p))
                .unwrap_or_else(|| *self.panes.keys().next().unwrap_or(&0))
        };
    }

    /// Take tab `i` out of this window, sessions running, for another
    /// window. The window keeps at least one other tab.
    fn detach_tab(
        &mut self,
        i: usize,
        position: Option<winit::dpi::PhysicalPosition<i32>>,
    ) -> Detached {
        let tab = self.ws.tabs[i].clone();
        let mut panes = Vec::new();
        let mut panels = Vec::new();
        for id in self.ws.close_tab(i) {
            if let Some(p) = self.panes.remove(&id) {
                panes.push((id, p));
            }
            if let Some(p) = self.panels.remove(&id) {
                if let crate::panels::Kind::Script { ext, id: pid } = &p.kind {
                    self.scripts.panel_event(ext, pid, seraphim_script::PanelEvent::Open(false));
                }
                panels.push((id, p));
            }
            self.renderer.drop_slot(id);
        }
        for (_, p) in &mut panes {
            p.last_generation = u64::MAX;
            p.activity = false;
        }
        self.fix_term_focus();
        self.apply_layout();
        self.focus_changed();
        Detached { tab, panes, panels, size: self.window.inner_size(), position }
    }

    fn close_tab(&mut self, i: usize) -> bool {
        for id in self.ws.close_tab(i) {
            if let Some(p) = self.panels.remove(&id) {
                if let crate::panels::Kind::Script { ext, id: pid } = &p.kind {
                    self.scripts.panel_event(ext, pid, seraphim_script::PanelEvent::Open(false));
                }
            }
            self.panes.remove(&id);
            self.renderer.drop_slot(id);
        }
        if self.ws.tabs.is_empty() || self.panes.is_empty() {
            return true;
        }
        self.fix_term_focus();
        self.apply_layout();
        self.focus_changed();
        false
    }

    fn switch_tab(&mut self, f: impl FnOnce(&mut Workspace)) {
        // The keyboard goes to the tab (floating panels stay up).
        self.float_focus = None;
        let before = self.ws.active;
        f(&mut self.ws);
        if self.ws.active != before {
            self.bus.publisher("interface").publish(
                "interface.tab.changed",
                Some(self.focus_id()),
                seraphim_events::json!({ "tab": self.ws.active, "from": before, "tabs": self.ws.tabs.len() }),
            );
        }
        self.apply_layout();
        self.focus_changed();
    }

    fn in_tab_bar(&self, y: f64) -> bool {
        if !self.tab_bar_visible() {
            return false;
        }
        let top = self.tab_bar_top() as f64;
        y >= top && y < top + self.tab_bar_height() as f64
    }

    fn tab_bar_click(&mut self, button: WButton) {
        use crate::tabbar::Hit;
        let col = (self.mouse.pos.0 / self.overlay.cell_metrics().width as f64).max(0.0) as usize;
        match (self.tab_hits.at(col), button) {
            (Hit::Tab(i), WButton::Left) => {
                self.tab_drag = Some(i);
                self.switch_tab(|w| w.active = i);
            }
            (Hit::Tab(i) | Hit::Close(i), WButton::Middle) | (Hit::Close(i), WButton::Left) => {
                if self.close_tab(i) {
                    self.pending = Some(Request::Quit);
                }
            }
            (Hit::NewTab, _) | (Hit::Bar, WButton::Middle) => self.new_tab(),
            // With Seraphim's decorations the tab bar is the title bar.
            (Hit::Bar, WButton::Left) if self.decorations == "seraphim" => self.title_press(),
            _ => {}
        }
        self.window.request_redraw();
    }

    /// Facts panels show, gathered from the panes and tabs.
    fn session_rows(&self) -> Vec<crate::panels::SessionRow> {
        let mut rows = Vec::new();
        for (ti, t) in self.ws.tabs.iter().enumerate() {
            for id in t.root.panes() {
                let Some(p) = self.panes.get(&id) else { continue };
                let fg = p.session.foreground_process();
                let cwd = p
                    .session
                    .terminal()
                    .cwd()
                    .map(str::to_owned)
                    .or_else(|| {
                        fg.as_ref().and_then(|f| f.cwd.as_ref()).map(|c| c.display().to_string())
                    })
                    .unwrap_or_default();
                rows.push(crate::panels::SessionRow {
                    pane: id,
                    tab: ti,
                    label: self.tab_label(ti),
                    program: fg.map(|f| f.name).unwrap_or_default(),
                    cwd,
                    root: p.root,
                    focused: id == self.term_focus,
                });
            }
        }
        rows
    }

    /// Paint every visible panel into its renderer slot.
    fn paint_panels(&mut self, rects: &[(u64, Rect)], pad: f32) {
        let visible: Vec<(u64, Rect)> =
            rects.iter().filter(|(id, _)| self.panels.contains_key(id)).cloned().collect();
        if visible.is_empty() {
            return;
        }
        let sessions = self.session_rows();
        let settings: Vec<(crate::settings::Setting, String)> = match (
            visible.iter().any(|(id, _)| self.panels[id].kind == crate::panels::Kind::Settings),
            &self.config_path,
        ) {
            (true, Some(path)) => {
                let dir = path.parent().unwrap_or(std::path::Path::new("."));
                let all = crate::settings::all(dir, &self.extension_list);
                let values = crate::settings::values(path, &all);
                all.into_iter().zip(values).collect()
            }
            _ => Vec::new(),
        };
        let theme = self.renderer.theme().clone();
        let palette = menu::Palette::from_theme(&theme);
        let colors = crate::panel::Colors::from_theme(&theme);
        let focus = self.focus_id();
        for (id, r) in visible {
            let script = match &self.panels[&id].kind {
                crate::panels::Kind::Script { ext, id } => self.scripts.panel(ext, id),
                _ => None,
            };
            let ctx = crate::panels::Ctx {
                sessions: &sessions,
                history: &self.history,
                packages: &self.packages,
                settings: &settings,
                script,
            };
            let state = self.panels.get_mut(&id).expect("visible panel");
            let title = state.title(&ctx);
            let widgets = state.widgets(&ctx);
            let (cols, rows) =
                self.renderer.grid_size(r.w.max(1.0) as u32, r.h.max(1.0) as u32, pad);
            let (cols, rows) = (cols as usize, rows as usize);
            let painted = crate::panel::paint(
                &title,
                &widgets,
                cols,
                rows,
                &palette,
                colors,
                state.selected,
                self.focused && id == focus,
            );
            state.items = painted.items;
            state.rows = painted.rows.clone();
            if let Some(sel) = state.selected {
                if painted.items > 0 && sel >= painted.items {
                    state.selected = Some(painted.items - 1);
                }
            }
            let mut t = seraphim_terminal::Terminal::with_scrollback(cols, rows, 0);
            t.set_color_scheme(theme.scheme());
            t.advance(&painted.bytes);
            if self.float_rect(id).is_some() {
                // A close button in the title, a grip in the corner.
                let on = self.focused && id == focus;
                let (fg, bg) = if on {
                    (palette.hi_fg, palette.hi_bg)
                } else {
                    (palette.bar_fg, palette.bar_bg)
                };
                let c = |c: [u8; 3]| format!("{};{};{}", c[0], c[1], c[2]);
                t.advance(
                    format!(
                        "\x1b[1;{}H\x1b[0;38;2;{};48;2;{}m×\x1b[{};{}H\x1b[0;38;2;{};48;2;{}m◢\x1b[0m",
                        cols.saturating_sub(1).max(1),
                        c(fg),
                        c(bg),
                        rows,
                        cols,
                        c(palette.dim),
                        c(palette.menu_bg),
                    )
                    .as_bytes(),
                );
            }
            let view = View {
                width: r.w.max(1.0) as u32,
                height: r.h.max(1.0) as u32,
                padding: pad,
                focused: false,
                cursor_on: false,
            };
            self.renderer.select(&self.gpu.device, id);
            self.renderer.prepare(&self.gpu.device, &self.gpu.queue, &t, view);
        }
    }

    /// Carry out what a panel asked for.
    fn panel_act(&mut self, id: u64, act: crate::panels::Act) {
        use crate::panels::Act;
        match act {
            Act::None => return,
            Act::Redraw => {}
            Act::FocusPane(p) => {
                self.ws.set_focus(p);
                self.apply_layout();
                self.focus_changed();
            }
            Act::Type(text) => {
                let bytes = encode_paste(&text, focused!(self).terminal().modes());
                self.write(&bytes);
            }
            Act::Setting(i, dir) => self.change_setting(i, dir),
            Act::Script(ev) => {
                if let Some(crate::panels::Kind::Script { ext, id: pid }) =
                    self.panels.get(&id).map(|p| p.kind.clone())
                {
                    let event = match (ev["select"].as_i64(), ev["key"].as_str()) {
                        (Some(n), _) => seraphim_script::PanelEvent::Select(n),
                        (None, Some(k)) => seraphim_script::PanelEvent::Key {
                            key: k.to_owned(),
                            selected: ev["selected"].as_i64(),
                        },
                        _ => return,
                    };
                    self.scripts.panel_event(&ext, &pid, event);
                }
            }
            Act::Close => {
                if self.close_pane(id) {
                    self.pending = Some(Request::Quit);
                }
                return;
            }
        }
        self.window.request_redraw();
    }

    /// Settings panel: write one setting to the configuration file, then
    /// apply it the way live reload does.
    fn change_setting(&mut self, i: usize, dir: i32) {
        let Some(path) = self.config_path.clone() else {
            return self.notify("No configuration file to change");
        };
        let d = path.parent().unwrap_or(std::path::Path::new("."));
        let all = crate::settings::all(d, &self.extension_list);
        let Some(st) = all.get(i) else { return };
        let current = crate::settings::values(&path, std::slice::from_ref(st)).remove(0);
        let value = crate::settings::step(st, &current, dir);
        match crate::settings::set(&path, &st.key, value) {
            // With live reload on, the file watcher applies it.
            Ok(()) if self.watcher.is_some() => {}
            Ok(()) => self.pending = Some(Request::Files(vec![path])),
            Err(e) => self.notify(e),
        }
    }

    /// The pane under a pixel position.
    fn pane_at(&self, x: f32, y: f32) -> Option<u64> {
        self.float_at(x, y)
            .or_else(|| self.drawn.iter().find(|(_, r)| r.contains(x, y)).map(|(id, _)| *id))
    }

    /// Where a pane was drawn: floating, or in the tab.
    fn rect_of(&self, id: u64) -> Rect {
        self.float_rect(id)
            .or_else(|| self.drawn.iter().find(|(p, _)| *p == id).map(|(_, r)| *r))
            .unwrap_or_default()
    }

    /// The divider under a pixel position (a little slack to grab it).
    fn divider_at(&self, x: f32, y: f32) -> Option<Divider> {
        if self.float_at(x, y).is_some() {
            return None;
        }
        let (_, divs) = self.ws.layout(self.pane_area(), self.gap());
        divs.into_iter().find(|d| {
            let r = d.rect;
            let slack = 3.0;
            x >= r.x - slack && x <= r.x + r.w + slack && y >= r.y - slack && y <= r.y + r.h + slack
        })
    }

    /// Tab label from its focused pane and `[tabs] label`.
    fn tab_label(&self, i: usize) -> String {
        let t = &self.ws.tabs[i];
        if let Some(l) = &t.label {
            return l.clone();
        }
        let Some(p) = self.panes.get(&t.focus) else { return String::new() };
        let program = p.session.foreground_process().map(|i| i.name).unwrap_or_default();
        let (title, cwd) = {
            let term = p.session.terminal();
            (term.title().to_owned(), term.cwd().map(str::to_owned))
        };
        let dir = cwd
            .as_deref()
            .map(|c| {
                std::path::Path::new(c)
                    .file_name()
                    .map_or(c.to_owned(), |n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let title = if title.is_empty() { program.clone() } else { title };
        let label = self
            .tabs_cfg
            .label
            .replace("{title}", &title)
            .replace("{program}", &program)
            .replace("{cwd}", &dir)
            .replace("{index}", &(i + 1).to_string());
        if label.trim().is_empty() {
            "Shell".into()
        } else {
            label
        }
    }

    fn tab_infos(&self) -> Vec<crate::tabbar::TabInfo> {
        (0..self.ws.tabs.len())
            .map(|i| {
                let t = &self.ws.tabs[i];
                let ids = t.root.panes();
                let panes: Vec<&Pane> = ids.iter().filter_map(|id| self.panes.get(id)).collect();
                let program = self
                    .panes
                    .get(&t.focus)
                    .and_then(|p| p.session.foreground_process())
                    .map(|i| i.name)
                    .unwrap_or_default();
                crate::tabbar::TabInfo {
                    label: self.tab_label(i),
                    icon: self.tabs_cfg.icons.then(|| crate::tabbar::icon_for(&program)),
                    active: i == self.ws.active,
                    activity: panes.iter().any(|p| p.activity),
                    bell: panes.iter().any(|p| p.bell),
                    root: panes.iter().any(|p| p.root),
                    progress: panes.iter().find_map(|p| p.progress),
                    panes: ids.len(),
                    zoomed: t.zoomed,
                }
            })
            .collect()
    }

    /// Bells, progress and privilege per pane (for the tab bar).
    fn pump_pane_events(&mut self) -> bool {
        let visible: Vec<u64> = self.ws.tab().map(|t| t.root.panes()).unwrap_or_default();
        let mut changed = false;
        for e in self.pane_events.drain() {
            if e.name == "command.completed" {
                if let Some(cmd) = e.data["command"].as_str().filter(|c| !c.trim().is_empty()) {
                    self.history.retain(|h| h.command != cmd);
                    self.history.push_back(crate::panels::HistoryEntry {
                        command: cmd.to_owned(),
                        exit_code: e.data["exit_code"].as_i64(),
                        duration_ms: e.data["duration_ms"].as_u64().unwrap_or(0),
                        at: e.timestamp,
                    });
                    if self.history.len() > 500 {
                        self.history.pop_front();
                    }
                    changed |= !self.panels.is_empty();
                }
                continue;
            }
            if let Some(what) = e.name.strip_prefix("gabriel.") {
                let d = &e.data;
                let detail = ["version", "progress", "message", "error"]
                    .iter()
                    .filter_map(|k| match &d[*k] {
                        serde_json::Value::Null => None,
                        serde_json::Value::String(s) => Some(s.clone()),
                        v => Some(v.to_string()),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                self.packages.push_back(crate::panels::PackageEvent {
                    at: e.timestamp,
                    what: what.to_owned(),
                    package: d["package"].as_str().unwrap_or("").to_owned(),
                    detail,
                });
                if self.packages.len() > 500 {
                    self.packages.pop_front();
                }
                changed |= !self.panels.is_empty();
                continue;
            }
            let Some(p) = e.session.and_then(|id| self.panes.get_mut(&id)) else { continue };
            let id = e.session.unwrap_or(0);
            match e.name.as_str() {
                "terminal.bell" if !visible.contains(&id) => p.bell = true,
                "terminal.progress" => {
                    let n = |k: &str| e.data[k].as_u64().unwrap_or(0).min(255) as u8;
                    p.progress = crate::tabbar::Progress::parse(n("state"), n("percent"));
                }
                "session.privileged" => p.root = e.data["root"].as_bool().unwrap_or(false),
                _ => continue,
            }
            changed = true;
        }
        changed
    }

    fn bar_visible(&self) -> bool {
        self.menu_pinned || self.menu.engaged()
    }

    /// The overlay (menu bar, panels, notices) has something to draw.
    fn overlay_visible(&self) -> bool {
        self.bar_visible() || self.toast.is_some()
    }

    /// Show a notice for a few seconds (and log it).
    fn notify(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        eprintln!("seraphim: {msg}");
        // One line in the window; the log has the full text.
        self.toast = Some((short_notice(&msg), Instant::now() + TOAST_TIME));
        self.window.request_redraw();
    }

    /// Fresh menus for the current state (profile list, check marks).
    fn rebuild_menu(&mut self) {
        let (menus, path, profile) = &self.menu_src;
        let profiles = path
            .as_deref()
            .map(seraphim_config::Config::profiles)
            .unwrap_or_else(|| vec!["default".into()]);
        let ctx = menu::Context {
            profiles,
            profile: profile.clone(),
            menu_bar: self.menu_pinned,
            fullscreen: self.window.fullscreen().is_some(),
            layouts: self.layouts.iter().map(|l| l.name.clone()).collect(),
        };
        self.menu.menus = menu::build(menus, &ctx);
        // Commands extensions registered, in their own menu before Help.
        if !self.scripts.commands.is_empty() || !self.scripts.panels.is_empty() {
            let items = self
                .scripts
                .commands
                .iter()
                .map(|(ext, id, title)| menu::Item::Entry {
                    label: title.clone(),
                    key: None,
                    shortcut: self.scripts.shortcut_for(ext, id),
                    command: MenuCommand::Script { ext: ext.clone(), id: id.clone() },
                    checked: false,
                })
                .chain(self.scripts.panels.iter().map(|p| menu::Item::Entry {
                    label: format!("{} (panel)", p.title),
                    key: None,
                    shortcut: String::new(),
                    command: MenuCommand::Script { ext: p.ext.clone(), id: format!("panel:{}", p.id) },
                    checked: self.panels.values().any(|s| {
                        matches!(&s.kind, crate::panels::Kind::Script { ext, id } if *ext == p.ext && *id == p.id)
                    }),
                }))
                .collect();
            let m = menu::Menu { title: "Extensions".into(), key: Some(1), items };
            let at = self
                .menu
                .menus
                .iter()
                .position(|m| m.title == "Help")
                .unwrap_or(self.menu.menus.len());
            self.menu.menus.insert(at, m);
        }
    }

    /// Carry out what scripts asked for.
    fn script_effects(&mut self, fx: Vec<crate::scripting::Effect>) {
        use crate::scripting::Effect;
        let mut binds = false;
        let mut menu = false;
        for e in fx {
            match e {
                Effect::Notify(m) => self.notify(m),
                Effect::Send(text) => self.write(text.as_bytes()),
                Effect::Action(name) => match Action::from_name(&name) {
                    Some(a) => self.run_action(a),
                    None => self.notify(format!("Unknown action {name:?}")),
                },
                Effect::Profile(name) => self.pending = Some(Request::Profile(name)),
                Effect::MenuChanged => menu = true,
                Effect::BindsChanged => binds = true,
                Effect::Redraw => self.window.request_redraw(),
                Effect::OpenPanel { name, place } => {
                    if let Some(kind) = crate::panels::Kind::parse(&name) {
                        if !self.panels.values().any(|p| p.kind == kind) {
                            self.open_panel(kind, &place);
                        }
                    }
                }
                Effect::OpenLayout(name) => self.open_layout(&name),
                Effect::ClosePanels(ext) => {
                    let ids: Vec<u64> = self
                        .panels
                        .iter()
                        .filter(|(_, p)| matches!(&p.kind, crate::panels::Kind::Script { ext: e, .. } if *e == ext))
                        .map(|(id, _)| *id)
                        .collect();
                    for id in ids {
                        if self.close_pane(id) {
                            self.pending = Some(Request::Quit);
                        }
                    }
                }
            }
        }
        if binds {
            let all: Vec<_> =
                self.config_binds.iter().chain(self.scripts.binds()).cloned().collect();
            self.bindings.replace(all, &self.bus);
            self.bindings.reapply(&mut self.visual);
            self.window.request_redraw();
        }
        if menu {
            self.rebuild_menu();
            self.window.request_redraw();
        }
    }

    /// Tap Alt: open the first menu, or close an open one.
    fn toggle_menu(&mut self) {
        if self.menu.engaged() {
            self.menu.close();
        } else {
            self.rebuild_menu();
            self.menu.open_menu(0);
        }
        self.bus.publisher("interface").publish(
            "interface.menu.toggled",
            Some(focused!(self).id()),
            seraphim_events::json!({ "open": self.menu.engaged() }),
        );
        self.window.request_redraw();
    }

    fn run_menu_command(&mut self, cmd: MenuCommand) {
        self.menu.close();
        self.window.request_redraw();
        match cmd {
            MenuCommand::Action(a) => self.run_action(a),
            MenuCommand::Send(text) => self.write(text.as_bytes()),
            MenuCommand::Profile(name) => self.pending = Some(Request::Profile(name)),
            MenuCommand::Script { ext, id } => match id.strip_prefix("panel:") {
                Some(panel) => self.toggle_panel(
                    crate::panels::Kind::Script { ext, id: panel.to_owned() },
                    "right",
                ),
                None => self.scripts.invoke(&ext, &id),
            },
            MenuCommand::Layout(name) => self.open_layout(&name),
        }
    }

    fn apply_outcome(&mut self, o: Outcome) {
        match o {
            Outcome::Stay => self.window.request_redraw(),
            Outcome::Close => {
                self.menu.close();
                self.window.request_redraw();
            }
            Outcome::Run(cmd) => self.run_menu_command(cmd),
        }
    }

    /// Overlay grid size and the overlay cell under the pointer.
    fn overlay_grid(&self) -> (usize, usize, usize, usize) {
        let m = self.overlay.cell_metrics();
        let size = self.window.inner_size();
        let cols = (size.width as f32 / m.width).floor().max(1.0) as usize;
        let rows = (size.height as f32 / m.height).floor().max(1.0) as usize;
        let col = (self.mouse.pos.0 / m.width as f64).max(0.0) as usize;
        let row = (self.mouse.pos.1 / m.height as f64).max(0.0) as usize;
        (cols, rows, row, col)
    }

    /// Pointer motion for the menu. Returns true if the menu took it.
    fn menu_motion(&mut self) -> bool {
        if !self.bar_visible() || (self.mouse.left && !self.menu.engaged()) {
            return false;
        }
        let (cols, rows, row, col) = self.overlay_grid();
        let hit = self.menu.hit(row, col, cols, rows);
        if self.menu.panel.is_some() {
            return true;
        }
        if self.menu.open.is_some() {
            match hit {
                Hit::Title(i) if self.menu.open != Some(i) => self.menu.open_menu(i),
                Hit::Item(i) => self.menu.hover = Some(i),
                _ => {}
            }
            self.window.request_redraw();
            return true;
        }
        if hit != Hit::Outside {
            self.window.set_cursor(CursorIcon::Default);
            self.mouse.reporting = true; // re-evaluated on the way out
            return true;
        }
        false
    }

    /// A click for the menu. Returns true if the menu took it.
    fn menu_button(&mut self, pressed: bool, button: WButton) -> bool {
        if !self.bar_visible() {
            return false;
        }
        let (cols, rows, row, col) = self.overlay_grid();
        let hit = self.menu.hit(row, col, cols, rows);
        if self.menu.panel.is_some() {
            if pressed {
                self.menu.close();
                self.window.request_redraw();
            }
            return true;
        }
        if button != WButton::Left {
            // Any other button closes an open menu.
            if self.menu.open.is_some() && pressed {
                self.menu.close();
                self.window.request_redraw();
            }
            return self.menu.open.is_some() || hit != Hit::Outside;
        }
        match (hit, pressed, self.menu.open) {
            (Hit::Title(i), true, Some(o)) if o == i => self.menu.close(),
            (Hit::Title(i), true, open) => {
                if open.is_none() {
                    self.rebuild_menu();
                }
                self.menu.open_menu(i)
            }
            (Hit::Item(i), false, Some(_)) => {
                if let Some(cmd) = self.menu.command(i) {
                    self.run_menu_command(cmd);
                }
            }
            (Hit::Outside | Hit::Bar, true, Some(_)) => self.menu.close(),
            (Hit::Outside, _, None) => return false,
            _ => {}
        }
        self.window.request_redraw();
        true
    }

    fn cell_at(&self, pad: f32) -> (usize, usize) {
        let m = self.renderer.cell_metrics();
        let r = self.panes.get(&self.focus_id()).map(|p| p.rect).unwrap_or_default();
        let t = focused!(self).terminal();
        actions::cell_at(
            self.mouse.pos.0 - r.x as f64,
            self.mouse.pos.1 - r.y as f64,
            pad,
            m.width,
            m.height,
            t.cols(),
            t.rows(),
        )
    }

    /// Should mouse events go to the application? Shift always selects.
    fn reporting(&self) -> bool {
        mouse_reporting(focused!(self).terminal().modes()) && !self.modifiers.shift_key()
    }

    fn update_pointer_icon(&mut self) {
        let reporting = self.reporting();
        if reporting != self.mouse.reporting {
            self.mouse.reporting = reporting;
            self.window.set_cursor(if reporting { CursorIcon::Default } else { CursorIcon::Text });
        }
    }

    fn report_mouse(&mut self, button: MouseButton, action: MouseAction, cell: (usize, usize)) {
        let bytes = encode_mouse(
            button,
            action,
            cell.1,
            cell.0,
            self.mods(),
            focused!(self).terminal().modes(),
        );
        if let Some(b) = bytes {
            let _ = focused!(self).write(&b);
        }
    }

    /// The window edge under the pointer, when Seraphim (not the desktop)
    /// handles resizing.
    fn edge_under_pointer(&self) -> Option<winit::window::ResizeDirection> {
        if self.decorations == "system" || self.window.is_maximized() {
            return None;
        }
        let s = self.window.inner_size();
        let border = (4.0 * self.scale).max(2.0);
        let (x, y) = self.mouse.pos;
        crate::decor::edge_at(x, y, s.width as f64, s.height as f64, border)
    }

    /// Whether the pointer is in the row of window buttons, and on which.
    fn decor_at_pointer(&self) -> (bool, Option<crate::decor::Button>) {
        let Some((_, top, h)) = self.buttons_row_span() else { return (false, None) };
        let y = self.mouse.pos.1;
        if y < top || y >= top + h {
            return (false, None);
        }
        let (cols, _, _, col) = self.overlay_grid();
        (true, crate::decor::button_at(col, cols))
    }

    /// Pointer motion over Seraphim's decorations: resize arrows on the
    /// edges, highlighted buttons. True if it took the motion.
    fn decor_motion(&mut self) -> bool {
        if self.title_drag {
            self.title_drag = false;
            let _ = self.window.drag_window();
            return true;
        }
        if self.decorations == "system" || self.mouse.left || self.drag.is_some() {
            return false;
        }
        if let Some(d) = self.edge_under_pointer() {
            self.window.set_cursor(crate::decor::cursor_for(d));
            self.edge_cursor = true;
            return true;
        }
        if self.edge_cursor {
            self.edge_cursor = false;
            let reporting = self.reporting();
            self.window.set_cursor(if reporting { CursorIcon::Default } else { CursorIcon::Text });
            self.mouse.reporting = reporting;
        }
        let (in_row, hover) = self.decor_at_pointer();
        if hover != self.decor_hover {
            self.decor_hover = hover;
            self.window.request_redraw();
        }
        if in_row && (hover.is_some() || self.buttons_row() == Some(ButtonsRow::Title)) {
            self.window.set_cursor(CursorIcon::Default);
            self.mouse.reporting = true; // the icon is re-evaluated on the way out
            return true;
        }
        false
    }

    /// A press on the window's decorations: an edge, a button, or the title
    /// bar. True if it took the press.
    fn decor_press(&mut self) -> bool {
        if self.decorations == "system" || self.menu.engaged() {
            return false;
        }
        if let Some(d) = self.edge_under_pointer() {
            let _ = self.window.drag_resize_window(d);
            return true;
        }
        let (in_row, button) = self.decor_at_pointer();
        use crate::decor::Button;
        match button {
            Some(Button::Minimize) => self.window.set_minimized(true),
            Some(Button::Maximize) => self.window.set_maximized(!self.window.is_maximized()),
            Some(Button::Close) => self.pending = Some(Request::Quit),
            None if in_row && self.buttons_row() == Some(ButtonsRow::Title) => self.title_press(),
            None => return false,
        }
        true
    }

    /// Pressing empty title bar: drag the window; double-click maximizes.
    fn title_press(&mut self) {
        let now = Instant::now();
        if self.title_click.is_some_and(|t| now - t < Duration::from_millis(400)) {
            self.title_click = None;
            self.window.set_maximized(!self.window.is_maximized());
            return;
        }
        // The window moves once the pointer does, so that a second click
        // still reaches us.
        self.title_click = Some(now);
        self.title_drag = true;
    }

    fn on_cursor_moved(&mut self, x: f64, y: f64, pad: f32) {
        self.mouse.pos = (x, y);
        if self.menu_motion() {
            return;
        }
        if self.decor_motion() {
            return;
        }
        if let Some(d) = self.float_drag {
            let (dx, dy) = ((x - d.from.0) as f32, (y - d.from.1) as f32);
            if let Some(f) = self.floats.iter_mut().find(|f| f.id == d.id) {
                if d.resize {
                    f.rect.w = d.start.w + dx;
                    f.rect.h = d.start.h + dy;
                } else {
                    f.rect.x = (d.start.x + dx).round();
                    f.rect.y = (d.start.y + dy).round();
                }
            }
            self.clamp_floats();
            self.window.request_redraw();
            return;
        }
        if self.float_at(x as f32, y as f32).is_some() {
            self.window.set_cursor(CursorIcon::Default);
            self.mouse.reporting = true; // the icon is re-evaluated on the way out
            return;
        }
        // Dragging a divider resizes the panes on either side.
        if let Some(d) = &self.drag {
            let ratio = match d.split {
                Split::Right => (x as f32 - d.area.x) / d.area.w.max(1.0),
                Split::Down => (y as f32 - d.area.y) / d.area.h.max(1.0),
            };
            let path = d.path.clone();
            self.ws.set_ratio(&path, ratio);
            self.apply_layout();
            return;
        }
        if let Some(d) = self.divider_at(x as f32, y as f32) {
            self.window.set_cursor(match d.split {
                Split::Right => CursorIcon::ColResize,
                Split::Down => CursorIcon::RowResize,
            });
            self.mouse.reporting = true; // the icon is re-evaluated on the way out
            return;
        }
        if self.in_tab_bar(y) {
            self.window.set_cursor(CursorIcon::Default);
            self.mouse.reporting = true;
            return;
        }
        let cell = self.cell_at(pad);
        let moved = self.mouse.cell != Some(cell);
        self.mouse.cell = Some(cell);
        self.update_pointer_icon();
        if !moved {
            return;
        }
        if self.reporting() {
            self.report_mouse(self.mouse.held(), MouseAction::Motion, cell);
            return;
        }
        if !self.mouse.left {
            return;
        }
        let mut t = focused!(self).terminal();
        let point = t.viewport_point(cell.0, cell.1);
        if let Some((anchor, block)) = self.mouse.pending.take() {
            if anchor != point {
                t.start_selection(actions::selection_kind(1, block), anchor);
            }
        }
        if t.selection().is_some() {
            t.update_selection(point);
            drop(t);
            self.window.request_redraw();
        }
    }

    fn on_mouse_button(&mut self, state: ElementState, button: WButton, pad: f32) {
        let pressed = state == ElementState::Pressed;
        self.alt_tap = false;
        if !pressed && button == WButton::Left && self.title_drag {
            self.title_drag = false;
            return;
        }
        if pressed && button == WButton::Left && self.tab_drag.is_none() && self.decor_press() {
            return;
        }
        // Releases of a press the terminal saw still go to the terminal.
        let terminal_drag = !pressed && button == WButton::Left && self.mouse.left;
        if !terminal_drag && self.menu_button(pressed, button) {
            return;
        }
        let (x, y) = (self.mouse.pos.0 as f32, self.mouse.pos.1 as f32);
        if self.drag.is_some() {
            if !pressed && button == WButton::Left {
                self.drag = None;
            }
            return;
        }
        if let (Some(from), false, WButton::Left) = (self.tab_drag, pressed, button) {
            self.tab_drag = None;
            // Dropped outside the window: the tab gets a window of its own
            // there.
            let size = self.window.inner_size();
            let (x, y) = self.mouse.pos;
            let outside = x < 0.0 || y < 0.0 || x >= size.width as f64 || y >= size.height as f64;
            if outside && self.ws.tabs.len() > 1 {
                let origin = self.window.outer_position().unwrap_or_default();
                let at = winit::dpi::PhysicalPosition::new(
                    origin.x + x as i32 - 40,
                    origin.y + y as i32 - 10,
                );
                self.ws.active = from.min(self.ws.tabs.len() - 1);
                self.pending = Some(Request::DetachTab(Some(at)));
                return;
            }
            let col =
                (self.mouse.pos.0 / self.overlay.cell_metrics().width as f64).max(0.0) as usize;
            if let crate::tabbar::Hit::Tab(to) | crate::tabbar::Hit::Close(to) =
                self.tab_hits.at(col)
            {
                if to != from {
                    self.ws.reorder(from, to);
                    self.window.request_redraw();
                }
            }
            return;
        }
        if !terminal_drag && self.in_tab_bar(self.mouse.pos.1) {
            if pressed {
                self.tab_bar_click(button);
            }
            return;
        }
        if pressed && button == WButton::Left {
            if let Some(d) = self.divider_at(x, y) {
                self.drag = Some(d);
                return;
            }
        }
        if self.float_drag.is_some() && !pressed && button == WButton::Left {
            self.float_drag = None;
            return;
        }
        // Floating panels: the title moves one, the corner resizes it, ×
        // closes it; a press anywhere on one raises it and gives it the
        // keyboard. A press elsewhere gives the keyboard back to the tab.
        if pressed && !terminal_drag {
            if let Some(id) = self.float_at(x, y) {
                self.raise_float(id);
                if self.float_focus != Some(id) {
                    self.float_focus = Some(id);
                    self.focus_changed();
                }
                let r = self.rect_of(id);
                let m = self.renderer.cell_metrics();
                let title = y < r.y + m.height;
                let close = title && x >= r.x + r.w - m.width * 2.0;
                let corner = y >= r.y + r.h - m.height && x >= r.x + r.w - m.width * 2.0;
                if button == WButton::Left && close {
                    if self.close_pane(id) {
                        self.pending = Some(Request::Quit);
                    }
                    return;
                }
                if button == WButton::Left && (title || corner) {
                    self.float_drag =
                        Some(FloatDrag { id, resize: corner, from: self.mouse.pos, start: r });
                    return;
                }
            } else if self.float_focus.take().is_some() {
                self.focus_changed();
            }
        }
        // A press in another pane focuses it (and does nothing else).
        if pressed && !terminal_drag {
            if let Some(id) = self
                .pane_at(x, y)
                .filter(|id| *id != self.focus_id() && self.float_rect(*id).is_none())
            {
                self.ws.set_focus(id);
                self.focus_changed();
                return;
            }
        }
        // Clicks in a panel pick its rows.
        if let Some(id) = self.pane_at(x, y).filter(|id| self.panels.contains_key(id)) {
            if pressed && button == WButton::Left {
                let r = self.rect_of(id);
                // Floating panels have no padding.
                let pad = if self.float_rect(id).is_some() { 0.0 } else { pad };
                let ch = self.renderer.cell_metrics().height;
                let row = ((y - r.y - pad) / ch).floor().max(0.0) as usize;
                let sessions = self.session_rows();
                let ctx = crate::panels::Ctx {
                    sessions: &sessions,
                    history: &self.history,
                    packages: &self.packages,
                    settings: &[],
                    script: None,
                };
                let act = self.panels.get_mut(&id).expect("panel").click(row, &ctx);
                self.panel_act(id, act);
            }
            return;
        }
        let cell = self.cell_at(pad);
        match button {
            WButton::Left => self.mouse.left = pressed,
            WButton::Middle => self.mouse.middle = pressed,
            WButton::Right => self.mouse.right = pressed,
            _ => return,
        }
        if self.reporting() {
            let b = match button {
                WButton::Left => MouseButton::Left,
                WButton::Middle => MouseButton::Middle,
                _ => MouseButton::Right,
            };
            let action = if pressed { MouseAction::Press } else { MouseAction::Release };
            self.report_mouse(b, action, cell);
            return;
        }
        match (button, pressed) {
            (WButton::Left, true) => {
                let clicks = self.mouse.clicks.press(Instant::now(), cell.0, cell.1);
                let block = self.modifiers.control_key();
                let mut t = focused!(self).terminal();
                let point = t.viewport_point(cell.0, cell.1);
                if self.modifiers.shift_key() && t.selection().is_some() && clicks == 1 {
                    t.update_selection(point); // Shift+click extends
                } else if clicks >= 2 {
                    t.start_selection(actions::selection_kind(clicks, false), point);
                } else {
                    t.clear_selection();
                    self.mouse.pending = Some((point, block));
                }
                drop(t);
                self.window.request_redraw();
            }
            (WButton::Left, false) => {
                self.mouse.pending = None;
                self.copy_selection(Board::Primary);
            }
            (WButton::Middle, true) => self.paste(Board::Primary),
            _ => {}
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta, pad: f32) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64,
            MouseScrollDelta::PixelDelta(p) => {
                self.mouse.wheel_accum += p.y / self.renderer.cell_metrics().height as f64;
                let whole = self.mouse.wheel_accum.trunc();
                self.mouse.wheel_accum -= whole;
                whole / WHEEL_LINES as f64
            }
        };
        let notches = lines.round() as i64;
        if notches == 0 {
            return;
        }
        let (x, y) = (self.mouse.pos.0 as f32, self.mouse.pos.1 as f32);
        if let Some(id) = self.pane_at(x, y).filter(|id| self.panels.contains_key(id)) {
            let k = if notches > 0 { crate::panels::Key::Up } else { crate::panels::Key::Down };
            let ctx = crate::panels::Ctx {
                sessions: &[],
                history: &self.history,
                packages: &self.packages,
                settings: &[],
                script: None,
            };
            let p = self.panels.get_mut(&id).expect("panel");
            for _ in 0..notches.unsigned_abs() {
                p.key(k, &ctx);
            }
            self.window.request_redraw();
            return;
        }
        let cell = self.cell_at(pad);
        // Note: `reporting()` locks the terminal itself, so call it before
        // taking the lock here (the mutex is not re-entrant).
        let reporting = self.reporting();
        let (alt_scroll, alt_screen) = {
            let t = focused!(self).terminal();
            let m = t.modes();
            (m.alternate_scroll && m.mouse == MouseMode::Off, t.is_alternate_screen())
        };
        if reporting {
            let b = if notches > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
            for _ in 0..notches.abs() {
                self.report_mouse(b, MouseAction::Press, cell);
            }
        } else if alt_screen && alt_scroll {
            // Full-screen apps without mouse support (less, man): wheel
            // becomes cursor keys.
            let key = if notches > 0 { Key::Up } else { Key::Down };
            let modes = focused!(self).terminal().modes().clone();
            let one = encode_key(key, Mods::default(), &modes);
            let bytes = one.repeat((notches.unsigned_abs() as usize) * WHEEL_LINES as usize);
            let _ = focused!(self).write(&bytes);
        } else {
            focused!(self).terminal().scroll_display(notches * WHEEL_LINES);
            self.window.request_redraw();
        }
    }

    fn on_key(&mut self, event: &KeyEvent) {
        let pressed = event.state == ElementState::Pressed;
        // A tap of Alt (Option) on its own opens the menu bar, as in
        // Konsole and most desktop apps. Alt+key chords still reach the
        // shell. Apps that asked for every key (Kitty flag 8) keep Alt.
        if event.logical_key == WKey::Named(NamedKey::Alt) {
            if pressed && !event.repeat {
                self.alt_tap = true;
            } else if !pressed && std::mem::take(&mut self.alt_tap) {
                let wants_all = focused!(self).terminal().modes().kitty_keyboard & 8 != 0;
                if !wants_all {
                    self.toggle_menu();
                    return;
                }
            }
        } else if pressed {
            self.alt_tap = false;
        }
        if let Some(label) = &mut self.renaming {
            if pressed {
                match &event.logical_key {
                    WKey::Named(NamedKey::Enter) => {
                        let l = label.trim().to_owned();
                        if let Some(t) = self.ws.tab_mut() {
                            t.label = (!l.is_empty()).then_some(l);
                        }
                        self.renaming = None;
                        self.apply_layout();
                    }
                    WKey::Named(NamedKey::Escape) => {
                        self.renaming = None;
                        self.apply_layout();
                    }
                    WKey::Named(NamedKey::Backspace) => {
                        label.pop();
                    }
                    WKey::Named(NamedKey::Space) => label.push(' '),
                    WKey::Character(c) if label.chars().count() < 64 => label.push_str(c),
                    _ => {}
                }
                self.window.request_redraw();
            }
            return;
        }
        if self.menu.engaged() {
            if pressed {
                let k = match &event.logical_key {
                    WKey::Named(NamedKey::ArrowLeft) => Some(MenuKey::Left),
                    WKey::Named(NamedKey::ArrowRight) => Some(MenuKey::Right),
                    WKey::Named(NamedKey::ArrowUp) => Some(MenuKey::Up),
                    WKey::Named(NamedKey::ArrowDown) => Some(MenuKey::Down),
                    WKey::Named(NamedKey::Enter | NamedKey::Space) => Some(MenuKey::Enter),
                    WKey::Named(NamedKey::Escape) => Some(MenuKey::Escape),
                    WKey::Named(NamedKey::Alt | NamedKey::Shift | NamedKey::Control) => None,
                    WKey::Character(s) => s.chars().next().map(MenuKey::Char),
                    _ => Some(MenuKey::Escape),
                };
                if let Some(k) = k {
                    let o = self.menu.key(k);
                    self.apply_outcome(o);
                }
            }
            return;
        }
        if event.state != ElementState::Pressed {
            // Releases only matter to apps using the Kitty protocol with
            // event reporting.
            if focused!(self).terminal().modes().kitty_keyboard & 2 != 0 {
                if let Some(input) = key_input(event, self.mods()) {
                    let bytes = encode_key_input(&input, focused!(self).terminal().modes());
                    if !bytes.is_empty() {
                        let _ = focused!(self).write(&bytes);
                    }
                }
            }
            return;
        }
        let m = actions::Modifiers {
            shift: self.modifiers.shift_key(),
            ctrl: self.modifiers.control_key(),
            alt: self.modifiers.alt_key(),
        };
        let sk = match &event.logical_key {
            WKey::Character(s) => s.chars().next().map(ShortcutKey::Char),
            WKey::Named(NamedKey::Insert) => Some(ShortcutKey::Insert),
            WKey::Named(NamedKey::PageUp) => Some(ShortcutKey::PageUp),
            WKey::Named(NamedKey::PageDown) => Some(ShortcutKey::PageDown),
            WKey::Named(NamedKey::Home) => Some(ShortcutKey::Home),
            WKey::Named(NamedKey::End) => Some(ShortcutKey::End),
            WKey::Named(NamedKey::ArrowUp) => Some(ShortcutKey::Up),
            WKey::Named(NamedKey::ArrowDown) => Some(ShortcutKey::Down),
            WKey::Named(NamedKey::ArrowLeft) => Some(ShortcutKey::Left),
            WKey::Named(NamedKey::ArrowRight) => Some(ShortcutKey::Right),
            WKey::Named(NamedKey::Tab) => Some(ShortcutKey::Tab),
            WKey::Named(NamedKey::F10) => Some(ShortcutKey::F(10)),
            WKey::Named(NamedKey::F11) => Some(ShortcutKey::F(11)),
            _ => None,
        };
        // Extension shortcuts come first: the user installed them.
        if let Some(chord) = chord_of(event, m) {
            if let Some((ext, id)) = self.scripts.key(&chord) {
                self.scripts.invoke(&ext, &id);
                return;
            }
        }
        let alt_screen = focused!(self).terminal().is_alternate_screen();
        let ctx = actions::Workspace {
            tabs: self.ws.tabs.len(),
            panes: self.ws.tab().map_or(1, |t| t.root.panes().len()),
        };
        if let Some(action) = sk.and_then(|k| actions::shortcut_in(k, m, alt_screen, ctx)) {
            self.run_action(action);
            return;
        }
        let focus = self.focus_id();
        if self.panels.contains_key(&focus) {
            use crate::panels::Key as P;
            let key = match &event.logical_key {
                WKey::Named(NamedKey::ArrowUp) => Some(P::Up),
                WKey::Named(NamedKey::ArrowDown) => Some(P::Down),
                WKey::Named(NamedKey::ArrowLeft) => Some(P::Left),
                WKey::Named(NamedKey::ArrowRight) => Some(P::Right),
                WKey::Named(NamedKey::Enter) => Some(P::Enter),
                WKey::Named(NamedKey::Space) => Some(P::Char(' ')),
                WKey::Named(NamedKey::Home) => Some(P::Home),
                WKey::Named(NamedKey::End) => Some(P::End),
                WKey::Named(NamedKey::PageUp) => Some(P::PageUp),
                WKey::Named(NamedKey::PageDown) => Some(P::PageDown),
                WKey::Named(NamedKey::Escape) => Some(P::Escape),
                WKey::Character(s) if !m.ctrl && !m.alt => s.chars().next().map(P::Char),
                _ => None,
            };
            if let Some(k) = key {
                let sessions = self.session_rows();
                let ctx = crate::panels::Ctx {
                    sessions: &sessions,
                    history: &self.history,
                    packages: &self.packages,
                    settings: &[],
                    script: None,
                };
                let act = self.panels.get_mut(&focus).expect("panel").key(k, &ctx);
                self.panel_act(focus, act);
            }
            return;
        }
        if let Some(input) = key_input(event, self.mods()) {
            let modes = focused!(self).terminal().modes().clone();
            let bytes = encode_key_input(&input, &modes);
            if !bytes.is_empty() {
                self.renderer.key_pressed();
                if let Some(t) = &mut self.timing {
                    if t.key_at.is_none() {
                        t.key_at = Some(Instant::now());
                        t.echoed = false;
                    }
                }
            }
            self.write(&bytes);
        }
    }

    /// Apply the clipboard policy to OSC 52 requests from the application.
    fn handle_clipboard_requests(&mut self, allow_read: bool) {
        for req in focused!(self).take_clipboard_requests() {
            match req {
                ClipboardRequest::Store { selection, data } => {
                    let board =
                        if selection.contains('c') { Board::Clipboard } else { Board::Primary };
                    match actions::decode_base64(&data).map(String::from_utf8) {
                        Some(Ok(text)) => self.clipboards.set(board, text),
                        _ => eprintln!("seraphim: ignored malformed OSC 52 clipboard data"),
                    }
                }
                ClipboardRequest::Load { selection } => {
                    if allow_read {
                        let board =
                            if selection.contains('c') { Board::Clipboard } else { Board::Primary };
                        let text = self.clipboards.get(board).unwrap_or_default();
                        let reply =
                            format!("\x1b]52;{selection};{}\x07", encode_base64(text.as_bytes()));
                        let _ = focused!(self).write(reply.as_bytes());
                    } else if !self.warned_clipboard_read {
                        self.warned_clipboard_read = true;
                        eprintln!("seraphim: denied an application's request to read the clipboard (OSC 52)");
                    }
                }
            }
        }
    }
}

/// `SERAPHIM_DEBUG_INPUT=1` logs raw window input events to stderr.
/// One line for the window: file names instead of full paths, and for
/// multi-line errors (TOML snippets) the last line, which says what's wrong.
fn short_notice(msg: &str) -> String {
    let lines: Vec<&str> = msg.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let first = lines.first().copied().unwrap_or_default();
    let shorten = |w: &str| -> String {
        match w.strip_suffix(':') {
            Some(p) if p.starts_with('/') => {
                let name =
                    std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned());
                format!("{}:", name.unwrap_or_else(|| p.to_owned()))
            }
            _ => w.to_owned(),
        }
    };
    let mut out: Vec<String> = first.split(' ').map(shorten).collect();
    if lines.len() > 1 {
        let last = lines[lines.len() - 1];
        if !last.starts_with('|') && !last.starts_with('^') {
            out.push("—".into());
            out.push(last.to_owned());
        }
    }
    out.join(" ")
}

/// Open a folder in the desktop's file manager.
fn open_path(path: &std::path::Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = std::process::Command::new(opener).arg(path).spawn() {
        eprintln!("seraphim: {opener} {}: {e}", path.display());
    }
}

fn debug_input() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SERAPHIM_DEBUG_INPUT").is_some_and(|v| v != "0"))
}

fn encode_base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl Win {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(a) => self.active = Some(a),
            Err(e) => {
                self.error = Some(e);
                self.closed = true;
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if let UserEvent::ConfigChanged(_, files) = event {
            self.handle(Request::Files(files), event_loop);
            return;
        }
        if let UserEvent::Script(_) = event {
            if let Some(a) = &mut self.active {
                let theme = a.renderer.theme().clone();
                let fx = a.scripts.apply(&mut a.visual, &theme);
                a.script_effects(fx);
                if let Some(r) = a.pending.take() {
                    self.handle(r, event_loop);
                }
            }
            return;
        }
        let allow_read = self.options.allow_clipboard_read;
        let Some(a) = &mut self.active else { return };
        // Panes whose program ended close; the last one closes the window.
        let exited: Vec<(u64, i32)> = a
            .panes
            .iter()
            .filter_map(|(id, p)| p.session.exit_info().map(|e| (*id, e.code.unwrap_or(1))))
            .collect();
        for (id, code) in exited {
            if a.close_pane(id) {
                self.exit_code = code;
                self.closed = true;
                return;
            }
        }
        if let Some(r) = a.pending.take() {
            self.handle(r, event_loop);
        }
        let Some(a) = &mut self.active else { return };
        a.handle_clipboard_requests(allow_read);
        if a.pump_pane_events() && a.tab_bar_visible() {
            a.window.request_redraw();
        }
        // Output in panes out of sight marks their tab; visible ones redraw.
        let visible: Vec<u64> = a.drawn.iter().map(|(id, _)| *id).collect();
        let focus = a.focus_id();
        let mut other_changed = false;
        for (id, p) in a.panes.iter_mut() {
            if *id == focus || p.session.generation() == p.last_generation {
                continue;
            }
            if visible.contains(id) {
                other_changed = true;
            } else if p.last_generation == u64::MAX {
                // Never shown (a new background tab printing its prompt):
                // that isn't activity.
                p.last_generation = p.session.generation();
            } else if !p.activity {
                p.activity = true;
                p.last_generation = p.session.generation();
                other_changed |= a.tabs_cfg.bar != "never";
            }
        }
        if other_changed {
            a.window.request_redraw();
        }
        let focused_changed =
            a.panes.get(&focus).is_some_and(|p| p.session.generation() != p.last_generation);
        if focused_changed {
            if let Some(t) = &mut a.timing {
                t.echoed |= t.key_at.is_some();
            }
            a.reset_blink();
            a.update_pointer_icon();
            let synchronized = focused!(a).terminal().modes().synchronized_output;
            if synchronized {
                // Hold the frame until the app finishes (or the timeout,
                // handled in about_to_wait).
                a.sync_since.get_or_insert_with(Instant::now);
            } else {
                a.sync_since = None;
                a.window.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let pad = self.active.as_ref().map(|a| self.padding(a.scale)).unwrap_or(0.0);
        let Some(a) = &mut self.active else { return };
        if debug_input()
            && !matches!(event, WindowEvent::RedrawRequested | WindowEvent::CursorMoved { .. })
        {
            eprintln!("seraphim: input {event:?}");
        }
        match event {
            WindowEvent::CloseRequested => self.closed = true,
            WindowEvent::Resized(size) => {
                a.gpu.config.width = size.width.max(1);
                a.gpu.config.height = size.height.max(1);
                a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
                // The terminals follow once the resize pauses (about_to_wait).
                a.resize_due = Some(Instant::now() + RESIZE_SETTLE);
                a.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                a.scale = scale_factor;
                let config = FontConfig {
                    family: self.options.font_family.clone(),
                    size_px: font_px(self.options.font_size + self.zoom, scale_factor),
                };
                if let (Ok(f), Ok(g)) = (Fonts::new(&config), Fonts::new(&config)) {
                    a.renderer.set_fonts(f);
                    a.overlay.set_fonts(g);
                    let m = a.renderer.cell_metrics();
                    for p in a.panes.values() {
                        let _ = p.session.set_cell_pixels(m.width, m.height);
                    }
                }
                a.pad = pad;
                a.apply_layout();
            }
            WindowEvent::Occluded(occluded) => {
                a.occluded = occluded;
                a.update_pause();
                if !occluded {
                    a.window.request_redraw();
                }
            }
            WindowEvent::Focused(focused) => {
                a.focused = focused;
                a.bus.publisher("interface").publish(
                    "interface.focus.changed",
                    Some(focused!(a).id()),
                    seraphim_events::json!({ "focused": focused }),
                );
                // Alt+Tab: the release happens in another window.
                a.alt_tap = false;
                if !focused && a.menu.engaged() {
                    a.menu.close();
                }
                a.update_pause();
                a.reset_blink();
                let bytes = encode_focus(focused, focused!(a).terminal().modes());
                if !bytes.is_empty() {
                    let _ = focused!(a).write(&bytes);
                }
                a.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => {
                a.modifiers = m.state();
                a.update_pointer_icon();
            }
            // Synthetic presses are replayed by winit for keys already held
            // when focus arrives; forwarding them would duplicate input.
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => a.on_key(&event),
            WindowEvent::Ime(Ime::Commit(text)) => {
                a.renderer.key_pressed();
                a.write(text.as_bytes());
            }
            WindowEvent::CursorMoved { position, .. } => {
                a.on_cursor_moved(position.x, position.y, pad)
            }
            WindowEvent::CursorLeft { .. } => a.mouse.cell = None,
            WindowEvent::MouseInput { state, button, .. } => a.on_mouse_button(state, button, pad),
            WindowEvent::MouseWheel { delta, .. } => a.on_wheel(delta, pad),
            WindowEvent::DroppedFile(path) => {
                // Paste the path, quoted, like most terminals do.
                let quoted = format!("'{}' ", path.display().to_string().replace('\'', "'\\''"));
                let bytes = encode_paste(&quoted, focused!(a).terminal().modes());
                a.write(&bytes);
            }
            WindowEvent::RedrawRequested => {
                let size = a.window.inner_size();
                let (rects, dividers) = a.ws.layout(a.pane_area(), a.gap());
                let focus = a.focus_id();
                a.paint_panels(&rects, pad);
                let floats: Vec<(u64, Rect)> = a.floats.iter().map(|f| (f.id, f.rect)).collect();
                a.paint_panels(&floats, 0.0);
                for (id, r) in &rects {
                    let Some(p) = a.panes.get_mut(id) else { continue };
                    let view = View {
                        width: r.w.max(1.0) as u32,
                        height: r.h.max(1.0) as u32,
                        padding: pad,
                        focused: a.focused && *id == focus,
                        cursor_on: a.blink_on || *id != focus,
                    };
                    p.rect = *r;
                    a.renderer.select(&a.gpu.device, *id);
                    let term = p.session.terminal();
                    p.last_generation = p.session.generation();
                    if *id == focus && term.title() != a.title {
                        a.title = term.title().to_owned();
                        let t =
                            if a.title.is_empty() { "Seraphim".into() } else { a.title.clone() };
                        a.window.set_title(&t);
                    }
                    a.renderer.prepare(&a.gpu.device, &a.gpu.queue, &term, view);
                }
                // Dividers, and a veil over panes without focus.
                let theme = a.renderer.theme().clone();
                let mix = |k: f32| {
                    let c = |i: usize| {
                        (theme.background[i] as f32
                            + (theme.foreground[i] as f32 - theme.background[i] as f32) * k)
                            / 255.0
                    };
                    [c(0), c(1), c(2), 1.0]
                };
                let mut decor: Vec<([f32; 4], [f32; 4])> = dividers
                    .iter()
                    .map(|d| ([d.rect.x, d.rect.y, d.rect.w, d.rect.h], mix(0.3)))
                    .collect();
                let dim = a.panes_cfg.dim_inactive.clamp(0.0, 1.0);
                if rects.len() > 1 && dim > 0.0 {
                    let bg = theme.background.map(|c| c as f32 / 255.0 * dim);
                    for (id, r) in rects.iter().filter(|(id, _)| *id != focus) {
                        let _ = id;
                        decor.push(([r.x, r.y, r.w, r.h], [bg[0], bg[1], bg[2], dim]));
                    }
                }
                a.renderer.select(&a.gpu.device, DECOR_SLOT);
                a.renderer.prepare_rects(
                    &a.gpu.device,
                    &a.gpu.queue,
                    size.width,
                    size.height,
                    &decor,
                );
                // Each floating panel's frame: a soft shadow and a border
                // (the accent when it has the keyboard).
                let palette = menu::Palette::from_theme(&theme);
                for f in a.floats.clone() {
                    let r = f.rect;
                    let on = a.focused && f.id == focus;
                    let edge = palette.border.map(|c| c as f32 / 255.0);
                    let accent = palette.accent.map(|c| c as f32 / 255.0);
                    let b = if on { accent } else { edge };
                    let mut rects = Vec::new();
                    for (grow, alpha) in [(10.0, 0.08), (6.0, 0.12), (3.0, 0.18)] {
                        rects.push((
                            [
                                r.x - grow + 3.0,
                                r.y - grow + 5.0,
                                r.w + 2.0 * grow,
                                r.h + 2.0 * grow,
                            ],
                            [0.0, 0.0, 0.0, alpha],
                        ));
                    }
                    rects.push((
                        [r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0],
                        [b[0], b[1], b[2], 1.0],
                    ));
                    a.renderer.select(&a.gpu.device, frame_slot(f.id));
                    a.renderer.prepare_rects(
                        &a.gpu.device,
                        &a.gpu.queue,
                        size.width,
                        size.height,
                        &rects,
                    );
                }
                a.renderer.select(&a.gpu.device, focus);
                a.drawn = rects;
                // The tab bar, below the menu bar.
                if a.tab_bar_visible() {
                    let m = a.overlay.cell_metrics();
                    let cols = (size.width as f32 / m.width).floor().max(1.0) as usize;
                    let palette = menu::Palette::from_theme(&theme);
                    let design = crate::tabbar::Design::new(&a.tabs_cfg, &palette);
                    let alert = theme.ansi[9];
                    // The window buttons may share the row.
                    let buttons = a.buttons_row() == Some(ButtonsRow::Tabs);
                    let tab_cols =
                        if buttons { cols.saturating_sub(crate::decor::BUTTONS) } else { cols };
                    let (bytes, hits) = crate::tabbar::paint(
                        &a.tab_infos(),
                        tab_cols,
                        alert,
                        a.renaming.as_deref(),
                        &design,
                    );
                    let mut t = seraphim_terminal::Terminal::with_scrollback(cols, 1, 0);
                    t.advance(&bytes);
                    if buttons {
                        let c = a.decor_colors();
                        let max = a.window.is_maximized();
                        t.advance(&crate::decor::paint_buttons(cols, a.decor_hover, max, &c));
                    }
                    t.advance(b"\x1b[?25l");
                    a.tab_hits = hits;
                    let view = View {
                        width: size.width,
                        height: m.height.ceil() as u32,
                        padding: 0.0,
                        focused: true,
                        cursor_on: false,
                    };
                    a.overlay.select(&a.gpu.device, TAB_BAR_SLOT);
                    a.overlay.prepare(&a.gpu.device, &a.gpu.queue, &t, view);
                    a.overlay.select(&a.gpu.device, 0);
                }
                // Seraphim's title bar.
                if a.title_bar_height() > 0 {
                    let m = a.overlay.cell_metrics();
                    let cols = (size.width as f32 / m.width).floor().max(1.0) as usize;
                    let title = if a.title.is_empty() { "Seraphim" } else { a.title.as_str() };
                    let c = a.decor_colors();
                    let bytes = crate::decor::paint_title(
                        title,
                        cols,
                        a.focused,
                        a.decor_hover,
                        a.window.is_maximized(),
                        &c,
                    );
                    let mut t = seraphim_terminal::Terminal::with_scrollback(cols, 1, 0);
                    t.advance(&bytes);
                    t.advance(b"\x1b[?25l");
                    let view = View {
                        width: size.width,
                        height: a.title_bar_height(),
                        padding: 0.0,
                        focused: true,
                        cursor_on: false,
                    };
                    a.overlay.select(&a.gpu.device, TITLE_SLOT);
                    a.overlay.prepare(&a.gpu.device, &a.gpu.queue, &t, view);
                    a.overlay.select(&a.gpu.device, 0);
                }
                if a.overlay_visible() {
                    let (cols, rows, _, _) = a.overlay_grid();
                    let palette = menu::Palette::from_theme(a.renderer.theme());
                    let mut t = seraphim_terminal::Terminal::with_scrollback(cols, rows, 0);
                    a.menu.toast = a.toast.as_ref().map(|t| t.0.clone());
                    t.advance(&a.menu.paint(cols, rows, a.menu_pinned, &palette));
                    if a.buttons_row() == Some(ButtonsRow::Menu) {
                        let c = a.decor_colors();
                        let max = a.window.is_maximized();
                        t.advance(&crate::decor::paint_buttons(cols, a.decor_hover, max, &c));
                    }
                    let view = View {
                        width: size.width,
                        height: size.height,
                        padding: 0.0,
                        focused: true,
                        cursor_on: false,
                    };
                    a.overlay.prepare(&a.gpu.device, &a.gpu.queue, &t, view);
                }
                a.last_frame = Instant::now();
                a.visual_due = None;
                draw(a);
            }
            _ => {}
        }
        if let Some(req) = self.active.as_mut().and_then(|a| a.pending.take()) {
            self.handle(req, event_loop);
        }
    }

    /// Timers and animation; returns when the window next needs the loop.
    fn about_to_wait(&mut self) -> Option<Instant> {
        let Some(a) = &mut self.active else { return None };
        let now = Instant::now();
        let mut wake: Option<Instant> = None;

        // bench --window: one key at a time, the next once its echo is on
        // screen, at uneven intervals so samples fall at different points
        // of the display's refresh cycle.
        if let (Some(p), Some(t)) = (&mut a.probe, &mut a.timing) {
            if t.first_frame {
                // Let the program start first.
                let due = *p.next.get_or_insert(now + Duration::from_millis(800));
                // An echo that never came (shouldn't happen) doesn't stall it.
                if t.key_at.is_some_and(|k| now - k > Duration::from_secs(1)) {
                    t.key_at = None;
                }
                if now >= due && t.key_at.is_none() {
                    if p.left == 0 {
                        self.closed = true;
                        return None;
                    }
                    p.left -= 1;
                    p.sent += 1;
                    p.next = Some(now + Duration::from_millis(40 + (p.sent as u64 * 7) % 23));
                    t.key_at = Some(now);
                    t.echoed = false;
                    let key = [b'a' + (p.sent % 26) as u8];
                    a.renderer.key_pressed();
                    a.write(&key);
                }
                if let Some(n) = a.probe.as_ref().and_then(|p| p.next) {
                    wake = Some(wake.map_or(n, |w| w.min(n)));
                }
            }
        }

        if let Some(since) = a.sync_since {
            if now >= since + SYNC_TIMEOUT {
                a.sync_since = None;
                a.window.request_redraw();
            } else {
                wake = Some(since + SYNC_TIMEOUT);
            }
        }

        if let Some((_, until)) = a.toast {
            if now >= until {
                a.toast = None;
                a.window.request_redraw();
            } else {
                wake = Some(wake.map_or(until, |w| w.min(until)));
            }
        }

        // A live resize has paused: resize the terminals (one reflow and
        // one prompt redraw instead of one per step of the drag).
        if let Some(due) = a.resize_due {
            if now >= due {
                a.resize_due = None;
                a.apply_layout();
                let size = a.window.inner_size();
                let (cols, rows) = {
                    let t = focused!(a).terminal();
                    (t.cols(), t.rows())
                };
                a.bus.publisher("interface").publish(
                    "interface.resized",
                    Some(focused!(a).id()),
                    seraphim_events::json!({
                        "cols": cols, "rows": rows, "width": size.width, "height": size.height,
                    }),
                );
            } else {
                wake = Some(wake.map_or(due, |w| w.min(due)));
            }
        }

        // Panels that change by themselves (the system monitor).
        let visible: Vec<u64> =
            a.drawn.iter().map(|(id, _)| *id).chain(a.floats.iter().map(|f| f.id)).collect();
        for (id, p) in a.panels.iter_mut().filter(|(id, _)| visible.contains(id)) {
            let _ = id;
            if let Some((sampled, next)) = p.tick(now) {
                if sampled {
                    a.window.request_redraw();
                }
                wake = Some(wake.map_or(next, |w| w.min(next)));
            }
        }

        // Extensions that read the terminal see it at most 50 ms late.
        let focus = a.focus_id();
        if let Some(t) =
            a.panes.get(&focus).and_then(|p| a.scripts.refresh_snapshot(&p.session, now))
        {
            wake = Some(wake.map_or(t, |w| w.min(t)));
        }

        // Bindings: metrics on their intervals, latest event values.
        let (next, changed) = a.bindings.tick(now, &mut a.visual);
        if let Some(t) = next {
            wake = Some(wake.map_or(t, |w| w.min(t)));
        }
        if changed {
            a.window.request_redraw();
        }

        // Interaction rules react to bus events (published by the session
        // thread, which also wakes this loop).
        let (acted, warnings) = a.interactions.pump(&mut a.visual);
        for w in warnings {
            eprintln!("seraphim: {w}");
        }
        if acted {
            a.window.request_redraw();
        }

        // Animated background or cursor effects: draw frames at up to
        // max_fps while something moves.
        // Animated images only change at their frame boundaries, so they
        // wake the loop then, not at the frame-rate cap.
        let visual_next = a.visual.next_frame_in();
        let effects = a.renderer.effects_animating();
        if visual_next.is_some() || effects {
            let cap =
                a.last_frame + Duration::from_secs_f64(1.0 / a.visual_cfg.max_fps.max(1) as f64);
            let next = match visual_next {
                Some(d) if d > 0.0 && !effects => {
                    // Remember the deadline: once it passes, the time left
                    // in the (new) frame would be a whole frame again.
                    let due = *a.visual_due.get_or_insert(now + Duration::from_secs_f64(d));
                    cap.max(due)
                }
                _ => cap,
            };
            if now >= next {
                a.window.request_redraw();
            }
            wake = Some(wake.map_or(next, |w| w.min(next)));
        }

        let blinking = {
            let t = focused!(a).terminal();
            t.modes().cursor_blink && t.modes().cursor_visible && t.display_offset() == 0
        };
        if blinking && a.focused {
            if now >= a.blink_deadline {
                a.blink_on = !a.blink_on;
                a.blink_deadline = now + BLINK_INTERVAL;
                a.window.request_redraw();
            }
            wake = Some(wake.map_or(a.blink_deadline, |w| w.min(a.blink_deadline)));
        } else {
            a.blink_on = true;
        }
        wake
    }

    fn exiting(&mut self) {
        if let Some(t) = self.active.as_ref().and_then(|a| a.timing.as_ref()) {
            t.report();
        }
        // Dropping the session hangs up the shell (SIGHUP to its group).
        self.active = None;
    }
}

fn draw(a: &mut Active) {
    let started = Instant::now();
    let (w, h) = (a.gpu.config.width, a.gpu.config.height);
    a.visual.prepare(w, h);
    // Premultiplied background at the window opacity (1 = opaque).
    let mut clear = a.renderer.clear_color();
    let alpha = a.visual_cfg.window_opacity.clamp(0.0, 1.0) as f64;
    clear = wgpu::Color { r: clear.r * alpha, g: clear.g * alpha, b: clear.b * alpha, a: alpha };
    // A suboptimal frame (the window is being resized and the surface no
    // longer matches it) is still drawn and presented; the surface is
    // reconfigured afterwards. Reconfiguring while holding the frame is an
    // error, which used to crash Seraphim mid-resize.
    let mut reconfigure = false;
    let frame = match a.gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) => f,
        wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
            reconfigure = true;
            f
        }
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            a.window.request_redraw();
            return;
        }
        // Timeout / occluded / validation: skip this frame.
        _ => return,
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = a.gpu.device.create_command_encoder(&Default::default());
    // Offscreen background work (blur layers, styled transitions).
    a.visual.encode(&mut encoder);
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        a.visual.render(&mut pass);
        // Each pane in its own rectangle (renderer slots share the atlas).
        let focus = a.focus_id();
        for (id, r) in &a.drawn {
            if r.w < 1.0 || r.h < 1.0 {
                continue;
            }
            a.renderer.select(&a.gpu.device, *id);
            pass.set_viewport(r.x, r.y, r.w, r.h, 0.0, 1.0);
            a.renderer.render(&mut pass);
        }
        pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        a.renderer.select(&a.gpu.device, DECOR_SLOT);
        a.renderer.render(&mut pass);
        // Floating panels, bottom first, each over its frame.
        for f in &a.floats {
            let r = f.rect;
            if r.w < 1.0 || r.h < 1.0 || r.x + r.w > w as f32 + 0.5 || r.y + r.h > h as f32 + 0.5 {
                continue;
            }
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            a.renderer.select(&a.gpu.device, frame_slot(f.id));
            a.renderer.render(&mut pass);
            pass.set_viewport(r.x.max(0.0), r.y.max(0.0), r.w, r.h, 0.0, 1.0);
            a.renderer.select(&a.gpu.device, f.id);
            a.renderer.render(&mut pass);
        }
        pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
        a.renderer.select(&a.gpu.device, focus);
        let tb = a.title_bar_height() as f32;
        if tb > 0.0 && tb <= h as f32 {
            pass.set_viewport(0.0, 0.0, w as f32, tb, 0.0, 1.0);
            a.overlay.select(&a.gpu.device, TITLE_SLOT);
            a.overlay.render(&mut pass);
            a.overlay.select(&a.gpu.device, 0);
        }
        if a.tab_bar_visible() {
            let top = a.tab_bar_top() as f32;
            let th = a.tab_bar_height() as f32;
            if top + th <= h as f32 {
                pass.set_viewport(0.0, top, w as f32, th, 0.0, 1.0);
                a.overlay.select(&a.gpu.device, TAB_BAR_SLOT);
                a.overlay.render(&mut pass);
                a.overlay.select(&a.gpu.device, 0);
            }
        }
        if a.overlay_visible() {
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            a.overlay.render(&mut pass);
        }
    }
    a.gpu.queue.submit([encoder.finish()]);
    a.window.pre_present_notify();
    a.gpu.queue.present(frame);
    if reconfigure {
        a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
    }
    if let Some(t) = &mut a.timing {
        let now = Instant::now();
        t.draw_ms.push((now - started).as_secs_f64() * 1e3);
        t.span = Some((t.span.map_or(now, |s| s.0), now));
        if !t.first_frame {
            t.first_frame = true;
            if let Some(s) = STARTED.get() {
                eprintln!(
                    "seraphim: timing: first frame {:.1} ms after start",
                    (now - *s).as_secs_f64() * 1e3
                );
            }
        }
        if t.echoed {
            if let Some(k) = t.key_at.take() {
                t.samples.push((now - k).as_secs_f64() * 1e3);
            }
            t.echoed = false;
        }
    }
}

/// Translate a winit key event into a protocol-neutral [`KeyInput`].
/// The shortcut a key press makes, for extension shortcuts.
fn chord_of(event: &KeyEvent, m: actions::Modifiers) -> Option<crate::scripting::Chord> {
    let key = match &event.logical_key {
        WKey::Character(s) => s.to_lowercase(),
        WKey::Named(n) => match n {
            NamedKey::Enter => "enter".into(),
            NamedKey::Space => "space".into(),
            NamedKey::Tab => "tab".into(),
            NamedKey::Escape => "escape".into(),
            NamedKey::Backspace => "backspace".into(),
            NamedKey::ArrowUp => "up".into(),
            NamedKey::ArrowDown => "down".into(),
            NamedKey::ArrowLeft => "left".into(),
            NamedKey::ArrowRight => "right".into(),
            NamedKey::Home => "home".into(),
            NamedKey::End => "end".into(),
            NamedKey::PageUp => "pageup".into(),
            NamedKey::PageDown => "pagedown".into(),
            NamedKey::Insert => "insert".into(),
            NamedKey::Delete => "delete".into(),
            other => {
                let name = format!("{other:?}");
                let n = name.strip_prefix('F').and_then(|n| n.parse::<u8>().ok())?;
                format!("f{n}")
            }
        },
        _ => return None,
    };
    Some(crate::scripting::Chord { ctrl: m.ctrl, shift: m.shift, alt: m.alt, key })
}

fn key_input(event: &KeyEvent, mods: Mods) -> Option<KeyInput<'_>> {
    use winit::keyboard::{KeyCode, PhysicalKey};
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

    let kind = match (event.state, event.repeat) {
        (ElementState::Released, _) => KeyKind::Release,
        (ElementState::Pressed, true) => KeyKind::Repeat,
        (ElementState::Pressed, false) => KeyKind::Press,
    };
    let side = |left: KeyCode| event.physical_key == PhysicalKey::Code(left);
    let key = match &event.logical_key {
        WKey::Named(n) => match n {
            NamedKey::Enter => Key::Enter,
            NamedKey::Tab => Key::Tab,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Escape => Key::Escape,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::Insert => Key::Insert,
            NamedKey::Delete => Key::Delete,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::Space => Key::Char(' '),
            NamedKey::F1 => Key::F(1),
            NamedKey::F2 => Key::F(2),
            NamedKey::F3 => Key::F(3),
            NamedKey::F4 => Key::F(4),
            NamedKey::F5 => Key::F(5),
            NamedKey::F6 => Key::F(6),
            NamedKey::F7 => Key::F(7),
            NamedKey::F8 => Key::F(8),
            NamedKey::F9 => Key::F(9),
            NamedKey::F10 => Key::F(10),
            NamedKey::F11 => Key::F(11),
            NamedKey::F12 => Key::F(12),
            NamedKey::Shift => Key::Modifier(if side(KeyCode::ShiftLeft) {
                ModifierKey::LeftShift
            } else {
                ModifierKey::RightShift
            }),
            NamedKey::Control => Key::Modifier(if side(KeyCode::ControlLeft) {
                ModifierKey::LeftCtrl
            } else {
                ModifierKey::RightCtrl
            }),
            NamedKey::Alt => Key::Modifier(if side(KeyCode::AltLeft) {
                ModifierKey::LeftAlt
            } else {
                ModifierKey::RightAlt
            }),
            NamedKey::Super | NamedKey::Meta => Key::Modifier(if side(KeyCode::SuperLeft) {
                ModifierKey::LeftSuper
            } else {
                ModifierKey::RightSuper
            }),
            _ => return None,
        },
        WKey::Character(s) => {
            // The unshifted key (Kitty reports `a` for Shift+A).
            let base = match event.key_without_modifiers() {
                WKey::Character(b) => b.chars().next(),
                _ => None,
            };
            Key::Char(base.or_else(|| s.chars().next())?)
        }
        _ => return None,
    };
    let shifted = match &event.logical_key {
        WKey::Character(s) => s.chars().next(),
        WKey::Named(NamedKey::Space) => Some(' '),
        _ => None,
    };
    // winit updates its modifier state *after* delivering a modifier key's
    // own event; the Kitty protocol wants the state that results from it
    // (press sets the key's bit, release clears it).
    let mut mods = mods;
    if let Key::Modifier(k) = key {
        let on = kind != KeyKind::Release;
        match k {
            ModifierKey::LeftShift | ModifierKey::RightShift => mods.shift = on,
            ModifierKey::LeftCtrl | ModifierKey::RightCtrl => mods.ctrl = on,
            ModifierKey::LeftAlt | ModifierKey::RightAlt => mods.alt = on,
            ModifierKey::LeftSuper | ModifierKey::RightSuper => mods.super_key = on,
        }
    }
    Some(KeyInput { key, mods, kind, shifted, text: event.text.as_deref() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_are_short() {
        let e = "Configuration not applied: /home/u/.config/seraphim/config.toml: TOML parse error at line 3, column 8\n  |\n3 | size = \"huge\"\n  |        ^^^^^^\ninvalid type: string \"huge\", expected f32\n";
        assert_eq!(
            short_notice(e),
            "Configuration not applied: config.toml: TOML parse error at line 3, column 8 — invalid type: string \"huge\", expected f32"
        );
        assert_eq!(short_notice("Configuration reloaded"), "Configuration reloaded");
    }

    #[test]
    fn base64_round_trip() {
        for s in ["", "a", "ab", "abc", "hello world", "✓ 天使"] {
            let enc = encode_base64(s.as_bytes());
            assert_eq!(actions::decode_base64(&enc).unwrap(), s.as_bytes(), "{s:?} -> {enc}");
        }
        assert_eq!(encode_base64(b"hello"), "aGVsbG8=");
    }
}
