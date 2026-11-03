//! Spotting "erase the whole display" in a command's output.

/// Watches a command's output for `ESC [ 2 J` and `ESC [ 3 J`, which is what `clear` prints.
///
/// Text drawn on the alternate screen is ignored, because full-screen programs such as vim and
/// less erase their own screen when they start.
#[derive(Debug, Default)]
pub struct ClearDetector {
    state: Parse,
    params: Vec<u8>,
    in_alt_screen: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Parse {
    #[default]
    Ground,
    Escape,
    Csi,
}

impl ClearDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets everything. Call this when a new command starts.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Scans a chunk. Returns true when the display was erased in the main screen.
    /// Sequences may be split across chunks.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut cleared = false;
        for &byte in bytes {
            match self.state {
                Parse::Ground => {
                    if byte == 0x1b {
                        self.state = Parse::Escape;
                    }
                }
                Parse::Escape => match byte {
                    b'[' => {
                        self.state = Parse::Csi;
                        self.params.clear();
                    }
                    0x1b => {}
                    _ => self.state = Parse::Ground,
                },
                Parse::Csi => match byte {
                    0x20..=0x3f => {
                        if self.params.len() < 32 {
                            self.params.push(byte);
                        }
                    }
                    0x40..=0x7e => {
                        cleared |= self.finish_sequence(byte);
                        self.state = Parse::Ground;
                    }
                    0x1b => self.state = Parse::Escape,
                    _ => self.state = Parse::Ground,
                },
            }
        }
        cleared
    }

    fn finish_sequence(&mut self, final_byte: u8) -> bool {
        match (self.params.split_first(), final_byte) {
            (Some((b'?', modes)), b'h' | b'l') => {
                let toggles_alt = modes
                    .split(|&b| b == b';')
                    .any(|mode| matches!(mode, b"1049" | b"1047" | b"47"));
                if toggles_alt {
                    self.in_alt_screen = final_byte == b'h';
                }
                false
            }
            _ => {
                !self.in_alt_screen && final_byte == b'J' && matches!(&self.params[..], b"2" | b"3")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detects(bytes: &[u8]) -> bool {
        ClearDetector::new().feed(bytes)
    }

    #[test]
    fn what_the_clear_command_prints_is_detected() {
        assert!(detects(b"\x1b[H\x1b[2J\x1b[3J"));
        assert!(detects(b"\x1b[2J"));
        assert!(detects(b"\x1b[3J"));
    }

    #[test]
    fn other_erase_sequences_are_not_a_clear() {
        assert!(!detects(b"\x1b[J"));
        assert!(!detects(b"\x1b[0J"));
        assert!(!detects(b"\x1b[1J"));
        assert!(!detects(b"\x1b[2K"));
        assert!(!detects(b"\x1b[31mred\x1b[0m"));
        assert!(!detects(b"plain text 2J"));
    }

    #[test]
    fn a_sequence_split_across_chunks_is_still_detected() {
        let sequence = b"\x1b[3J";
        for split in 1..sequence.len() {
            let mut detector = ClearDetector::new();
            let (a, b) = sequence.split_at(split);
            assert!(!detector.feed(a), "split at {split}");
            assert!(detector.feed(b), "split at {split}");
        }
    }

    #[test]
    fn clears_inside_the_alternate_screen_are_ignored() {
        assert!(!detects(b"\x1b[?1049h\x1b[H\x1b[2J drawing"));
        assert!(!detects(b"\x1b[?1049h\x1b[2J\x1b[?1049l"));
    }

    #[test]
    fn a_clear_after_leaving_the_alternate_screen_counts() {
        assert!(detects(b"\x1b[?1049h\x1b[?1049l\x1b[2J"));
    }

    #[test]
    fn reset_forgets_the_alternate_screen() {
        let mut detector = ClearDetector::new();
        detector.feed(b"\x1b[?1049h");
        detector.reset();
        assert!(detector.feed(b"\x1b[2J"));
    }

    #[test]
    fn an_interrupted_sequence_does_not_confuse_later_ones() {
        assert!(detects(b"\x1b[2\x1b[2J"));
        assert!(!detects(b"\x1b[2\nJ"));
    }
}
