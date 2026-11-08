//! xterm as an oracle: run bytes in a real xterm (needs `xterm` and an X
//! display, e.g. Xvfb), read the cursor back with DSR 6 and the screen text
//! with xterm's Media Copy (`CSI i`, routed to a file via printerCommand).

use std::path::Path;
use std::process::Command;

const SCRIPT: &str = r#"#!/bin/bash
stty raw -echo
cat "$1"
# Discard xterm's answers to any queries contained in the case itself.
sleep 0.2
while IFS= read -r -t 0.2 -n 4096 _junk; do :; done
printf '\033[6n'
IFS= read -r -d R -t 2 reply
printf "%s" "${reply##*[}" > "$2/cursor.txt"
# DECPEX (?19h): print the whole page, not just the scroll region.
printf '\033[?19h\033[i'
sleep 0.3
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XtermScreen {
    /// Lines with trailing blanks trimmed.
    pub lines: Vec<String>,
    /// 0-based (row, col). Relative to the scroll region if origin mode
    /// was left on.
    pub cursor: Option<(usize, usize)>,
}

impl XtermScreen {
    pub fn text(&self) -> String {
        let mut v = self.lines.clone();
        while v.last().is_some_and(|l| l.is_empty()) {
            v.pop();
        }
        v.join("\n")
    }
}

pub fn available() -> bool {
    std::env::var_os("DISPLAY").is_some()
        && Command::new("xterm").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Run `bytes` in a `cols`x`rows` xterm.
pub fn run(cols: usize, rows: usize, bytes: &[u8], workdir: &Path) -> Option<XtermScreen> {
    std::fs::create_dir_all(workdir).ok()?;
    let script = workdir.join("run.sh");
    let case = workdir.join("case.bin");
    let screen = workdir.join("screen.txt");
    let cursor = workdir.join("cursor.txt");
    let _ = std::fs::remove_file(&screen);
    let _ = std::fs::remove_file(&cursor);
    std::fs::write(&script, SCRIPT).ok()?;
    std::fs::write(&case, bytes).ok()?;
    let status = Command::new("timeout")
        .arg("10")
        .arg("xterm")
        .args(["-geometry", &format!("{cols}x{rows}"), "-u8"])
        .args(["-xrm", &format!("XTerm*printerCommand: cat > {}", screen.display())])
        .args(["-xrm", "XTerm*printAttributes: 0", "-xrm", "XTerm*printerAutoClose: true"])
        .args(["-xrm", "XTerm*allowWindowOps: false"])
        // Like Seraphim (and most modern terminals), ignore DECCOLM's
        // switch to 132 columns.
        .args(["-xrm", "XTerm*c132: false", "+132"])
        .arg("-e")
        .args(["bash", &script.display().to_string(), &case.display().to_string()])
        .arg(workdir.display().to_string())
        .env("LC_ALL", "C.UTF-8")
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let text = std::fs::read_to_string(&screen).ok()?;
    let mut lines: Vec<String> =
        text.lines().map(|l| l.trim_end_matches(' ').to_owned()).take(rows).collect();
    lines.resize(rows, String::new());
    let cursor = std::fs::read_to_string(&cursor).ok().and_then(|c| {
        let (r, c) = c.split_once(';')?;
        Some((r.parse::<usize>().ok()?.checked_sub(1)?, c.parse::<usize>().ok()?.checked_sub(1)?))
    });
    Some(XtermScreen { lines, cursor })
}
