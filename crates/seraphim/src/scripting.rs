//! The window's side of scripting: starts the Lua runtime
//! (`seraphim-script`) with the configured extensions and grants, applies
//! the commands scripts send, and keeps what they registered (layers,
//! bindings, menu commands, shortcuts) across profile switches.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use seraphim_config::{ExtensionConfig, Scripting};
use seraphim_events::{EventBus, Subscription};
use seraphim_render::Theme;
use seraphim_script::{Command as Sc, Extension, Grant, Motion, Permission, Runtime};
use seraphim_visual::{Easing, LayerSpec, Transition, VisualEngine};

use crate::bindings::Bind;

/// A keyboard shortcut an extension registered (`ctrl+shift+e`, `f5`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Lowercase character, or a key name (`f5`, `enter`, `space`, ...).
    pub key: String,
}

impl Chord {
    pub fn parse(s: &str) -> Option<Chord> {
        let mut c = Chord { ctrl: false, shift: false, alt: false, key: String::new() };
        for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
            match part.as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" | "option" => c.alt = true,
                k if c.key.is_empty() && !k.is_empty() => c.key = k.to_owned(),
                _ => return None,
            }
        }
        let named = [
            "enter",
            "space",
            "tab",
            "escape",
            "backspace",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "pageup",
            "pagedown",
            "insert",
            "delete",
        ];
        let ok = c.key.chars().count() == 1
            || named.contains(&c.key.as_str())
            || c.key
                .strip_prefix('f')
                .and_then(|n| n.parse::<u8>().ok())
                .is_some_and(|n| (1..=24).contains(&n));
        // A bare letter would steal typing.
        let plain_char = c.key.chars().count() == 1 && !c.ctrl && !c.alt;
        (ok && !plain_char).then_some(c)
    }
}

/// What the window must do for a script.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Notify(String),
    Send(String),
    Action(String),
    Profile(String),
    /// Extension commands changed: rebuild the menus.
    MenuChanged,
    /// Script bindings changed: rebuild the binding set.
    BindsChanged,
    Redraw,
}

/// What the runtime was started from (restart when it changes).
#[derive(Clone, Debug, PartialEq)]
pub struct Sources {
    pub scripting: Scripting,
    pub extensions: BTreeMap<String, ExtensionConfig>,
    pub config_dir: Option<PathBuf>,
    pub cli: Vec<PathBuf>,
    pub live_reload: bool,
}

pub struct ScriptHost {
    runtime: Option<Runtime>,
    sources: Option<Sources>,
    /// Extension folders (script layers' files must be inside).
    dirs: HashMap<String, PathBuf>,
    /// Layers scripts added, re-added after every scene rebuild.
    layers: Vec<(String, LayerSpec)>,
    binds: Vec<(String, u64, Bind)>,
    /// (extension, command id, title), for the Extensions menu.
    pub commands: Vec<(String, String, String)>,
    keys: Vec<(String, Chord, String)>,
    privileged: Option<Subscription>,
    /// Sessions running as root, and the focused one.
    roots: HashSet<u64>,
    session: u64,
    snapshot_at: Option<Instant>,
    snapshot_gen: u64,
    warned: HashSet<String>,
}

impl Default for ScriptHost {
    fn default() -> Self {
        ScriptHost {
            runtime: None,
            sources: None,
            dirs: HashMap::new(),
            layers: Vec::new(),
            binds: Vec::new(),
            commands: Vec::new(),
            keys: Vec::new(),
            privileged: None,
            roots: HashSet::new(),
            session: 1,
            snapshot_at: None,
            snapshot_gen: u64::MAX,
            warned: HashSet::new(),
        }
    }
}

/// The grant `[extensions.<name>]` describes.
pub fn grant_for(
    cfg: Option<&ExtensionConfig>,
    config_dir: Option<&Path>,
    warnings: &mut Vec<String>,
    name: &str,
) -> Grant {
    let mut g = Grant::default();
    let Some(cfg) = cfg else { return g };
    if let Some(list) = &cfg.grant {
        g.permissions.clear();
        for p in list {
            match Permission::parse(p) {
                Some(p) => {
                    g.permissions.insert(p);
                }
                None => warnings.push(format!("extensions.{name}: unknown permission {p:?}")),
            }
        }
    }
    let dir = config_dir.map(Path::to_path_buf).unwrap_or_default();
    g.paths = cfg
        .paths
        .iter()
        .map(|p| match p.strip_prefix("~/") {
            Some(rest) => {
                std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest)
            }
            None => dir.join(p),
        })
        .collect();
    g.hosts = cfg.hosts.clone();
    g.commands = cfg.commands.clone();
    g.settings = serde_json::to_value(&cfg.settings).unwrap_or_default();
    g
}

fn transition(m: &Motion) -> Result<Transition, String> {
    Ok(match m.easing.as_str() {
        "instant" => Transition::Instant,
        "spring" => Transition::Spring { stiffness: 170.0, damping: 26.0 },
        _ if m.duration_ms == 0 => Transition::Instant,
        e => Transition::Tween {
            duration: m.duration_ms as f64 / 1000.0,
            easing: Easing::parse(e).ok_or_else(|| {
                format!("unknown easing {e:?} ({})", seraphim_config::EASINGS.join(", "))
            })?,
        },
    })
}

impl ScriptHost {
    /// Start, restart or leave the runtime as it is for these sources.
    /// Returns problems to show (unreadable extensions, bad grants).
    pub fn configure(
        &mut self,
        src: Sources,
        bus: &EventBus,
        wake: seraphim_script::Waker,
        visual: &mut VisualEngine,
    ) -> (Vec<String>, Vec<Effect>) {
        if self.sources.as_ref() == Some(&src) {
            return (Vec::new(), Vec::new());
        }
        let effects = self.stop(visual);
        let mut warnings = Vec::new();
        if !src.scripting.enabled {
            self.sources = Some(src);
            return (warnings, effects);
        }
        let (found, errors) = match &src.config_dir {
            Some(d) => seraphim_script::discover(d, &src.cli),
            None => seraphim_script::discover(Path::new("/nonexistent"), &src.cli),
        };
        warnings.extend(errors);
        let mut list: Vec<(Extension, Grant)> = Vec::new();
        for ext in found {
            let cfg = src.extensions.get(&ext.name);
            if cfg.is_some_and(|c| c.disabled) {
                continue;
            }
            let grant = grant_for(cfg, src.config_dir.as_deref(), &mut warnings, &ext.name);
            self.dirs.insert(ext.name.clone(), ext.dir.clone());
            list.push((ext, grant));
        }
        for name in src.extensions.keys() {
            if !list.iter().any(|(e, _)| &e.name == name) && !src.extensions[name].disabled {
                warnings.push(format!("[extensions.{name}]: no such extension installed"));
            }
        }
        if !list.is_empty() {
            let settings = seraphim_script::Settings {
                budget: Duration::from_millis(src.scripting.budget_ms.clamp(1, 10_000) as u64),
                memory: (src.scripting.memory_mb.clamp(1, 4096) as usize) << 20,
                live_reload: src.live_reload,
                ..Default::default()
            };
            self.privileged = Some(bus.subscribe("session.privileged", 8));
            self.runtime = Some(Runtime::start(list, settings, bus, wake));
            self.snapshot_gen = u64::MAX;
        }
        self.sources = Some(src);
        (warnings, effects)
    }

    /// Restart on the next [`ScriptHost::configure`] even if nothing
    /// changed (Reload Configuration: re-enables disabled extensions).
    pub fn invalidate(&mut self) {
        self.sources = None;
    }

    /// Stop the runtime and undo what scripts set up.
    fn stop(&mut self, visual: &mut VisualEngine) -> Vec<Effect> {
        self.runtime = None;
        let fade = Transition::tween(0.3);
        for (_, l) in self.layers.drain(..) {
            visual.remove_layer(&l.id, fade);
        }
        let had = !self.binds.is_empty() || !self.commands.is_empty();
        self.binds.clear();
        self.commands.clear();
        self.keys.clear();
        self.dirs.clear();
        if had {
            vec![Effect::BindsChanged, Effect::MenuChanged]
        } else {
            Vec::new()
        }
    }

    /// Bindings scripts created.
    pub fn binds(&self) -> impl Iterator<Item = &Bind> {
        self.binds.iter().map(|(_, _, b)| b)
    }

    /// The scene was rebuilt (profile switch): put script layers back.
    pub fn reapply_layers(&self, visual: &mut VisualEngine) {
        for (ext, l) in &self.layers {
            if let Err(e) = visual.upsert_layer(l, Transition::Instant) {
                eprintln!("seraphim: extension {ext}: {e}");
            }
        }
    }

    /// The focused pane's session (snapshots, typing).
    pub fn set_session(&mut self, id: u64) {
        if id != self.session {
            self.session = id;
            self.snapshot_gen = u64::MAX;
        }
    }

    /// The registered command for a shortcut.
    pub fn key(&self, chord: &Chord) -> Option<(String, String)> {
        self.keys.iter().find(|(_, c, _)| c == chord).map(|(e, _, id)| (e.clone(), id.clone()))
    }

    /// The shortcut bound to a command, as menus show it (`Ctrl+Shift+T`).
    pub fn shortcut_for(&self, ext: &str, command: &str) -> String {
        let Some((_, c, _)) = self.keys.iter().find(|(e, _, id)| e == ext && id == command) else {
            return String::new();
        };
        let mut parts: Vec<String> = Vec::new();
        for (on, name) in [(c.ctrl, "Ctrl"), (c.alt, "Alt"), (c.shift, "Shift")] {
            if on {
                parts.push(name.into());
            }
        }
        let mut k = c.key.clone();
        if let Some(first) = k.get(..1) {
            k = first.to_uppercase() + &k[1..];
        }
        parts.push(k);
        parts.join("+")
    }

    pub fn invoke(&self, ext: &str, command: &str) {
        if let Some(rt) = &self.runtime {
            rt.invoke(ext, command);
        }
    }

    /// Keep the terminal snapshot fresh (at most every 50 ms) while an
    /// extension may read it. Returns when to check again.
    pub fn refresh_snapshot(
        &mut self,
        session: &seraphim_session::Session,
        now: Instant,
    ) -> Option<Instant> {
        let rt = self.runtime.as_ref()?;
        if !rt.reads_terminal() || session.generation() == self.snapshot_gen {
            return None;
        }
        if let Some(at) = self.snapshot_at {
            let due = at + Duration::from_millis(50);
            if now < due {
                return Some(due);
            }
        }
        self.snapshot_at = Some(now);
        self.snapshot_gen = session.generation();
        let foreground = session.foreground_process();
        let shell = session.shell_process();
        let proc = |p: seraphim_session::ProcessInfo| seraphim_script::Process {
            pid: p.pid,
            name: p.name,
            cwd: p.cwd.map(|c| c.to_string_lossy().into_owned()),
        };
        let t = session.terminal();
        let mut lines: Vec<String> = t.screen().rows_iter().map(|r| r.text()).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let c = t.cursor();
        let snap = seraphim_script::TerminalSnapshot {
            cols: t.cols(),
            rows: t.rows(),
            lines,
            cursor: (c.row, c.col),
            title: t.title().to_owned(),
            alt_screen: t.is_alternate_screen(),
            selection: t.selection_text(),
            root: self.roots.contains(&self.session),
            foreground: foreground.map(proc),
            shell: shell.map(proc),
        };
        drop(t);
        rt.update_snapshot(|s| *s = snap);
        None
    }

    /// Apply queued script commands.
    pub fn apply(&mut self, visual: &mut VisualEngine, theme: &Theme) -> Vec<Effect> {
        if let Some(sub) = &self.privileged {
            for e in sub.drain() {
                let id = e.session.unwrap_or(0);
                if e.data["root"].as_bool().unwrap_or(false) {
                    self.roots.insert(id);
                } else {
                    self.roots.remove(&id);
                }
            }
        }
        let Some(rt) = &self.runtime else { return Vec::new() };
        let mut effects = Vec::new();
        for cmd in rt.take_commands() {
            let ext = cmd.ext().to_owned();
            match self.apply_one(cmd, visual, theme, &mut effects) {
                Ok(()) => {}
                Err(e) => {
                    let msg = format!("{ext}: {e}");
                    eprintln!("seraphim: extension {msg}");
                    // The same mistake every frame would flood the corner.
                    if self.warned.insert(msg.clone()) {
                        effects.push(Effect::Notify(msg));
                    }
                }
            }
        }
        effects
    }

    fn apply_one(
        &mut self,
        cmd: Sc,
        visual: &mut VisualEngine,
        theme: &Theme,
        fx: &mut Vec<Effect>,
    ) -> Result<(), String> {
        match cmd {
            Sc::SetParam { layer, param, value, from, motion, .. } => {
                let t = transition(&motion)?;
                let ids: Vec<String> = match layer {
                    Some(l) => vec![l],
                    None => visual.layer_ids().iter().map(|s| s.to_string()).collect(),
                };
                let mut any = false;
                for id in &ids {
                    let set = |v: &mut VisualEngine, x: f32, t: Transition| {
                        if param == "opacity" {
                            v.set_opacity(id, x, t)
                        } else {
                            v.set_param(id, &param, x, t)
                        }
                    };
                    if let Some(f) = from {
                        set(visual, f, Transition::Instant);
                    }
                    any |= set(visual, value, t);
                }
                fx.push(Effect::Redraw);
                if !any {
                    return Err(match ids.as_slice() {
                        [one] => format!("layer {one:?} has no parameter {param:?}"),
                        _ => format!("no layer has a parameter {param:?}"),
                    });
                }
            }
            Sc::SetOpacity { layer, value, from, motion, .. } => {
                let t = transition(&motion)?;
                if let Some(f) = from {
                    visual.set_opacity(&layer, f, Transition::Instant);
                }
                if !visual.set_opacity(&layer, value, t) {
                    return Err(format!("no layer {layer:?}"));
                }
                fx.push(Effect::Redraw);
            }
            Sc::SetColor { layer, name, color, motion, .. } => {
                let t = transition(&motion)?;
                let rgba = crate::profile::resolve_color(&color, theme)
                    .ok_or_else(|| format!("invalid color {color:?}"))?;
                let layer = match layer {
                    Some(l) => l,
                    None => visual
                        .layer_ids()
                        .first()
                        .map(|s| s.to_string())
                        .ok_or("there are no layers")?,
                };
                let name = match name {
                    Some(n) => n,
                    None => visual
                        .color_names(&layer)
                        .first()
                        .map(|s| s.to_string())
                        .ok_or_else(|| format!("layer {layer:?} has no colors"))?,
                };
                if !visual.set_color(&layer, &name, rgba, t) {
                    return Err(format!(
                        "layer {layer:?} has no color {name:?} (it has: {})",
                        visual.color_names(&layer).join(", ")
                    ));
                }
                fx.push(Effect::Redraw);
            }
            Sc::AddLayer { ext, layer, motion } => {
                let t = transition(&motion)?;
                let b: seraphim_config::Background =
                    serde_json::from_value(layer).map_err(|e| format!("add_layer: {e}"))?;
                b.validate().map_err(|e| format!("add_layer: {e}"))?;
                let dir = self.dirs.get(&ext).cloned().unwrap_or_default();
                let mut files = Vec::new();
                let spec = crate::profile::layer_spec(&b, 0, &dir, theme, &mut files)?;
                // Files come from the extension's own folder only.
                let root = dir.canonicalize().unwrap_or(dir.clone());
                for f in &files {
                    let real = f.canonicalize().unwrap_or(f.clone());
                    if !real.starts_with(&root) {
                        return Err(format!(
                            "add_layer: {} is outside the extension's folder",
                            f.display()
                        ));
                    }
                }
                visual.upsert_layer(&spec, t)?;
                self.layers.retain(|(_, l)| l.id != spec.id);
                self.layers.push((ext, spec));
                fx.push(Effect::Redraw);
            }
            Sc::RemoveLayer { id, motion, .. } => {
                let t = transition(&motion)?;
                self.layers.retain(|(_, l)| l.id != id);
                if !visual.remove_layer(&id, t) {
                    return Err(format!("no layer {id:?}"));
                }
                fx.push(Effect::Redraw);
            }
            Sc::SetProfile { name, .. } => fx.push(Effect::Profile(name)),
            Sc::Bind { ext, id, spec } => {
                let b: seraphim_config::Bind =
                    serde_json::from_value(spec).map_err(|e| format!("bind: {e}"))?;
                b.validate().map_err(|e| format!("bind: {e}"))?;
                let bind = Bind::from_config(&b, theme).ok_or("bind: invalid binding")?;
                self.binds.push((ext, id, bind));
                fx.push(Effect::BindsChanged);
            }
            Sc::Unbind { ext, id } => {
                self.binds.retain(|(e, i, _)| !(*e == ext && *i == id));
                fx.push(Effect::BindsChanged);
            }
            Sc::Notify { ext, text, .. } => fx.push(Effect::Notify(format!("{ext}: {text}"))),
            Sc::Send { text, .. } => {
                // Checked in the runtime too; this is the authoritative one.
                if self.roots.contains(&self.session) {
                    return Err("terminal.send refused: the session runs as root".into());
                }
                fx.push(Effect::Send(text));
            }
            Sc::RegisterCommand { ext, id, title } => {
                self.commands.retain(|(e, i, _)| !(*e == ext && *i == id));
                self.commands.push((ext, id, title));
                fx.push(Effect::MenuChanged);
            }
            Sc::RegisterKey { ext, chord, command } => {
                let c = Chord::parse(&chord).ok_or_else(|| {
                    format!("ui.key: {chord:?} is not a shortcut (e.g. \"ctrl+shift+e\", \"f5\")")
                })?;
                if let Some((other, _)) = self.key(&c).filter(|(e, _)| *e != ext) {
                    return Err(format!("ui.key: {chord} is already taken by {other}"));
                }
                self.keys.retain(|(e, k, _)| !(*e == ext && *k == c));
                self.keys.push((ext, c, command));
                fx.push(Effect::MenuChanged);
            }
            Sc::Action { name, .. } => fx.push(Effect::Action(name)),
            Sc::Reset { ext } => {
                let fade = Transition::tween(0.3);
                for (_, l) in self.layers.iter().filter(|(e, _)| *e == ext) {
                    visual.remove_layer(&l.id, fade);
                }
                self.layers.retain(|(e, _)| *e != ext);
                self.binds.retain(|(e, _, _)| *e != ext);
                self.commands.retain(|(e, _, _)| *e != ext);
                self.keys.retain(|(e, _, _)| *e != ext);
                self.warned.retain(|w| !w.starts_with(&format!("{ext}:")));
                fx.extend([Effect::BindsChanged, Effect::MenuChanged, Effect::Redraw]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords() {
        let c = Chord::parse("Ctrl+Shift+E").unwrap();
        assert_eq!(c, Chord { ctrl: true, shift: true, alt: false, key: "e".into() });
        assert_eq!(Chord::parse("f5").unwrap().key, "f5");
        assert!(Chord::parse("alt+enter").is_some());
        assert!(Chord::parse("e").is_none(), "a bare letter would steal typing");
        assert!(Chord::parse("shift+e").is_none());
        assert!(Chord::parse("ctrl+").is_none());
        assert!(Chord::parse("ctrl+a+b").is_none());
        assert!(Chord::parse("f99").is_none());
    }

    #[test]
    fn grants_from_config() {
        let cfg = ExtensionConfig {
            grant: Some(vec!["visual".into(), "network".into(), "root".into()]),
            hosts: vec!["api.example.com".into()],
            paths: vec!["notes".into()],
            ..Default::default()
        };
        let mut w = Vec::new();
        let g = grant_for(Some(&cfg), Some(Path::new("/cfg")), &mut w, "x");
        assert_eq!(g.permissions, [Permission::Visual, Permission::Network].into());
        assert_eq!(g.paths, [PathBuf::from("/cfg/notes")]);
        assert_eq!(w.len(), 1);
        let g = grant_for(None, None, &mut w, "y");
        assert_eq!(g.permissions, seraphim_script::DEFAULT_GRANT.iter().copied().collect());
    }

    #[test]
    fn motions() {
        assert_eq!(
            transition(&Motion { duration_ms: 0, easing: "linear".into() }),
            Ok(Transition::Instant)
        );
        assert!(matches!(
            transition(&Motion { duration_ms: 500, easing: "ease-out".into() }),
            Ok(Transition::Tween { .. })
        ));
        assert!(transition(&Motion { duration_ms: 5, easing: "wobble".into() }).is_err());
    }

    /// Scripts drive a real visual engine through the host.
    #[test]
    fn commands_reach_the_engine() {
        use seraphim_render::headless::{Headless, FORMAT};
        let Ok(fonts) = seraphim_render::Fonts::new(&seraphim_render::FontConfig {
            family: None,
            size_px: 16.0,
        }) else {
            return;
        };
        let Ok(h) = Headless::new(fonts, Theme::default()) else { return };
        let mut visual = VisualEngine::new(&h.device, &h.queue, FORMAT);
        let base = LayerSpec::new("base", seraphim_visual::LayerKind::Solid([0.0, 0.0, 0.0, 1.0]));
        visual.set_layers(&[base], Transition::Instant);

        let root = std::env::temp_dir().join(format!("seraphim-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ext = root.join("extensions/t");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(
            ext.join("extension.toml"),
            "name = \"t\"\npermissions = [\"visual\", \"interface\", \"terminal.write\"]\n",
        )
        .unwrap();
        std::fs::write(
            ext.join("init.lua"),
            r##"
            visual.add_layer({ id = "tint", kind = "solid", color = "#ff0000", opacity = 0.5 })
            visual.set_color("theme.bright.4", { layer = "tint" })
            visual.set_parameter("nope", 1)
            visual.add_layer({ id = "pic", kind = "image", path = "/etc/passwd" })
            visual.bind("tint.color", { source = "terminal.last_exit_code", mapping = { [0] = "#00ff00", default = "#ff0000" } })
            ui.command("go", function() end)
            ui.key("ctrl+shift+g", "go")
            terminal.send("hi\n")
            ui.notify("done")
            "##,
        )
        .unwrap();
        let mut cfg = BTreeMap::new();
        cfg.insert(
            "t".to_owned(),
            ExtensionConfig {
                grant: Some(vec!["visual".into(), "interface".into(), "terminal.write".into()]),
                ..Default::default()
            },
        );
        let src = Sources {
            scripting: Scripting::default(),
            extensions: cfg,
            config_dir: Some(root.clone()),
            cli: Vec::new(),
            live_reload: false,
        };
        let bus = EventBus::new();
        let mut host = ScriptHost::default();
        let (warnings, _) = host.configure(src, &bus, std::sync::Arc::new(|| {}), &mut visual);
        assert!(warnings.is_empty(), "{warnings:?}");
        let theme = Theme::default();
        let mut fx = Vec::new();
        let t = Instant::now();
        while !fx.contains(&Effect::Notify("t: done".into()))
            && t.elapsed() < Duration::from_secs(3)
        {
            fx.extend(host.apply(&mut visual, &theme));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(visual.layer_ids(), ["base", "tint"], "{fx:?}");
        let notices: Vec<_> = fx
            .iter()
            .filter_map(|e| match e {
                Effect::Notify(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            notices.iter().any(|n| n.contains("no layer has a parameter \"nope\"")),
            "{notices:?}"
        );
        assert!(
            notices.iter().any(|n| n.contains("outside the extension's folder")),
            "{notices:?}"
        );
        assert!(fx.contains(&Effect::Send("hi\n".into())));
        assert!(fx.contains(&Effect::BindsChanged) && fx.contains(&Effect::MenuChanged));
        assert_eq!(host.binds().count(), 1);
        assert_eq!(host.commands, [("t".to_owned(), "go".to_owned(), "go".to_owned())]);
        let chord = Chord::parse("ctrl+shift+g").unwrap();
        assert_eq!(host.key(&chord), Some(("t".into(), "go".into())));
        // Scene rebuilt by a profile switch: the script layer comes back.
        visual.set_layers(&[], Transition::Instant);
        host.reapply_layers(&mut visual);
        assert_eq!(visual.layer_ids(), ["tint"]);
        // A root session: typing is refused.
        bus.publisher("session").publish(
            "session.privileged",
            Some(1),
            seraphim_events::json!({"root": true}),
        );
        std::thread::sleep(Duration::from_millis(20));
        let _ = host.apply(&mut visual, &theme);
        let mut fx = Vec::new();
        host.apply_one(
            Sc::Send { ext: "t".into(), text: "x".into() },
            &mut visual,
            &theme,
            &mut fx,
        )
        .unwrap_err();
        assert!(fx.is_empty());
    }
}
