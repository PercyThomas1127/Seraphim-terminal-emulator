//! Deterministic fuzz smoke test (the long-running fuzzer is
//! `examples/fuzz.rs`).

#[allow(dead_code)]
mod core {
    include!("../examples/support/fuzz_core.rs");
}

#[test]
fn fuzz_smoke() {
    // FUZZ_SEED=n reruns one case with the panic message visible.
    if let Some(seed) = std::env::var("FUZZ_SEED").ok().and_then(|s| s.parse().ok()) {
        let (_, failure) = core::run_one(seed);
        assert!(failure.is_none(), "seed {seed}: {failure:?}");
        return;
    }
    // Silence the default panic message for expected-to-be-caught panics.
    std::panic::set_hook(Box::new(|_| {}));
    for seed in 1..=1500u64 {
        let (input, failure) = core::run_one(seed);
        if let Some(f) = failure {
            let _ = std::panic::take_hook();
            panic!("fuzz seed {seed}: {f}\ninput: {:?}", String::from_utf8_lossy(&input));
        }
    }
}
