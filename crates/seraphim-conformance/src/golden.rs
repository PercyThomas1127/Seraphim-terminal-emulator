//! Golden screens recorded from real xterm, so conformance can be checked
//! in CI without xterm or a display. Format: one JSON object per line:
//! `{"name", "cols", "rows", "input_hex", "lines", "cursor", "origin"}`.

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Golden {
    pub name: String,
    pub cols: usize,
    pub rows: usize,
    pub input: Vec<u8>,
    /// xterm's screen, trailing blanks trimmed.
    pub lines: Vec<String>,
    pub cursor: Option<(usize, usize)>,
    /// Origin mode was on at the end, so `cursor` is region-relative.
    pub origin: bool,
}

/// Handwritten cases covering each feature family (also recorded).
pub const HANDWRITTEN: &[(&str, &[u8])] = &[
    ("wrap", b"0123456789abcdef"),
    ("pending wrap + CR", b"0123456789\rX"),
    ("cup/erase", b"aaaa\r\nbbbb\r\ncccc\x1b[2;2H\x1b[K\x1b[1J"),
    ("insert/delete", b"abcdefgh\x1b[1;3H\x1b[2@\x1b[1P\x1b[3X"),
    ("scroll region", b"1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[4;1H\n\n\x1b[r"),
    ("IL/DL", b"1\r\n2\r\n3\r\n4\x1b[2;1H\x1b[2L\x1b[3;1H\x1b[M"),
    ("reverse index", b"a\r\nb\x1b[H\x1bM\x1bM"),
    ("origin mode", b"\x1b[2;4r\x1b[?6h\x1b[1;1HX\x1b[99;1HY\x1b[?6l"),
    ("tabs", b"a\tb\tc\x1b[3g\x1b[5G\x1bH\r\tZ"),
    ("SGR", b"\x1b[1;3;4;7;9;31;42mA\x1b[22;23;24;27;29mB\x1b[38;5;123;48;2;1;2;3mC\x1b[0mD"),
    ("wide chars", "日本語テキスト".as_bytes()),
    ("wide at edge", "abcdefghi日".as_bytes()),
    ("combining", "e\u{301}a\u{308}".as_bytes()),
    ("DEC graphics", b"\x1b(0lqk\x1b(B"),
    ("alt screen", b"main\x1b[?1049h\x1b[Halt\x1b[?1049l"),
    ("save/restore", b"\x1b[3;3H\x1b7\x1b[1;1Hx\x1b8y"),
    ("ECH/REP", b"abcdef\x1b[1;2H\x1b[2Xz\x1b[3b"),
    ("CHA/VPA/HPR", b"\x1b[5Gx\x1b[3dy\x1b[2az"),
    ("CNL/CPL", b"\x1b[3;5Hx\x1b[1Fy\x1b[2Ez"),
    ("DECALN", b"\x1b#8"),
    ("RIS", b"junk\x1b[31m\x1bcclean"),
    ("autowrap off", b"\x1b[?7l0123456789abc\x1b[?7h"),
    ("SU/SD", b"1\r\n2\r\n3\x1b[2S\x1b[1T"),
    ("wide delete", "日本\x1b[1;1H\x1b[2P".as_bytes()),
    ("wide no-autowrap", "\x1b[?7labc日本".as_bytes()),
    ("1049 re-entry", b"main\x1b[?1049hq\x1b[?1049h"),
    ("tab pending wrap", b"0123456789\tg"),
];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

impl Golden {
    pub fn to_json_line(&self) -> String {
        json!({
            "name": self.name,
            "cols": self.cols,
            "rows": self.rows,
            "input_hex": hex(&self.input),
            "lines": self.lines,
            "cursor": self.cursor.map(|(r, c)| vec![r, c]),
            "origin": self.origin,
        })
        .to_string()
    }

    pub fn from_json_line(line: &str) -> Option<Golden> {
        let v: Value = serde_json::from_str(line).ok()?;
        Some(Golden {
            name: v["name"].as_str()?.to_owned(),
            cols: v["cols"].as_u64()? as usize,
            rows: v["rows"].as_u64()? as usize,
            input: unhex(v["input_hex"].as_str()?)?,
            lines: v["lines"]
                .as_array()?
                .iter()
                .map(|l| l.as_str().map(str::to_owned))
                .collect::<Option<_>>()?,
            cursor: v["cursor"]
                .as_array()
                .and_then(|c| Some((c.first()?.as_u64()? as usize, c.get(1)?.as_u64()? as usize))),
            origin: v["origin"].as_bool()?,
        })
    }

    pub fn text(&self) -> String {
        normalize(&self.lines.join("\n"))
    }
}

/// Trim trailing blanks on each line and trailing empty lines, and apply
/// Unicode NFC (xterm's print output composes `e` + U+0301 into `é`).
pub fn normalize(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches(' ')).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n").nfc().collect()
}

/// Whether origin mode is left on at the end of `input`.
pub fn origin_left_on(input: &[u8]) -> bool {
    let find = |pat: &[u8]| input.windows(pat.len()).rposition(|w| w == pat);
    let last_off = find(b"\x1b[?6l").max(find(b"\x1bc"));
    match (find(b"\x1b[?6h"), last_off) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}
