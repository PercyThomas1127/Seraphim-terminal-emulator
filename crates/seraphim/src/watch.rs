//! Live reload: watch the files the configuration was built from.
//!
//! A background thread compares each file's modification time and size
//! twice a second. That is cheap (a handful of `stat` calls), needs no
//! platform file-notification API, sees files that appear later (a
//! config.toml created after launch) and survives editors that replace
//! files instead of writing them in place.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub const INTERVAL: Duration = Duration::from_millis(500);

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
                    } else if now != last {
                        let what = changed(&files, &last, &now);
                        if !on_change(what) {
                            return;
                        }
                    }
                    last = now;
                    std::thread::sleep(INTERVAL);
                }
            })
            .expect("spawn config watcher");
        Watcher { shared }
    }

    /// Replace the watched files (after a reload: profile, theme or
    /// shaders may differ).
    pub fn set(&self, files: Vec<PathBuf>) {
        let mut g = self.shared.lock().unwrap();
        if g.files != files {
            g.files = files;
            g.generation += 1;
        }
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
