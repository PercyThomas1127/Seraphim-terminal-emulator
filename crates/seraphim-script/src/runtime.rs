//! The script thread: owns every extension's Lua state, dispatches bus
//! events to handlers (by priority, with cancellation and throttling),
//! runs timers and asynchronous completions, enforces budgets and reloads
//! extensions whose files change.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use seraphim_events::{Event, EventBus, Pattern, PatternHandle};

use crate::api::{self, Ctx, Shared, TimerAction};
use crate::manifest::Extension;
use crate::perm::Grant;
use crate::sandbox;
use crate::{Command, Settings, TerminalSnapshot, Waker};

/// Bus events queued for the script thread at most; beyond this the
/// forwarder waits and the bus drops (and counts) events for us.
const MAX_QUEUED: usize = 1024;
/// Error notices per extension per minute (the rest go to stderr).
const NOTICES_PER_MINUTE: u32 = 5;

pub(crate) enum Inbox {
    Event(Arc<Event>),
    Invoke { ext: String, command: String },
    Done { ext: String, callback: u64, result: serde_json::Value },
    Replace(Vec<(Extension, Grant)>),
    Shutdown,
}

/// The host's handle on the script thread. Dropping it stops the thread.
pub struct Runtime {
    inbox: Sender<Inbox>,
    commands: Receiver<Command>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    forwarder: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl Runtime {
    /// Start the script thread with these extensions (each with the grant
    /// the user gave it; [`Grant::effective`] is applied here).
    pub fn start(
        extensions: Vec<(Extension, Grant)>,
        settings: Settings,
        bus: &EventBus,
        wake: Waker,
    ) -> Runtime {
        let (inbox, rx) = mpsc::channel();
        let (out, commands) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let sub = bus.subscribe(Pattern::Any(Vec::new()), 4096);
        let patterns = sub.pattern_handle();
        let shared = Arc::new(Shared {
            snapshot: RwLock::new(TerminalSnapshot::default()),
            tracked: Mutex::new(Default::default()),
            reads_terminal: AtomicBool::new(false),
            out,
            wake,
            inbox: inbox.clone(),
            bus: bus.clone(),
            settings,
        });
        // Forward bus events into the inbox with backpressure.
        let stop = Arc::new(AtomicBool::new(false));
        let forwarder = {
            let (stop, inbox, queued) = (stop.clone(), inbox.clone(), queued.clone());
            std::thread::Builder::new()
                .name("seraphim-script-bus".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let Ok(e) = sub.recv_timeout(Duration::from_millis(200)) else { continue };
                        while queued.load(Ordering::Relaxed) >= MAX_QUEUED {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        queued.fetch_add(1, Ordering::Relaxed);
                        if inbox.send(Inbox::Event(e)).is_err() {
                            return;
                        }
                    }
                })
                .expect("spawn script bus thread")
        };
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("seraphim-script".into())
                .spawn(move || {
                    let mut t = Thread {
                        shared,
                        exts: Vec::new(),
                        patterns,
                        queued,
                        last_poll: Instant::now(),
                    };
                    t.replace(extensions);
                    t.run(rx);
                })
                .expect("spawn script thread")
        };
        Runtime {
            inbox,
            commands,
            shared,
            thread: Some(thread),
            forwarder: Some((stop, forwarder)),
        }
    }

    /// Commands queued since the last call.
    pub fn take_commands(&self) -> Vec<Command> {
        self.commands.try_iter().collect()
    }

    /// Run a command an extension registered (menu item or shortcut).
    pub fn invoke(&self, ext: &str, command: &str) {
        let _ = self.inbox.send(Inbox::Invoke { ext: ext.into(), command: command.into() });
    }

    /// Replace every extension (configuration reload).
    pub fn replace(&self, extensions: Vec<(Extension, Grant)>) {
        let _ = self.inbox.send(Inbox::Replace(extensions));
    }

    /// Some running extension may read the terminal: keep the snapshot
    /// up to date.
    pub fn reads_terminal(&self) -> bool {
        self.shared.reads_terminal.load(Ordering::Relaxed)
    }

    /// Update the terminal snapshot extensions read.
    pub fn update_snapshot(&self, f: impl FnOnce(&mut TerminalSnapshot)) {
        f(&mut self.shared.snapshot.write().unwrap());
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.inbox.send(Inbox::Shutdown);
        if let Some((stop, h)) = self.forwarder.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = h.join();
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

/// A running (or failed, kept for live reload) extension.
struct Running {
    ctx: Arc<Ctx>,
    lua: Option<mlua::Lua>,
    deadline: Option<sandbox::Deadline>,
    faults: u32,
    disabled: bool,
    notices: (Instant, u32),
    /// Fingerprint of its files, for live reload.
    stamp: u64,
}

struct Thread {
    shared: Arc<Shared>,
    exts: Vec<Running>,
    patterns: PatternHandle,
    queued: Arc<AtomicUsize>,
    last_poll: Instant,
}

/// Events the runtime follows for `terminal.last_exit_code()` and friends.
const TRACKED: &[&str] =
    &["command.completed", "command.started", "session.cwd.changed", "session.privileged"];

impl Thread {
    fn run(&mut self, rx: Receiver<Inbox>) {
        loop {
            let now = Instant::now();
            let next = self
                .next_due()
                .map_or(Duration::from_millis(500), |d| d.saturating_duration_since(now));
            let next = if self.shared.settings.live_reload {
                next.min(Duration::from_secs(1))
            } else {
                next
            };
            match rx.recv_timeout(next) {
                Ok(Inbox::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.run_timers();
            if self.shared.settings.live_reload
                && self.last_poll.elapsed() >= Duration::from_secs(1)
            {
                self.last_poll = Instant::now();
                self.poll_files();
            }
        }
    }

    fn handle(&mut self, msg: Inbox) {
        match msg {
            Inbox::Event(e) => {
                self.queued.fetch_sub(1, Ordering::Relaxed);
                self.dispatch(e);
            }
            Inbox::Invoke { ext, command } => {
                let Some(i) = self.find(&ext) else { return };
                let f = self.exts[i].ctx.state.lock().unwrap().commands.get(&command).cloned();
                match f {
                    Some(f) => {
                        self.call(i, |_| f.call::<()>(()).map(|_| mlua::Value::Nil));
                    }
                    None => self.notify(i, format!("no command {command:?}"), true),
                }
            }
            Inbox::Done { ext, callback, result } => {
                let Some(i) = self.find(&ext) else { return };
                let f = self.exts[i].ctx.state.lock().unwrap().callbacks.remove(&callback);
                if let Some(f) = f {
                    self.call(i, |lua| {
                        let v = api::to_lua(lua, &result)?;
                        f.call::<()>(v).map(|_| mlua::Value::Nil)
                    });
                }
            }
            Inbox::Replace(list) => self.replace(list),
            Inbox::Shutdown => {}
        }
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.exts.iter().position(|e| e.ctx.name == name && !e.disabled && e.lua.is_some())
    }

    fn replace(&mut self, list: Vec<(Extension, Grant)>) {
        for i in 0..self.exts.len() {
            self.reset_host(i);
        }
        self.exts.clear();
        for (ext, grant) in list {
            let stamp = fingerprint(&ext);
            let r = self.load(ext, grant, stamp);
            self.exts.push(r);
            let i = self.exts.len() - 1;
            self.start_ext(i);
        }
        self.after_change();
    }

    fn load(&self, ext: Extension, grant: Grant, stamp: u64) -> Running {
        let ctx = Arc::new(Ctx::new(ext, grant, self.shared.clone()));
        Running {
            ctx,
            lua: None,
            deadline: None,
            faults: 0,
            disabled: false,
            notices: (Instant::now(), 0),
            stamp,
        }
    }

    /// Build the Lua state and run the main file.
    fn start_ext(&mut self, i: usize) {
        let ctx = self.exts[i].ctx.clone();
        let missing: Vec<&str> =
            ctx.ext.requested.iter().filter(|p| !ctx.grant.has(**p)).map(|p| p.name()).collect();
        if !missing.is_empty() {
            self.notify(
                i,
                format!(
                    "not granted: {} (add them to grant under [extensions.{}])",
                    missing.join(", "),
                    ctx.name
                ),
                false,
            );
        }
        let state = sandbox::new_state(&ctx.name, &ctx.ext.dir, self.shared.settings.memory)
            .and_then(|(lua, d)| api::install(&lua, &ctx).map(|_| (lua, d)));
        let (lua, deadline) = match state {
            Ok(s) => s,
            Err(e) => return self.notify(i, format!("could not start: {e}"), true),
        };
        let source = match std::fs::read_to_string(&ctx.ext.main) {
            Ok(s) => s,
            Err(e) => return self.notify(i, format!("{}: {e}", ctx.ext.main.display()), true),
        };
        self.exts[i].lua = Some(lua);
        self.exts[i].deadline = Some(deadline);
        let name = format!("@{}", ctx.ext.main.display());
        // The main file may set up a lot; it gets a few budgets' worth.
        let ok = self.call_with(i, 4, |lua| {
            lua.load(source)
                .set_mode(mlua::chunk::ChunkMode::Text)
                .set_name(name)
                .exec()
                .map(|_| mlua::Value::Nil)
        });
        if ok.is_none() {
            // A broken main file: nothing of it stays registered.
            self.exts[i].lua = None;
            self.reset_host(i);
        }
    }

    /// Recompute the bus pattern and whether anyone reads the terminal.
    fn after_change(&mut self) {
        let mut ps: Vec<Pattern> = TRACKED.iter().map(|p| Pattern::parse(p)).collect();
        let mut reads = false;
        for e in self.exts.iter().filter(|e| !e.disabled && e.lua.is_some()) {
            reads |= e.ctx.grant.has(crate::Permission::TerminalRead);
            for h in &e.ctx.state.lock().unwrap().handlers {
                if !ps.contains(&h.pattern) {
                    ps.push(h.pattern.clone());
                }
            }
        }
        self.patterns.set(Pattern::Any(ps));
        self.shared.reads_terminal.store(reads, Ordering::Relaxed);
    }

    fn dispatch(&mut self, e: Arc<Event>) {
        self.shared.track(&e);
        let now = Instant::now();
        // (priority, registration order, extension, handler id)
        let mut calls: Vec<(i32, u64, usize, u64)> = Vec::new();
        for (i, r) in self.exts.iter().enumerate() {
            if r.disabled || r.lua.is_none() {
                continue;
            }
            let mut st = r.ctx.state.lock().unwrap();
            for h in st.handlers.iter_mut().filter(|h| h.pattern.matches(&e.name)) {
                if let Some(last) = h.last {
                    if !h.throttle.is_zero() && now < last + h.throttle {
                        // Too soon: deliver the latest one when the window ends.
                        h.pending = Some(e.clone());
                        continue;
                    }
                }
                calls.push((h.priority, h.id, i, h.id));
            }
        }
        calls.sort_by_key(|c| (-c.0, c.1));
        self.deliver(&e, &calls, now);
    }

    /// Call handlers in order; one returning `false` cancels the rest.
    fn deliver(&mut self, e: &Arc<Event>, calls: &[(i32, u64, usize, u64)], now: Instant) {
        for &(_, _, i, id) in calls {
            let f = {
                let mut st = self.exts[i].ctx.state.lock().unwrap();
                match st.handlers.iter_mut().find(|h| h.id == id) {
                    Some(h) => {
                        h.last = Some(now);
                        h.pending = None;
                        h.func.clone()
                    }
                    None => continue, // removed by an earlier handler
                }
            };
            if self.exts[i].disabled {
                continue;
            }
            let r = self.call(i, |lua| {
                let t = api::event_table(lua, e)?;
                f.call::<mlua::Value>(t)
            });
            if matches!(r, Some(mlua::Value::Boolean(false))) {
                break;
            }
        }
    }

    fn call<F>(&mut self, i: usize, f: F) -> Option<mlua::Value>
    where
        F: FnOnce(&mlua::Lua) -> mlua::Result<mlua::Value>,
    {
        self.call_with(i, 1, f)
    }

    /// Run `f` in extension `i`'s state under `budgets` time budgets.
    /// Errors are reported; repeated overruns disable the extension.
    fn call_with<F>(&mut self, i: usize, budgets: u32, f: F) -> Option<mlua::Value>
    where
        F: FnOnce(&mlua::Lua) -> mlua::Result<mlua::Value>,
    {
        let r = &self.exts[i];
        let (Some(lua), Some(d)) = (&r.lua, &r.deadline) else { return None };
        d.start(self.shared.settings.budget * budgets);
        let result = f(lua);
        d.clear();
        let changed = std::mem::take(&mut r.ctx.state.lock().unwrap().patterns_changed);
        if changed {
            self.after_change();
        }
        match result {
            Ok(v) => Some(v),
            Err(e) if sandbox::is_budget_error(&e) => {
                self.exts[i].faults += 1;
                let (faults, max) = (self.exts[i].faults, self.shared.settings.max_faults);
                let ms = self.shared.settings.budget.as_millis() * budgets as u128;
                if faults >= max {
                    self.disable(i, format!("disabled: ran longer than {ms} ms {faults} times"));
                } else {
                    self.notify(i, format!("stopped a handler that ran longer than {ms} ms"), true);
                }
                None
            }
            Err(e) => {
                self.notify(i, describe(&e), true);
                None
            }
        }
    }

    fn disable(&mut self, i: usize, why: String) {
        self.exts[i].disabled = true;
        self.reset_host(i);
        self.notify(i, why, true);
        self.after_change();
    }

    /// Forget everything the extension registered, here and in the host.
    fn reset_host(&mut self, i: usize) {
        let ctx = &self.exts[i].ctx;
        ctx.state.lock().unwrap().clear();
        let _ = self.shared.out.send(Command::Reset { ext: ctx.name.clone() });
        (self.shared.wake)();
    }

    fn notify(&mut self, i: usize, text: String, error: bool) {
        let name = self.exts[i].ctx.name.clone();
        eprintln!("seraphim: extension {name}: {text}");
        let n = &mut self.exts[i].notices;
        if n.0.elapsed() > Duration::from_secs(60) {
            *n = (Instant::now(), 0);
        }
        n.1 += 1;
        if n.1 <= NOTICES_PER_MINUTE {
            let _ = self.shared.out.send(Command::Notify { ext: name, text, error });
            (self.shared.wake)();
        }
    }

    fn next_due(&self) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for r in self.exts.iter().filter(|r| !r.disabled && r.lua.is_some()) {
            let st = r.ctx.state.lock().unwrap();
            let due = st.timers.iter().map(|t| t.due).chain(
                st.handlers
                    .iter()
                    .filter(|h| h.pending.is_some())
                    .filter_map(|h| h.last.map(|l| l + h.throttle)),
            );
            for d in due {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        }
        next
    }

    fn run_timers(&mut self) {
        let now = Instant::now();
        for i in 0..self.exts.len() {
            if self.exts[i].disabled || self.exts[i].lua.is_none() {
                continue;
            }
            // Throttled events whose window has ended.
            let flush: Vec<(Arc<Event>, u64, i32)> = {
                let mut st = self.exts[i].ctx.state.lock().unwrap();
                st.handlers
                    .iter_mut()
                    .filter(|h| {
                        h.pending.is_some() && h.last.is_some_and(|l| now >= l + h.throttle)
                    })
                    .map(|h| (h.pending.take().unwrap(), h.id, h.priority))
                    .collect()
            };
            for (e, id, p) in flush {
                self.deliver(&e, &[(p, id, i, id)], now);
            }
            let due: Vec<TimerAction> = {
                let mut st = self.exts[i].ctx.state.lock().unwrap();
                let mut due = Vec::new();
                st.timers.retain_mut(|t| {
                    if t.due > now {
                        return true;
                    }
                    due.push(t.action.clone());
                    match t.every {
                        Some(every) => {
                            // Skip missed ticks rather than bursting.
                            while t.due <= now {
                                t.due += every;
                            }
                            true
                        }
                        None => false,
                    }
                });
                due
            };
            for a in due {
                if self.exts[i].disabled {
                    break;
                }
                match a {
                    TimerAction::Lua(f) => {
                        self.call(i, |_| f.call::<()>(()).map(|_| mlua::Value::Nil));
                    }
                    TimerAction::Command(c) => {
                        let _ = self.shared.out.send(c);
                        (self.shared.wake)();
                    }
                    TimerAction::Completed(name) => {
                        let ext = self.exts[i].ctx.name.clone();
                        self.shared.bus.publisher("animation").publish(
                            "animation.completed",
                            None,
                            serde_json::json!({ "name": name, "extension": ext }),
                        );
                    }
                }
            }
        }
    }

    /// Live reload: restart extensions whose files changed.
    fn poll_files(&mut self) {
        for i in 0..self.exts.len() {
            let stamp = fingerprint(&self.exts[i].ctx.ext);
            if stamp == self.exts[i].stamp {
                continue;
            }
            let ext = &self.exts[i].ctx.ext;
            let path = if ext.script { ext.main.clone() } else { ext.dir.clone() };
            let grant = self.exts[i].ctx.full_grant.clone();
            self.reset_host(i);
            match Extension::from_path(&path) {
                Ok(new) => {
                    self.exts[i] = self.load(new, grant, stamp);
                    self.start_ext(i);
                    if self.exts[i].lua.is_some() {
                        self.notify(i, "reloaded".into(), false);
                    }
                }
                Err(e) => {
                    self.exts[i].lua = None;
                    self.exts[i].stamp = stamp;
                    self.notify(i, e, true);
                }
            }
        }
        self.after_change();
    }
}

/// An error message without mlua's stack-traceback noise.
fn describe(e: &mlua::Error) -> String {
    let s = match e {
        mlua::Error::CallbackError { cause, .. } => return describe(cause),
        other => other.to_string(),
    };
    let s =
        s.lines().take_while(|l| !l.starts_with("stack traceback")).collect::<Vec<_>>().join(" ");
    s.replace("runtime error: ", "")
}

/// mtimes and sizes of the extension's Lua files and manifest.
fn fingerprint(ext: &Extension) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut files: Vec<PathBuf> = vec![ext.main.clone()];
    if !ext.script {
        files.push(ext.dir.join("extension.toml"));
        collect_lua(&ext.dir, &mut files, 3);
    }
    files.sort();
    files.dedup();
    for f in files {
        f.hash(&mut h);
        if let Ok(m) = std::fs::metadata(&f) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    }
    h.finish()
}

fn collect_lua(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth > 0 {
            collect_lua(&p, out, depth - 1);
        } else if p.extension().is_some_and(|x| x == "lua") {
            out.push(p);
        }
    }
}
