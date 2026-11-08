//! A byte-level filter in front of `vte` that bounds OSC payloads.
//!
//! With `std`, vte buffers an OSC string without limit, so a program could
//! exhaust memory with one endless `ESC ]`. Once an OSC exceeds `max`
//! bytes, the filter sends vte a CAN (0x18), which aborts the sequence, and
//! drops the rest of the payload up to its terminator (BEL, ST, CAN/SUB).
//!
//! This is also where APC strings (the Kitty graphics protocol), which vte
//! discards, will be intercepted (ADR 0002).

/// Default OSC limit: comfortably above the 1 MiB OSC 52 clipboard cap.
pub const DEFAULT_MAX_OSC: usize = 2 << 20;

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
}

#[derive(Debug)]
pub(crate) struct OscLimiter {
    st: St,
    len: usize,
    pub max: usize,
    pub dropped: u64,
}

impl OscLimiter {
    pub fn new(max: usize) -> Self {
        OscLimiter { st: St::Ground, len: 0, max, dropped: 0 }
    }

    /// Pass `bytes` through, calling `feed` with the parts vte should see.
    pub fn filter(&mut self, bytes: &[u8], mut feed: impl FnMut(&[u8])) {
        let mut start = 0; // start of the current pass-through run
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match self.st {
                St::Ground => {
                    // Fast path: skip ordinary text to the next ESC.
                    match bytes[i..].iter().position(|&x| x == ESC) {
                        Some(off) => {
                            i += off;
                            self.st = St::Esc;
                        }
                        None => break,
                    }
                }
                St::Esc => self.after_esc(b),
                St::Osc => {
                    self.len += 1;
                    match b {
                        BEL | CAN | SUB => self.st = St::Ground,
                        ESC => self.st = St::OscEsc,
                        _ if self.len > self.max => {
                            // Hand vte what it has so far, then abort.
                            feed(&bytes[start..i]);
                            feed(&[CAN]);
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
                        feed(&[ESC]);
                        start = i;
                        self.after_esc(b);
                    }
                }
            }
            i += 1;
        }
        // While discarding (including a trailing ESC that may begin ST),
        // nothing from this chunk's tail is forwarded.
        if start < bytes.len() && !matches!(self.st, St::Discard | St::DiscardEsc) {
            feed(&bytes[start..]);
        }
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
            f.filter(c, |p| out.extend_from_slice(p));
        }
        out
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
