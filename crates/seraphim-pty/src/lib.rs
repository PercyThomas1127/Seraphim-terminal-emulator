//! # seraphim-pty
//!
//! PTY management for the Terminal Core (spec §3.2): creating a
//! pseudo-terminal, launching a shell or program as its session leader,
//! propagating window size, and tracking the child process.
//!
//! Linux-first (spec §13 "Portability"); the API avoids Linux-only types so
//! a macOS/BSD backend can be added behind the same surface.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

/// Terminal dimensions in cells (and optionally pixels, for image protocols).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl PtySize {
    pub fn new(cols: u16, rows: u16) -> Self {
        PtySize { cols, rows, pixel_width: 0, pixel_height: 0 }
    }

    fn winsize(self) -> libc::winsize {
        libc::winsize {
            ws_row: self.rows.max(1),
            ws_col: self.cols.max(1),
            ws_xpixel: self.pixel_width,
            ws_ypixel: self.pixel_height,
        }
    }
}

impl Default for PtySize {
    fn default() -> Self {
        PtySize::new(80, 24)
    }
}

/// What to run inside the PTY.
#[derive(Clone, Debug)]
pub struct PtyCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Extra/overridden environment variables.
    pub env: Vec<(OsString, OsString)>,
    /// Variables removed from the inherited environment.
    pub env_remove: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Run the program as a login shell (`argv[0]` prefixed with `-`).
    pub login: bool,
}

impl PtyCommand {
    pub fn new(program: impl Into<OsString>) -> Self {
        PtyCommand {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            cwd: None,
            login: false,
        }
    }

    /// The user's default shell: `$SHELL`, then the passwd entry, then `/bin/sh`.
    pub fn default_shell() -> Self {
        PtyCommand::new(default_shell_path())
    }

    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

pub fn default_shell_path() -> OsString {
    if let Some(s) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return s;
    }
    // SAFETY: getpwuid returns a pointer into static storage or NULL; we
    // copy the string out immediately.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            let s = std::ffi::CStr::from_ptr((*pw).pw_shell);
            if !s.to_bytes().is_empty() {
                use std::os::unix::ffi::OsStrExt;
                return std::ffi::OsStr::from_bytes(s.to_bytes()).to_owned();
            }
        }
    }
    "/bin/sh".into()
}

/// A running child attached to a pseudo-terminal.
pub struct Pty {
    master: OwnedFd,
    child: Child,
}

impl Pty {
    /// Open a PTY and spawn `cmd` as the session leader with the PTY as its
    /// controlling terminal.
    pub fn spawn(cmd: &PtyCommand, size: PtySize) -> io::Result<Pty> {
        let ws = size.winsize();
        let pty = nix::pty::openpty(Some(&ws), None).map_err(io::Error::from)?;
        let (master, slave) = (pty.master, pty.slave);
        set_cloexec(&master)?;

        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        if cmd.login {
            use std::os::unix::ffi::OsStrExt;
            let base = std::path::Path::new(&cmd.program)
                .file_name()
                .map(|n| n.as_bytes().to_vec())
                .unwrap_or_default();
            let mut argv0 = b"-".to_vec();
            argv0.extend_from_slice(&base);
            command.arg0(std::ffi::OsStr::from_bytes(&argv0));
        }
        for k in &cmd.env_remove {
            command.env_remove(k);
        }
        command
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "seraphim")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // Seraphim decodes output as UTF-8 only. If neither the inherited nor
        // the configured environment selects a locale, programs would fall
        // back to "C" and treat UTF-8 as binary; default the character type.
        let has_locale = |k: &str| {
            cmd.env.iter().any(|(ek, v)| ek == k && !v.is_empty())
                || (!cmd.env_remove.iter().any(|r| r == k)
                    && std::env::var_os(k).is_some_and(|v| !v.is_empty()))
        };
        if !["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|k| has_locale(k)) {
            command.env("LC_CTYPE", "C.UTF-8");
        }
        for (k, v) in &cmd.env {
            command.env(k, v);
        }
        if let Some(dir) = &cmd.cwd {
            command.current_dir(dir);
        }
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));

        // SAFETY: only async-signal-safe libc calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                // Undo any signal state inherited from the emulator.
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
                for sig in [
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                    libc::SIGPIPE,
                ] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        // `command` (and the slave fds it holds) is dropped here, so the
        // master sees EOF/EIO once the child and its descendants exit.
        drop(command);
        Ok(Pty { master, child })
    }

    /// An independent handle for reading child output (blocking).
    ///
    /// On Linux, reading after the child side closes yields `EIO`; use
    /// [`is_eof_error`] to treat that as end-of-stream.
    pub fn reader(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }

    /// An independent handle for writing input to the child.
    pub fn writer(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }

    /// Propagate new dimensions; the kernel delivers SIGWINCH to the
    /// foreground process group.
    pub fn resize(&self, size: PtySize) -> io::Result<()> {
        let ws = size.winsize();
        // SAFETY: valid fd and pointer to a properly initialised winsize.
        let r = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
        if r == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn size(&self) -> io::Result<PtySize> {
        // SAFETY: as above.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
        if r == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(PtySize {
            cols: ws.ws_col,
            rows: ws.ws_row,
            pixel_width: ws.ws_xpixel,
            pixel_height: ws.ws_ypixel,
        })
    }

    /// PID of the directly spawned child (the shell).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Process group currently in the foreground of this terminal (e.g. the
    /// `vim` or `gabriel` started from the shell).
    pub fn foreground_pgid(&self) -> Option<u32> {
        // SAFETY: valid fd.
        let pg = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        (pg > 0).then_some(pg as u32)
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Send SIGHUP to the session (what closing a terminal window does).
    pub fn hangup(&self) -> io::Result<()> {
        // Negative pid: the whole process group led by the shell.
        // SAFETY: plain syscall.
        let r = unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGHUP) };
        if r == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.hangup();
            // Reap if it exits promptly; otherwise leave it (it has no
            // terminal anymore and will receive EIO/SIGHUP).
            for _ in 0..20 {
                if let Ok(Some(_)) = self.child.try_wait() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

/// Whether a read error from the master side means "the child side closed".
pub fn is_eof_error(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EIO)
}

/// Process metadata useful to the Session Manager and (via trusted
/// metadata only) to extensions: name, cwd, and effective uid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub euid: Option<u32>,
}

impl ProcessInfo {
    /// Read from `/proc`. Returns `None` if the process is gone.
    pub fn read(pid: u32) -> Option<ProcessInfo> {
        let base = PathBuf::from(format!("/proc/{pid}"));
        let name = std::fs::read_to_string(base.join("comm")).ok()?.trim_end().to_owned();
        let cwd = std::fs::read_link(base.join("cwd")).ok();
        let euid = std::fs::read_to_string(base.join("status")).ok().and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        });
        Some(ProcessInfo { pid, name, cwd, euid })
    }

    pub fn is_root(&self) -> bool {
        self.euid == Some(0)
    }
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: valid fd.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
        if flags == -1 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) == -1
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
