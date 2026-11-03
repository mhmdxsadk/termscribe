//! Spotting the copy shortcut in the keystrokes that go to the shell.

/// The sequence the terminal sends for the copy shortcut: `CSI 9000 ~`.
pub const COPY_TRIGGER: &[u8] = b"\x1b[9000~";

/// A piece of the keyboard stream after filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputItem {
    /// Bytes for the shell.
    Data(Vec<u8>),
    /// The copy shortcut was pressed.
    CopyShortcut,
}

/// Cuts the copy shortcut out of keyboard input that arrives in arbitrary chunks.
///
/// A lone ESC is never held back, so the Escape key stays instant in vim. Only a partial
/// sequence of two bytes or more waits for the rest.
#[derive(Debug, Default)]
pub struct InputFilter {
    held: Vec<u8>,
}

impl InputFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<InputItem> {
        self.held.extend_from_slice(chunk);
        let mut items = Vec::new();
        let mut data = Vec::new();
        loop {
            let Some(esc) = self.held.iter().position(|&b| b == 0x1b) else {
                data.append(&mut self.held);
                break;
            };
            data.extend(self.held.drain(..esc));

            let overlap = self.held.len().min(COPY_TRIGGER.len());
            if self.held[..overlap] != COPY_TRIGGER[..overlap] {
                data.push(self.held.remove(0));
                continue;
            }
            if self.held.len() >= COPY_TRIGGER.len() {
                self.held.drain(..COPY_TRIGGER.len());
                if !data.is_empty() {
                    items.push(InputItem::Data(std::mem::take(&mut data)));
                }
                items.push(InputItem::CopyShortcut);
                continue;
            }
            if self.held.len() == 1 {
                data.append(&mut self.held);
            }
            break;
        }
        if !data.is_empty() {
            items.push(InputItem::Data(data));
        }
        items
    }

    /// Releases held bytes, for example when the rest of the sequence did not arrive in time.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.held.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.held))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(bytes: &[u8]) -> InputItem {
        InputItem::Data(bytes.to_vec())
    }

    #[test]
    fn ordinary_typing_passes_through() {
        let mut filter = InputFilter::new();
        assert_eq!(filter.feed(b"ls -l\r"), [data(b"ls -l\r")]);
    }

    #[test]
    fn shortcut_is_removed() {
        let mut filter = InputFilter::new();
        assert_eq!(filter.feed(COPY_TRIGGER), [InputItem::CopyShortcut]);
    }

    #[test]
    fn shortcut_between_other_bytes_keeps_the_rest_in_order() {
        let mut filter = InputFilter::new();
        let mut input = b"ab".to_vec();
        input.extend_from_slice(COPY_TRIGGER);
        input.extend_from_slice(b"cd");
        assert_eq!(
            filter.feed(&input),
            [data(b"ab"), InputItem::CopyShortcut, data(b"cd")]
        );
    }

    #[test]
    fn lone_escape_is_forwarded_immediately() {
        let mut filter = InputFilter::new();
        assert_eq!(filter.feed(b"\x1b"), [data(b"\x1b")]);
        assert!(!filter.is_holding());
    }

    #[test]
    fn arrow_keys_pass_through() {
        let mut filter = InputFilter::new();
        assert_eq!(filter.feed(b"\x1b[A"), [data(b"\x1b[A")]);
        assert_eq!(filter.feed(b"\x1b[1;5C"), [data(b"\x1b[1;5C")]);
    }

    #[test]
    fn shortcut_split_across_chunks_is_still_found() {
        for split in 2..COPY_TRIGGER.len() {
            let mut filter = InputFilter::new();
            let (a, b) = COPY_TRIGGER.split_at(split);
            assert!(filter.feed(a).is_empty(), "split at {split}");
            assert!(filter.is_holding());
            assert_eq!(
                filter.feed(b),
                [InputItem::CopyShortcut],
                "split at {split}"
            );
        }
    }

    #[test]
    fn held_partial_sequence_that_turns_out_different_is_forwarded() {
        let mut filter = InputFilter::new();
        assert!(filter.feed(b"\x1b[90").is_empty());
        assert_eq!(filter.feed(b"1~"), [data(b"\x1b[901~")]);
    }

    #[test]
    fn flush_releases_held_bytes() {
        let mut filter = InputFilter::new();
        assert!(filter.feed(b"\x1b[9").is_empty());
        assert_eq!(filter.flush(), Some(b"\x1b[9".to_vec()));
        assert_eq!(filter.flush(), None);
    }

    #[test]
    fn two_shortcuts_in_a_row() {
        let mut filter = InputFilter::new();
        let mut input = COPY_TRIGGER.to_vec();
        input.extend_from_slice(COPY_TRIGGER);
        assert_eq!(
            filter.feed(&input),
            [InputItem::CopyShortcut, InputItem::CopyShortcut]
        );
    }
}
