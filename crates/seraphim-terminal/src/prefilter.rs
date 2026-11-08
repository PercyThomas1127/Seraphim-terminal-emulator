//! A byte-level filter in front of `vte` that bounds OSC payloads.
//!
//! With `std`, vte buffers an OSC string without limit, so a program could
//! exhaust memory with one endless `ESC ]`. Once an OSC exceeds `max`
//! bytes, the filter sends vte a CAN (0x18), which aborts the sequence, and
//! drops the rest of the payload up to its terminator (BEL, ST, CAN/SUB).
//!
//! It also intercepts APC strings (`ESC _ ... ESC \`, the Kitty graphics
//! protocol), which vte would discard, and hands them over in stream order
//! (ADR 0002).

/// Default OSC limit: comfortably above the 1 MiB OSC 52 clipboard cap.
pub const DEFAULT_MAX_OSC: usize = 2 << 20;
/// Longest APC payload kept; longer ones are dropped whole.
pub const MAX_APC: usize = 64 << 20;

/// What the filter passes on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Part<'a> {
    /// Bytes for vte.
    Bytes(&'a [u8]),
    /// A complete APC payload (between `ESC _` and `ESC \`).
    Apc(Vec<u8>),
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Ground,
    Esc,
    Osc,
    OscEsc,
    Discard,
    DiscardEsc,
    Apc,
    ApcEsc,
}

#[derive(Debug)]
pub(crate) struct OscLimiter {
    st: St,
    len: usize,
    pub max: usize,
    pub dropped: u64,
    /// Index in the current chunk of the ESC that began `St::Esc` from
    /// ground; `None` if it was held back from the previous chunk.
    esc_at: Option<usize>,
    /// The previous chunk ended in a ground ESC we have not forwarded.
    held_esc: bool,
    /// `St::Esc` was entered from ground (only then can `_` start an APC).
    esc_from_ground: bool,
    apc: Vec<u8>,
    apc_overflow: bool,
}

impl OscLimiter {
    pub fn new(max: usize) -> Self {
        OscLimiter {
            st: St::Ground,
            len: 0,
            max,
            dropped: 0,
            esc_at: None,
            held_esc: false,
            esc_from_ground: false,
            apc: Vec::new(),
            apc_overflow: false,
        }
    }

    /// Pass `bytes` through, calling `feed` with the parts vte should see
    /// and with APC payloads, in order.
    pub fn filter(&mut self, bytes: &[u8], mut feed: impl FnMut(Part<'_>)) {
        let mut start = 0; // start of the current pass-through run
        let mut i = 0;
        if std::mem::take(&mut self.held_esc) {
            match bytes.first() {
                Some(b'_') => {
                    self.begin_apc();
                    i = 1;
                    start = 1;
                }
                Some(_) => {
                    // Not an APC after all: vte gets the ESC, and the byte
                    // after it is handled as usual (it may start an OSC).
                    feed(Part::Bytes(&[ESC]));
                    self.st = St::Esc;
                    self.esc_at = None;
                    self.esc_from_ground = true;
                }
                None => {
                    self.held_esc = true;
                    return;
                }
            }
        }
        while i < bytes.len() {
            let b = bytes[i];
            match self.st {
                St::Ground => {
                    // Fast path: skip ordinary text to the next ESC.
                    match bytes[i..].iter().position(|&x| x == ESC) {
                        Some(off) => {
                            i += off;
                            self.st = St::Esc;
                            self.esc_at = Some(i);
                            self.esc_from_ground = true;
                        }
                        None => break,
                    }
                }
                St::Esc if b == b'_' && self.esc_from_ground => {
                    if let Some(at) = self.esc_at {
                        feed(Part::Bytes(&bytes[start..at]));
                    }
                    self.begin_apc();
                    start = i + 1;
                }
                St::Esc if b == ESC => self.esc_at = Some(i),
                St::Esc => self.after_esc(b),
                St::Apc => {
                    match b {
                        ESC => self.st = St::ApcEsc,
                        CAN | SUB => self.st = St::Ground,
                        _ if self.apc.len() >= MAX_APC => {
                            self.apc_overflow = true;
                            self.apc = Vec::new();
                        }
                        _ if self.apc_overflow => {}
                        _ => {
                            // Copy the run up to the next ESC/CAN/SUB at once.
                            let run = bytes[i..]
                                .iter()
                                .position(|&x| x == ESC || x == CAN || x == SUB)
                                .unwrap_or(bytes.len() - i);
                            let take = run.min(MAX_APC - self.apc.len());
                            self.apc.extend_from_slice(&bytes[i..i + take]);
                            i += take;
                            start = i;
                            continue;
                        }
                    }
                    start = i + 1;
                }
                St::ApcEsc => {
                    if b == b'\\' {
                        self.st = St::Ground;
                        if !std::mem::take(&mut self.apc_overflow) {
                            feed(Part::Apc(std::mem::take(&mut self.apc)));
                        }
                        start = i + 1;
                    } else {
                        // ESC inside an APC ends it (unterminated: dropped)
                        // and starts a new sequence.
                        self.apc = Vec::new();
                        self.apc_overflow = false;
                        feed(Part::Bytes(&[ESC]));
                        start = i;
                        self.esc_from_ground = false;
                        self.after_esc(b);
                    }
                }
                St::Osc => {
                    self.len += 1;
                    match b {
                        BEL | CAN | SUB => self.st = St::Ground,
                        ESC => self.st = St::OscEsc,
                        _ if self.len > self.max => {
                            // Hand vte what it has so far, then abort.
                            feed(Part::Bytes(&bytes[start..i]));
                            feed(Part::Bytes(&[CAN]));
                            self.dropped += 1;
                            self.st = St::Discard;
                            start = i + 1;
                        }
                        _ => {}
                    }
                }
                St::OscEsc => {
                    if b == b'\\' {
                        self.st = St::Ground;
                    } else {
                        self.esc_from_ground = false;
                        // ESC + anything else ends the OSC and starts a new
                        // escape sequence.
                        self.after_esc(b);
                    }
                }
                St::Discard => {
                    match b {
                        BEL | CAN | SUB => self.st = St::Ground,
                        ESC => self.st = St::DiscardEsc,
                        _ => {}
                    }
                    start = i + 1;
                }
                St::DiscardEsc => {
                    if b == b'\\' {
                        self.st = St::Ground;
                        start = i + 1;
                    } else {
                        // The ESC we swallowed starts a real sequence.
                        feed(Part::Bytes(&[ESC]));
                        start = i;
                        self.esc_from_ground = false;
                        self.after_esc(b);
                    }
                }
            }
            i += 1;
        }
        // While discarding or inside an APC (including a trailing ESC that
        // may begin ST), nothing from this chunk's tail is forwarded. A
        // trailing ground ESC is held back: it may start an APC.
        match self.st {
            St::Discard | St::DiscardEsc | St::Apc | St::ApcEsc => {}
            St::Esc if self.esc_from_ground && self.esc_at.is_some() => {
                let at = self.esc_at.unwrap();
                if bytes.len() == at + 1 {
                    if start < at {
                        feed(Part::Bytes(&bytes[start..at]));
                    }
                    self.held_esc = true;
                    self.st = St::Ground;
                } else if start < bytes.len() {
                    feed(Part::Bytes(&bytes[start..]));
                }
            }
            _ if start < bytes.len() => feed(Part::Bytes(&bytes[start..])),
            _ => {}
        }
    }

    fn begin_apc(&mut self) {
        self.st = St::Apc;
        self.apc.clear();
        self.apc_overflow = false;
    }

    fn after_esc(&mut self, b: u8) {
        self.st = match b {
            b']' => {
                self.len = 0;
                St::Osc
            }
            ESC => St::Esc,
            _ => St::Ground,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(f: &mut OscLimiter, chunks: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in chunks {
            f.filter(c, |p| match p {
                Part::Bytes(b) => out.extend_from_slice(b),
                Part::Apc(a) => {
                    out.extend_from_slice(b"<APC:");
                    out.extend_from_slice(&a);
                    out.push(b'>');
                }
            });
        }
        out
    }

    #[test]
    fn apc_is_cut_out_in_order() {
        let mut f = OscLimiter::new(16);
        let out = run(&mut f, &[b"a\x1b_Gi=1;QUJD\x1b\\b\x1b[1mc"]);
        assert_eq!(out, b"a<APC:Gi=1;QUJD>b\x1b[1mc");
    }

    #[test]
    fn apc_across_every_chunk_boundary() {
        let input: &[u8] = b"x\x1b_Ga=T;AAAA\x1b\\y\x1b[0m\x1b]0;t\x07z";
        let want = b"x<APC:Ga=T;AAAA>y\x1b[0m\x1b]0;t\x07z".to_vec();
        for split in 0..=input.len() {
            let mut f = OscLimiter::new(64);
            let (a, b) = input.split_at(split);
            assert_eq!(run(&mut f, &[a, b]), want, "split at {split}");
        }
        // One byte at a time.
        let mut f = OscLimiter::new(64);
        let chunks: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(run(&mut f, &chunks), want);
    }

    #[test]
    fn trailing_esc_is_forwarded_when_not_apc() {
        let mut f = OscLimiter::new(16);
        assert_eq!(run(&mut f, &[b"a\x1b", b"[31mred"]), b"a\x1b[31mred");
        let mut f = OscLimiter::new(16);
        assert_eq!(run(&mut f, &[b"\x1b", b"\x1b", b"[m"]), b"\x1b\x1b[m");
    }

    #[test]
    fn unterminated_apc_is_dropped_and_esc_starts_a_sequence() {
        let mut f = OscLimiter::new(16);
        assert_eq!(run(&mut f, &[b"\x1b_Gjunk\x1b[1mb"]), b"\x1b[1mb");
        let mut f = OscLimiter::new(16);
        assert_eq!(run(&mut f, &[b"\x1b_Gjunk\x18after"]), b"after", "CAN aborts");
    }

    #[test]
    fn passes_normal_traffic_untouched() {
        let mut f = OscLimiter::new(16);
        let input: &[u8] = b"hi \x1b[1mbold\x1b]2;title\x07 \x1b]8;;u\x1b\\x";
        assert_eq!(run(&mut f, &[input]), input);
        assert_eq!(f.dropped, 0);
    }

    #[test]
    fn oversized_osc_is_cancelled_and_payload_dropped() {
        let mut f = OscLimiter::new(8);
        let out = run(&mut f, &[b"a\x1b]52;c;0123456789abcdef\x07b"]);
        // Exactly `max` payload bytes ("52;c;012") reach vte.
        assert_eq!(out, b"a\x1b]52;c;012\x18b");
        assert_eq!(f.dropped, 1);
    }

    #[test]
    fn limit_spans_chunk_boundaries() {
        let mut f = OscLimiter::new(8);
        let out = run(&mut f, &[b"\x1b]52;c", b";01234", b"56789", b"\x1b", b"\\after"]);
        assert_eq!(out, b"\x1b]52;c;012\x18after");
    }

    #[test]
    fn escape_inside_discarded_payload_starts_new_sequence() {
        let mut f = OscLimiter::new(4);
        let out = run(&mut f, &[b"\x1b]0;aaaaaaaa\x1b[31mred"]);
        assert_eq!(out, b"\x1b]0;aa\x18\x1b[31mred");
    }
}
