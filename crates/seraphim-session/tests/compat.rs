//! Compatibility tests against real full-screen programs. Each test is
//! skipped (passes trivially, with a note) if the program is not installed.

use std::path::Path;
use std::time::{Duration, Instant};

use seraphim_events::EventBus;
use seraphim_session::{Command, Session, SessionConfig};

/// The child gets a temporary HOME, but Seraphim's TERM may be an entry in
/// the real ~/.terminfo: keep that where ncurses looks.
fn terminfo_dirs() -> std::ffi::OsString {
    let mut dirs = std::ffi::OsString::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(std::path::Path::new(&home).join(".terminfo"));
    }
    // An empty entry is the system's own directories.
    dirs.push(":");
    if let Some(d) = std::env::var_os("TERMINFO_DIRS") {
        dirs.push(d);
    }
    dirs
}

fn have(prog: &str) -> bool {
    let found = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
        .unwrap_or(false);
    if !found {
        eprintln!("skipping: {prog} not installed");
    }
    found
}

fn wait_until(s: &Session, what: &str, f: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen = s.terminal().screen_text();
        if f(&screen) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; screen:\n{screen}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn(cmd: Command, cols: u16, rows: u16) -> (EventBus, Session) {
    let bus = EventBus::new();
    let s =
        Session::spawn(1, SessionConfig { command: cmd, cols, rows, ..Default::default() }, &bus)
            .unwrap();
    (bus, s)
}

fn fixture(name: &str, lines: usize) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("seraphim-compat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let body: String = (1..=lines).map(|i| format!("line {i:03} ✦ 天使\n")).collect();
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn vim_edit_save_and_restore_screen() {
    if !have("vim") {
        return;
    }
    let file = fixture("vim.txt", 100);
    let cmd = Command::new("vim")
        .args(["-u", "NONE", "-i", "NONE", "-N", "-n"])
        .arg(file.as_os_str())
        .env("HOME", std::env::temp_dir())
        .env("TERMINFO_DIRS", terminfo_dirs());
    let (_bus, s) = spawn(cmd, 60, 15);

    wait_until(&s, "vim to draw the file", |t| t.contains("line 001 ✦ 天使"));
    assert!(s.terminal().is_alternate_screen());

    // Jump to the end, append a line, save, quit.
    s.write(b"Go\xe2\x9c\xa8 seraphim was here\x1b").unwrap();
    wait_until(&s, "typed text", |t| t.contains("✨ seraphim was here"));
    {
        let t = s.terminal();
        assert!(t.screen_text().contains("line 100"), "scrolled to end");
        assert!(!t.screen_text().contains("line 001"), "top of file scrolled away");
    }
    s.write(b":wq\r").unwrap();
    let exit = s.wait_exit(Duration::from_secs(10)).expect("vim exited");
    assert_eq!(exit.code, Some(0));
    assert!(!s.terminal().is_alternate_screen(), "alternate screen left");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.ends_with("line 100 ✦ 天使\n✨ seraphim was here\n"), "{saved:?}");
}

#[test]
fn less_pages_and_quits() {
    if !have("less") {
        return;
    }
    let file = fixture("less.txt", 200);
    let cmd = Command::new("less").arg(file.as_os_str()).env("LESS", "").env("LESSHISTFILE", "-");
    let (_bus, s) = spawn(cmd, 40, 10);
    wait_until(&s, "first page", |t| t.contains("line 001") && t.contains("line 009"));
    s.write(b" ").unwrap(); // page down
    wait_until(&s, "second page", |t| t.contains("line 010") && !t.contains("line 001"));
    s.write(b"G").unwrap(); // end
    wait_until(&s, "last page", |t| t.contains("line 200"));
    s.write(b"q").unwrap();
    assert_eq!(s.wait_exit(Duration::from_secs(10)).unwrap().code, Some(0));
}

#[test]
fn tmux_draws_status_bar_and_runs_commands() {
    if !have("tmux") {
        return;
    }
    let sock = std::env::temp_dir().join(format!("seraphim-tmux-{}", std::process::id()));
    let cmd = Command::new("tmux")
        .args(["-S"])
        .arg(sock.as_os_str())
        .args(["-f", "/dev/null", "new-session", "sh"])
        .env("PS1", "$ ");
    let (_bus, s) = spawn(cmd, 70, 12);

    // tmux paints its status line on the last row, using a scroll region
    // for the pane above it.
    wait_until(&s, "tmux status bar", |_| {
        let t = s.terminal();
        let last = t.screen().row(t.rows() - 1).text();
        last.contains("[0]") || last.contains("0:")
    });
    s.write(b"for i in $(seq 1 30); do echo row$i; done\r").unwrap();
    wait_until(&s, "pane output", |t| t.contains("row30"));
    {
        let t = s.terminal();
        let last = t.screen().row(t.rows() - 1).text();
        assert!(
            last.contains("0:") || last.contains("[0]"),
            "status bar survived scrolling: {last:?}"
        );
    }
    s.write(b"exit\r").unwrap();
    s.wait_exit(Duration::from_secs(10)).expect("tmux exited");
    let _ = std::fs::remove_file(Path::new(&sock));
}

#[test]
fn nano_opens_and_exits() {
    if !have("nano") {
        return;
    }
    let file = fixture("nano.txt", 5);
    let cmd = Command::new("nano").args(["--ignorercfiles"]).arg(file.as_os_str());
    let (_bus, s) = spawn(cmd, 60, 12);
    wait_until(&s, "nano UI", |t| t.contains("line 001") && t.contains("^X"));
    s.write(b"\x18").unwrap(); // Ctrl-X
    assert_eq!(s.wait_exit(Duration::from_secs(10)).unwrap().code, Some(0));
}
