//! Run vttest against Seraphim and compare every screen with real xterm.
//!
//!   DISPLAY=:99 cargo run -p seraphim-conformance --example vttest -- [TEST...]
//!
//! vttest runs in a PTY; Seraphim's Terminal Core interprets its output and
//! answers its queries. At each pause ("Push <RETURN>") the byte stream so
//! far is replayed into xterm and the two screens are compared. Default
//! tests: 1 (cursor movement), 2 (screen features), 8 (VT102 insert/delete).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use seraphim_conformance::{snapshot_seraphim, xterm};
use seraphim_pty::{is_eof_error, Pty, PtyCommand, PtySize};
use seraphim_terminal::Terminal;

const COLS: usize = 80;
const ROWS: usize = 24;

struct Run {
    pty: Pty,
    writer: std::fs::File,
    bytes: Arc<Mutex<Vec<u8>>>,
    fed: usize,
    term: Terminal,
}

impl Run {
    fn start() -> Run {
        let cmd = PtyCommand::new("vttest").env("TERM", "xterm");
        let pty = Pty::spawn(&cmd, PtySize::new(COLS as u16, ROWS as u16)).expect("spawn vttest");
        let mut reader = pty.reader().unwrap();
        let writer = pty.writer().unwrap();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let sink = bytes.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&buf[..n]),
                    Err(e) if is_eof_error(&e) => break,
                    Err(_) => break,
                }
            }
        });
        Run { pty, writer, bytes, fed: 0, term: Terminal::new(COLS, ROWS) }
    }

    /// Feed new output to the terminal (answering queries) until quiet.
    fn settle(&mut self) {
        let mut last_change = Instant::now();
        loop {
            let new: Vec<u8> = {
                let b = self.bytes.lock().unwrap();
                b[self.fed..].to_vec()
            };
            if !new.is_empty() {
                self.fed += new.len();
                self.term.advance(&new);
                let resp = self.term.take_responses();
                if !resp.is_empty() {
                    let _ = self.writer.write_all(&resp);
                }
                last_change = Instant::now();
            } else if last_change.elapsed() > Duration::from_millis(400) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, s: &str) {
        self.writer.write_all(s.as_bytes()).unwrap();
        self.settle();
    }

    fn screen(&self) -> String {
        self.term.screen_text()
    }
}

fn main() {
    if !xterm::available() {
        eprintln!("needs xterm and an X display (try Xvfb :99 & DISPLAY=:99)");
        std::process::exit(2);
    }
    let tests: Vec<String> = {
        let a: Vec<String> = std::env::args().skip(1).collect();
        if a.is_empty() {
            vec!["1".into(), "2".into(), "8".into()]
        } else {
            a
        }
    };
    let work = std::env::temp_dir().join(format!("seraphim-vttest-{}", std::process::id()));
    let mut run = Run::start();
    run.settle();
    let (mut screens, mut matching) = (0, 0);
    let mut report = Vec::new();
    for test in &tests {
        assert!(run.screen().contains("Choose test type"), "not at main menu:\n{}", run.screen());
        run.send(&format!("{test}\r"));
        for step in 1..=60 {
            let s = run.screen();
            if s.contains("Choose test type") {
                break;
            }
            // Compare this screen with xterm fed the identical byte stream.
            let all = without_column_switching(&run.bytes.lock().unwrap()[..run.fed]);
            let ours = snapshot_seraphim(&run.term);
            if let Some(x) = xterm::run(COLS, ROWS, &all, &work) {
                screens += 1;
                let norm = |t: String| {
                    t.lines()
                        .map(|l| l.trim_end().to_owned())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim_end()
                        .to_owned()
                };
                let same_text = norm(ours.text()) == norm(x.text());
                // In origin mode xterm reports the cursor relative to the
                // scroll region; only compare absolute positions.
                let same_cursor = run.term.modes().origin || x.cursor == Some(ours.cursor);
                if same_text && same_cursor {
                    matching += 1;
                } else {
                    if std::env::var_os("VTTEST_DUMP").is_some() {
                        println!(
                            "--- test {test} screen {step} seraphim:\n{}\n--- xterm:\n{}\n---",
                            ours.text(),
                            x.text()
                        );
                    }
                    let first_bad = ours
                        .lines
                        .iter()
                        .zip(&x.lines)
                        .enumerate()
                        .find(|(_, (a, b))| a.trim_end() != b.trim_end());
                    let raw = &run.bytes.lock().unwrap()[..run.fed];
                    let find = |p: &[u8]| raw.windows(p.len()).rposition(|w| w == p);
                    let wide = find(b"\x1b[?3h") > find(b"\x1b[?3l");
                    report.push(format!(
                        "test {test} screen {step}{}: text {} cursor seraphim {:?} xterm {:?}{}",
                        if wide { " [program requested 132 columns]" } else { "" },
                        if same_text { "same" } else { "DIFFERS" },
                        ours.cursor,
                        x.cursor,
                        first_bad
                            .map(|(i, (a, b))| format!(
                                "\n    row {i}: seraphim {a:?}\n            xterm    {b:?}"
                            ))
                            .unwrap_or_default()
                    ));
                }
            }
            if s.contains("Enter choice number") {
                // A sub-menu: leave it.
                run.send("0\r");
            } else {
                run.send("\r");
            }
        }
    }
    run.send("0\r");
    let _ = run.pty.hangup();
    for r in &report {
        println!("{r}");
    }
    println!("\nvttest {tests:?}: {matching}/{screens} screens identical to xterm");
}

/// Seraphim (like most modern terminals) ignores the 80/132-column switch.
/// xterm honours it once a program enables it with `?40h`, so remove those
/// sequences from xterm's copy of the stream for a like-for-like comparison.
fn without_column_switching(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for pat in [&b"\x1b[?40h"[..], b"\x1b[?3h", b"\x1b[?3l", b"\x1b[132$|", b"\x1b[80$|"] {
        while let Some(i) = out.windows(pat.len()).position(|w| w == pat) {
            out.drain(i..i + pat.len());
        }
    }
    out
}
