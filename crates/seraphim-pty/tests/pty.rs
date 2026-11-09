use std::io::{Read, Write};
use std::time::{Duration, Instant};

use seraphim_pty::{is_eof_error, ProcessInfo, Pty, PtyCommand, PtySize};

/// Read everything until the child side closes.
fn read_all(pty: &Pty) -> String {
    let mut r = pty.reader().unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if is_eof_error(&e) => break,
            Err(e) => panic!("{e}"),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn runs_command_and_reports_exit() {
    let cmd = PtyCommand::new("/bin/sh").args(["-c", "printf 'hi\\n'; exit 3"]);
    let mut pty = Pty::spawn(&cmd, PtySize::new(80, 24)).unwrap();
    let out = read_all(&pty);
    assert_eq!(out, "hi\r\n", "pty translates \\n to \\r\\n");
    assert_eq!(pty.wait().unwrap().code(), Some(3));
}

#[test]
fn child_is_a_tty_with_our_size_and_env() {
    let cmd = PtyCommand::new("/bin/sh")
        .args(["-c", "test -t 0 && test -t 1 && stty size && echo $TERM $FOO"])
        .env("FOO", "bar");
    let pty = Pty::spawn(&cmd, PtySize::new(100, 30)).unwrap();
    assert_eq!(read_all(&pty).trim(), "30 100\r\nxterm-256color bar");
}

#[test]
fn resize_is_visible_to_child() {
    let cmd = PtyCommand::new("/bin/sh").args(["-c", "read x; stty size"]);
    let pty = Pty::spawn(&cmd, PtySize::new(80, 24)).unwrap();
    pty.resize(PtySize::new(132, 43)).unwrap();
    assert_eq!(pty.size().unwrap(), PtySize::new(132, 43));
    pty.writer().unwrap().write_all(b"go\n").unwrap();
    let out = read_all(&pty);
    assert!(out.ends_with("43 132\r\n"), "{out:?}");
}

#[test]
fn input_reaches_child_and_cwd_is_honoured() {
    let dir = std::env::temp_dir();
    let cmd =
        PtyCommand::new("/bin/sh").args(["-c", "read line; echo got:$line; pwd -P"]).cwd(&dir);
    let pty = Pty::spawn(&cmd, PtySize::default()).unwrap();
    let info = wait_for(|| ProcessInfo::read(pty.pid()));
    assert_eq!(
        info.cwd.as_deref().map(|p| p.canonicalize().unwrap()),
        Some(dir.canonicalize().unwrap())
    );
    pty.writer().unwrap().write_all(b"hello\n").unwrap();
    let out = read_all(&pty);
    assert!(out.contains("got:hello"), "{out:?}");
}

#[test]
fn dropping_pty_hangs_up_the_session() {
    let cmd = PtyCommand::new("/bin/sh").args(["-c", "sleep 30"]);
    let pty = Pty::spawn(&cmd, PtySize::default()).unwrap();
    let pid = pty.pid();
    let start = Instant::now();
    drop(pty);
    // Reaped by Drop (or at least no longer alive as a running process).
    let gone = wait_for(|| match ProcessInfo::read(pid) {
        None => Some(true),
        Some(_) if start.elapsed() > Duration::from_secs(2) => Some(false),
        Some(_) => None,
    });
    assert!(gone, "child survived hangup");
}

fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn finds_terminfo_entries() {
    let d = std::env::temp_dir().join(format!("seraphim-ti-{}", std::process::id()));
    std::fs::create_dir_all(d.join("x")).unwrap();
    std::fs::create_dir_all(d.join("79")).unwrap();
    std::fs::write(d.join("x/xterm-seraphim-test"), b"").unwrap();
    std::fs::write(d.join("79/ylinux-hex"), b"").unwrap();
    std::env::set_var("TERMINFO", &d);
    assert!(seraphim_pty::terminfo_installed("xterm-seraphim-test"));
    assert!(seraphim_pty::terminfo_installed("ylinux-hex"), "macOS-style hex directories");
    assert!(!seraphim_pty::terminfo_installed("no-such-terminal"));
    assert!(!seraphim_pty::terminfo_installed(""));
}
