//! Record xterm golden screens for CI:
//!
//!   DISPLAY=:99 cargo run -p seraphim-conformance --example record_golden -- [GENERATED_CASES]
//!
//! Writes tests/fixtures/xterm-golden.jsonl.

use std::io::Write;

use seraphim_conformance::gen::{stream, Gen};
use seraphim_conformance::golden::{origin_left_on, Golden, HANDWRITTEN};
use seraphim_conformance::xterm;

fn main() {
    let generated: u64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(400);
    if !xterm::available() {
        eprintln!("needs xterm and an X display");
        std::process::exit(2);
    }
    let work = std::env::temp_dir().join(format!("seraphim-golden-{}", std::process::id()));
    let mut cases: Vec<(String, usize, usize, Vec<u8>)> =
        HANDWRITTEN.iter().map(|(n, b)| (n.to_string(), 10, 5, b.to_vec())).collect();
    for seed in 1..=generated {
        let (cols, rows, input) = stream(&mut Gen::new(seed));
        cases.push((format!("generated seed {seed}"), cols, rows, input));
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/xterm-golden.jsonl");
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut recorded = 0;
    for (name, cols, rows, input) in cases {
        let Some(x) = xterm::run(cols, rows, &input, &work) else {
            eprintln!("{name}: xterm failed, skipped");
            continue;
        };
        let g = Golden {
            name,
            cols,
            rows,
            origin: origin_left_on(&input),
            lines: x.lines,
            cursor: x.cursor,
            input,
        };
        writeln!(out, "{}", g.to_json_line()).unwrap();
        recorded += 1;
    }
    eprintln!("recorded {recorded} cases to {path}");
}
