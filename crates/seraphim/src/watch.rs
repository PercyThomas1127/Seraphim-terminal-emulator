//! Live reload: watch the files the configuration was built from.
//!
//! What counts as a change is each file's modification time and size, so
//! files that appear later (a config.toml created after launch) and editors
//! that replace files instead of writing them in place are both seen.
//!
//! On Linux the watcher thread sleeps until inotify reports activity in one
//! of the files' directories, then compares. Elsewhere, or for a file whose
//! directory doesn't exist yet, it compares twice a second.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub const INTERVAL: Duration = Duration::from_millis(500);

/// After a notification, how long to let the rest of a save land (editors
/// truncate, write, rename) before comparing.
#[cfg(target_os = "linux")]
const SETTLE: Duration = Duration::from_millis(50);

type Signature = Vec<Option<(SystemTime, u64)>>;

fn signature(files: &[PathBuf]) -> Signature {
    files
        .iter()
        .map(|p| {
            std::fs::metadata(p)
                .ok()
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
        })
        .collect()
}

/// Which files differ between two signatures of the same list.
fn changed(files: &[PathBuf], a: &Signature, b: &Signature) -> Vec<PathBuf> {
    files.iter().zip(a.iter().zip(b)).filter(|(_, (x, y))| x != y).map(|(p, _)| p.clone()).collect()
}

struct Shared {
    files: Vec<PathBuf>,
    /// Bumped by [`Watcher::set`]; the thread re-baselines.
    generation: u64,
}

pub struct Watcher {
    shared: Arc<Mutex<Shared>>,
    wake: Wake,
}

impl Watcher {
    /// Watch `files`; `on_change` gets the changed paths and returns false
    /// to stop watching (e.g. the window closed).
    pub fn spawn(
        files: Vec<PathBuf>,
        on_change: impl Fn(Vec<PathBuf>) -> bool + Send + 'static,
    ) -> Watcher {
        let shared = Arc::new(Mutex::new(Shared { files, generation: 0 }));
        let s = shared.clone();
        let (wake, mut waiter) = Wake::new();
        std::thread::Builder::new()
            .name("seraphim-config-watch".into())
            .spawn(move || {
                let mut seen_generation = u64::MAX;
                let mut last: Signature = Vec::new();
                loop {
                    let (files, generation) = {
                        let g = s.lock().unwrap();
                        (g.files.clone(), g.generation)
                    };
                    let now = signature(&files);
                    if generation != seen_generation {
                        seen_generation = generation;
                        waiter.watch(&files);
                    } else if now != last {
                        let what = changed(&files, &last, &now);
                        if !on_change(what) {
                            return;
                        }
                    }
                    last = now;
                    waiter.wait();
                }
            })
            .expect("spawn config watcher");
        Watcher { shared, wake }
    }

    /// Replace the watched files (after a reload: profile, theme or
    /// shaders may differ).
    pub fn set(&self, files: Vec<PathBuf>) {
        let mut g = self.shared.lock().unwrap();
        if g.files != files {
            g.files = files;
            g.generation += 1;
            self.wake.wake();
        }
    }
}

/// Wakes the watcher thread when the file list changes.
#[cfg(target_os = "linux")]
struct Wake(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl Wake {
    fn new() -> (Wake, Waiter) {
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills both fds on success.
        let pipe = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        // SAFETY: inotify_init1 returns a new fd or -1.
        let inotify = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        let own = |fd: i32| (fd >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        let (rd, wr) = if pipe == 0 { (own(fds[0]), own(fds[1])) } else { (None, None) };
        let waiter = Waiter { inotify: own(inotify), pipe: rd, watches: Vec::new(), poll: true };
        // Without a pipe the waiter polls on a timer anyway; an fd that is
        // never written keeps `wake` harmless.
        let wr = wr.unwrap_or_else(|| std::fs::File::open("/dev/null").unwrap().into());
        (Wake(wr), waiter)
    }

    fn wake(&self) {
        use std::os::fd::AsRawFd;
        // SAFETY: writing one byte from a valid buffer to an owned fd. A full
        // pipe (EAGAIN) means a wake is already pending.
        unsafe { libc::write(self.0.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
    }
}

/// The watcher thread's side: sleeps until something may have changed.
#[cfg(target_os = "linux")]
struct Waiter {
    inotify: Option<std::os::fd::OwnedFd>,
    pipe: Option<std::os::fd::OwnedFd>,
    /// Watch descriptors on the files' directories.
    watches: Vec<i32>,
    /// Some file can't be watched (no inotify, or its directory doesn't
    /// exist yet): compare on a timer too.
    poll: bool,
}

#[cfg(target_os = "linux")]
impl Waiter {
    /// Watch the directories of `files` instead of what was watched.
    fn watch(&mut self, files: &[PathBuf]) {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        let Some(fd) = &self.inotify else {
            self.poll = true;
            return;
        };
        for wd in self.watches.drain(..) {
            // SAFETY: removing a watch this fd returned.
            unsafe { libc::inotify_rm_watch(fd.as_raw_fd(), wd) };
        }
        let mut dirs: Vec<PathBuf> =
            files.iter().filter_map(|f| f.parent().map(|d| d.to_path_buf())).collect();
        dirs.sort();
        dirs.dedup();
        self.poll = false;
        let mask = libc::IN_CLOSE_WRITE
            | libc::IN_MODIFY
            | libc::IN_ATTRIB
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO;
        for dir in dirs {
            let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
                self.poll = true;
                continue;
            };
            // SAFETY: a NUL-terminated path and a valid inotify fd.
            let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c.as_ptr(), mask) };
            if wd < 0 {
                self.poll = true;
            } else {
                self.watches.push(wd);
            }
        }
    }

    /// Block until a watched directory or the file list changes (or, when
    /// polling, at most [`INTERVAL`]), then let the change settle.
    fn wait(&mut self) {
        use std::os::fd::AsRawFd;
        let mut fds: Vec<libc::pollfd> = [&self.inotify, &self.pipe]
            .into_iter()
            .flatten()
            .map(|fd| libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 })
            .collect();
        let timeout = if self.poll || fds.len() < 2 { INTERVAL.as_millis() as i32 } else { -1 };
        // SAFETY: `fds` is a valid array of pollfd for its length.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if n > 0 {
            std::thread::sleep(SETTLE);
            self.drain();
        }
    }

    /// Discard pending notifications and wakes; the signature says what
    /// changed.
    fn drain(&self) {
        use std::os::fd::AsRawFd;
        let mut buf = [0u8; 4096];
        for fd in [&self.inotify, &self.pipe].into_iter().flatten() {
            // SAFETY: reading into a valid buffer from an owned, non-blocking fd.
            while unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
        }
    }
}

#[cfg(not(target_os = "linux"))]
struct Wake;

#[cfg(not(target_os = "linux"))]
impl Wake {
    fn new() -> (Wake, Waiter) {
        (Wake, Waiter)
    }
    fn wake(&self) {}
}

#[cfg(not(target_os = "linux"))]
struct Waiter;

#[cfg(not(target_os = "linux"))]
impl Waiter {
    fn watch(&mut self, _files: &[PathBuf]) {}
    fn wait(&mut self) {
        std::thread::sleep(INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn reports_changes_and_new_files() {
        let dir = std::env::temp_dir().join(format!("seraphim-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.toml");
        let b = dir.join("b.toml"); // does not exist yet
        std::fs::write(&a, "x = 1").unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = Watcher::spawn(vec![a.clone(), b.clone()], move |c| tx.send(c).is_ok());
        std::thread::sleep(INTERVAL + Duration::from_millis(200));
        std::fs::write(&a, "x = 22").unwrap(); // size changes even if mtime granularity is coarse
        let got = rx.recv_timeout(INTERVAL * 4).expect("change seen");
        assert_eq!(got, vec![a.clone()]);
        std::fs::write(&b, "y = 1").unwrap();
        let got = rx.recv_timeout(INTERVAL * 4).expect("creation seen");
        assert_eq!(got, vec![b.clone()]);
        assert!(rx.recv_timeout(INTERVAL * 2).is_err(), "quiet when nothing changes");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
