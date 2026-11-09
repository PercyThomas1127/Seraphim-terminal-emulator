//! Every example extension in `examples/` starts without errors when given
//! the permissions it asks for, and the spec's Gabriel example reacts to
//! install events.

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
        ["cpu-glow", "error-flash", "exit-color", "gabriel-glow", "git-panel", "clock"]
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

    // The spec's §8.3 example.
    let gabriel = bus.publisher("gabriel");
    gabriel.publish("gabriel.install.started", None, json!({ "package": "firefox" }));
    let got = collect(&rt, |c| c.iter().any(|c| matches!(c, Command::Notify { .. })));
    assert!(got.iter().any(|c| matches!(c, Command::SetParam { param, value, .. } if param == "install" && *value == 1.0)), "{got:?}");
    assert!(
        got.iter()
            .any(|c| matches!(c, Command::Notify { text, .. } if text == "Installing firefox…")),
        "{got:?}"
    );
    gabriel.publish("gabriel.install.completed", None, json!({ "package": "firefox" }));
    // The success animation ends after 2.3 s, then says so.
    let got = collect(&rt, |c| {
        c.iter().any(|c| matches!(c, Command::Notify { text, .. } if text == "Installed."))
    });
    assert!(
        got.iter().any(|c| matches!(c, Command::Notify { text, .. } if text == "Installed.")),
        "{got:?}"
    );
}
