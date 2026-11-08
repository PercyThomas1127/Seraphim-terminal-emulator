//! Seraphim against screens recorded from real xterm (the reference). The
//! recordings come from `examples/record_golden.rs`; this test needs no
//! xterm or display.

use seraphim_conformance::golden::{normalize, Golden};
use seraphim_conformance::{seraphim, KNOWN_DIFFERENCES};

#[test]
fn matches_recorded_xterm_screens() {
    let data = include_str!("fixtures/xterm-golden.jsonl");
    let goldens: Vec<Golden> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Golden::from_json_line(l).expect("valid fixture line"))
        .collect();
    assert!(goldens.len() > 100, "fixtures missing ({} cases)", goldens.len());
    let mut failures = Vec::new();
    for g in &goldens {
        let ours = seraphim(g.cols, g.rows, &g.input);
        let text_ok = normalize(&ours.text()) == g.text();
        let cursor_ok = g.origin || g.cursor == Some(ours.cursor);
        if !(text_ok && cursor_ok) {
            failures.push(format!(
                "{} ({}x{}): {:?}\n  xterm   : {:?} cursor {:?}\n  seraphim: {:?} cursor {:?}",
                g.name,
                g.cols,
                g.rows,
                String::from_utf8_lossy(&g.input),
                g.text(),
                g.cursor,
                normalize(&ours.text()),
                ours.cursor
            ));
        }
    }
    if !failures.is_empty() {
        let n = failures.len();
        failures.truncate(5);
        panic!(
            "{n} of {} recorded xterm screens differ (known differences: {:?}):\n\n{}",
            goldens.len(),
            KNOWN_DIFFERENCES.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            failures.join("\n\n")
        );
    }
}
