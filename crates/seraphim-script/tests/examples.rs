//! Every example extension in `examples/` starts without errors when given
//! the permissions it asks for, and the package example reacts to package
//! manager events.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use seraphim_events::{json, EventBus};
use seraphim_script::{discover, Command, Grant, Runtime, Settings};

fn collect(rt: &Runtime, until: impl Fn(&[Command]) -> bool) -> Vec<Command> {
    let t = Instant::now();
    let mut got = Vec::new();
    while t.elapsed() < Duration::from_secs(3) && !until(&got) {
        got.extend(rt.take_commands());
        std::thread::sleep(Duration::from_millis(5));
    }
    got
}

#[test]
fn examples_start_cleanly() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let (found, errors) = discover(&root, &[]);
    assert!(errors.is_empty(), "{errors:?}");
    let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["cpu-glow", "error-flash", "exit-color", "git-panel", "package-glow", "clock"]
    );
    let bus = EventBus::new();
    let data = std::env::temp_dir().join(format!("seraphim-examples-{}", std::process::id()));
    let list = found
        .into_iter()
        .map(|e| {
            let g = Grant { permissions: e.requested.clone(), ..Default::default() };
            (e, g)
        })
        .collect();
    let rt = Runtime::start(
        list,
        Settings { data_dir: data, ..Default::default() },
        &bus,
        Arc::new(|| {}),
    );
    // Give the main files time to run, then look for errors.
    std::thread::sleep(Duration::from_millis(300));
    let got = rt.take_commands();
    let errors: Vec<_> =
        got.iter().filter(|c| matches!(c, Command::Notify { error: true, .. })).collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(got.iter().any(|c| matches!(c, Command::AddLayer { ext, .. } if ext == "error-flash")));
    assert!(got.iter().any(|c| matches!(c, Command::Bind { ext, .. } if ext == "exit-color")));
    assert!(got.iter().any(|c| matches!(c, Command::RegisterKey { ext, .. } if ext == "clock")));

    // The spec's §8.3 example, for any package manager.
    let pkg = bus.publisher("package");
    let firefox = json!({ "manager": "pacman", "operation": "install", "package": "firefox" });
    pkg.publish("package.install.started", None, firefox.clone());
    let got = collect(&rt, |c| c.iter().any(|c| matches!(c, Command::Notify { .. })));
    assert!(got.iter().any(|c| matches!(c, Command::SetParam { param, value, .. } if param == "install" && *value == 1.0)), "{got:?}");
    assert!(
        got.iter().any(
            |c| matches!(c, Command::Notify { text, .. } if text == "pacman: installing firefox…")
        ),
        "{got:?}"
    );
    pkg.publish("package.install.completed", None, firefox);
    // The success animation ends after 2.3 s, then says so.
    let got = collect(&rt, |c| {
        c.iter().any(|c| matches!(c, Command::Notify { text, .. } if text == "Done."))
    });
    assert!(
        got.iter().any(|c| matches!(c, Command::Notify { text, .. } if text == "Done.")),
        "{got:?}"
    );
}
