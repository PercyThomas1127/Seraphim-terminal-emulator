//! The Lua API (spec §8.2): the `seraphim`, `visual`, `animation`,
//! `terminal`, `ui`, `system`, `process`, `fs` and `net` tables. A table
//! (or function) is only installed when the extension holds the
//! permission it needs.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use mlua::{Function, Lua, LuaSerdeExt, Table, Value};
use seraphim_events::{Event, EventBus, Pattern, Publisher};
use serde_json::json;

use crate::manifest::{Extension, API_VERSION};
use crate::metrics::{Metric, Sampler};
use crate::perm::{Grant, Permission};
use crate::runtime::Inbox;
use crate::{Command, Motion, Settings, TerminalSnapshot, Waker};

/// Shared by every extension.
pub(crate) struct Shared {
    pub snapshot: RwLock<TerminalSnapshot>,
    pub tracked: Mutex<Tracked>,
    pub reads_terminal: AtomicBool,
    pub out: Sender<Command>,
    pub wake: Waker,
    pub inbox: Sender<Inbox>,
    pub bus: EventBus,
    pub settings: Settings,
}

/// Facts the runtime follows from events.
#[derive(Default)]
pub(crate) struct Tracked {
    pub last_exit_code: Option<i64>,
    pub last_command: Option<String>,
    pub running: Option<String>,
    pub cwd: Option<String>,
    /// Sessions running as root.
    pub roots: std::collections::HashSet<u64>,
}

impl Shared {
    pub fn track(&self, e: &Event) {
        let mut t = self.tracked.lock().unwrap();
        match e.name.as_str() {
            "command.started" => t.running = e.data["command"].as_str().map(str::to_owned),
            "command.completed" => {
                t.running = None;
                t.last_exit_code = e.data["exit_code"].as_i64();
                t.last_command = e.data["command"].as_str().map(str::to_owned);
            }
            "session.cwd.changed" => t.cwd = e.data["cwd"].as_str().map(str::to_owned),
            "session.privileged" => {
                let id = e.session.unwrap_or(0);
                if e.data["root"].as_bool().unwrap_or(false) {
                    t.roots.insert(id);
                } else {
                    t.roots.remove(&id);
                }
            }
            _ => {}
        }
    }

    fn send(&self, c: Command) {
        let _ = self.out.send(c);
        (self.wake)();
    }
}

pub(crate) struct Handler {
    pub id: u64,
    pub pattern: Pattern,
    pub func: Function,
    pub priority: i32,
    pub throttle: Duration,
    pub last: Option<Instant>,
    pub pending: Option<Arc<Event>>,
}

#[derive(Clone)]
pub(crate) enum TimerAction {
    Lua(Function),
    Command(Command),
    /// Publish `animation.completed` for this animation.
    Completed(String),
    /// Re-render an open panel.
    RenderPanel(String),
}

/// A panel an extension registered.
#[derive(Clone)]
pub(crate) struct PanelDef {
    pub render: Function,
    pub on_select: Option<Function>,
    pub on_key: Option<Function>,
    pub interval: Option<Duration>,
    pub open: bool,
}

pub(crate) struct Timer {
    pub id: u64,
    pub due: Instant,
    pub every: Option<Duration>,
    pub action: TimerAction,
    /// The animation this step belongs to.
    pub tag: Option<String>,
}

#[derive(Clone, Debug)]
struct Step {
    layer: Option<String>,
    param: Option<String>,
    color: Option<String>,
    color_name: Option<String>,
    to: f32,
    from: Option<f32>,
    delay: Duration,
    motion: Motion,
}

#[derive(Default)]
pub(crate) struct State {
    pub handlers: Vec<Handler>,
    pub timers: Vec<Timer>,
    pub callbacks: HashMap<u64, Function>,
    pub commands: HashMap<String, Function>,
    pub panels: HashMap<String, PanelDef>,
    animations: HashMap<String, Vec<Step>>,
    next_id: u64,
    sampler: Sampler,
    pub patterns_changed: bool,
}

impl State {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn next_timer_id(&mut self) -> u64 {
        self.id()
    }

    /// Drop every handler, timer and callback (the extension stopped).
    pub fn clear(&mut self) {
        let next = self.next_id;
        *self = State { next_id: next, patterns_changed: true, ..Default::default() };
    }
}

/// One extension's side of the API.
pub(crate) struct Ctx {
    pub name: String,
    pub ext: Extension,
    /// Requested ∩ granted.
    pub grant: Grant,
    /// As granted (kept for reloads, which may request more).
    pub full_grant: Grant,
    pub shared: Arc<Shared>,
    pub state: Mutex<State>,
    publisher: Publisher,
}

impl Ctx {
    pub fn new(ext: Extension, grant: Grant, shared: Arc<Shared>) -> Ctx {
        let publisher = shared.bus.publisher(format!("extension.{}", ext.name));
        Ctx {
            name: ext.name.clone(),
            grant: grant.effective(&ext.requested),
            full_grant: grant,
            ext,
            shared,
            state: Mutex::new(State::default()),
            publisher,
        }
    }

    fn send(&self, c: Command) {
        self.shared.send(c)
    }

    fn data_dir(&self) -> PathBuf {
        self.shared.settings.data_dir.join(&self.name)
    }
}

fn err(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(msg.into())
}

fn ser() -> mlua::serde::ser::Options {
    mlua::serde::ser::Options::new().serialize_none_to_null(false).serialize_unit_to_null(false)
}

pub(crate) fn to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    lua.to_value_with(v, ser())
}

/// A panel's `render` result (a list of widget tables) as JSON; the
/// window validates the widgets.
pub(crate) fn widgets_json(lua: &Lua, v: Value) -> Result<serde_json::Value, String> {
    match v {
        Value::Table(_) => match from_lua(lua, v).map_err(|e| e.to_string())? {
            j @ serde_json::Value::Array(_) => Ok(j),
            serde_json::Value::Object(m) if m.is_empty() => Ok(serde_json::Value::Array(vec![])),
            _ => Err("render must return a list of widgets".into()),
        },
        Value::Nil => Ok(serde_json::Value::Array(vec![])),
        _ => Err("render must return a list of widgets".into()),
    }
}

fn from_lua(lua: &Lua, v: Value) -> mlua::Result<serde_json::Value> {
    lua.from_value_with(v, mlua::serde::de::Options::new().deny_unsupported_types(false))
}

/// `{name, timestamp, source, session, data}`; fields of `data` are also
/// readable on the event itself (`event.text`).
pub(crate) fn event_table(lua: &Lua, e: &Event) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("name", e.name.as_str())?;
    t.set("timestamp", e.timestamp.duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()))?;
    t.set("source", e.source.as_str())?;
    t.set("session", e.session)?;
    let data = to_lua(lua, &e.data)?;
    if let Value::Table(d) = &data {
        let meta = lua.create_table()?;
        meta.set("__index", d.clone())?;
        t.set_metatable(Some(meta))?;
    }
    t.set("data", data)?;
    Ok(t)
}

/// `{duration = ms, easing = "..."}` from an options table.
fn motion(opts: &Option<Table>) -> mlua::Result<Motion> {
    let mut m = Motion::default();
    if let Some(o) = opts {
        if let Some(d) = o.get::<Option<f64>>("duration")? {
            m.duration_ms = d.clamp(0.0, 600_000.0) as u32;
        }
        if let Some(e) = o.get::<Option<String>>("easing")? {
            m.easing = e;
        }
    }
    Ok(m)
}

fn opt<T: mlua::FromLua>(opts: &Option<Table>, key: &str) -> mlua::Result<Option<T>> {
    match opts {
        Some(o) => o.get(key),
        None => Ok(None),
    }
}

/// Install every API table the extension may use.
pub(crate) fn install(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let g = lua.globals();
    g.set("seraphim", core(lua, ctx)?)?;
    let has = |p| ctx.grant.has(p);
    if has(Permission::Visual) {
        g.set("visual", visual(lua, ctx)?)?;
        g.set("animation", animation(lua, ctx)?)?;
    }
    if has(Permission::TerminalRead) || has(Permission::TerminalWrite) {
        g.set("terminal", terminal(lua, ctx)?)?;
    }
    if has(Permission::Interface) {
        g.set("ui", ui(lua, ctx)?)?;
    }
    if has(Permission::Metrics) || has(Permission::System) {
        g.set("system", system(lua, ctx)?)?;
    }
    if has(Permission::ProcessRead) || has(Permission::ProcessExec) {
        g.set("process", process(lua, ctx)?)?;
    }
    if has(Permission::Filesystem) {
        g.set("fs", fs(lua, ctx)?)?;
    }
    if has(Permission::Network) {
        g.set("net", net(lua, ctx)?)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- seraphim

fn core(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("name", ctx.name.as_str())?;
    t.set("version", env!("CARGO_PKG_VERSION"))?;
    t.set("api_version", API_VERSION)?;
    let perms: Vec<&str> = ctx.grant.permissions.iter().map(|p| p.name()).collect();
    t.set("permissions", perms)?;
    t.set("settings", to_lua(lua, &ctx.grant.settings)?)?;

    let c = ctx.clone();
    t.set(
        "has",
        lua.create_function(move |_, p: String| {
            Ok(Permission::parse(&p).is_some_and(|p| c.grant.has(p)))
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "on",
        lua.create_function(move |_, (pattern, f, opts): (String, Function, Option<Table>)| {
            let priority = opt::<i32>(&opts, "priority")?.unwrap_or(0);
            let throttle = opt::<f64>(&opts, "throttle")?.unwrap_or(0.0).max(0.0);
            let mut st = c.state.lock().unwrap();
            let id = st.id();
            st.handlers.push(Handler {
                id,
                pattern: Pattern::parse(&pattern),
                func: f,
                priority,
                throttle: Duration::from_millis(throttle as u64),
                last: None,
                pending: None,
            });
            st.patterns_changed = true;
            Ok(id)
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "off",
        lua.create_function(move |_, id: u64| {
            let mut st = c.state.lock().unwrap();
            let before = st.handlers.len();
            st.handlers.retain(|h| h.id != id);
            st.patterns_changed = true;
            Ok(st.handlers.len() != before)
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "emit",
        lua.create_function(move |lua, (name, data): (String, Option<Value>)| {
            let own = format!("{}.", c.name);
            if !name.starts_with(&own) || name.len() == own.len() {
                return Err(err(format!(
                    "emit: extension events are named {own}<something> (got {name:?})"
                )));
            }
            let data = match data {
                Some(v) => from_lua(lua, v)?,
                None => json!({}),
            };
            Ok(c.publisher.publish(name, None, data))
        })?,
    )?;

    for (key, repeat) in [("after", false), ("every", true)] {
        let c = ctx.clone();
        t.set(
            key,
            lua.create_function(move |_, (ms, f): (f64, Function)| {
                let ms = if repeat { ms.max(16.0) } else { ms.max(0.0) };
                let d = Duration::from_micros((ms * 1000.0) as u64);
                let mut st = c.state.lock().unwrap();
                let id = st.id();
                st.timers.push(Timer {
                    id,
                    due: Instant::now() + d,
                    every: repeat.then_some(d),
                    action: TimerAction::Lua(f),
                    tag: None,
                });
                Ok(id)
            })?,
        )?;
    }

    let c = ctx.clone();
    t.set(
        "cancel",
        lua.create_function(move |_, id: u64| {
            let mut st = c.state.lock().unwrap();
            let before = st.timers.len();
            st.timers.retain(|t| t.id != id);
            Ok(st.timers.len() != before)
        })?,
    )?;

    let name = ctx.name.clone();
    t.set(
        "log",
        lua.create_function(move |_, args: mlua::Variadic<Value>| {
            let parts: Vec<String> =
                args.iter().map(|v| v.to_string().unwrap_or_else(|_| "?".into())).collect();
            eprintln!("seraphim: [{name}] {}", parts.join(" "));
            Ok(())
        })?,
    )?;

    t.set("storage", storage(lua, ctx)?)?;
    Ok(t)
}

/// Small persistent key-value store in the extension's data folder.
fn storage(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let file = ctx.data_dir().join("storage.json");
    let load = {
        let file = file.clone();
        move || -> serde_json::Map<String, serde_json::Value> {
            std::fs::read_to_string(&file)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        }
    };
    let l = load.clone();
    t.set(
        "get",
        lua.create_function(move |lua, key: String| {
            to_lua(lua, l().get(&key).unwrap_or(&serde_json::Value::Null))
        })?,
    )?;
    t.set(
        "set",
        lua.create_function(move |lua, (key, v): (String, Value)| {
            let mut all = load();
            let v = from_lua(lua, v)?;
            if v.is_null() {
                all.remove(&key);
            } else {
                all.insert(key, v);
            }
            let text = serde_json::to_string_pretty(&all).map_err(|e| err(e.to_string()))?;
            if text.len() > 1 << 20 {
                return Err(err("storage: more than 1 MiB"));
            }
            if let Some(d) = file.parent() {
                std::fs::create_dir_all(d).map_err(|e| err(format!("storage: {e}")))?;
            }
            std::fs::write(&file, text).map_err(|e| err(format!("storage: {e}")))?;
            Ok(())
        })?,
    )?;
    Ok(t)
}

// ---------------------------------------------------------------- visual

fn visual(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;

    let c = ctx.clone();
    t.set(
        "set_parameter",
        lua.create_function(move |_, (param, value, opts): (String, f32, Option<Table>)| {
            c.send(Command::SetParam {
                ext: c.name.clone(),
                layer: opt(&opts, "layer")?,
                param,
                value,
                from: opt(&opts, "from")?,
                motion: motion(&opts)?,
            });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "set_opacity",
        lua.create_function(move |_, (layer, value, opts): (String, f32, Option<Table>)| {
            c.send(Command::SetOpacity {
                ext: c.name.clone(),
                layer,
                value,
                from: opt(&opts, "from")?,
                motion: motion(&opts)?,
            });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "set_color",
        lua.create_function(move |_, (color, opts): (String, Option<Table>)| {
            c.send(Command::SetColor {
                ext: c.name.clone(),
                layer: opt(&opts, "layer")?,
                name: opt(&opts, "name")?,
                color,
                motion: motion(&opts)?,
            });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "add_layer",
        lua.create_function(move |lua, (layer, opts): (Table, Option<Table>)| {
            let id: Option<String> = layer.get("id")?;
            if id.as_deref().is_none_or(str::is_empty) {
                return Err(err("add_layer: the layer needs an id"));
            }
            let spec = from_lua(lua, Value::Table(layer))?;
            c.send(Command::AddLayer { ext: c.name.clone(), layer: spec, motion: motion(&opts)? });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "remove_layer",
        lua.create_function(move |_, (id, opts): (String, Option<Table>)| {
            c.send(Command::RemoveLayer { ext: c.name.clone(), id, motion: motion(&opts)? });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "set_background",
        lua.create_function(move |_, name: String| {
            c.send(Command::SetProfile { ext: c.name.clone(), name });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "bind",
        lua.create_function(move |lua, (target, spec): (String, Table)| {
            let json = bind_spec(lua, &target, spec)?;
            let id = c.state.lock().unwrap().id();
            c.send(Command::Bind { ext: c.name.clone(), id, spec: json });
            Ok(id)
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "unbind",
        lua.create_function(move |_, id: u64| {
            c.send(Command::Unbind { ext: c.name.clone(), id });
            Ok(())
        })?,
    )?;
    Ok(t)
}

/// `visual.bind("layer.param", {source = ..., min, max, mapping, ...})` as
/// a `[[bind]]` table. Targets: `layer.param`, `layer.opacity`,
/// `layer.color` (first color) or `layer.color.name`.
fn bind_spec(lua: &Lua, target: &str, spec: Table) -> mlua::Result<serde_json::Value> {
    let (layer, param) = target
        .split_once('.')
        .filter(|(l, p)| !l.is_empty() && !p.is_empty())
        .ok_or_else(|| err(format!("bind: target {target:?} must be layer.parameter")))?;
    let mut out = serde_json::Map::new();
    out.insert("layer".into(), json!(layer));
    for key in ["source", "event", "field"] {
        if let Some(v) = spec.get::<Option<String>>(key)? {
            out.insert(key.into(), json!(v));
        }
    }
    let min: Option<f32> = spec.get("min")?;
    let max: Option<f32> = spec.get("max")?;
    if min.is_some() || max.is_some() {
        out.insert("output".into(), json!([min.unwrap_or(0.0), max.unwrap_or(1.0)]));
    }
    if let Some(input) = spec.get::<Option<Vec<f32>>>("input")? {
        out.insert("input".into(), json!(input));
    }
    if let Some(ms) = spec.get::<Option<u32>>("smoothing")? {
        out.insert("smoothing_ms".into(), json!(ms));
    }
    if let Some(ms) = spec.get::<Option<u32>>("interval")? {
        out.insert("interval_ms".into(), json!(ms));
    }
    let mut numbers = serde_json::Map::new();
    let mut colors = serde_json::Map::new();
    if let Some(m) = spec.get::<Option<Table>>("mapping")? {
        for pair in m.pairs::<Value, Value>() {
            let (k, v) = pair?;
            let key = match &k {
                Value::String(s) => s.to_str()?.to_owned(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) if n.fract() == 0.0 => (*n as i64).to_string(),
                Value::Number(n) => n.to_string(),
                Value::Boolean(b) => b.to_string(),
                _ => return Err(err("bind: mapping keys are strings, numbers or booleans")),
            };
            match v {
                Value::String(s) => {
                    colors.insert(key, json!(s.to_str()?.to_owned()));
                }
                Value::Integer(i) => {
                    numbers.insert(key, json!(i as f64));
                }
                Value::Number(n) => {
                    numbers.insert(key, json!(n));
                }
                _ => return Err(err("bind: mapping values are numbers or colors")),
            }
        }
    }
    if !numbers.is_empty() && !colors.is_empty() {
        return Err(err("bind: a mapping holds either numbers or colors"));
    }
    let is_color = param == "color" || param.starts_with("color.");
    if is_color {
        if !numbers.is_empty() {
            return Err(err("bind: a color target maps to colors (\"#rrggbb\")"));
        }
        out.insert("param".into(), json!(param));
        out.insert("colors".into(), serde_json::Value::Object(colors));
    } else {
        if !colors.is_empty() {
            return Err(err(format!("bind: {param} is a number; map to numbers")));
        }
        out.insert("param".into(), json!(param));
        if !numbers.is_empty() {
            out.insert("map".into(), serde_json::Value::Object(numbers));
        }
    }
    let _ = lua;
    Ok(serde_json::Value::Object(out))
}

// ---------------------------------------------------------------- animation

fn step(t: &Table) -> mlua::Result<Step> {
    let o = Some(t.clone());
    let delay: f64 = t.get::<Option<f64>>("delay")?.unwrap_or(0.0).max(0.0);
    Ok(Step {
        layer: t.get("layer")?,
        param: t.get("param")?,
        color: t.get("color")?,
        color_name: t.get("name")?,
        to: t.get::<Option<f32>>("to")?.unwrap_or(0.0),
        from: t.get("from")?,
        delay: Duration::from_millis(delay as u64),
        motion: motion(&o)?,
    })
}

impl Step {
    fn command(&self, ext: &str) -> Result<Command, String> {
        if let Some(color) = &self.color {
            return Ok(Command::SetColor {
                ext: ext.into(),
                layer: self.layer.clone(),
                name: self.color_name.clone(),
                color: color.clone(),
                motion: self.motion.clone(),
            });
        }
        let param = self.param.clone().ok_or("a step needs param (or color)")?;
        Ok(if param == "opacity" {
            Command::SetOpacity {
                ext: ext.into(),
                layer: self.layer.clone().ok_or("an opacity step needs a layer")?,
                value: self.to,
                from: self.from,
                motion: self.motion.clone(),
            }
        } else {
            Command::SetParam {
                ext: ext.into(),
                layer: self.layer.clone(),
                param,
                value: self.to,
                from: self.from,
                motion: self.motion.clone(),
            }
        })
    }
}

fn animation(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;

    let c = ctx.clone();
    t.set(
        "tween",
        lua.create_function(
            move |_, (layer, param, to, opts): (Option<String>, String, f32, Option<Table>)| {
                let s = Step {
                    layer,
                    param: Some(param),
                    color: None,
                    color_name: None,
                    to,
                    from: opt(&opts, "from")?,
                    delay: Duration::ZERO,
                    motion: motion(&opts)?,
                };
                c.send(s.command(&c.name).map_err(err)?);
                Ok(())
            },
        )?,
    )?;

    let c = ctx.clone();
    t.set(
        "define",
        lua.create_function(move |_, (name, steps): (String, Table)| {
            let mut list = Vec::new();
            for s in steps.sequence_values::<Table>() {
                let s = step(&s?)?;
                s.command(&c.name).map_err(|e| err(format!("animation {name:?}: {e}")))?;
                list.push(s);
            }
            c.state.lock().unwrap().animations.insert(name, list);
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "play",
        lua.create_function(move |_, name: String| {
            let mut st = c.state.lock().unwrap();
            let Some(steps) = st.animations.get(&name).cloned() else {
                return Err(err(format!(
                    "animation.play: no animation {name:?} (define it first)"
                )));
            };
            // Restarting an animation drops its steps still pending.
            st.timers.retain(|t| t.tag.as_deref() != Some(name.as_str()));
            let now = Instant::now();
            let mut end = Duration::ZERO;
            for s in &steps {
                let cmd = s.command(&c.name).map_err(err)?;
                end = end.max(s.delay + Duration::from_millis(s.motion.duration_ms as u64));
                if s.delay.is_zero() {
                    let _ = c.shared.out.send(cmd);
                } else {
                    let id = st.id();
                    st.timers.push(Timer {
                        id,
                        due: now + s.delay,
                        every: None,
                        action: TimerAction::Command(cmd),
                        tag: Some(name.clone()),
                    });
                }
            }
            let id = st.id();
            st.timers.push(Timer {
                id,
                due: now + end,
                every: None,
                action: TimerAction::Completed(name.clone()),
                tag: Some(name),
            });
            (c.shared.wake)();
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "stop",
        lua.create_function(move |_, name: String| {
            c.state.lock().unwrap().timers.retain(|t| t.tag.as_deref() != Some(name.as_str()));
            Ok(())
        })?,
    )?;
    Ok(t)
}

// ---------------------------------------------------------------- terminal

fn terminal(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    if ctx.grant.has(Permission::TerminalRead) {
        let snap = |c: &Arc<Ctx>| c.shared.snapshot.read().unwrap().clone();
        macro_rules! getter {
            ($name:literal, |$lua:ident, $c:ident| $body:expr) => {{
                let $c = ctx.clone();
                t.set($name, lua.create_function(move |$lua, ()| $body)?)?;
            }};
        }
        getter!("text", |_l, c| Ok(snap(&c).lines.join("\n")));
        getter!("size", |l, c| {
            let s = snap(&c);
            to_lua(l, &json!({ "cols": s.cols, "rows": s.rows }))
        });
        getter!("cursor", |l, c| {
            let s = snap(&c);
            to_lua(l, &json!({ "row": s.cursor.0 + 1, "col": s.cursor.1 + 1 }))
        });
        getter!("title", |_l, c| Ok(snap(&c).title));
        getter!("alt_screen", |_l, c| Ok(snap(&c).alt_screen));
        getter!("selection", |_l, c| Ok(snap(&c).selection));
        getter!("root", |_l, c| Ok(snap(&c).root));
        getter!("cwd", |_l, c| Ok(c.shared.tracked.lock().unwrap().cwd.clone()));
        getter!("last_exit_code", |_l, c| Ok(c.shared.tracked.lock().unwrap().last_exit_code));
        getter!("last_command", |_l, c| Ok(c.shared.tracked.lock().unwrap().last_command.clone()));
        getter!("running_command", |_l, c| Ok(c.shared.tracked.lock().unwrap().running.clone()));
        let c = ctx.clone();
        t.set(
            "lines",
            lua.create_function(move |_, (first, last): (Option<i64>, Option<i64>)| {
                let s = snap(&c);
                let n = s.lines.len() as i64;
                let a = first.unwrap_or(1).max(1);
                let b = last.unwrap_or(n).min(n);
                Ok((a..=b).map(|i| s.lines[(i - 1) as usize].clone()).collect::<Vec<_>>())
            })?,
        )?;
        let c = ctx.clone();
        t.set(
            "line",
            lua.create_function(move |_, n: i64| {
                let s = snap(&c);
                Ok(usize::try_from(n - 1).ok().and_then(|i| s.lines.get(i).cloned()))
            })?,
        )?;
    }
    if ctx.grant.has(Permission::TerminalWrite) {
        let c = ctx.clone();
        t.set(
            "send",
            lua.create_function(move |_, text: String| {
                // An extension never types into a root shell: that would
                // hand it the session's privileges (spec §8.5).
                let root = !c.shared.tracked.lock().unwrap().roots.is_empty()
                    || c.shared.snapshot.read().unwrap().root;
                if root {
                    return Err(err("terminal.send: refused, the session runs as root"));
                }
                if text.len() > 64 << 10 {
                    return Err(err("terminal.send: more than 64 KiB"));
                }
                c.send(Command::Send { ext: c.name.clone(), text });
                Ok(())
            })?,
        )?;
    }
    Ok(t)
}

// ---------------------------------------------------------------- ui

fn ui(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;

    let c = ctx.clone();
    t.set(
        "notify",
        lua.create_function(move |_, text: String| {
            c.send(Command::Notify { ext: c.name.clone(), text, error: false });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "command",
        lua.create_function(move |_, (id, spec): (String, Value)| {
            let (title, run) = match spec {
                Value::Function(f) => (id.clone(), f),
                Value::Table(t) => (
                    t.get::<Option<String>>("title")?.unwrap_or_else(|| id.clone()),
                    t.get::<Function>("run")
                        .map_err(|_| err("ui.command: run must be a function"))?,
                ),
                _ => return Err(err("ui.command(id, function) or ui.command(id, {title=, run=})")),
            };
            c.state.lock().unwrap().commands.insert(id.clone(), run);
            c.send(Command::RegisterCommand { ext: c.name.clone(), id, title });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "key",
        lua.create_function(move |_, (chord, target): (String, Value)| {
            let command = match target {
                Value::String(s) => s.to_str()?.to_owned(),
                Value::Function(f) => {
                    let id = format!("key:{chord}");
                    c.state.lock().unwrap().commands.insert(id.clone(), f);
                    id
                }
                _ => return Err(err("ui.key(chord, command_id or function)")),
            };
            c.send(Command::RegisterKey { ext: c.name.clone(), chord, command });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "panel",
        lua.create_function(move |_, (id, spec): (String, Table)| {
            let render: Function =
                spec.get("render").map_err(|_| err("ui.panel: render must be a function"))?;
            let title = spec.get::<Option<String>>("title")?.unwrap_or_else(|| id.clone());
            let interval = spec
                .get::<Option<f64>>("interval")?
                .map(|ms| Duration::from_millis(ms.max(100.0) as u64));
            let def = PanelDef {
                render,
                on_select: spec.get("on_select")?,
                on_key: spec.get("on_key")?,
                interval,
                open: false,
            };
            c.state.lock().unwrap().panels.insert(id.clone(), def);
            c.send(Command::RegisterPanel { ext: c.name.clone(), id, title });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "refresh",
        lua.create_function(move |lua, id: String| {
            let def = c.state.lock().unwrap().panels.get(&id).cloned();
            let def = def.ok_or_else(|| err(format!("ui.refresh: no panel {id:?}")))?;
            if !def.open {
                return Ok(());
            }
            let v = def.render.call::<Value>(())?;
            let widgets = widgets_json(lua, v).map_err(err)?;
            c.send(Command::PanelContent { ext: c.name.clone(), id, widgets });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "open_panel",
        lua.create_function(move |_, (id, opts): (String, Option<Table>)| {
            if !c.state.lock().unwrap().panels.contains_key(&id) {
                return Err(err(format!("ui.open_panel: no panel {id:?} (ui.panel first)")));
            }
            let place = opt::<String>(&opts, "place")?.unwrap_or_else(|| "right".into());
            if !["right", "down", "tab", "float"].contains(&place.as_str()) {
                return Err(err("ui.open_panel: place is right, down, tab or float"));
            }
            c.send(Command::OpenPanel { ext: c.name.clone(), id, place });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "open_layout",
        lua.create_function(move |_, name: String| {
            c.send(Command::OpenLayout { ext: c.name.clone(), name });
            Ok(())
        })?,
    )?;

    let c = ctx.clone();
    t.set(
        "action",
        lua.create_function(move |_, name: String| {
            c.send(Command::Action { ext: c.name.clone(), name });
            Ok(())
        })?,
    )?;
    Ok(t)
}

// ---------------------------------------------------------------- system

fn system(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    if ctx.grant.has(Permission::Metrics) {
        for (name, m) in [
            ("cpu", Metric::Cpu),
            ("memory", Metric::Memory),
            ("load", Metric::Load),
            ("hour", Metric::Hour),
            ("temperature", Metric::CpuTemp),
        ] {
            let c = ctx.clone();
            t.set(
                name,
                lua.create_function(move |_, ()| {
                    Ok(c.state.lock().unwrap().sampler.read(m, Instant::now()))
                })?,
            )?;
        }
        // Tables of related readings; fields are nil where unknown.
        for (name, fields) in [
            (
                "gpu",
                &[
                    ("busy", Metric::Gpu),
                    ("memory", Metric::GpuMemory),
                    ("temperature", Metric::GpuTemp),
                ][..],
            ),
            (
                "network",
                &[("rx", Metric::NetRx), ("tx", Metric::NetTx), ("online", Metric::NetOnline)][..],
            ),
            ("disk", &[("read", Metric::DiskRead), ("write", Metric::DiskWrite)][..]),
        ] {
            let c = ctx.clone();
            t.set(
                name,
                lua.create_function(move |l, ()| {
                    let mut st = c.state.lock().unwrap();
                    let now = Instant::now();
                    let out = l.create_table()?;
                    for (k, m) in fields {
                        let v = st.sampler.read(*m, now);
                        if *m == Metric::NetOnline {
                            out.set(*k, v.map(|v| v > 0.5))?;
                        } else {
                            out.set(*k, v)?;
                        }
                    }
                    Ok(Value::Table(out))
                })?,
            )?;
        }
        let c = ctx.clone();
        t.set(
            "battery",
            lua.create_function(move |l, ()| {
                let mut st = c.state.lock().unwrap();
                let now = Instant::now();
                match st.sampler.read(Metric::BatteryLevel, now) {
                    Some(level) => {
                        let charging = st.sampler.read(Metric::BatteryCharging, now) == Some(1.0);
                        to_lua(l, &json!({ "level": level, "charging": charging }))
                    }
                    None => Ok(Value::Nil),
                }
            })?,
        )?;
    }
    if ctx.grant.has(Permission::System) {
        t.set(
            "notify",
            lua.create_function(|_, (title, body): (String, Option<String>)| {
                Ok(crate::desktop::notify(&title, body.as_deref().unwrap_or("")))
            })?,
        )?;
    }
    Ok(t)
}

// ---------------------------------------------------------------- process

fn process(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    if ctx.grant.has(Permission::ProcessRead) {
        for (name, fg) in [("foreground", true), ("shell", false)] {
            let c = ctx.clone();
            t.set(
                name,
                lua.create_function(move |l, ()| {
                    let s = c.shared.snapshot.read().unwrap();
                    match if fg { &s.foreground } else { &s.shell } {
                        Some(p) => {
                            to_lua(l, &json!({ "pid": p.pid, "name": p.name, "cwd": p.cwd }))
                        }
                        None => Ok(Value::Nil),
                    }
                })?,
            )?;
        }
    }
    if ctx.grant.has(Permission::ProcessExec) {
        let c = ctx.clone();
        t.set(
            "run",
            lua.create_function(move |_, (argv, a, b): (Vec<String>, Value, Value)| {
                let (opts, cb) = match (a, b) {
                    (Value::Function(f), _) => (None, Some(f)),
                    (Value::Table(t), Value::Function(f)) => (Some(t), Some(f)),
                    (Value::Table(t), _) => (Some(t), None),
                    _ => (None, None),
                };
                let program = argv.first().ok_or_else(|| err("process.run: empty command"))?;
                if !c.grant.command_allowed(program) {
                    return Err(err(format!(
                        "process.run: {program:?} is not in commands under [extensions.{}]",
                        c.name
                    )));
                }
                let timeout =
                    Duration::from_millis(opt::<u64>(&opts, "timeout")?.unwrap_or(10_000));
                let cwd: Option<String> = opt(&opts, "cwd")?;
                let input: Option<String> = opt(&opts, "input")?;
                let callback = cb.map(|f| {
                    let mut st = c.state.lock().unwrap();
                    let id = st.id();
                    st.callbacks.insert(id, f);
                    id
                });
                let (inbox, ext, argv) = (c.shared.inbox.clone(), c.name.clone(), argv.clone());
                std::thread::Builder::new()
                    .name("seraphim-script-run".into())
                    .spawn(move || {
                        let result = run_process(&argv, cwd.as_deref(), input, timeout);
                        if let Some(callback) = callback {
                            let _ = inbox.send(Inbox::Done { ext, callback, result });
                        }
                    })
                    .map_err(|e| err(e.to_string()))?;
                Ok(())
            })?,
        )?;
    }
    Ok(t)
}

/// Run a program; `{ok, code, stdout, stderr}` or `{ok = false, error}`.
fn run_process(
    argv: &[String],
    cwd: Option<&str>,
    input: Option<String>,
    timeout: Duration,
) -> serde_json::Value {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    const LIMIT: u64 = 1 << 20;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("{}: {e}", argv[0]) }),
    };
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || stdin.write_all(text.as_bytes()));
    }
    let read = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(r) = r {
                let _ = r.take(LIMIT).read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = read(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let errs = read(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
    };
    let (stdout, stderr) = (out.join().unwrap_or_default(), errs.join().unwrap_or_default());
    match status {
        Some(s) => {
            json!({ "ok": s.success(), "code": s.code(), "stdout": stdout, "stderr": stderr })
        }
        None => json!({ "ok": false, "error": "timed out", "stdout": stdout, "stderr": stderr }),
    }
}

// ---------------------------------------------------------------- fs

/// Lexically normalized absolute path (`..` resolved, `~` expanded).
fn normalize(p: &str, base: &Path) -> Option<PathBuf> {
    let p = match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => base.join(p),
    };
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out)
}

/// The real location of `p` (symlinks resolved through its deepest
/// existing ancestor), so a link cannot lead out of an approved folder.
fn resolve(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name().map(|n| n.to_owned()), existing.parent()) {
            (Some(n), Some(parent)) => {
                rest.push(n);
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut real = existing.canonicalize().unwrap_or(existing);
    for n in rest.into_iter().rev() {
        real.push(n);
    }
    real
}

fn fs(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let data = ctx.data_dir();
    let mut writable: Vec<PathBuf> = vec![data.clone()];
    writable.extend(ctx.grant.paths.iter().cloned());
    let readable_extra = vec![ctx.ext.dir.clone()];
    let check = {
        let data = data.clone();
        move |path: &str, write: bool| -> mlua::Result<PathBuf> {
            let p = normalize(path, &data).ok_or_else(|| err("fs: bad path"))?;
            let real = resolve(&p);
            let ok = |roots: &[PathBuf]| roots.iter().any(|r| real.starts_with(resolve(r)));
            if ok(&writable) || (!write && ok(&readable_extra)) {
                Ok(real)
            } else {
                Err(err(format!("fs: {path} is outside the approved paths")))
            }
        }
    };
    let check = Arc::new(check);

    let k = check.clone();
    t.set(
        "read",
        lua.create_function(move |_, path: String| {
            let p = k(&path, false)?;
            let meta = std::fs::metadata(&p).map_err(|e| err(format!("fs.read: {path}: {e}")))?;
            if meta.len() > 16 << 20 {
                return Err(err(format!("fs.read: {path}: larger than 16 MiB")));
            }
            std::fs::read_to_string(&p).map_err(|e| err(format!("fs.read: {path}: {e}")))
        })?,
    )?;
    for (name, append) in [("write", false), ("append", true)] {
        let k = check.clone();
        t.set(
            name,
            lua.create_function(move |_, (path, text): (String, String)| {
                use std::io::Write;
                let p = k(&path, true)?;
                if let Some(d) = p.parent() {
                    std::fs::create_dir_all(d).map_err(|e| err(format!("fs: {path}: {e}")))?;
                }
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(append)
                    .truncate(!append)
                    .open(&p)
                    .map_err(|e| err(format!("fs: {path}: {e}")))?;
                f.write_all(text.as_bytes()).map_err(|e| err(format!("fs: {path}: {e}")))?;
                Ok(true)
            })?,
        )?;
    }
    let k = check.clone();
    t.set(
        "list",
        lua.create_function(move |_, path: String| {
            let p = k(&path, false)?;
            let mut names: Vec<String> = std::fs::read_dir(&p)
                .map_err(|e| err(format!("fs.list: {path}: {e}")))?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Ok(names)
        })?,
    )?;
    let k = check.clone();
    t.set(
        "exists",
        lua.create_function(move |_, path: String| {
            Ok(k(&path, false).map(|p| p.exists()).unwrap_or(false))
        })?,
    )?;
    let k = check;
    t.set(
        "remove",
        lua.create_function(move |_, path: String| {
            let p = k(&path, true)?;
            std::fs::remove_file(&p).map_err(|e| err(format!("fs.remove: {path}: {e}")))?;
            Ok(true)
        })?,
    )?;
    t.set("data_dir", data.to_string_lossy().into_owned())?;
    Ok(t)
}

// ---------------------------------------------------------------- net

/// Host of an `http(s)://` URL.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    let host = if let Some(v6) = hostport.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        hostport.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn net(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let c = ctx.clone();
    let request = lua.create_function(move |_, (opts, cb): (Table, Option<Function>)| {
        let url: String = opts.get("url")?;
        let host = url_host(&url).ok_or_else(|| err("net: only http:// and https:// URLs"))?;
        if !c.grant.host_allowed(&host) {
            return Err(err(format!("net: {host} is not in hosts under [extensions.{}]", c.name)));
        }
        let method =
            opts.get::<Option<String>>("method")?.unwrap_or_else(|| "GET".into()).to_uppercase();
        let body: Option<String> = opts.get("body")?;
        let timeout = Duration::from_millis(opts.get::<Option<u64>>("timeout")?.unwrap_or(15_000));
        let mut headers = Vec::new();
        if let Some(h) = opts.get::<Option<Table>>("headers")? {
            for pair in h.pairs::<String, String>() {
                headers.push(pair?);
            }
        }
        let callback = cb.map(|f| {
            let mut st = c.state.lock().unwrap();
            let id = st.id();
            st.callbacks.insert(id, f);
            id
        });
        let (inbox, ext) = (c.shared.inbox.clone(), c.name.clone());
        std::thread::Builder::new()
            .name("seraphim-script-net".into())
            .spawn(move || {
                let result = http(&method, &url, &headers, body, timeout);
                if let Some(callback) = callback {
                    let _ = inbox.send(Inbox::Done { ext, callback, result });
                }
            })
            .map_err(|e| err(e.to_string()))?;
        Ok(())
    })?;
    t.set("request", request.clone())?;
    let r = request.clone();
    t.set(
        "get",
        lua.create_function(move |lua, (url, cb): (String, Option<Function>)| {
            let o = lua.create_table()?;
            o.set("url", url)?;
            r.call::<()>((o, cb))
        })?,
    )?;
    let r = request;
    t.set(
        "post",
        lua.create_function(move |lua, (url, body, cb): (String, String, Option<Function>)| {
            let o = lua.create_table()?;
            o.set("url", url)?;
            o.set("method", "POST")?;
            o.set("body", body)?;
            r.call::<()>((o, cb))
        })?,
    )?;
    Ok(t)
}

/// One request; redirects are not followed (the target host would need
/// its own approval). `{ok, status, body, headers}` or `{ok = false, error}`.
fn http(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<String>,
    timeout: Duration,
) -> serde_json::Value {
    use std::io::Read;
    let proxy = url_host(url).is_some_and(|h| {
        !bypass_proxy(
            &h,
            std::env::var("NO_PROXY").ok().or_else(|| std::env::var("no_proxy").ok()).as_deref(),
        )
    });
    let agent =
        ureq::AgentBuilder::new().redirects(0).timeout(timeout).try_proxy_from_env(proxy).build();
    let mut req = agent.request(method, url).set("User-Agent", "seraphim");
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let resp = match body {
        Some(b) => req.send_string(&b),
        None => req.call(),
    };
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return json!({ "ok": false, "error": e.to_string() }),
    };
    let status = resp.status();
    let mut hs = serde_json::Map::new();
    for name in resp.headers_names() {
        if let Some(v) = resp.header(&name) {
            hs.insert(name.to_ascii_lowercase(), json!(v));
        }
    }
    let mut buf = Vec::new();
    let _ = resp.into_reader().take(4 << 20).read_to_end(&mut buf);
    json!({
        "ok": (200..300).contains(&status),
        "status": status,
        "headers": hs,
        "body": String::from_utf8_lossy(&buf),
    })
}

/// Loopback hosts and `NO_PROXY` entries (`.example.com`, `example.com`,
/// `*`) go direct.
fn bypass_proxy(host: &str, no_proxy: Option<&str>) -> bool {
    if host == "localhost" || host == "::1" || host.starts_with("127.") {
        return true;
    }
    no_proxy.unwrap_or("").split(',').map(|e| e.trim().to_ascii_lowercase()).any(|e| {
        let e = e.trim_start_matches("*.").trim_start_matches('.');
        !e.is_empty() && (e == "*" || host == e || host.ends_with(&format!(".{e}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_bypass() {
        assert!(bypass_proxy("127.0.0.1", None));
        assert!(bypass_proxy("a.corp.net", Some("x.org, .corp.net")));
        assert!(bypass_proxy("corp.net", Some("corp.net")));
        assert!(!bypass_proxy("example.org", Some("x.org")));
        assert!(!bypass_proxy("example.org", None));
    }

    #[test]
    fn hosts_from_urls() {
        assert_eq!(url_host("https://Api.Example.com/x?y").as_deref(), Some("api.example.com"));
        assert_eq!(url_host("http://user:pw@evil.com@good.com:8080/").as_deref(), Some("good.com"));
        assert_eq!(url_host("http://[::1]:80/").as_deref(), Some("::1"));
        assert_eq!(url_host("file:///etc/passwd"), None);
        assert_eq!(url_host("https://"), None);
    }

    #[test]
    fn paths_normalize() {
        let base = Path::new("/data/ext");
        assert_eq!(normalize("a/../b", base).unwrap(), PathBuf::from("/data/ext/b"));
        assert_eq!(normalize("../../etc/passwd", base).unwrap(), PathBuf::from("/etc/passwd"));
        assert_eq!(normalize("/tmp/./x", base).unwrap(), PathBuf::from("/tmp/x"));
    }
}
