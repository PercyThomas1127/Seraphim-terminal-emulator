//! # seraphim-session
//!
//! The Session Manager (spec §3.4): a [`Session`] owns a PTY and a
//! [`Terminal`], runs the I/O thread that feeds one into the other, and
//! publishes normalized events on the [`EventBus`].
//!
//! Sessions are independent of windows, tabs and visual profiles: the
//! renderer only ever *reads* terminal state through [`Session::terminal`],
//! so closing a tab or switching profile never touches the process.
//!
//! ## Events published (source = `"session"`)
//!
//! | name                     | data                                        |
//! |--------------------------|---------------------------------------------|
//! | `session.created`        | `{pid, cols, rows}`                         |
//! | `session.exited`         | `{exit_code, signal}`                       |
//! | `session.privileged`     | `{root: bool}` — trusted, from `/proc`      |
//! | `session.cwd.changed`    | `{cwd}`                                     |
//! | `terminal.title.changed` | `{title}`                                   |
//! | `terminal.bell`          | `{}`                                        |
//! | `terminal.output`        | `{bytes}` — byte count only, rate-limited by queue |
//! | `terminal.screen.alternate` | `{active}`                               |
//! | `shell.prompt`           | `{}`                                        |
//! | `command.started`        | `{command?}`                                |
//! | `command.completed`      | `{command?, exit_code?, duration_ms}`       |
//! | `clipboard.requested`    | `{selection, op}` — not applied here        |

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use seraphim_events::{json, EventBus, Publisher};
use seraphim_pty::{is_eof_error, ProcessInfo, Pty, PtyCommand, PtySize};
use seraphim_terminal::{Terminal, TerminalEvent};

pub use seraphim_pty::PtyCommand as Command;

pub type SessionId = u64;

/// Called from the session's I/O thread whenever terminal state changes
/// (e.g. to wake a UI event loop). Must be cheap and non-blocking.
#[derive(Clone)]
pub struct Notifier(pub Arc<dyn Fn() + Send + Sync>);

impl std::fmt::Debug for Notifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notifier")
    }
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub command: PtyCommand,
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
    pub notify: Option<Notifier>,
    /// Base colors used to answer OSC 10/11/4 color queries. Set before the
    /// program starts, so even its very first query sees the real theme.
    pub color_scheme: Option<seraphim_terminal::palette::ColorScheme>,
    /// Initial (and RIS) state of grapheme clustering, mode 2027.
    pub grapheme_clustering: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            command: PtyCommand::default_shell(),
            cols: 80,
            rows: 24,
            scrollback: seraphim_terminal::DEFAULT_SCROLLBACK,
            notify: None,
            color_scheme: None,
            grapheme_clustering: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// An OSC 52 clipboard request from the application. The data never goes
/// on the event bus (only `clipboard.requested` metadata does): the UI
/// drains these with [`Session::take_clipboard_requests`] and applies its
/// clipboard policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRequest {
    /// Set `selection` (`c` clipboard, `p` primary, ...) to base64 `data`.
    Store { selection: String, data: String },
    /// The application asks to read the clipboard.
    Load { selection: String },
}

/// Largest OSC 52 payload accepted (base64 bytes).
pub const MAX_CLIPBOARD_REQUEST: usize = 1 << 20;
const MAX_PENDING_CLIPBOARD: usize = 8;

#[derive(Default)]
struct Shared {
    clipboard: Mutex<Vec<ClipboardRequest>>,
    exit: Mutex<Option<ExitInfo>>,
    exited: Condvar,
    /// Bumped whenever terminal state changes; renderers poll/compare this
    /// to decide whether a frame is needed.
    generation: AtomicU64,
}

/// One running terminal session.
pub struct Session {
    id: SessionId,
    terminal: Arc<Mutex<Terminal>>,
    pty: Arc<Mutex<Pty>>,
    writer: Arc<Mutex<std::fs::File>>,
    shared: Arc<Shared>,
    io_thread: Option<JoinHandle<()>>,
}

impl Session {
    pub fn spawn(id: SessionId, config: SessionConfig, bus: &EventBus) -> io::Result<Session> {
        let size = PtySize::new(config.cols, config.rows);
        let pty = Pty::spawn(&config.command, size)?;
        let mut term = Terminal::with_scrollback(
            config.cols as usize,
            config.rows as usize,
            config.scrollback,
        );
        if let Some(scheme) = config.color_scheme.clone() {
            term.set_color_scheme(scheme);
        }
        term.set_grapheme_clustering(config.grapheme_clustering);
        let terminal = Arc::new(Mutex::new(term));
        let reader = pty.reader()?;
        let writer = Arc::new(Mutex::new(pty.writer()?));
        let pid = pty.pid();
        let pty = Arc::new(Mutex::new(pty));
        let shared = Arc::new(Shared::default());
        let publisher = bus.publisher("session");

        publisher.publish(
            "session.created",
            Some(id),
            json!({"pid": pid, "cols": config.cols, "rows": config.rows}),
        );

        let io = IoLoop {
            id,
            reader,
            terminal: terminal.clone(),
            writer: writer.clone(),
            pty: pty.clone(),
            shared: shared.clone(),
            publisher,
            shell_pid: pid,
            command: None,
            root: None,
            notify: config.notify,
        };
        let io_thread = std::thread::Builder::new()
            .name(format!("seraphim-session-{id}"))
            .spawn(move || io.run())?;

        Ok(Session { id, terminal, pty, writer, shared, io_thread: Some(io_thread) })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Lock the terminal state for reading (rendering) or input encoding.
    /// Hold the lock briefly: the I/O thread needs it to make progress.
    ///
    /// # Panics
    /// If this thread already holds this session's terminal. The mutex is
    /// not re-entrant, so that would otherwise deadlock silently.
    pub fn terminal(&self) -> TerminalGuard<'_> {
        let key = Arc::as_ptr(&self.terminal) as usize;
        HELD.with(|h| {
            if h.borrow().contains(&key) {
                panic!("Session::terminal() re-entered on the same thread (would deadlock)");
            }
            h.borrow_mut().push(key);
        });
        TerminalGuard { guard: self.terminal.lock().unwrap(), key }
    }

    /// Monotonic counter incremented on every state change.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Send raw bytes (already-encoded input) to the application.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.writer.lock().unwrap().write_all(bytes)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.terminal.lock().unwrap().resize(cols as usize, rows as usize);
        self.shared.generation.fetch_add(1, Ordering::Release);
        self.pty.lock().unwrap().resize(PtySize::new(cols, rows))
    }

    pub fn exit_info(&self) -> Option<ExitInfo> {
        *self.shared.exit.lock().unwrap()
    }

    /// Block until the session's process exits (or `timeout` elapses).
    pub fn wait_exit(&self, timeout: Duration) -> Option<ExitInfo> {
        let guard = self.shared.exit.lock().unwrap();
        let (guard, _) =
            self.shared.exited.wait_timeout_while(guard, timeout, |e| e.is_none()).unwrap();
        *guard
    }

    /// Drain pending OSC 52 requests (oldest first).
    pub fn take_clipboard_requests(&self) -> Vec<ClipboardRequest> {
        std::mem::take(&mut *self.shared.clipboard.lock().unwrap())
    }

    /// Info on the process in the terminal's foreground (from `/proc`).
    pub fn foreground_process(&self) -> Option<ProcessInfo> {
        let pgid = self.pty.lock().unwrap().foreground_pgid()?;
        ProcessInfo::read(pgid)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.pty.lock().unwrap().hangup();
        if let Some(h) = self.io_thread.take() {
            // The reader sees EIO once the child side closes; don't hang the
            // UI if a background job keeps the slave open.
            let deadline = Instant::now() + Duration::from_millis(500);
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }
}

thread_local! {
    /// Terminals locked by this thread via [`Session::terminal`].
    static HELD: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Exclusive access to a session's [`Terminal`]; see [`Session::terminal`].
pub struct TerminalGuard<'a> {
    guard: MutexGuard<'a, Terminal>,
    key: usize,
}

impl std::ops::Deref for TerminalGuard<'_> {
    type Target = Terminal;
    fn deref(&self) -> &Terminal {
        &self.guard
    }
}

impl std::ops::DerefMut for TerminalGuard<'_> {
    fn deref_mut(&mut self) -> &mut Terminal {
        &mut self.guard
    }
}

impl Drop for TerminalGuard<'_> {
    fn drop(&mut self) {
        let key = self.key;
        HELD.with(|h| h.borrow_mut().retain(|&k| k != key));
    }
}

struct IoLoop {
    id: SessionId,
    reader: std::fs::File,
    terminal: Arc<Mutex<Terminal>>,
    writer: Arc<Mutex<std::fs::File>>,
    pty: Arc<Mutex<Pty>>,
    shared: Arc<Shared>,
    publisher: Publisher,
    shell_pid: u32,
    command: Option<(Option<String>, Instant)>,
    root: Option<bool>,
    notify: Option<Notifier>,
}

impl IoLoop {
    fn changed(&self) {
        self.shared.generation.fetch_add(1, Ordering::Release);
        if let Some(n) = &self.notify {
            (n.0)();
        }
    }
}

impl IoLoop {
    fn run(mut self) {
        let mut buf = vec![0u8; 64 * 1024];
        self.check_privilege();
        loop {
            let n = match self.reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if is_eof_error(&e) => break,
                Err(_) => break,
            };
            let (events, responses) = {
                let mut term = self.terminal.lock().unwrap();
                term.advance(&buf[..n]);
                (term.take_events(), term.take_responses())
            };
            self.changed();
            if !responses.is_empty() {
                let _ = self.writer.lock().unwrap().write_all(&responses);
            }
            self.publisher.publish("terminal.output", Some(self.id), json!({"bytes": n}));
            for ev in events {
                self.handle(ev);
            }
        }
        let status = self.pty.lock().unwrap().wait().ok();
        use std::os::unix::process::ExitStatusExt;
        let info = ExitInfo {
            code: status.and_then(|s| s.code()),
            signal: status.and_then(|s| s.signal()),
        };
        self.publisher.publish(
            "session.exited",
            Some(self.id),
            json!({"exit_code": info.code, "signal": info.signal}),
        );
        *self.shared.exit.lock().unwrap() = Some(info);
        self.shared.exited.notify_all();
        self.changed();
    }

    fn handle(&mut self, ev: TerminalEvent) {
        let id = Some(self.id);
        let p = &self.publisher;
        match ev {
            TerminalEvent::Bell => {
                p.publish("terminal.bell", id, json!({}));
            }
            TerminalEvent::TitleChanged(title) => {
                p.publish("terminal.title.changed", id, json!({"title": title}));
            }
            TerminalEvent::CwdChanged(cwd) => {
                p.publish("session.cwd.changed", id, json!({"cwd": cwd}));
            }
            TerminalEvent::AlternateScreen(active) => {
                p.publish("terminal.screen.alternate", id, json!({"active": active}));
            }
            TerminalEvent::PromptStarted => {
                p.publish("shell.prompt", id, json!({}));
                self.check_privilege();
            }
            TerminalEvent::CommandInputStarted | TerminalEvent::ModeChanged => {}
            TerminalEvent::CommandExecuted { command } => {
                p.publish("command.started", id, json!({"command": command}));
                self.command = Some((command, Instant::now()));
                self.check_privilege();
            }
            TerminalEvent::CommandFinished { exit_code } => {
                // A D without a preceding C (e.g. empty prompt) is not a command.
                if let Some((command, start)) = self.command.take() {
                    p.publish(
                        "command.completed",
                        id,
                        json!({
                            "command": command,
                            "exit_code": exit_code,
                            "duration_ms": start.elapsed().as_millis() as u64,
                        }),
                    );
                }
            }
            TerminalEvent::ClipboardStore { selection, data } => {
                p.publish(
                    "clipboard.requested",
                    id,
                    json!({"selection": selection, "op": "store"}),
                );
                if data.len() <= MAX_CLIPBOARD_REQUEST {
                    self.queue_clipboard(ClipboardRequest::Store { selection, data });
                }
            }
            TerminalEvent::ClipboardLoad { selection } => {
                p.publish("clipboard.requested", id, json!({"selection": selection, "op": "load"}));
                self.queue_clipboard(ClipboardRequest::Load { selection });
            }
        }
    }

    fn queue_clipboard(&self, req: ClipboardRequest) {
        let mut q = self.shared.clipboard.lock().unwrap();
        if q.len() >= MAX_PENDING_CLIPBOARD {
            q.remove(0);
        }
        q.push(req);
    }

    /// Spec §11.5: detect privileged sessions from trusted process metadata
    /// (never from terminal output, which any program can forge).
    fn check_privilege(&mut self) {
        let pgid = self.pty.lock().unwrap().foreground_pgid();
        let info = pgid.and_then(ProcessInfo::read).or_else(|| ProcessInfo::read(self.shell_pid));
        if let Some(root) = info.map(|i| i.is_root()) {
            if self.root != Some(root) {
                self.root = Some(root);
                self.publisher.publish("session.privileged", Some(self.id), json!({"root": root}));
            }
        }
    }
}

/// Owns all sessions, independent of any window/tab layout.
pub struct SessionManager {
    bus: EventBus,
    sessions: HashMap<SessionId, Session>,
    next_id: SessionId,
    active: Option<SessionId>,
}

impl SessionManager {
    pub fn new(bus: EventBus) -> Self {
        SessionManager { bus, sessions: HashMap::new(), next_id: 1, active: None }
    }

    pub fn create(&mut self, config: SessionConfig) -> io::Result<SessionId> {
        let id = self.next_id;
        self.next_id += 1;
        let session = Session::spawn(id, config, &self.bus)?;
        self.sessions.insert(id, session);
        self.active.get_or_insert(id);
        Ok(id)
    }

    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    pub fn ids(&self) -> Vec<SessionId> {
        let mut ids: Vec<_> = self.sessions.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn active(&self) -> Option<&Session> {
        self.active.and_then(|id| self.sessions.get(&id))
    }

    pub fn switch_to(&mut self, id: SessionId) -> bool {
        if self.sessions.contains_key(&id) {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    /// Close (hang up) a session.
    pub fn close(&mut self, id: SessionId) -> bool {
        let removed = self.sessions.remove(&id).is_some();
        if self.active == Some(id) {
            self.active = self.ids().first().copied();
        }
        if removed {
            self.bus.publisher("session").publish("session.closed", Some(id), json!({}));
        }
        removed
    }

    /// Drop sessions whose process has exited; returns their ids.
    pub fn reap(&mut self) -> Vec<SessionId> {
        let dead: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.exit_info().is_some())
            .map(|(&id, _)| id)
            .collect();
        for &id in &dead {
            self.close(id);
        }
        dead
    }
}
