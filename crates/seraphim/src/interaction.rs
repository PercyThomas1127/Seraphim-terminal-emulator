//! Interaction rules (`[[on]]`): when an event is published on the bus,
//! animate a background layer. This is the declarative layer that the Lua
//! scripting runtime (Phase III) will share: a script handler and a TOML
//! rule both end up as "event → layer property animation".

use seraphim_events::{Event, Pattern, Value};
use seraphim_visual::{Easing, Transition, VisualEngine};

/// A validated `[[on]]` rule.
#[derive(Clone, Debug)]
pub struct Rule {
    pub event: String,
    pattern: Pattern,
    when: Vec<(Vec<String>, Cond)>,
    pub layer: String,
    pub param: String,
    pub to: f32,
    pub from: Option<f32>,
    pub transition: Transition,
}

#[derive(Clone, Debug, PartialEq)]
enum Cond {
    /// Any of these globs matches the value's text.
    Globs(Vec<String>),
    Bool(bool),
    Number(f64),
}

impl Rule {
    pub fn from_config(r: &seraphim_config::Rule) -> Rule {
        let when = r
            .when
            .iter()
            .map(|(k, v)| {
                let path = k.split('.').map(str::to_owned).collect();
                let cond = match v {
                    toml::Value::Boolean(b) => Cond::Bool(*b),
                    toml::Value::Integer(i) => Cond::Number(*i as f64),
                    toml::Value::Float(f) => Cond::Number(*f),
                    toml::Value::String(s) => Cond::Globs(vec![s.clone()]),
                    toml::Value::Array(a) => Cond::Globs(
                        a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect(),
                    ),
                    // Rejected by config validation.
                    _ => Cond::Globs(Vec::new()),
                };
                (path, cond)
            })
            .collect();
        let duration = r.duration_ms as f64 / 1000.0;
        let transition = match r.easing.as_str() {
            "instant" => Transition::Instant,
            "spring" => Transition::Spring { stiffness: 170.0, damping: 26.0 },
            e => Transition::Tween { duration, easing: Easing::parse(e).unwrap_or_default() },
        };
        Rule {
            event: r.event.clone(),
            pattern: Pattern::parse(&r.event),
            when,
            layer: r.layer.clone(),
            param: r.param.clone(),
            to: r.to,
            from: r.from,
            transition,
        }
    }

    pub fn matches(&self, name: &str, data: &Value) -> bool {
        self.pattern.matches(name)
            && self.when.iter().all(|(path, cond)| {
                let v = path.iter().try_fold(data, |v, k| v.get(k));
                match (cond, v) {
                    (Cond::Bool(b), Some(Value::Bool(x))) => b == x,
                    (Cond::Number(n), Some(Value::Number(x))) => x.as_f64() == Some(*n),
                    (Cond::Globs(g), Some(Value::String(s))) => g.iter().any(|g| glob(g, s)),
                    (Cond::Globs(g), Some(v @ (Value::Number(_) | Value::Bool(_)))) => {
                        let s = v.to_string();
                        g.iter().any(|g| glob(g, &s))
                    }
                    _ => false,
                }
            })
    }

    /// Run the rule's animation. Returns false if the layer or parameter
    /// does not exist.
    pub fn apply(&self, visual: &mut VisualEngine) -> bool {
        let set = |v: &mut VisualEngine, value: f32, t: Transition| {
            if self.param == "opacity" {
                v.set_opacity(&self.layer, value, t)
            } else {
                v.set_param(&self.layer, &self.param, value, t)
            }
        };
        if let Some(from) = self.from {
            if !set(visual, from, Transition::Instant) {
                return false;
            }
        }
        set(visual, self.to, self.transition)
    }
}

/// Events that describe a lasting state rather than a moment. The latest of
/// each is replayed into new rules (profile switch, config reload) so, for
/// example, a root shell stays red after Ctrl+Shift+R.
const STATE_EVENTS: &[&str] = &["session.privileged", "terminal.screen.alternate"];

/// Rules plus the bus subscriptions that feed them.
pub struct Interactions {
    rules: Vec<Rule>,
    subs: Vec<seraphim_events::Subscription>,
    session: u64,
    warned: std::collections::HashSet<usize>,
    /// Latest state events, per session (0: no session).
    state: std::collections::HashMap<u64, Vec<std::sync::Arc<Event>>>,
}

impl Interactions {
    /// Subscribe to every event the rules listen for. Only events about
    /// `session` (or about no session) are acted on.
    pub fn new(rules: Vec<Rule>, bus: &seraphim_events::EventBus, session: u64) -> Self {
        let mut patterns: Vec<&str> = rules.iter().map(|r| r.event.as_str()).collect();
        patterns.sort_unstable();
        patterns.dedup();
        let mut subs: Vec<_> = patterns.iter().map(|p| bus.subscribe(*p, 256)).collect();
        // State events are tracked even when no rule listens yet.
        subs.extend(STATE_EVENTS.iter().map(|p| bus.subscribe(*p, 16)));
        Interactions { rules, subs, session, warned: Default::default(), state: Default::default() }
    }

    /// New rules (after a profile switch), brought up to date with the
    /// current state: the matching rules run instantly.
    pub fn replace(
        &mut self,
        rules: Vec<Rule>,
        bus: &seraphim_events::EventBus,
        visual: &mut VisualEngine,
    ) {
        // Anything still queued belongs to the old rules' view of the world.
        self.pump(visual);
        let state = std::mem::take(&mut self.state);
        *self = Interactions::new(rules, bus, self.session);
        self.state = state;
        self.replay(visual);
    }

    /// Follow another session (the focused pane changed): its state
    /// applies at once.
    pub fn set_session(&mut self, session: u64, visual: &mut VisualEngine) {
        if session == self.session {
            return;
        }
        self.pump(visual);
        self.session = session;
        self.replay(visual);
    }

    fn replay(&self, visual: &mut VisualEngine) {
        let events = self.state.get(&self.session).into_iter().chain(self.state.get(&0)).flatten();
        for e in events {
            for r in self.rules.iter().filter(|r| r.matches(&e.name, &e.data)) {
                let instant = Rule { transition: Transition::Instant, from: None, ..r.clone() };
                instant.apply(visual);
            }
        }
    }

    /// Apply rules for every pending event. Returns whether anything
    /// happened, and warnings for rules that target missing layers (once
    /// per rule).
    pub fn pump(&mut self, visual: &mut VisualEngine) -> (bool, Vec<String>) {
        let mut events: Vec<std::sync::Arc<Event>> =
            self.subs.iter().flat_map(|s| s.drain()).collect();
        // Overlapping patterns (e.g. "*" and an exact name) deliver the
        // same event twice; keep publish order and run each once.
        events.sort_by_key(|e| e.timestamp);
        events.dedup_by(|a, b| std::sync::Arc::ptr_eq(a, b));
        let mut acted = false;
        let mut warnings = Vec::new();
        for e in events {
            if STATE_EVENTS.contains(&e.name.as_str()) {
                let list = self.state.entry(e.session.unwrap_or(0)).or_default();
                list.retain(|s| s.name != e.name);
                list.push(e.clone());
            }
            if e.session.is_some_and(|s| s != self.session) {
                continue;
            }
            for (i, r) in self.rules.iter().enumerate() {
                if r.matches(&e.name, &e.data) {
                    if r.apply(visual) {
                        acted = true;
                    } else if self.warned.insert(i) {
                        warnings.push(format!(
                            "on {:?}: no layer {:?} with parameter {:?}",
                            r.event, r.layer, r.param
                        ));
                    }
                }
            }
        }
        (acted, warnings)
    }
}

/// Shell-style glob: `*` any run of characters, `?` one character.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use seraphim_events::json;

    fn rule(toml_text: &str) -> Rule {
        let c = seraphim_config::Config::parse(toml_text, std::path::Path::new("t.toml")).unwrap();
        Rule::from_config(&c.on[0])
    }

    #[test]
    fn globs() {
        assert!(glob("*apt install *", "sudo apt install vim"));
        assert!(!glob("*apt install *", "apt list"));
        assert!(glob("a?c", "abc") && !glob("a?c", "ac"));
        assert!(glob("*", "") && glob("", "") && !glob("", "x"));
        assert!(glob("*.rs", "main.rs") && !glob("*.rs", "main.rsx"));
        assert!(glob("ünï*", "ünïcode"));
    }

    #[test]
    fn matching() {
        let r = rule("[[on]]\nevent = \"session.privileged\"\nwhen = { root = true }\nlayer = \"sky\"\nparam = \"alert\"\nto = 1\n");
        assert!(r.matches("session.privileged", &json!({"root": true})));
        assert!(!r.matches("session.privileged", &json!({"root": false})));
        assert!(!r.matches("session.privileged", &json!({})), "missing field");
        assert!(!r.matches("session.closed", &json!({"root": true})));

        let r = rule("[[on]]\nevent = \"command.*\"\nwhen = { command = [\"*apt install *\", \"*brew install *\"], \"x.code\" = 0 }\nlayer = \"sky\"\nparam = \"install\"\n");
        let ok = json!({"command": "brew install jq", "x": {"code": 0}});
        assert!(r.matches("command.started", &ok));
        assert!(!r.matches("command.started", &json!({"command": "ls", "x": {"code": 0}})));
        assert!(
            !r.matches("command.started", &json!({"command": "brew install jq", "x": {"code": 1}}))
        );
        assert!(!r.matches("command.started", &json!({"command": null, "x": {"code": 0}})));
    }

    #[test]
    fn transitions() {
        let r = rule("[[on]]\nevent = \"x\"\nlayer = \"a\"\nparam = \"p\"\nduration_ms = 2600\neasing = \"linear\"\nfrom = 0\nto = 1\n");
        assert_eq!(r.transition, Transition::Tween { duration: 2.6, easing: Easing::Linear });
        assert_eq!((r.from, r.to), (Some(0.0), 1.0));
        let r = rule("[[on]]\nevent = \"x\"\nlayer = \"a\"\nparam = \"p\"\neasing = \"instant\"\n");
        assert_eq!(r.transition, Transition::Instant);
    }

    #[test]
    fn builtin_preset_rules() {
        let c = seraphim_config::Config::parse(
            seraphim_config::BUILTIN_PRESETS[0].1,
            std::path::Path::new("heavens.toml"),
        )
        .unwrap();
        let rules: Vec<Rule> = c.on.iter().map(Rule::from_config).collect();
        let fire = |name: &str, data: Value| -> Vec<(&str, f32)> {
            rules
                .iter()
                .filter(|r| r.matches(name, &data))
                .map(|r| (r.param.as_str(), r.to))
                .collect()
        };
        assert_eq!(fire("session.privileged", json!({"root": true})), [("alert", 1.0)]);
        assert_eq!(fire("session.privileged", json!({"root": false})), [("alert", 0.0)]);
        assert_eq!(
            fire("command.started", json!({"command": "sudo apt install ripgrep"})),
            [("install", 1.0)]
        );
        assert_eq!(fire("command.started", json!({"command": "gabriel bestow firefox"})).len(), 1);
        assert!(fire("command.started", json!({"command": "ls -la"})).is_empty());
        assert!(fire("command.started", json!({"command": null})).is_empty());
        assert_eq!(fire("gabriel.install.started", json!({"package": "x"})).len(), 1);
    }
}
