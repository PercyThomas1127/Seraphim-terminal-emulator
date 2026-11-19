//! Background loading of the system font list. This switches the whole
//! process to background mode, so it lives in a test binary of its own.

use std::sync::mpsc;
use std::time::Duration;

use seraphim_render::{
    load_system_fonts_in_background, FontConfig, FontStyle, Fonts, BUILTIN_FAMILY,
};

#[test]
fn early_misses_are_redrawn_once_the_scan_is_in() {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    load_system_fonts_in_background(move || {
        let _ = tx.lock().unwrap().send(());
    });

    // With no family configured nothing waits for the scan.
    let mut fonts = Fonts::new(&FontConfig { family: None, size_px: 16.0 }).unwrap();
    assert_eq!(fonts.family_name, BUILTIN_FAMILY);

    // JetBrains Mono has no CJK. Before the scan is in this is blank (and
    // recorded as an early miss); it must not be cached as missing.
    let early = fonts.lookup('日', FontStyle::Regular);
    let missed = fonts.take_early_misses();
    assert!(early.is_none() || !missed, "a hit is never an early miss");

    rx.recv_timeout(Duration::from_secs(120)).expect("the scan finishes");
    assert!(!fonts.take_early_misses());
    let late = fonts.lookup('日', FontStyle::Regular);
    if missed {
        // Some installed font has CJK (else the lookup stays empty).
        let any_cjk = Fonts::new(&FontConfig { family: None, size_px: 16.0 })
            .map(|mut f| f.lookup('日', FontStyle::Regular).is_some())
            .unwrap_or(false);
        assert_eq!(late.is_some(), any_cjk, "the early miss wasn't cached");
    } else {
        assert_eq!(late, early);
    }
    assert!(!fonts.take_early_misses(), "lookups after the scan aren't early");
}
