// Shared by examples/fuzz.rs and tests/fuzz.rs (via `include!`).

use std::panic::{catch_unwind, AssertUnwindSafe};

use seraphim_terminal::{Point, SelectionKind, Terminal};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const PIECES: &[&str] = &[
    "a", "日", "é", "\u{301}", "😀", "\t", "\r", "\n", "\x08", "\x1b", "\x1b[", ";", "?", "$", "\"",
    "\x1b[2J", "\x1b[3J", "\x1b[K", "\x1b[1K", "\x1b[@", "\x1b[P", "\x1b[X", "\x1b[L", "\x1b[M",
    "\x1b[S", "\x1b[T", "\x1b[b", "\x1b[g", "\x1b[3g", "\x1bH", "\x1bM", "\x1bD", "\x1bE", "\x1b7",
    "\x1b8", "\x1bc", "\x1b#8", "\x1b(0", "\x1b(B", "\x0e", "\x0f", "\x1b[?6h", "\x1b[?6l",
    "\x1b[?7h", "\x1b[?7l", "\x1b[4h", "\x1b[4l", "\x1b[?1049h", "\x1b[?1049l", "\x1b[?1047h",
    "\x1b[?1047l", "\x1b[?47h", "\x1b[?1048h", "\x1b[?1048l", "\x1b[s", "\x1b[u", "\x1b[!p",
    "\x1b]0;t\x07", "\x1b]8;;u\x1b\\", "\x1b]8;;\x1b\\", "\x1b]52;c;aGk=\x07", "\x1b]4;1;#123456\x07",
    "\x1b]11;?\x07", "\x1b]133;A\x07", "\x1b]133;D;1\x07", "\x1bP$qm\x1b\\", "\x1b[6n", "\x1b[c",
    "\x1b[?2026$p", "\x1b[>q", "\x1b[38;2;1;2;3m", "\x1b[48:5:9m", "\x1b[0m",
];

fn piece(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(10) {
        0 => out.push(rng.next() as u8),
        1 => {
            // CSI with random numeric params.
            out.extend_from_slice(b"\x1b[");
            for i in 0..rng.below(4) {
                if i > 0 {
                    out.push(b';');
                }
                let max = if rng.below(8) == 0 { 70000 } else { 30 };
                out.extend_from_slice(rng.below(max).to_string().as_bytes());
            }
            out.push(b"ABCDEFGHJKLMPSTXZ@`abdefghlmnqrsu"[rng.below(33) as usize]);
        }
        _ => out.extend_from_slice(PIECES[rng.below(PIECES.len() as u64) as usize].as_bytes()),
    }
}

/// Run one fuzz case. Returns the input bytes and an error description if
/// the terminal panicked or violated an invariant.
pub fn run_one(seed: u64) -> (Vec<u8>, Option<String>) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut log = Vec::new();
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let mut t = Terminal::with_scrollback(1 + rng.below(30) as usize, 1 + rng.below(12) as usize, rng.below(50) as usize);
        for _ in 0..200 {
            let mut chunk = Vec::new();
            for _ in 0..rng.below(20) {
                piece(&mut rng, &mut chunk);
            }
            log.extend_from_slice(&chunk);
            t.advance(&chunk);
            match rng.below(25) {
                0 => t.resize(1 + rng.below(30) as usize, 1 + rng.below(12) as usize),
                1 => t.scroll_display(rng.below(20) as i64 - 10),
                2 => {
                    let kinds = [SelectionKind::Simple, SelectionKind::Word, SelectionKind::Line, SelectionKind::Block];
                    let p = |r: &mut Rng| Point::new(r.below(20) as i64 - 10, r.below(35) as usize);
                    t.start_selection(kinds[rng.below(4) as usize], p(&mut rng));
                    t.update_selection(p(&mut rng));
                    let _ = t.selection_text();
                }
                _ => {}
            }
            let _ = (t.take_events(), t.take_responses(), t.take_damage());
            for r in 0..t.rows() {
                let _ = t.visible_row(r).text();
            }
            t.check_invariants()?;
        }
        Ok(())
    }));
    let failure = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("invariant violated: {e}")),
        Err(_) => Some("panic".to_owned()),
    };
    (log, failure)
}
