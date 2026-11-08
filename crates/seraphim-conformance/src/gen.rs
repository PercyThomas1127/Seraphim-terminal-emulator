//! Deterministic generator of plausible terminal output streams.

/// Deterministic generator of plausible terminal output.
pub struct Gen(u64);

impl Gen {
    pub fn new(seed: u64) -> Self {
        Gen(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next_u64() % xs.len() as u64) as usize]
    }
    pub fn num(&mut self, max: u64) -> u64 {
        self.next_u64() % (max + 1)
    }

    pub fn token(&mut self, out: &mut Vec<u8>) {
        let n = |g: &mut Gen, m| g.num(m).to_string();
        let t = match self.num(13) {
            0..=3 => self.pick(&["hello ", "x", "日本", "é", "—", "abc def", "\t"]).to_string(),
            4 => self.pick(&["\r", "\n", "\r\n", "\x08"]).to_string(),
            5 => format!("\x1b[{};{}H", n(self, 8), n(self, 14)),
            6 => format!(
                "\x1b[{}{}",
                n(self, 5),
                self.pick(&["A", "B", "C", "D", "E", "F", "G", "d", "a", "e"])
            ),
            7 => {
                let op = self.pick(&["J", "K", "X", "@", "P", "L", "M", "S", "T", "b"]);
                // Explicit `CSI 0 T` is a documented known difference.
                let count = if op == "T" { self.num(2) + 1 } else { self.num(3) };
                format!("\x1b[{count}{op}")
            }
            8 => format!(
                "\x1b[{}m",
                self.pick(&[
                    "0",
                    "1",
                    "3",
                    "4",
                    "7",
                    "9",
                    "22",
                    "31",
                    "42",
                    "38;5;99",
                    "48;2;9;8;7",
                    "39;49",
                    "91",
                    "104"
                ])
            ),
            9 => format!("\x1b[{};{}r", self.num(3) + 1, self.num(3) + 4),
            10 => self
                .pick(&["\x1bM", "\x1bD", "\x1bE", "\x1b7", "\x1b8", "\x1bH", "\x1b[3g", "\x1b[g"])
                .to_string(),
            11 => self
                .pick(&["\x1b[?6h", "\x1b[?6l", "\x1b[?7l", "\x1b[?7h", "\x1b[4h", "\x1b[4l"])
                .to_string(),
            12 => self.pick(&["\x1b(0", "\x1b(B", "lqkxmj"]).to_string(),
            _ => self.pick(&["\x1b[?1049h", "\x1b[?1049l", "\x1b[s", "\x1b[u"]).to_string(),
        };
        out.extend_from_slice(t.as_bytes());
    }
}

/// One generated case: (cols, rows, bytes).
pub fn stream(g: &mut Gen) -> (usize, usize, Vec<u8>) {
    let mut input = Vec::new();
    for _ in 0..(5 + g.num(40)) {
        g.token(&mut input);
    }
    (8 + g.num(8) as usize, 3 + g.num(5) as usize, input)
}
