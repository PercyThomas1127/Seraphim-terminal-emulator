//! Cost of one resize with a full scrollback:
//!   cargo run --release -p seraphim-terminal --example resize_bench
use std::time::Instant;

use seraphim_terminal::Terminal;

fn main() {
    let mut t = Terminal::new(200, 50);
    for i in 0..12_000 {
        t.advance(
            format!("line {i:05} the quick brown fox jumps over the lazy dog, again and again\r\n")
                .as_bytes(),
        );
    }
    for (name, cols, rows) in [("width change", 199, 50), ("height only", 199, 49)] {
        let n = 50;
        let start = Instant::now();
        for k in 0..n {
            let (c, r) = if k % 2 == 0 { (cols, rows) } else { (200, 50) };
            t.resize(c, r);
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / n as f64;
        println!("{name}: {ms:.2} ms per resize ({} lines of history)", t.scrollback().len());
    }
}
