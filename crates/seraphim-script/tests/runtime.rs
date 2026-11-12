//! The runtime end to end: real Lua, a real bus, the script thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use seraphim_events::{json, EventBus};
use seraphim_script::{Command, Extension, Grant, Permission, Runtime, Settings};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("seraphim-rt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An extension folder with these permissions and this init.lua.
fn ext(root: &Path, name: &str, perms: &[&str], lua: &str) -> Extension {
    let d = root.join(name);
    std::fs::create_dir_all(&d).unwrap();
    let perms: Vec<String> = perms.iter().map(|p| format!("{p:?}")).collect();
    std::fs::write(
        d.join("extension.toml"),
        format!("name = {name:?}\npermissions = [{}]\n", perms.join(", ")),
    )
    .unwrap();
    std::fs::write(d.join("init.lua"), lua).unwrap();
    Extension::from_dir(&d).unwrap()
}

fn grant(perms: &[Permission]) -> Grant {
    Grant { permissions: perms.iter().copied().collect(), ..Default::default() }
}

fn settings(root: &Path) -> Settings {
    Settings {
        data_dir: root.join("data"),
        budget: Duration::from_millis(40),
        ..Default::default()
    }
}

fn start(exts: Vec<(Extension, Grant)>, root: &Path, bus: &EventBus) -> Runtime {
    Runtime::start(exts, settings(root), bus, Arc::new(|| {}))
}

/// Collect commands until `done` says so (or 3 s pass).
fn wait(rt: &Runtime, mut done: impl FnMut(&[Command]) -> bool) -> Vec<Command> {
    let t = Instant::now();
    let mut got = Vec::new();
    while t.elapsed() < Duration::from_secs(3) {
        got.extend(rt.take_commands());
        if done(&got) {
            return got;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out; got {got:#?}");
}

fn notices(cmds: &[Command]) -> Vec<&str> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Notify { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn has_notice(cmds: &[Command], needle: &str) -> bool {
    notices(cmds).iter().any(|n| n.contains(needle))
}

/// Wait until the script thread has run the main files (they notify).
fn ready(rt: &Runtime, n: usize) -> Vec<Command> {
    wait(rt, |c| notices(c).iter().filter(|t| t.starts_with("ready")).count() >= n)
}

const V: &[Permission] = &[Permission::Visual, Permission::Interface];

#[test]
fn events_reach_handlers_and_become_commands() {
    let root = dir("events");
    let bus = EventBus::new();
    let e = ext(
        &root,
        "glow",
        &["visual", "interface"],
        r#"
        seraphim.on("work.done", function(event)
          -- data fields are readable on the event itself
          visual.set_parameter("intensity", event.level, { layer = "sky", duration = 250, easing = "linear" })
          visual.set_color('#ff5577', { layer = "sky", name = "accent" })
        end)
        ui.notify("ready")
        "#,
    );
    let rt = start(vec![(e, grant(V))], &root, &bus);
    ready(&rt, 1);
    bus.publisher("work").publish("work.done", None, json!({ "level": 0.75 }));
    let got = wait(&rt, |c| c.len() >= 2);
    assert_eq!(
        got[0],
        Command::SetParam {
            ext: "glow".into(),
            layer: Some("sky".into()),
            param: "intensity".into(),
            value: 0.75,
            from: None,
            motion: seraphim_script::Motion { duration_ms: 250, easing: "linear".into() },
        }
    );
    assert!(
        matches!(&got[1], Command::SetColor { color, name: Some(n), .. } if color == "#ff5577" && n == "accent")
    );
}

#[test]
fn priorities_and_cancellation_across_extensions() {
    let root = dir("prio");
    let bus = EventBus::new();
    let low = ext(
        &root,
        "low",
        &["interface"],
        r#"
        seraphim.on("x.*", function() ui.notify("low") end, { priority = -5 })
        ui.notify("ready")"#,
    );
    let high = ext(
        &root,
        "high",
        &["interface"],
        r#"
        seraphim.on("x.go", function(e)
          ui.notify("high")
          if e.stop then return false end
        end, { priority = 10 })
        ui.notify("ready")"#,
    );
    let rt = start(vec![(low, grant(V)), (high, grant(V))], &root, &bus);
    ready(&rt, 2);
    let p = bus.publisher("x");
    p.publish("x.go", None, json!({}));
    let got = wait(&rt, |c| notices(c).len() >= 2);
    assert_eq!(notices(&got), ["high", "low"], "higher priority first");
    p.publish("x.go", None, json!({ "stop": true }));
    p.publish("x.other", None, json!({}));
    let got = wait(&rt, |c| notices(c).len() >= 2);
    assert_eq!(notices(&got), ["high", "low"], "cancelled x.go never reached low; x.other did");
}

#[test]
fn throttled_handlers_get_the_latest_event() {
    let root = dir("throttle");
    let bus = EventBus::new();
    let e = ext(
        &root,
        "t",
        &["interface"],
        r#"
        seraphim.on("tick", function(e) ui.notify("n=" .. e.n) end, { throttle = 150 })
        ui.notify("ready")"#,
    );
    let rt = start(vec![(e, grant(V))], &root, &bus);
    ready(&rt, 1);
    let p = bus.publisher("t");
    for n in 0..50 {
        p.publish("tick", None, json!({ "n": n }));
    }
    let got = wait(&rt, |c| notices(c).len() >= 2);
    assert_eq!(notices(&got), ["n=0", "n=49"], "first at once, the latest when the window ends");
}

#[test]
fn runaway_handlers_are_stopped_then_disabled() {
    let root = dir("budget");
    let bus = EventBus::new();
    let e = ext(
        &root,
        "spin",
        &["interface"],
        r#"
        seraphim.on("spin", function() while true do pcall(function() while true do end end) end end)
        seraphim.on("ping", function() ui.notify("pong") end)
        ui.notify("ready")"#,
    );
    let rt = start(vec![(e, grant(V))], &root, &bus);
    ready(&rt, 1);
    let p = bus.publisher("t");
    let t = Instant::now();
    p.publish("spin", None, json!({}));
    p.publish("ping", None, json!({}));
    let got = wait(&rt, |c| has_notice(c, "pong"));
    assert!(has_notice(&got, "longer than 40 ms"), "{got:?}");
    assert!(t.elapsed() < Duration::from_secs(1), "the next event is still handled promptly");
    p.publish("spin", None, json!({}));
    p.publish("spin", None, json!({}));
    let got = wait(&rt, |c| c.iter().any(|c| matches!(c, Command::Reset { .. })));
    assert!(has_notice(&got, "disabled"), "{got:?}");
    p.publish("ping", None, json!({}));
    std::thread::sleep(Duration::from_millis(150));
    assert!(!has_notice(&rt.take_commands(), "pong"), "a disabled extension gets no events");
}

#[test]
fn errors_are_reported_with_location() {
    let root = dir("errors");
    let bus = EventBus::new();
    let bad = ext(
        &root,
        "bad",
        &["interface"],
        "seraphim.on('e', function() local x = nil; return x.y end)\nui.notify('ready')",
    );
    let broken = ext(&root, "broken", &["interface"], "this is not lua");
    let rt = start(vec![(bad, grant(V)), (broken, grant(V))], &root, &bus);
    let got = wait(&rt, |c| has_notice(c, "ready") && notices(c).len() >= 2);
    assert!(notices(&got).iter().any(|n| n.contains("init.lua:1")), "{got:?}");
    bus.publisher("t").publish("e", None, json!({}));
    let got = wait(&rt, |c| !notices(c).is_empty());
    assert!(
        notices(&got)[0].contains("init.lua:1") && notices(&got)[0].contains("index"),
        "{got:?}"
    );
}

#[test]
fn permissions_decide_which_tables_exist() {
    let root = dir("perms");
    let bus = EventBus::new();
    // Asks for a lot, granted only visual + interface.
    let e = ext(
        &root,
        "greedy",
        &["visual", "interface", "terminal.read", "network", "process.exec"],
        r#"
        ui.notify("tables: " .. tostring(terminal) .. " " .. tostring(net) .. " " .. tostring(process) .. " " .. tostring(fs))
        ui.notify("has: " .. tostring(seraphim.has("visual")) .. " " .. tostring(seraphim.has("network")))
        ui.notify("ready")"#,
    );
    let rt = start(
        vec![(e, grant(&[Permission::Visual, Permission::Interface, Permission::Filesystem]))],
        &root,
        &bus,
    );
    let got = ready(&rt, 1);
    let n = notices(&got);
    assert!(
        n.iter().any(|t| t.contains("not granted: terminal.read, process.exec, network")),
        "{n:?}"
    );
    assert!(n.contains(&"tables: nil nil nil nil"), "filesystem granted but not requested: {n:?}");
    assert!(n.contains(&"has: true false"));
}

#[test]
fn terminal_snapshot_and_root_guard() {
    let root = dir("terminal");
    let bus = EventBus::new();
    let e = ext(
        &root,
        "reader",
        &["terminal.read", "terminal.write", "interface"],
        r#"
        seraphim.on("check", function()
          local c = terminal.cursor()
          ui.notify(terminal.line(2) .. "|" .. #terminal.lines() .. "|" .. c.row .. "," .. c.col .. "|" .. tostring(terminal.last_exit_code()))
          local ok, err = pcall(terminal.send, "rm -rf /\n")
          ui.notify(ok and "sent" or err)
        end)
        ui.notify("ready")"#,
    );
    let g = grant(&[Permission::TerminalRead, Permission::TerminalWrite, Permission::Interface]);
    let rt = start(vec![(e, g)], &root, &bus);
    ready(&rt, 1);
    assert!(rt.reads_terminal());
    rt.update_snapshot(|s| {
        s.lines = vec!["$ ls".into(), "ERROR: disk full".into()];
        s.cursor = (1, 4);
    });
    let session = bus.publisher("session");
    session.publish("command.started", Some(1), json!({ "command": "make" }));
    session.publish("command.completed", Some(1), json!({ "command": "make", "exit_code": 2 }));
    session.publish("check", None, json!({}));
    let got = wait(&rt, |c| notices(c).len() >= 2);
    assert_eq!(notices(&got)[0], "ERROR: disk full|2|2,5|2");
    assert_eq!(notices(&got)[1], "sent");
    assert!(got.iter().any(|c| matches!(c, Command::Send { text, .. } if text == "rm -rf /\n")));
    session.publish("session.privileged", Some(1), json!({ "root": true }));
    session.publish("check", None, json!({}));
    let got = wait(&rt, |c| notices(c).len() >= 2);
    assert!(notices(&got)[1].contains("refused, the session runs as root"), "{got:?}");
    assert!(!got.iter().any(|c| matches!(c, Command::Send { .. })));
}

#[test]
fn timers_and_animations() {
    let root = dir("anim");
    let bus = EventBus::new();
    let done = bus.subscribe("animation.completed", 8);
    let e = ext(
        &root,
        "anim",
        &["visual", "interface"],
        r#"
        animation.define("flash", {
          { layer = "sky", param = "alert", from = 0, to = 1, duration = 50 },
          { layer = "sky", param = "alert", to = 0, delay = 100, duration = 50 },
          { layer = "sky", color = '#ff0000', name = "accent", delay = 20 },
        })
        local n = 0
        local id
        id = seraphim.every(20, function()
          n = n + 1
          if n == 3 then seraphim.cancel(id); animation.play("flash") end
        end)
        seraphim.after(1, function() ui.notify("ready") end)"#,
    );
    let rt = start(vec![(e, grant(V))], &root, &bus);
    let got = wait(&rt, |c| c.iter().filter(|c| !matches!(c, Command::Notify { .. })).count() >= 3);
    let steps: Vec<_> = got.iter().filter(|c| !matches!(c, Command::Notify { .. })).collect();
    assert!(
        matches!(steps[0], Command::SetParam { value, from: Some(f), .. } if *value == 1.0 && *f == 0.0)
    );
    assert!(matches!(steps[1], Command::SetColor { .. }), "delays order the steps");
    assert!(matches!(steps[2], Command::SetParam { value, .. } if *value == 0.0));
    let e = done.recv_timeout(Duration::from_secs(2)).expect("animation.completed");
    assert_eq!(e.data["name"], "flash");
    assert_eq!(e.data["extension"], "anim");
    assert_eq!(e.source, "animation");
}

#[test]
fn emit_only_into_own_namespace() {
    let root = dir("emit");
    let bus = EventBus::new();
    let sub = bus.subscribe("*", 16);
    let e = ext(
        &root,
        "pkg",
        &["interface"],
        r#"
        seraphim.emit("pkg.hello", { n = 1 })
        local ok, err = pcall(seraphim.emit, "package.install.started", {})
        ui.notify(err)
        ui.notify("ready")"#,
    );
    let rt = start(vec![(e, grant(V))], &root, &bus);
    let got = ready(&rt, 1);
    assert!(notices(&got)[0].contains("named pkg.<something>"), "{got:?}");
    let events: Vec<_> = sub.drain().into_iter().filter(|e| e.name.starts_with("pkg")).collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source, "extension.pkg", "trusted source, set by the runtime");
}

#[test]
fn processes_files_and_storage() {
    let root = dir("io");
    let bus = EventBus::new();
    let allowed = root.join("allowed");
    std::fs::create_dir_all(&allowed).unwrap();
    let lua = format!(
        r#"
        process.run({{"echo", "hi there"}}, function(r) ui.notify("echo:" .. r.stdout .. tostring(r.code)) end)
        local ok, err = pcall(process.run, {{"sh", "-c", "true"}})
        ui.notify(err)
        fs.write({a:?}, "note")
        ui.notify("read:" .. fs.read({a:?}))
        local ok2, err2 = pcall(fs.read, "/etc/hostname")
        ui.notify(err2)
        local ok3, err3 = pcall(fs.write, {esc:?}, "x")
        ui.notify(err3)
        seraphim.storage.set("count", (seraphim.storage.get("count") or 0) + 1)
        ui.notify("count:" .. seraphim.storage.get("count"))
        ui.notify("ready")"#,
        a = allowed.join("n.txt").display().to_string(),
        esc = allowed.join("../outside.txt").display().to_string(),
    );
    let e = ext(&root, "io", &["interface", "process.exec", "filesystem"], &lua);
    let g = Grant {
        permissions: [Permission::Interface, Permission::ProcessExec, Permission::Filesystem]
            .into(),
        commands: vec!["echo".into()],
        paths: vec![allowed.clone()],
        ..Default::default()
    };
    let rt = start(vec![(e.clone(), g.clone())], &root, &bus);
    let got = wait(&rt, |c| has_notice(c, "echo:"));
    let n = notices(&got);
    assert!(n.iter().any(|t| t.contains("\"sh\" is not in commands")), "{n:?}");
    assert!(n.contains(&"read:note"));
    assert!(n.iter().filter(|t| t.contains("outside the approved paths")).count() == 2, "{n:?}");
    assert!(n.contains(&"count:1"));
    assert!(n.contains(&"echo:hi there\n0"), "{n:?}");
    assert!(!root.join("outside.txt").exists());
    drop(rt);
    // Storage persists across restarts.
    let rt = start(vec![(e, g)], &root, &bus);
    let got = wait(&rt, |c| has_notice(c, "count:"));
    assert!(notices(&got).contains(&"count:2"));
}

#[test]
fn bindings_and_commands_and_keys() {
    let root = dir("bind");
    let bus = EventBus::new();
    let e = ext(
        &root,
        "b",
        &["visual", "interface"],
        r#"
        visual.bind("sky.color", { source = "terminal.last_exit_code", mapping = { [0] = '#f5a6d0', default = '#ff5577' } })
        visual.bind("sky.intensity", { source = "system.cpu", min = 0.1, max = 1.0 })
        ui.command("hello", { title = "Say hello", run = function() ui.notify("hello!") end })
        ui.key("ctrl+shift+h", "hello")
        ui.notify("ready")"#,
    );
    let rt = start(vec![(e, grant(V))], &root, &bus);
    let got = ready(&rt, 1);
    let binds: Vec<_> = got
        .iter()
        .filter_map(|c| match c {
            Command::Bind { spec, .. } => Some(spec.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        binds[0],
        json!({ "layer": "sky", "param": "color", "source": "terminal.last_exit_code",
                "colors": { "0": "#f5a6d0", "default": "#ff5577" } })
    );
    assert_eq!(
        binds[1],
        json!({ "layer": "sky", "param": "intensity", "source": "system.cpu", "output": [0.10000000149011612, 1.0] })
    );
    assert!(got.iter().any(|c| matches!(c, Command::RegisterCommand { id, title, .. } if id == "hello" && title == "Say hello")));
    assert!(got.iter().any(|c| matches!(c, Command::RegisterKey { chord, command, .. } if chord == "ctrl+shift+h" && command == "hello")));
    rt.invoke("b", "hello");
    let got = wait(&rt, |c| has_notice(c, "hello!"));
    assert_eq!(notices(&got), ["hello!"]);
}

#[test]
fn live_reload_restarts_a_changed_extension() {
    let root = dir("reload");
    let bus = EventBus::new();
    let e = ext(&root, "live", &["interface"], "ui.notify('ready v1')");
    let mut s = settings(&root);
    s.live_reload = true;
    let rt = Runtime::start(vec![(e, grant(V))], s, &bus, Arc::new(|| {}));
    wait(&rt, |c| has_notice(c, "ready v1"));
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(root.join("live/init.lua"), "ui.notify('ready v2, longer')").unwrap();
    let got = wait(&rt, |c| has_notice(c, "ready v2"));
    assert!(got.iter().any(|c| matches!(c, Command::Reset { .. })), "old registrations dropped");
    assert!(has_notice(&got, "reloaded"));
}

#[test]
fn network_only_to_approved_hosts_without_redirects() {
    use std::io::{Read, Write};
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for (i, s) in server.incoming().enumerate() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let reply = if i == 0 {
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: yes\r\n\r\nhello".to_owned()
            } else {
                "HTTP/1.1 302 Found\r\nLocation: http://example.org/\r\nContent-Length: 0\r\n\r\n"
                    .to_owned()
            };
            let _ = s.write_all(reply.as_bytes());
        }
    });
    let root = dir("net");
    let bus = EventBus::new();
    let lua = format!(
        r#"
        local ok, err = pcall(net.get, "https://example.org/", function() end)
        ui.notify(err)
        net.get("http://127.0.0.1:{port}/a", function(r)
          ui.notify("got " .. r.status .. " " .. r.body .. " " .. r.headers["x-test"])
          net.get("http://127.0.0.1:{port}/b", function(r2) ui.notify("then " .. r2.status) end)
        end)"#
    );
    let e = ext(&root, "web", &["interface", "network"], &lua);
    let g = Grant {
        permissions: [Permission::Interface, Permission::Network].into(),
        hosts: vec!["127.0.0.1".into()],
        ..Default::default()
    };
    let rt = start(vec![(e, g)], &root, &bus);
    let got = wait(&rt, |c| has_notice(c, "then"));
    let n = notices(&got);
    assert!(n[0].contains("example.org is not in hosts"), "{n:?}");
    assert!(n.contains(&"got 200 hello yes"), "{n:?}");
    assert!(n.contains(&"then 302"), "redirects are returned, not followed: {n:?}");
}

#[test]
fn extension_panels_render_and_react() {
    let root = dir("panels");
    let bus = EventBus::new();
    let e = ext(
        &root,
        "pkg",
        &["interface"],
        r##"
        local picked = "nothing"
        ui.panel("status", {
          title = "Package Status",
          render = function()
            return {
              { type = "heading", text = "Packages" },
              { type = "list", items = { { text = "firefox" }, { text = "gimp" } } },
              { type = "text", text = "picked " .. picked },
            }
          end,
          on_select = function(n) picked = tostring(n) end,
        })
        ui.open_panel("status", { place = "down" })
        local ok, err = pcall(ui.open_panel, "nope")
        ui.notify(err)
        ui.notify("ready")"##,
    );
    let rt = start(vec![(e, grant(V))], &root, &bus);
    let got = ready(&rt, 1);
    assert!(got.contains(&Command::RegisterPanel {
        ext: "pkg".into(),
        id: "status".into(),
        title: "Package Status".into()
    }));
    assert!(got.contains(&Command::OpenPanel {
        ext: "pkg".into(),
        id: "status".into(),
        place: "down".into()
    }));
    assert!(notices(&got)[0].contains("no panel \"nope\""));
    rt.panel("pkg", "status", seraphim_script::PanelEvent::Open(true));
    let got = wait(&rt, |c| c.iter().any(|c| matches!(c, Command::PanelContent { .. })));
    let Some(Command::PanelContent { widgets, .. }) =
        got.iter().find(|c| matches!(c, Command::PanelContent { .. }))
    else {
        unreachable!()
    };
    assert_eq!(widgets[1]["items"][1]["text"], "gimp");
    assert_eq!(widgets[2]["text"], "picked nothing");
    rt.panel("pkg", "status", seraphim_script::PanelEvent::Select(2));
    let got = wait(&rt, |c| c.iter().any(|c| matches!(c, Command::PanelContent { .. })));
    let Some(Command::PanelContent { widgets, .. }) =
        got.iter().find(|c| matches!(c, Command::PanelContent { .. }))
    else {
        unreachable!()
    };
    assert_eq!(widgets[2]["text"], "picked 2", "re-rendered after the selection");
}
