//! Desktop notifications through the freedesktop notification service:
//! `notify-send` when it is installed, else `gdbus` (part of GLib, on
//! nearly every desktop). Never blocks: the command runs and is reaped on
//! a thread of its own.

use std::process::{Command, Stdio};

/// Show a notification. Returns false if neither tool could be started.
pub fn notify(title: &str, body: &str) -> bool {
    let title = if title.is_empty() { "Seraphim" } else { title };
    let mut send = Command::new("notify-send");
    send.args(["--app-name=Seraphim", "--icon=seraphim", "--", title]);
    send.arg(notify_send_body(body));
    if spawn(send) {
        return true;
    }
    let mut gdbus = Command::new("gdbus");
    gdbus.args([
        "call",
        "--session",
        "--dest=org.freedesktop.Notifications",
        "--object-path=/org/freedesktop/Notifications",
        "--method=org.freedesktop.Notifications.Notify",
        "Seraphim",
        "0",
        "seraphim",
        &gvariant_string(title),
        &gvariant_string(body),
        "[]",
        "{}",
        "-1",
    ]);
    spawn(gdbus)
}

fn spawn(mut cmd: Command) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
            true
        }
        Err(_) => false,
    }
}

/// `notify-send` unescapes the body, not the summary (`\n` becomes a line
/// break, `\a` an `a`), so the body's backslashes are doubled to arrive as
/// typed.
fn notify_send_body(s: &str) -> String {
    s.replace('\\', "\\\\")
}

/// A GVariant string literal, as `gdbus call` parses its arguments.
fn gvariant_string(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn gvariant_quoting() {
        assert_eq!(super::gvariant_string("it's a \\ test\nok"), "'it\\'s a \\\\ test\\nok'");
    }

    #[test]
    fn notify_send_quoting() {
        assert_eq!(super::notify_send_body("printf '\\a' C:\\x\nok"), "printf '\\\\a' C:\\\\x\nok");
    }
}
