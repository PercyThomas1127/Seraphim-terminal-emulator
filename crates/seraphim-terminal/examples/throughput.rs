//! Parser + state throughput: `cargo run --release -p seraphim-terminal --example throughput`
use std::time::Instant;

use seraphim_terminal::Terminal;

fn corpus(kind: &str, mb: usize) -> Vec<u8> {
    let line: Vec<u8> = match kind {
        "ascii" => b"The quick brown fox jumps over the lazy dog 0123456789 ~!@#$%^&*()_+\r\n".to_vec(),
        "sgr" => b"\x1b[1;31mERROR\x1b[0m \x1b[38;2;137;168;255mtruecolor\x1b[0m \x1b[4mlink\x1b[24m done\r\n".to_vec(),
        "unicode" => "日本語のテキスト ✓ → λ ∑ π émoji-free wide and narrow mixed\r\n".as_bytes().to_vec(),
        _ => unreachable!(),
    };
    line.iter().copied().cycle().take(mb << 20).collect()
}

fn micro() {
    let cases: [(&str, Vec<u8>); 4] = [
        ("no-newline ascii", b"abcdefghij".iter().copied().cycle().take(50 << 20).collect()),
        ("only LF", b"\n".iter().copied().cycle().take(5 << 20).collect()),
        ("only CR", b"\r".iter().copied().cycle().take(50 << 20).collect()),
        ("vte only", Vec::new()),
    ];
    for (name, data) in cases {
        let data = if data.is_empty() { corpus("ascii", 50) } else { data };
        let start = Instant::now();
        if name == "vte only" {
            struct Nop;
            impl vte::Perform for Nop {}
            let mut p = vte::Parser::new();
            p.advance(&mut Nop, &data);
        } else {
            let mut term = Terminal::new(200, 50);
            for chunk in data.chunks(64 * 1024) {
                term.advance(chunk);
            }
        }
        let s = start.elapsed().as_secs_f64();
        println!("{name:>18}: {:8.1} M bytes/s", data.len() as f64 / s / 1e6);
    }
}

fn main() {
    if std::env::args().any(|a| a == "--profile") {
        let data = if std::env::var_os("PROFILE_NO_NEWLINE").is_some() {
            b"abcdefghij".iter().copied().cycle().take(8 << 20).collect()
        } else {
            corpus("ascii", 8)
        };
        let mut term = Terminal::new(200, 50);
        for chunk in data.chunks(64 * 1024) {
            term.advance(chunk);
        }
        return;
    }
    if std::env::args().any(|a| a == "--micro") {
        return micro();
    }
    for kind in ["ascii", "sgr", "unicode"] {
        let data = corpus(kind, 100);
        let mut term = Terminal::new(200, 50);
        let start = Instant::now();
        for chunk in data.chunks(64 * 1024) {
            term.advance(chunk);
        }
        let secs = start.elapsed().as_secs_f64();
        println!("{kind:>8}: {:6.0} MB/s  ({} MB in {secs:.2}s)", 100.0 / secs, 100);
    }
}
