//! Triage disagreements between Seraphim and alacritty_terminal using a
//! real xterm as the referee.
//!
//!   DISPLAY=:99 cargo run -p seraphim-conformance --example triage -- [CASES] [FIRST_SEED]
//!
//! For each generated stream where the two emulators differ (in text or
//! cursor), xterm decides who is right.

use seraphim_conformance::gen::{stream, Gen};
use seraphim_conformance::{alacritty, seraphim, xterm, Snapshot};

/// Text with all trailing blanks removed (xterm's print does this even for
/// colored blanks).
fn norm(text: &str) -> String {
    let mut lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches(' ')).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Split into escape-sequence-aligned tokens (for shrinking).
fn tokens(b: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let len = if b[i] == 0x1b && i + 1 < b.len() {
            match b[i + 1] {
                b'[' => {
                    let mut j = i + 2;
                    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                        j += 1;
                    }
                    (j + 1).min(b.len()) - i
                }
                b'(' | b')' | b'#' => 3.min(b.len() - i),
                _ => 2,
            }
        } else {
            match b[i] {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            }
            .min(b.len() - i)
        };
        out.push(b[i..i + len].to_vec());
        i += len;
    }
    out
}

/// Does Seraphim disagree with xterm on this input?
fn seraphim_wrong(cols: usize, rows: usize, input: &[u8], work: &std::path::Path) -> Option<bool> {
    let x = xterm::run(cols, rows, input, work)?;
    let ours = seraphim(cols, rows, input);
    let origin_on = last_toggle(input, b"\x1b[?6h", b"\x1b[?6l");
    Some(norm(&ours.text()) != norm(&x.text()) || (!origin_on && Some(ours.cursor) != x.cursor))
}

/// Delta-debug `seed`'s stream down to a minimal input on which Seraphim
/// still disagrees with xterm.
fn shrink(seed: u64, work: &std::path::Path) {
    let (cols, rows, input) = stream(&mut Gen::new(seed));
    if seraphim_wrong(cols, rows, &input, work) != Some(true) {
        println!("seed {seed}: seraphim already agrees with xterm");
        return;
    }
    let mut toks = tokens(&input);
    let mut chunk = toks.len() / 2;
    while chunk >= 1 {
        let mut i = 0;
        let mut progressed = false;
        while i < toks.len() {
            let mut trial = toks.clone();
            trial.drain(i..(i + chunk).min(trial.len()));
            if seraphim_wrong(cols, rows, &trial.concat(), work) == Some(true) {
                toks = trial;
                progressed = true;
            } else {
                i += chunk;
            }
        }
        if !progressed {
            chunk /= 2;
        }
    }
    let min = toks.concat();
    let x = xterm::run(cols, rows, &min, work).unwrap();
    let ours = seraphim(cols, rows, &min);
    let theirs = alacritty(cols, rows, &min);
    println!("seed {seed} minimal ({cols}x{rows}): {:?}", String::from_utf8_lossy(&min));
    println!("  xterm    : {:?} cursor {:?}", norm(&x.text()), x.cursor);
    println!("  seraphim : {:?} cursor {:?}", norm(&ours.text()), ours.cursor);
    println!("  alacritty: {:?} cursor {:?}", norm(&theirs.text()), theirs.cursor);
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("--shrink") {
        let work = std::env::temp_dir().join(format!("seraphim-shrink-{}", std::process::id()));
        for seed in raw[1..].iter().filter_map(|a| a.parse().ok()) {
            shrink(seed, &work);
        }
        return;
    }
    let args: Vec<u64> = raw.iter().filter_map(|a| a.parse().ok()).collect();
    let cases = args.first().copied().unwrap_or(200);
    let first = args.get(1).copied().unwrap_or(1);
    if !xterm::available() {
        eprintln!("needs xterm and an X display (try Xvfb :99 & DISPLAY=:99)");
        std::process::exit(2);
    }
    let work = std::env::temp_dir().join(format!("seraphim-triage-{}", std::process::id()));
    let (mut agree, mut ours_ok, mut theirs_ok, mut neither) = (0, 0, 0, 0);
    for seed in first..first + cases {
        let (cols, rows, input) = stream(&mut Gen::new(seed));
        // Leave origin mode off so xterm's cursor report is absolute.
        let ours = seraphim(cols, rows, &input);
        let theirs = alacritty(cols, rows, &input);
        let same =
            |a: &Snapshot, b: &Snapshot| norm(&a.text()) == norm(&b.text()) && a.cursor == b.cursor;
        if same(&ours, &theirs) {
            agree += 1;
            continue;
        }
        let Some(x) = xterm::run(cols, rows, &input, &work) else {
            eprintln!("seed {seed}: xterm run failed");
            continue;
        };
        // xterm's cursor report is relative to the scroll region if origin
        // mode was left on; only compare cursors when it is off.
        let origin_on = last_toggle(&input, b"\x1b[?6h", b"\x1b[?6l");
        let matches = |s: &Snapshot| {
            norm(&s.text()) == norm(&x.text()) && (origin_on || Some(s.cursor) == x.cursor)
        };
        let verdict = match (matches(&ours), matches(&theirs)) {
            (true, _) => {
                ours_ok += 1;
                "seraphim = xterm"
            }
            (false, true) => {
                theirs_ok += 1;
                "SERAPHIM WRONG (alacritty = xterm)"
            }
            (false, false) => {
                neither += 1;
                "neither matches xterm"
            }
        };
        println!("seed {seed} {cols}x{rows}: {verdict}");
        if verdict != "seraphim = xterm" {
            println!("  input    : {:?}", String::from_utf8_lossy(&input));
            println!("  xterm    : {:?} cursor {:?}", x.text(), x.cursor);
            println!("  seraphim : {:?} cursor {:?}", ours.text(), ours.cursor);
            println!("  alacritty: {:?} cursor {:?}", theirs.text(), theirs.cursor);
        }
    }
    println!(
        "\n{cases} cases: {agree} agree; of the disagreements, seraphim matched xterm {ours_ok}, \
         alacritty matched xterm {theirs_ok}, neither {neither}"
    );
}

/// Whether `on` occurs after the last `off` (or RIS) in `input`.
fn last_toggle(input: &[u8], on: &[u8], off: &[u8]) -> bool {
    let find = |pat: &[u8]| input.windows(pat.len()).rposition(|w| w == pat);
    let last_off = find(off).max(find(b"\x1bc"));
    match (find(on), last_off) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}
