//! A Lua state with nothing dangerous in it.
//!
//! * Libraries: base, table, string, math, utf8, coroutine, and a pruned
//!   `os` (time, clock, date, difftime). No `io`, `debug`, `package`, no
//!   `os.execute`/`getenv`/`remove`, no `dofile`/`loadfile`, no
//!   `string.dump`, and `load` accepts source text only (precompiled
//!   bytecode can break out of the VM).
//! * `require` loads plain Lua modules from the extension's own folder.
//! * A time budget per call, enforced by an instruction hook. A budget
//!   error cannot be swallowed by `pcall`, `xpcall` or `coroutine.resume`:
//!   the wrappers below rethrow it.
//! * A memory limit.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mlua::chunk::ChunkMode;
use mlua::{HookTriggers, Lua, LuaOptions, StdLib, VmState};

/// Marker in budget errors (checked by the pcall wrappers and the runtime).
pub const BUDGET_ERROR: &str = "seraphim: time budget exceeded";

/// When the current call must end, in microseconds since `base`; 0 = none.
#[derive(Clone)]
pub struct Deadline {
    base: Instant,
    at: Arc<AtomicU64>,
}

impl Deadline {
    fn new() -> Self {
        Deadline { base: Instant::now(), at: Arc::new(AtomicU64::new(0)) }
    }

    pub fn start(&self, budget: Duration) {
        let at = self.base.elapsed() + budget;
        self.at.store(at.as_micros().max(1) as u64, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        self.at.store(0, Ordering::Relaxed);
    }

    fn passed(&self) -> bool {
        let at = self.at.load(Ordering::Relaxed);
        at != 0 && self.base.elapsed().as_micros() as u64 > at
    }
}

const PRELUDE: &str = r#"
local raw_pcall, raw_xpcall, raw_resume = pcall, xpcall, coroutine.resume
local find, select, tostring, error, pack, unpack = string.find, select, tostring, error, table.pack, table.unpack
local MARK = ...
local function fatal(e)
  return find(tostring(e), MARK, 1, true) ~= nil
end
local type, match, gsub = type, string.match, string.gsub
-- Errors raised by Seraphim's API arrive as userdata; scripts get the
-- message as a plain string, without the Rust-side traceback.
local function clean(e)
  if type(e) ~= "userdata" then return e end
  local s = tostring(e)
  s = match(s, "^(.-)\nstack traceback") or s
  s = gsub(s, "^runtime error: ", "")
  s = gsub(s, "^callback error: ", "")
  return s
end
local function pass(r)
  if not r[1] then
    if fatal(r[2]) then error(r[2], 0) end
    r[2] = clean(r[2])
  end
  return unpack(r, 1, r.n)
end
pcall = function(f, ...) return pass(pack(raw_pcall(f, ...))) end
xpcall = function(f, h, ...)
  return pass(pack(raw_xpcall(f, function(e)
    if fatal(e) then return e end
    return h(clean(e))
  end, ...)))
end
coroutine.resume = function(co, ...) return pass(pack(raw_resume(co, ...))) end
coroutine.wrap = function(f)
  local co = coroutine.create(f)
  return function(...)
    local r = pack(raw_resume(co, ...))
    if not r[1] then error(r[2], 0) end
    return unpack(r, 2, r.n)
  end
end
"#;

/// A new sandboxed state for the extension in `dir`.
pub fn new_state(name: &str, dir: &Path, memory: usize) -> mlua::Result<(Lua, Deadline)> {
    let libs = StdLib::TABLE
        | StdLib::STRING
        | StdLib::MATH
        | StdLib::UTF8
        | StdLib::COROUTINE
        | StdLib::OS;
    let lua = Lua::new_with(libs, LuaOptions::new())?;
    lua.set_memory_limit(memory)?;
    let g = lua.globals();

    for f in ["dofile", "loadfile", "collectgarbage"] {
        g.raw_set(f, mlua::Nil)?;
    }
    let string: mlua::Table = g.get("string")?;
    string.raw_set("dump", mlua::Nil)?;
    let os: mlua::Table = g.get("os")?;
    let safe_os = lua.create_table()?;
    for f in ["time", "clock", "date", "difftime"] {
        safe_os.raw_set(f, os.get::<mlua::Value>(f)?)?;
    }
    g.raw_set("os", safe_os)?;

    // load: text only, with an optional environment.
    let load = lua.create_function(
        |lua,
         (chunk, chunkname, _mode, env): (
            mlua::Value,
            Option<String>,
            Option<String>,
            Option<mlua::Table>,
        )| {
            let text = match chunk {
                mlua::Value::String(s) => s.to_str()?.to_owned(),
                _ => return Ok((mlua::Nil, Some("load: only source text is accepted".to_owned()))),
            };
            let mut c = lua.load(text).set_mode(ChunkMode::Text);
            if let Some(n) = chunkname {
                c = c.set_name(n);
            }
            if let Some(env) = env {
                c = c.set_environment(env);
            }
            match c.into_function() {
                Ok(f) => Ok((mlua::Value::Function(f), None)),
                Err(e) => Ok((mlua::Nil, Some(e.to_string()))),
            }
        },
    )?;
    g.raw_set("load", load)?;

    // require: modules from the extension folder only.
    let loaded = lua.create_table()?;
    let root = dir.to_owned();
    let require = lua.create_function(move |lua, module: String| {
        if let Some(v) = loaded.raw_get::<Option<mlua::Value>>(module.as_str())? {
            return Ok(v);
        }
        let path = module_path(&root, &module)
            .ok_or_else(|| mlua::Error::runtime(format!("require: bad module name {module:?}")))?;
        let text = std::fs::read_to_string(&path).map_err(|_| {
            mlua::Error::runtime(format!(
                "require: module {module:?} not found in the extension folder"
            ))
        })?;
        let v: mlua::Value = lua
            .load(text)
            .set_mode(ChunkMode::Text)
            .set_name(format!("@{}", path.display()))
            .call(module.as_str())?;
        let v = if v.is_nil() { mlua::Value::Boolean(true) } else { v };
        loaded.raw_set(module.as_str(), v.clone())?;
        Ok(v)
    })?;
    g.raw_set("require", require)?;

    let ext = name.to_owned();
    let print = lua.create_function(move |_, args: mlua::Variadic<mlua::Value>| {
        let parts: Vec<String> =
            args.iter().map(|v| v.to_string().unwrap_or_else(|_| "?".into())).collect();
        eprintln!("seraphim: [{ext}] {}", parts.join("\t"));
        Ok(())
    })?;
    g.raw_set("print", print)?;

    lua.load(PRELUDE).set_name("=sandbox").call::<()>(BUDGET_ERROR)?;

    let deadline = Deadline::new();
    let d = deadline.clone();
    // The global hook also runs in coroutines (they inherit it).
    lua.set_global_hook(HookTriggers::new().every_nth_instruction(1000), move |_, _| {
        if d.passed() {
            Err(mlua::Error::runtime(BUDGET_ERROR))
        } else {
            Ok(VmState::Continue)
        }
    })?;
    Ok((lua, deadline))
}

/// `a.b` → `<root>/a/b.lua`; `None` for names that could leave the folder.
fn module_path(root: &Path, module: &str) -> Option<PathBuf> {
    if module.is_empty()
        || module.split('.').any(str::is_empty)
        || !module.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return None;
    }
    let rel: PathBuf = module.split('.').collect();
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let mut p = root.join(rel);
    p.set_extension("lua");
    Some(p)
}

pub fn is_budget_error(e: &mlua::Error) -> bool {
    e.to_string().contains(BUDGET_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (Lua, Deadline) {
        new_state("t", &std::env::temp_dir(), 16 << 20).unwrap()
    }

    #[test]
    fn dangerous_things_are_gone() {
        let (lua, _) = state();
        let gone: bool = lua
            .load(
                "return io == nil and debug == nil and package == nil and dofile == nil
                 and loadfile == nil and string.dump == nil and os.execute == nil
                 and os.getenv == nil and os.remove == nil and os.exit == nil
                 and type(os.time()) == 'number'",
            )
            .eval()
            .unwrap();
        assert!(gone);
    }

    #[test]
    fn load_refuses_bytecode() {
        let (lua, _) = state();
        let (f, err): (mlua::Value, Option<String>) =
            lua.load("return load('\\27Lua\\84\\0')").eval().unwrap();
        assert!(f.is_nil());
        let err = err.unwrap();
        assert!(err.contains("binary chunk"), "binary chunks rejected: {err}");
        let v: i32 = lua.load("return load('return 1 + 1')()").eval().unwrap();
        assert_eq!(v, 2);
        let v: i32 = lua.load("return load('return x', 'c', 't', { x = 5 })()").eval().unwrap();
        assert_eq!(v, 5);
    }

    #[test]
    fn budget_stops_loops_even_through_pcall() {
        let (lua, d) = state();
        for src in [
            "while true do end",
            "while true do pcall(function() while true do end end) end",
            "while true do xpcall(function() while true do end end, function() return 1 end) end",
            "local co = coroutine.create(function() while true do end end)
             while true do coroutine.resume(co) end",
            "local f = coroutine.wrap(function() while true do end end) while true do pcall(f) end",
        ] {
            d.start(Duration::from_millis(30));
            let t = Instant::now();
            let e = lua.load(src).exec().unwrap_err();
            d.clear();
            assert!(is_budget_error(&e), "{src}: {e}");
            assert!(t.elapsed() < Duration::from_secs(2), "{src}");
        }
        // Ordinary errors are still catchable.
        let ok: bool = lua.load("return not pcall(error, 'x')").eval().unwrap();
        assert!(ok);
    }

    #[test]
    fn memory_is_limited() {
        let (lua, _) = state();
        let e = lua.load("local t = {} for i = 1, 1e9 do t[i] = ('x'):rep(1000) .. i end").exec();
        assert!(matches!(e, Err(mlua::Error::MemoryError(_))), "{e:?}");
    }

    #[test]
    fn require_stays_in_the_folder() {
        let dir = std::env::temp_dir().join(format!("seraphim-req-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(
            dir.join("lib/util.lua"),
            "local m = {} function m.two() return 2 end return m",
        )
        .unwrap();
        let (lua, _) = new_state("t", &dir, 16 << 20).unwrap();
        let v: i32 = lua
            .load("return require('lib.util').two() + require('lib.util').two()")
            .eval()
            .unwrap();
        assert_eq!(v, 4);
        for bad in ["../x", "/etc/passwd", "a..b", ""] {
            let e = lua.load(format!("return require({bad:?})")).exec().unwrap_err();
            assert!(e.to_string().contains("require"), "{bad}: {e}");
        }
        let e = lua.load("return require('missing')").exec().unwrap_err();
        assert!(e.to_string().contains("not found"));
        assert!(module_path(&dir, "..").is_none());
    }
}
