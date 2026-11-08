//! Stable-Rust fuzzer for the Terminal Core:
//!
//!   cargo run --release -p seraphim-terminal --example fuzz -- [SECONDS] [SEED]
//!
//! Feeds a mix of random bytes and structured escape sequences, with random
//! resizes, viewport scrolling and selections, and checks the terminal's
//! structural invariants after every step. On failure, the input is written
//! to `fuzz-crash-<seed>.bin`.

use std::time::{Duration, Instant};

include!("support/fuzz_core.rs");

fn main() {
    let args: Vec<u64> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let secs = args.first().copied().unwrap_or(10);
    let mut seed = args.get(1).copied().unwrap_or(0x5e7a_9417);
    let deadline = Instant::now() + Duration::from_secs(secs);
    let (mut runs, mut bytes) = (0u64, 0u64);
    while Instant::now() < deadline {
        seed = seed.wrapping_add(1);
        let (log, failure) = run_one(seed);
        bytes += log.len() as u64;
        runs += 1;
        if let Some(f) = failure {
            let path = format!("fuzz-crash-{seed}.bin");
            std::fs::write(&path, &log).unwrap();
            eprintln!("seed {seed}: {f}; input saved to {path}");
            std::process::exit(1);
        }
    }
    println!("fuzz: {runs} runs, {:.1} MB, no failures", bytes as f64 / 1e6);
}
