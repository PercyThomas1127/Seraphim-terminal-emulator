use std::sync::Arc;
use std::time::{Duration, Instant};

use seraphim_events::{json, Event, EventBus, Subscription};
use seraphim_session::{Command, Session, SessionConfig, SessionManager};

fn integration_script() -> String {
    format!("{}/../../shell-integration/seraphim.bash", env!("CARGO_MANIFEST_DIR"))
}

fn wait_event(sub: &Subscription, name: &str) -> Arc<Event> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match sub.recv_timeout(left) {
            Ok(e) if e.name == name => return e,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for {name}"),
        }
    }
}

fn wait_screen(s: &Session, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.terminal().screen_text().contains(needle) {
        assert!(
            Instant::now() < deadline,
            "screen never showed {needle:?}:\n{}",
            s.terminal().screen_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn runs_program_and_reports_exit() {
    let bus = EventBus::new();
    let sub = bus.subscribe("session.*", 64);
    let cmd = Command::new("/bin/sh").args(["-c", "printf '\\033[1mhello\\033[0m world'; exit 7"]);
    let s = Session::spawn(1, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    let exit = s.wait_exit(Duration::from_secs(10)).expect("exited");
    assert_eq!(exit.code, Some(7));
    assert_eq!(s.terminal().screen_text(), "hello world");
    assert_eq!(wait_event(&sub, "session.created").session, Some(1));
    assert_eq!(wait_event(&sub, "session.exited").data["exit_code"], 7);
}

#[test]
fn answers_device_status_queries() {
    // The app asks for the cursor position and prints what it receives.
    let script = "stty -echo -icanon; printf 'ab\\033[6n'; \
                  r=$(dd bs=1 count=6 2>/dev/null | od -An -c | tr -d ' \\n'); echo \" [$r]\"";
    let bus = EventBus::new();
    let cmd = Command::new("/bin/sh").args(["-c", script]);
    let s = Session::spawn(1, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    s.wait_exit(Duration::from_secs(10)).expect("exited");
    assert!(s.terminal().screen_text().contains(r"[033[1;3R]"), "{}", s.terminal().screen_text());
}

#[test]
fn bash_shell_integration_publishes_command_events() {
    let bus = EventBus::new();
    let sub = bus.subscribe("*", 4096);
    let cmd = Command::new("bash")
        .args(["--noprofile", "--rcfile", &integration_script(), "-i"])
        .env("PS1", "$ ")
        .env("HISTFILE", "/dev/null")
        .cwd("/");
    let s = Session::spawn(
        1,
        SessionConfig { command: cmd, cols: 60, rows: 10, ..Default::default() },
        &bus,
    )
    .unwrap();

    wait_event(&sub, "shell.prompt");
    s.write(b"cd /tmp && (exit 3)\r").unwrap();

    let started = wait_event(&sub, "command.started");
    assert_eq!(started.data["command"], "cd /tmp && (exit 3)");
    let done = wait_event(&sub, "command.completed");
    assert_eq!(done.data["command"], "cd /tmp && (exit 3)");
    assert_eq!(done.data["exit_code"], 3);
    let cwd = wait_event(&sub, "session.cwd.changed");
    assert_eq!(cwd.data["cwd"], "/tmp");
    assert_eq!(s.terminal().cwd(), Some("/tmp"));

    // An empty line is not a command.
    s.write(b"\r").unwrap();
    wait_event(&sub, "shell.prompt");
    s.write(b"echo marker-$((6*7))\r").unwrap();
    let done = wait_event(&sub, "command.completed");
    assert_eq!(done.data["command"], "echo marker-$((6*7))");
    assert_eq!(done.data["exit_code"], 0);
    wait_screen(&s, "marker-42");

    s.write(b"exit\r").unwrap();
    assert!(s.wait_exit(Duration::from_secs(10)).is_some());
}

#[test]
fn privilege_is_reported_from_process_metadata() {
    let bus = EventBus::new();
    let sub = bus.subscribe("session.privileged", 8);
    let cmd = Command::new("/bin/sh").args(["-c", "sleep 0.2"]);
    let _s = Session::spawn(1, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    let ev = wait_event(&sub, "session.privileged");
    let is_root = unsafe { libc_geteuid() } == 0;
    assert_eq!(ev.data["root"], is_root);
}

extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

#[test]
fn resize_propagates_to_terminal_and_child() {
    let bus = EventBus::new();
    let cmd = Command::new("/bin/sh").args(["-c", "read x; stty size"]);
    let s = Session::spawn(1, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    s.resize(100, 40).unwrap();
    {
        let t = s.terminal(); // one guard: the lock is not re-entrant
        assert_eq!((t.cols(), t.rows()), (100, 40));
    }
    s.write(b"\r").unwrap();
    s.wait_exit(Duration::from_secs(10)).unwrap();
    assert!(s.terminal().screen_text().contains("40 100"));
}

#[test]
fn manager_tracks_sessions_independently() {
    let bus = EventBus::new();
    let sub = bus.subscribe("session.closed", 8);
    let mut m = SessionManager::new(bus);
    let quick = Command::new("/bin/sh").args(["-c", "exit 0"]);
    let long = Command::new("/bin/sh").args(["-c", "sleep 30"]);
    let a = m.create(SessionConfig { command: quick, ..Default::default() }).unwrap();
    let b = m.create(SessionConfig { command: long, ..Default::default() }).unwrap();
    assert_eq!(m.active().unwrap().id(), a);
    assert!(m.switch_to(b));
    m.get(a).unwrap().wait_exit(Duration::from_secs(10)).unwrap();
    assert_eq!(m.reap(), vec![a]);
    assert_eq!(m.ids(), vec![b]);
    assert_eq!(wait_event(&sub, "session.closed").session, Some(a));
    assert!(m.close(b));
    assert!(m.ids().is_empty());
}

#[test]
fn clipboard_requests_are_queued_privately() {
    let bus = EventBus::new();
    let sub = bus.subscribe("clipboard.requested", 8);
    let cmd =
        Command::new("/bin/sh").args(["-c", "printf '\\033]52;c;aGVsbG8=\\007\\033]52;p;?\\007'"]);
    let s = Session::spawn(1, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    s.wait_exit(Duration::from_secs(10)).unwrap();
    assert_eq!(
        s.take_clipboard_requests(),
        vec![
            seraphim_session::ClipboardRequest::Store {
                selection: "c".into(),
                data: "aGVsbG8=".into()
            },
            seraphim_session::ClipboardRequest::Load { selection: "p".into() },
        ]
    );
    assert!(s.take_clipboard_requests().is_empty());
    let ev = wait_event(&sub, "clipboard.requested");
    assert_eq!(ev.data["op"], "store");
    assert!(ev.data.get("data").is_none(), "payload must not be broadcast");
}

#[test]
#[should_panic(expected = "re-entered")]
fn relocking_terminal_on_same_thread_panics_instead_of_deadlocking() {
    let bus = EventBus::new();
    let cmd = Command::new("/bin/sh").args(["-c", "sleep 5"]);
    let s = Session::spawn(1, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    let _a = s.terminal();
    let _b = s.terminal();
}

#[test]
fn terminal_guard_releases_on_drop() {
    let bus = EventBus::new();
    let cmd = Command::new("/bin/sh").args(["-c", "sleep 5"]);
    let s = Session::spawn(1, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    let rows = s.terminal().rows();
    let cols = s.terminal().cols(); // a second, sequential lock is fine
    assert_eq!((cols, rows), (80, 24));
}

#[test]
fn a_moved_session_publishes_on_its_new_bus() {
    // A tab moved to another window: the shell keeps running, and its
    // events (and its current directory) go to the new window's bus.
    let old = EventBus::new();
    let new = EventBus::new();
    let old_sub = old.subscribe("*", 256);
    let new_sub = new.subscribe("*", 256);
    let cmd = Command::new("/bin/sh").args([
        "-c",
        "printf '\\033]7;file:///tmp\\007'; read x; printf 'after %s\\a' \"$x\"; read y",
    ]);
    let s = Session::spawn(5, SessionConfig { command: cmd, ..Default::default() }, &old).unwrap();
    wait_event(&old_sub, "session.cwd.changed");
    s.move_to(&new);
    assert_eq!(wait_event(&new_sub, "session.cwd.changed").data["cwd"], "/tmp");
    s.write(b"moved\n").unwrap();
    wait_screen(&s, "after moved");
    assert_eq!(wait_event(&new_sub, "terminal.bell").session, Some(5));
    while let Ok(e) = old_sub.recv_timeout(Duration::from_millis(50)) {
        assert_ne!(e.name, "terminal.bell", "the old bus still gets events");
    }
    s.write(b"\n").unwrap();
    s.wait_exit(Duration::from_secs(10)).expect("exited");
}

#[test]
fn package_manager_commands_become_package_events() {
    // Shell integration reports `pacman -S a b`; its output says how far
    // it got.
    let bus = EventBus::new();
    let sub = bus.subscribe("package.*", 64);
    let script = "printf '\\033]133;C;cmdline_url=sudo%%20pacman%%20-S%%20a%%20b\\007'; \
                  sleep 0.2; printf 'resolving dependencies...\\r\\n(1/2) installing a\\r\\n'; sleep 0.2; \
                  printf '(2/2) installing b\\r\\n'; sleep 0.2; printf '\\033]133;D;0\\007'; sleep 0.3";
    let cmd = Command::new("/bin/sh").args(["-c", script]);
    let s = Session::spawn(3, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    let started = wait_event(&sub, "package.install.started");
    assert_eq!(started.source, "package");
    assert_eq!(started.data["manager"], "pacman");
    assert_eq!(started.data["packages"], json!(["a", "b"]));
    let mut last = None;
    loop {
        let e = sub.recv_timeout(Duration::from_secs(10)).expect("events");
        if e.name == "package.install.progress" {
            last = Some(e.data["percent"].clone());
            continue;
        }
        assert_eq!(e.name, "package.install.completed");
        assert_eq!(e.data["exit_code"], 0);
        assert_eq!(e.data["done"], 2);
        break;
    }
    assert_eq!(last, Some(json!(100)));
    s.wait_exit(Duration::from_secs(10));
}

#[test]
fn package_managers_are_noticed_without_shell_integration() {
    if !std::path::Path::new("/bin/bash").exists() {
        return;
    }
    // A stand-in `apt` that prints like the real one.
    let dir = std::env::temp_dir().join(format!("seraphim-fake-apt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let apt = dir.join("apt");
    std::fs::write(
        &apt,
        "#!/bin/sh\nsleep 0.6\necho 'Unpacking htop (3.3) ...'\nsleep 0.4\necho 'Setting up htop (3.3) ...'\nsleep 0.6\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&apt, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bus = EventBus::new();
    let sub = bus.subscribe("package.*", 64);
    let cmd = Command::new("/bin/bash").args(["--norc", "--noprofile", "-i"]);
    let s = Session::spawn(4, SessionConfig { command: cmd, ..Default::default() }, &bus).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    s.write(format!("PS1='$ '; {} install htop; echo done\n", apt.display()).as_bytes()).unwrap();
    let e = wait_event(&sub, "package.install.started");
    assert_eq!(
        (e.data["manager"].clone(), e.data["packages"].clone()),
        (json!("apt"), json!(["htop"]))
    );
    let e = wait_event(&sub, "package.install.completed");
    assert_eq!(e.data["package"], "htop");
    assert!(e.data["exit_code"].is_null(), "unknown without shell integration");
    s.write(b"exit\n").unwrap();
    s.wait_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(dir);
}
