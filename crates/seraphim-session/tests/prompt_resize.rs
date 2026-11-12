//! Resizing while a real shell waits at its prompt. Skipped (passes, with
//! a note) without bash.

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

/// A prompt long enough to wrap when the terminal narrows.
const PS1: &str = "[a-rather-long-prompt-that-wraps-when-the-terminal-narrows] $ ";

fn have_bash() -> bool {
    let found = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("bash").is_file()))
        .unwrap_or(false);
    if !found {
        eprintln!("skipping: bash not installed");
    }
    found
}

fn bash(cols: u16, rows: u16) -> (EventBus, Session) {
    let bus = EventBus::new();
    let cmd = Command::new("bash")
        .args(["--norc", "--noprofile", "-i"])
        .env("PS1", PS1)
        .env("HOME", std::env::temp_dir())
        .env("TERMINFO_DIRS", terminfo_dirs());
    let s = Session::spawn(
        1,
        SessionConfig { command: cmd, cols, rows, scrollback: 1000, ..Default::default() },
        &bus,
    )
    .unwrap();
    (bus, s)
}

/// Wait until the terminal has been quiet for 250 ms.
fn settle(s: &Session) {
    let mut last = s.generation();
    let mut quiet = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        let g = s.generation();
        if g != last {
            last = g;
            quiet = Instant::now();
        }
        if quiet.elapsed() > Duration::from_millis(250) {
            return;
        }
    }
}

fn all_text(s: &Session) -> String {
    let t = s.terminal();
    let lines: Vec<String> =
        t.scrollback().iter().chain(t.screen().rows_iter()).map(|r| r.text()).collect();
    lines.join("\n")
}

fn wait_for(s: &Session, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !all_text(s).contains(needle) {
        assert!(Instant::now() < deadline, "timed out waiting for {needle:?}:\n{}", all_text(s));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The shell redraws its prompt after each resize; the old copy must not
/// stay behind in pieces (or be joined to the new one when widening).
#[test]
fn resizing_at_a_prompt_leaves_one_prompt() {
    if !have_bash() {
        return;
    }
    let (_bus, s) = bash(80, 12);
    // Some output above the prompt, which must survive.
    s.write(b"for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14; do echo line $i; done\r").unwrap();
    wait_for(&s, "line 14");
    settle(&s);
    for cols in [30, 50, 25, 80, 40, 80] {
        s.resize(cols, 12).unwrap();
        settle(&s);
    }
    let text = all_text(&s);
    assert_eq!(
        text.matches("a-rather-long-prompt").count(),
        2,
        "the command's prompt and one live prompt:\n{text}"
    );
    assert!(text.contains("line 14"), "{text}");
    // Near the bottom: readline redraws the (now one-row) prompt where its
    // three-row copy from 25 columns began, two rows up.
    let row = s.terminal().cursor().row;
    assert!(row >= 9, "the prompt stays at the bottom, not climbing: row {row}");
}

/// A program reading a line (here `cat`) owns the terminal: resizing must
/// not erase what has been typed so far.
#[test]
fn resizing_while_a_program_runs_keeps_its_line() {
    if !have_bash() {
        return;
    }
    let (_bus, s) = bash(80, 12);
    settle(&s);
    s.write(b"cat\r").unwrap();
    settle(&s);
    s.write(b"half a line").unwrap();
    wait_for(&s, "half a line");
    s.resize(40, 12).unwrap();
    settle(&s);
    s.resize(80, 12).unwrap();
    settle(&s);
    assert!(all_text(&s).contains("half a line"), "{}", all_text(&s));
    s.write(b"\x04").unwrap();
}
