use std::sync::Arc;
use std::time::{Duration, Instant};

use seraphim_events::{Event, EventBus, Subscription};
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
