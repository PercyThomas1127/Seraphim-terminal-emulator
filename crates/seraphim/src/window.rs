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
use seraphim_render::{FontConfig, Fonts, Renderer, Theme, View};
use seraphim_session::{ClipboardRequest, Command, Notifier, Session, SessionConfig};
use seraphim_terminal::input::{
    encode_focus, encode_key, encode_mouse, encode_paste, mouse_reporting, Key, Mods, MouseAction,
    MouseButton,
};
use seraphim_terminal::{MouseMode, Point};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton as WButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::actions::{self, Action, ClickTracker, ShortcutKey};

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
}

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
    session: Session,
    _bus: EventBus,
    clipboards: Clipboards,
    modifiers: ModifiersState,
    mouse: MouseState,
    focused: bool,
    scale: f64,
    title: String,
    last_generation: u64,
    blink_on: bool,
    blink_deadline: Instant,
    warned_clipboard_read: bool,
    /// When the app turned on synchronized output with a frame pending.
    sync_since: Option<Instant>,
}

struct App {
    options: WindowOptions,
    proxy: EventLoopProxy<UserEvent>,
    active: Option<Active>,
    error: Option<String>,
    exit_code: i32,
}

pub fn run(options: WindowOptions) -> Result<i32, String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("cannot open a window: {e}"))?;
    let mut app =
        App { options, proxy: event_loop.create_proxy(), active: None, error: None, exit_code: 0 };
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
        let size = PhysicalSize::new(
            (o.cols as f32 * m.width + 2.0 * pad) as u32,
            (o.rows as f32 * m.height + 2.0 * pad) as u32,
        );
        let attrs = Window::default_attributes()
            .with_title("Seraphim")
            .with_inner_size(size)
            .with_min_inner_size(PhysicalSize::new(
                (m.width * 10.0) as u32,
                (m.height * 3.0) as u32,
            ));
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        window.set_ime_allowed(true);
        window.set_cursor(CursorIcon::Text);
        let scale = window.scale_factor();

        let gpu = init_gpu(event_loop, window.clone())?;
        let mut renderer = Renderer::new(&gpu.device, gpu.config.format, fonts, o.theme.clone());
        if (font_px(o.font_size, scale) - renderer.fonts().size_px()).abs() > 0.1 {
            renderer.set_fonts(self.fonts_for(scale)?);
        }
        let inner = window.inner_size();
        let pad = (o.padding as f64 * scale).round() as f32;
        let (cols, rows) = renderer.grid_size(inner.width, inner.height, pad);

        let bus = EventBus::new();
        let proxy = self.proxy.clone();
        let notify = Notifier(Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Wake);
        }));
        let command = o.command.clone().unwrap_or_else(Command::default_shell);
        let session = Session::spawn(
            1,
            SessionConfig {
                command,
                cols,
                rows,
                notify: Some(notify),
                color_scheme: Some(renderer.theme().scheme()),
                scrollback: o.scrollback,
            },
            &bus,
        )
        .map_err(|e| format!("cannot start shell: {e}"))?;

        window.request_redraw();
        Ok(Active {
            window,
            gpu,
            renderer,
            session,
            _bus: bus,
            clipboards: Clipboards::new(),
            modifiers: ModifiersState::empty(),
            mouse: MouseState::default(),
            focused: true,
            scale,
            title: String::new(),
            last_generation: u64::MAX,
            blink_on: true,
            blink_deadline: Instant::now() + BLINK_INTERVAL,
            warned_clipboard_read: false,
            sync_since: None,
        })
    }

    fn fonts_for(&self, scale: f64) -> Result<Fonts, String> {
        Fonts::new(&FontConfig {
            family: self.options.font_family.clone(),
            size_px: font_px(self.options.font_size, scale),
        })
        .map_err(|e| e.to_string())
    }

    fn padding(&self, scale: f64) -> f32 {
        (self.options.padding as f64 * scale).round() as f32
    }
}

fn init_gpu(event_loop: &ActiveEventLoop, window: Arc<Window>) -> Result<Gpu, String> {
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
    surface.configure(&device, &config);
    eprintln!(
        "seraphim: {} ({:?}), surface {:?}",
        adapter.get_info().name,
        adapter.get_info().backend,
        config.format
    );
    Ok(Gpu { surface, device, queue, config })
}

impl Active {
    fn mods(&self) -> Mods {
        Mods {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            ctrl: self.modifiers.control_key(),
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
            let mut t = self.session.terminal();
            if t.display_offset() > 0 {
                t.scroll_to_bottom();
                self.window.request_redraw();
            }
        }
        self.reset_blink();
        let _ = self.session.write(bytes);
    }

    fn paste(&mut self, board: Board) {
        let Some(text) = self.clipboards.get(board) else { return };
        if text.is_empty() {
            return;
        }
        let bytes = encode_paste(&text, self.session.terminal().modes());
        self.write(&bytes);
    }

    fn copy_selection(&mut self, board: Board) {
        let text = self.session.terminal().selection_text();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.clipboards.set(board, text);
        }
    }

    fn run_action(&mut self, action: Action) {
        let rows = self.session.terminal().rows() as i64;
        let scroll = |a: &mut Active, delta: i64| {
            a.session.terminal().scroll_display(delta);
            a.window.request_redraw();
        };
        match action {
            Action::Copy => self.copy_selection(Board::Clipboard),
            Action::Paste => self.paste(Board::Clipboard),
            Action::ScrollPageUp => scroll(self, rows - 1),
            Action::ScrollPageDown => scroll(self, -(rows - 1)),
            Action::ScrollLineUp => scroll(self, 1),
            Action::ScrollLineDown => scroll(self, -1),
            Action::ScrollTop => scroll(self, i64::MAX / 2),
            Action::ScrollBottom => scroll(self, -(i64::MAX / 2)),
        }
    }

    fn cell_at(&self, pad: f32) -> (usize, usize) {
        let m = self.renderer.cell_metrics();
        let t = self.session.terminal();
        actions::cell_at(
            self.mouse.pos.0,
            self.mouse.pos.1,
            pad,
            m.width,
            m.height,
            t.cols(),
            t.rows(),
        )
    }

    /// Should mouse events go to the application? Shift always selects.
    fn reporting(&self) -> bool {
        mouse_reporting(self.session.terminal().modes()) && !self.modifiers.shift_key()
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
            self.session.terminal().modes(),
        );
        if let Some(b) = bytes {
            let _ = self.session.write(&b);
        }
    }

    fn on_cursor_moved(&mut self, x: f64, y: f64, pad: f32) {
        self.mouse.pos = (x, y);
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
        let mut t = self.session.terminal();
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
                let mut t = self.session.terminal();
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
            let t = self.session.terminal();
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
            let modes = self.session.terminal().modes().clone();
            let one = encode_key(key, Mods::default(), &modes);
            let bytes = one.repeat((notches.unsigned_abs() as usize) * WHEEL_LINES as usize);
            let _ = self.session.write(&bytes);
        } else {
            self.session.terminal().scroll_display(notches * WHEEL_LINES);
            self.window.request_redraw();
        }
    }

    fn on_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
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
            _ => None,
        };
        let alt_screen = self.session.terminal().is_alternate_screen();
        if let Some(action) = sk.and_then(|k| actions::shortcut(k, m, alt_screen)) {
            self.run_action(action);
            return;
        }
        if let Some(bytes) = key_bytes(event, self.modifiers, &self.session) {
            self.write(&bytes);
        }
    }

    /// Apply the clipboard policy to OSC 52 requests from the application.
    fn handle_clipboard_requests(&mut self, allow_read: bool) {
        for req in self.session.take_clipboard_requests() {
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
                        let _ = self.session.write(reply.as_bytes());
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: UserEvent) {
        let allow_read = self.options.allow_clipboard_read;
        let Some(a) = &mut self.active else { return };
        if let Some(exit) = a.session.exit_info() {
            self.exit_code = exit.code.unwrap_or(1);
            event_loop.exit();
            return;
        }
        a.handle_clipboard_requests(allow_read);
        if a.session.generation() != a.last_generation {
            a.reset_blink();
            a.update_pointer_icon();
            let synchronized = a.session.terminal().modes().synchronized_output;
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
                let (cols, rows) = a.renderer.grid_size(size.width, size.height, pad);
                let _ = a.session.resize(cols, rows);
                a.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                a.scale = scale_factor;
                let fonts = Fonts::new(&FontConfig {
                    family: self.options.font_family.clone(),
                    size_px: font_px(self.options.font_size, scale_factor),
                });
                if let Ok(fonts) = fonts {
                    a.renderer.set_fonts(fonts);
                }
                a.window.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                a.focused = focused;
                a.reset_blink();
                let bytes = encode_focus(focused, a.session.terminal().modes());
                if !bytes.is_empty() {
                    let _ = a.session.write(&bytes);
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
            WindowEvent::Ime(Ime::Commit(text)) => a.write(text.as_bytes()),
            WindowEvent::CursorMoved { position, .. } => {
                a.on_cursor_moved(position.x, position.y, pad)
            }
            WindowEvent::CursorLeft { .. } => a.mouse.cell = None,
            WindowEvent::MouseInput { state, button, .. } => a.on_mouse_button(state, button, pad),
            WindowEvent::MouseWheel { delta, .. } => a.on_wheel(delta, pad),
            WindowEvent::DroppedFile(path) => {
                // Paste the path, quoted, like most terminals do.
                let quoted = format!("'{}' ", path.display().to_string().replace('\'', "'\\''"));
                let bytes = encode_paste(&quoted, a.session.terminal().modes());
                a.write(&bytes);
            }
            WindowEvent::RedrawRequested => {
                let size = a.window.inner_size();
                let view = View {
                    width: size.width,
                    height: size.height,
                    padding: pad,
                    focused: a.focused,
                    cursor_on: a.blink_on,
                };
                {
                    let term = a.session.terminal();
                    a.last_generation = a.session.generation();
                    if term.title() != a.title {
                        a.title = term.title().to_owned();
                        let t =
                            if a.title.is_empty() { "Seraphim".into() } else { a.title.clone() };
                        a.window.set_title(&t);
                    }
                    a.renderer.prepare(&a.gpu.device, &a.gpu.queue, &term, view);
                }
                draw(a);
            }
            _ => {}
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

        let blinking = {
            let t = a.session.terminal();
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
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(a.renderer.clear_color()),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        a.renderer.render(&mut pass);
    }
    a.gpu.queue.submit([encoder.finish()]);
    a.window.pre_present_notify();
    a.gpu.queue.present(frame);
}

/// Translate a winit key press into bytes for the application.
fn key_bytes(event: &KeyEvent, mods: ModifiersState, session: &Session) -> Option<Vec<u8>> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let m = Mods { shift: mods.shift_key(), alt: mods.alt_key(), ctrl: mods.control_key() };
    let modes = session.terminal().modes().clone();
    let named = |k: Key| Some(encode_key(k, m, &modes));
    match &event.logical_key {
        WKey::Named(n) => match n {
            NamedKey::Enter => named(Key::Enter),
            NamedKey::Tab => named(Key::Tab),
            NamedKey::Backspace => named(Key::Backspace),
            NamedKey::Escape => named(Key::Escape),
            NamedKey::ArrowUp => named(Key::Up),
            NamedKey::ArrowDown => named(Key::Down),
            NamedKey::ArrowLeft => named(Key::Left),
            NamedKey::ArrowRight => named(Key::Right),
            NamedKey::Home => named(Key::Home),
            NamedKey::End => named(Key::End),
            NamedKey::Insert => named(Key::Insert),
            NamedKey::Delete => named(Key::Delete),
            NamedKey::PageUp => named(Key::PageUp),
            NamedKey::PageDown => named(Key::PageDown),
            NamedKey::Space => named(Key::Char(' ')),
            NamedKey::F1 => named(Key::F(1)),
            NamedKey::F2 => named(Key::F(2)),
            NamedKey::F3 => named(Key::F(3)),
            NamedKey::F4 => named(Key::F(4)),
            NamedKey::F5 => named(Key::F(5)),
            NamedKey::F6 => named(Key::F(6)),
            NamedKey::F7 => named(Key::F(7)),
            NamedKey::F8 => named(Key::F(8)),
            NamedKey::F9 => named(Key::F(9)),
            NamedKey::F10 => named(Key::F(10)),
            NamedKey::F11 => named(Key::F(11)),
            NamedKey::F12 => named(Key::F(12)),
            _ => None,
        },
        WKey::Character(s) if m.ctrl || m.alt => {
            let c = s.chars().next()?;
            Some(encode_key(Key::Char(c), m, &modes))
        }
        // Plain typing: use the composed text (dead keys, compose, layouts).
        _ => event.text.as_ref().map(|t| t.as_bytes().to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in ["", "a", "ab", "abc", "hello world", "✓ 天使"] {
            let enc = encode_base64(s.as_bytes());
            assert_eq!(actions::decode_base64(&enc).unwrap(), s.as_bytes(), "{s:?} -> {enc}");
        }
        assert_eq!(encode_base64(b"hello"), "aGVsbG8=");
    }
}
