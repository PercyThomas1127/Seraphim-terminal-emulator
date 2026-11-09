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
        $a.panes.get(&$a.ws.focus().unwrap_or(0)).expect("a focused pane").session
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
    scrollback: usize,
    grapheme_clustering: bool,
}

/// Renderer slot for pane dividers and dimming.
const DECOR_SLOT: u64 = u64::MAX;
/// Overlay slot for the tab bar (slot 0 is the menu bar).
const TAB_BAR_SLOT: u64 = 1;
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
    pub tabs: seraphim_config::Tabs,
    pub panes: seraphim_config::Panes,
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
            menus: seraphim_config::builtin_menu(),
            watch: Vec::new(),
            live_reload: true,
            notice: None,
            cursor: Default::default(),
            transition: Ok(Default::default()),
            scripting: Default::default(),
            extensions: Default::default(),
            cli_extensions: Vec::new(),
            tabs: Default::default(),
            panes: Default::default(),
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
    /// A watched configuration file changed on disk.
    ConfigChanged(Vec<std::path::PathBuf>),
    /// Scripts queued commands.
    Script,
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
    /// Terminal panes by id; `ws` arranges them into tabs and splits.
    panes: std::collections::BTreeMap<u64, Pane>,
    ws: Workspace,
    next_pane: u64,
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
    /// Click targets in the tab bar.
    tab_hits: crate::tabbar::Hits,
    /// A divider being dragged.
    drag: Option<Divider>,
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
    OpenConfigFolder,
    Quit,
}

struct App {
    options: WindowOptions,
    proxy: EventLoopProxy<UserEvent>,
    active: Option<Active>,
    error: Option<String>,
    exit_code: i32,
    /// Points added to the configured font size (View → Enlarge Font).
    zoom: f32,
}

pub fn run(options: WindowOptions) -> Result<i32, String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("cannot open a window: {e}"))?;
    let mut app = App {
        options,
        proxy: event_loop.create_proxy(),
        active: None,
        error: None,
        exit_code: 0,
        zoom: 0.0,
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(app.exit_code),
    }
}

fn font_px(points: f32, scale: f64) -> f32 {
    // 1pt = 1/72in at 96 dpi logical.
    (points * 96.0 / 72.0 * scale as f32).round()
}

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Active, String> {
        let o = &self.options;
        let scale = event_loop.primary_monitor().map(|m| m.scale_factor()).unwrap_or(1.0);
        let fonts = Fonts::new(&FontConfig {
            family: o.font_family.clone(),
            size_px: font_px(o.font_size, scale),
        })
        .map_err(|e| e.to_string())?;
        let m = fonts.metrics();
        let pad = (o.padding as f64 * scale).round() as f32;
        let bar = if o.menu_bar { m.height.ceil() } else { 0.0 };
        let size = PhysicalSize::new(
            (o.cols as f32 * m.width + 2.0 * pad) as u32,
            (o.rows as f32 * m.height + 2.0 * pad + bar) as u32,
        );
        let attrs = Window::default_attributes()
            .with_title("Seraphim")
            // Transparency must be requested when the window is created.
            .with_transparent(o.visual.window_opacity < 1.0)
            .with_inner_size(size)
            .with_min_inner_size(PhysicalSize::new(
                (m.width * 10.0) as u32,
                (m.height * 3.0) as u32,
            ));
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        window.set_ime_allowed(true);
        window.set_cursor(CursorIcon::Text);
        let scale = window.scale_factor();

        let gpu = init_gpu(event_loop, window.clone(), o.visual.window_opacity < 1.0)?;
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

        let bus = EventBus::new();
        let proxy = self.proxy.clone();
        let notify = Notifier(Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Wake);
        }));
        let command = o.command.clone().unwrap_or_else(Command::default_shell);
        // Subscribe before the first session starts: it reports privilege
        // at once.
        const FIRST_PANE: u64 = 1;
        let interactions = Interactions::new(o.rules.clone(), &bus, FIRST_PANE);
        let bindings = crate::bindings::Bindings::new(o.binds.clone(), &bus, FIRST_PANE);
        let pane_events = bus.subscribe(
            seraphim_events::Pattern::Any(vec![
                "terminal.bell".into(),
                "terminal.progress".into(),
                "session.privileged".into(),
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
            ws: Workspace::new(),
            next_pane: 0,
            last_focus: FIRST_PANE,
            spawn: SpawnConfig {
                command,
                scrollback: o.scrollback,
                grapheme_clustering: o.grapheme_clustering,
            },
            notifier: notify,
            pad,
            tabs_cfg: o.tabs.clone(),
            panes_cfg: o.panes.clone(),
            base_profile: o.profile.clone(),
            drawn: Vec::new(),
            tab_hits: Default::default(),
            drag: None,
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
            alt_tap: false,
            menu_src: (o.menus.clone(), o.config_path.clone(), o.profile.clone()),
            toast: o.notice.clone().map(|m| (m, Instant::now() + TOAST_TIME * 2)),
            watcher: o.live_reload.then(|| {
                let proxy = self.proxy.clone();
                crate::watch::Watcher::spawn(o.watch.clone(), move |changed| {
                    proxy.send_event(UserEvent::ConfigChanged(changed)).is_ok()
                })
            }),
            interactions,
            bindings,
            occluded: false,
            visual_due: None,
            scripts: crate::scripting::ScriptHost::default(),
            config_binds: o.binds.clone(),
        };
        let first = active.spawn_pane(None, None)?;
        active.ws.add_tab(Node::Pane(first), None);
        active.apply_layout();
        let proxy = self.proxy.clone();
        let wake: seraphim_script::Waker = Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Script);
        });
        let (warnings, _) =
            active.scripts.configure(o.script_sources(), &active.bus, wake, &mut active.visual);
        for w in &warnings {
            eprintln!("seraphim: {w}");
        }
        if let Some(w) = warnings.into_iter().next() {
            active.notify(w);
        }
        active.rebuild_menu();
        Ok(active)
    }

    /// Switch profile (or reload the current one) without restarting the
    /// shell: theme, fonts, background layers and visual settings change;
    /// the background crossfades.
    /// Handle a [`Request`] from the window.
    fn handle(&mut self, request: Request, event_loop: &ActiveEventLoop) {
        match request {
            Request::Quit => event_loop.exit(),
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
                let exe = std::env::current_exe();
                let mut cmd = match exe {
                    Ok(exe) => std::process::Command::new(exe),
                    Err(e) => return eprintln!("seraphim: new window: {e}"),
                };
                if let Some(p) = &self.options.config_path {
                    cmd.arg("--config").arg(p);
                }
                cmd.args(["--profile", &self.options.profile]);
                if let Some(p) = &self.options.cli_preset {
                    cmd.args(["--preset", p]);
                }
                if let Some(cwd) = self.active.as_ref().and_then(|a| a.focused_cwd()) {
                    cmd.current_dir(cwd);
                }
                if let Err(e) = cmd.spawn() {
                    eprintln!("seraphim: new window: {e}");
                }
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
        let wake: seraphim_script::Waker = Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Script);
        });
        if request_kind == Request::Reload {
            a.scripts.invalidate();
        }
        let (script_warnings, fx) =
            a.scripts.configure(o.script_sources(), &a.bus, wake, &mut a.visual);
        for w in &script_warnings {
            eprintln!("seraphim: {w}");
        }
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

fn init_gpu(
    event_loop: &ActiveEventLoop,
    window: Arc<Window>,
    transparent: bool,
) -> Result<Gpu, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
        Box::new(event_loop.owned_display_handle()),
    ));
    let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .map_err(|e| format!("no usable GPU adapter: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("seraphim"),
        required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
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

    /// Pixels at the top reserved for the menu bar and the tab bar.
    fn top_inset(&self) -> u32 {
        self.menu_height() + self.tab_bar_height()
    }

    /// Where panes go.
    fn pane_area(&self) -> Rect {
        let s = self.window.inner_size();
        let top = self.top_inset().min(s.height.saturating_sub(1));
        Rect::new(0.0, top as f32, s.width as f32, (s.height - top) as f32)
    }

    /// Divider width in pixels.
    fn gap(&self) -> f32 {
        (self.panes_cfg.divider * self.scale as f32).round().max(1.0)
    }

    fn focus_id(&self) -> u64 {
        self.ws.focus().unwrap_or(0)
    }

    /// Start a shell (or `command`) in a new pane; it isn't placed yet.
    fn spawn_pane(
        &mut self,
        cwd: Option<std::path::PathBuf>,
        command: Option<Command>,
    ) -> Result<u64, String> {
        self.next_pane += 1;
        let id = self.next_pane;
        let area = self.pane_area();
        let (cols, rows) = self.renderer.grid_size(area.w as u32, area.h as u32, self.pad);
        let mut command = command.unwrap_or_else(|| self.spawn.command.clone());
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
        if id == self.last_focus || !self.panes.contains_key(&id) {
            return;
        }
        if self.focused {
            if let Some(old) = self.panes.get(&self.last_focus) {
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
        let want =
            self.ws.tab().and_then(|t| t.profile.clone()).unwrap_or(self.base_profile.clone());
        if want != self.menu_src.2 {
            self.pending = Some(Request::TabProfile(want));
        }
        self.window.request_redraw();
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
        let closed = self.ws.close(id);
        self.panes.remove(&id);
        self.renderer.drop_slot(id);
        if closed == Closed::Last {
            return true;
        }
        self.apply_layout();
        self.focus_changed();
        false
    }

    fn close_tab(&mut self, i: usize) -> bool {
        for id in self.ws.close_tab(i) {
            self.panes.remove(&id);
            self.renderer.drop_slot(id);
        }
        if self.ws.tabs.is_empty() {
            return true;
        }
        self.apply_layout();
        self.focus_changed();
        false
    }

    fn switch_tab(&mut self, f: impl FnOnce(&mut Workspace)) {
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
        let top = self.menu_height() as f64;
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
            _ => {}
        }
        self.window.request_redraw();
    }

    /// The pane under a pixel position.
    fn pane_at(&self, x: f32, y: f32) -> Option<u64> {
        self.drawn.iter().find(|(_, r)| r.contains(x, y)).map(|(id, _)| *id)
    }

    /// The divider under a pixel position (a little slack to grab it).
    fn divider_at(&self, x: f32, y: f32) -> Option<Divider> {
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
        };
        self.menu.menus = menu::build(menus, &ctx);
        // Commands extensions registered, in their own menu before Help.
        if !self.scripts.commands.is_empty() {
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
            MenuCommand::Script { ext, id } => self.scripts.invoke(&ext, &id),
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

    fn on_cursor_moved(&mut self, x: f64, y: f64, pad: f32) {
        self.mouse.pos = (x, y);
        if self.menu_motion() {
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
        // A press in another pane focuses it (and does nothing else).
        if pressed && !terminal_drag {
            if let Some(id) = self.pane_at(x, y).filter(|id| *id != self.focus_id()) {
                self.ws.set_focus(id);
                self.focus_changed();
                return;
            }
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
        if let Some(input) = key_input(event, self.mods()) {
            let modes = focused!(self).terminal().modes().clone();
            let bytes = encode_key_input(&input, &modes);
            if !bytes.is_empty() {
                self.renderer.key_pressed();
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

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(a) => self.active = Some(a),
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if let UserEvent::ConfigChanged(files) = event {
            self.handle(Request::Files(files), event_loop);
            return;
        }
        if let UserEvent::Script = event {
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
                event_loop.exit();
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
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                a.gpu.config.width = size.width.max(1);
                a.gpu.config.height = size.height.max(1);
                a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
                a.apply_layout();
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
                a.renderer.select(&a.gpu.device, focus);
                a.drawn = rects;
                // The tab bar, below the menu bar.
                if a.tab_bar_visible() {
                    let m = a.overlay.cell_metrics();
                    let cols = (size.width as f32 / m.width).floor().max(1.0) as usize;
                    let palette = menu::Palette::from_theme(&theme);
                    let alert = theme.ansi[9];
                    let (bytes, hits) = crate::tabbar::paint(
                        &a.tab_infos(),
                        cols,
                        &palette,
                        alert,
                        a.renaming.as_deref(),
                    );
                    let mut t = seraphim_terminal::Terminal::with_scrollback(cols, 1, 0);
                    t.advance(&bytes);
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
                if a.overlay_visible() {
                    let (cols, rows, _, _) = a.overlay_grid();
                    let palette = menu::Palette::from_theme(a.renderer.theme());
                    let mut t = seraphim_terminal::Terminal::with_scrollback(cols, rows, 0);
                    a.menu.toast = a.toast.as_ref().map(|t| t.0.clone());
                    t.advance(&a.menu.paint(cols, rows, a.menu_pinned, &palette));
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(a) = &mut self.active else { return };
        let now = Instant::now();
        let mut wake: Option<Instant> = None;

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
        event_loop.set_control_flow(match wake {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Dropping the session hangs up the shell (SIGHUP to its group).
        self.active = None;
    }
}

fn draw(a: &mut Active) {
    let (w, h) = (a.gpu.config.width, a.gpu.config.height);
    a.visual.prepare(w, h);
    // Premultiplied background at the window opacity (1 = opaque).
    let mut clear = a.renderer.clear_color();
    let alpha = a.visual_cfg.window_opacity.clamp(0.0, 1.0) as f64;
    clear = wgpu::Color { r: clear.r * alpha, g: clear.g * alpha, b: clear.b * alpha, a: alpha };
    let frame = match a.gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) => f,
        wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
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
        a.renderer.select(&a.gpu.device, focus);
        if a.tab_bar_visible() {
            let top = a.menu_height() as f32;
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
