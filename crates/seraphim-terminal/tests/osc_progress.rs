use seraphim_terminal::{Terminal, TerminalEvent};

#[test]
fn osc_9_4_progress() {
    let mut t = Terminal::new(10, 2);
    t.advance(b"\x1b]9;4;1;42\x07\x1b]9;4;0;0\x1b\\\x1b]9;4;2;900\x07\x1b]9;hello\x07");
    let ev: Vec<_> = t
        .take_events()
        .into_iter()
        .filter(|e| matches!(e, TerminalEvent::Progress { .. }))
        .collect();
    assert_eq!(
        ev,
        [
            TerminalEvent::Progress { state: 1, percent: 42 },
            TerminalEvent::Progress { state: 0, percent: 0 },
            TerminalEvent::Progress { state: 2, percent: 100 },
        ]
    );
}
