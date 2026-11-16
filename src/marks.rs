//! The private marks that the zsh hooks write into the terminal stream.
//!
//! A mark is an OSC sequence: `ESC ] 7733 ; ts ; <kind> [; <payload>] BEL`.
//!
//! - `s;<cwd>;<command>` means a command started in directory `<cwd>`. Both parts are
//!   percent-encoded for `%`, ESC and BEL, and the directory also for `;`. An empty `<cwd>`
//!   means the shell did not report one.
//! - `r;<cwd>;<command>` means the shell read a line and rejected it before running anything, for
//!   example because of a syntax error. It is encoded like `s`. The shell has already printed
//!   its error by then, so the relay supplies that output from what it saw after the Enter key.
//! - `x` means a command started that must not be recorded.
//! - `e;<status>` means the command ended and the shell is back at a prompt.
//! - `@<nonce>;<host>;<mark>` wraps one of the marks above when it comes from a shell on another
//!   machine, started by `termscribe ssh`. `<nonce>` is the secret that the session gave to that
//!   shell, so the relay can tell it from output that merely looks like a mark. `<host>` is the
//!   name the user knows the machine by, percent-encoded like a directory. The directory inside
//!   the wrapped mark belongs to that machine, and its home is already written as `~`.
//!
//! The relay removes these marks before the terminal sees them.

/// Every mark starts with these bytes.
pub const PREFIX: &[u8] = b"\x1b]7733;ts;";

/// Marks longer than this are treated as ordinary output.
const MAX_MARK_LEN: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// A command started.
    Start {
        /// The directory it started in, when the shell reported one.
        cwd: Option<String>,
        command: String,
    },
    /// The shell rejected a line without running it. Its error message came before this mark.
    Rejected {
        /// The directory the shell was in, when it reported one.
        cwd: Option<String>,
        command: String,
    },
    /// A command started that stays out of the history.
    StartUnrecorded,
    /// The command ended with this status.
    End(Option<i32>),
    /// A mark from a shell on another machine.
    Remote {
        nonce: String,
        host: String,
        mark: Box<Mark>,
    },
}

/// A piece of the stream after scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Data(Vec<u8>),
    Mark(Mark),
}

/// Finds marks in a byte stream that arrives in arbitrary chunks.
#[derive(Debug, Default)]
pub struct MarkScanner {
    held: Vec<u8>,
}

impl MarkScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when bytes are held back because they might be the start of a mark.
    pub fn is_holding_partial_prefix(&self) -> bool {
        !self.held.is_empty() && self.held.len() < PREFIX.len()
    }

    /// Scans a chunk. Output items appear in stream order.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Item> {
        self.held.extend_from_slice(chunk);
        let mut items = Vec::new();
        let mut data = Vec::new();
        loop {
            let Some(esc) = self.held.iter().position(|&b| b == 0x1b) else {
                data.append(&mut self.held);
                break;
            };
            data.extend(self.held.drain(..esc));

            let overlap = self.held.len().min(PREFIX.len());
            if self.held[..overlap] != PREFIX[..overlap] {
                data.push(self.held.remove(0));
                continue;
            }
            if self.held.len() < PREFIX.len() {
                break;
            }
            match find_terminator(&self.held[PREFIX.len()..]) {
                Some((payload_len, terminator_len)) => {
                    let payload = &self.held[PREFIX.len()..PREFIX.len() + payload_len];
                    let parsed = parse_payload(payload);
                    let consumed = PREFIX.len() + payload_len + terminator_len;
                    self.held.drain(..consumed);
                    if let Some(mark) = parsed {
                        if !data.is_empty() {
                            items.push(Item::Data(std::mem::take(&mut data)));
                        }
                        items.push(Item::Mark(mark));
                    }
                }
                None if self.held.len() > MAX_MARK_LEN => {
                    data.append(&mut self.held);
                    break;
                }
                None => break,
            }
        }
        if !data.is_empty() {
            items.push(Item::Data(data));
        }
        items
    }

    /// Releases held bytes that turned out not to start a mark, for example after a pause.
    /// A mark that is still arriving stays held.
    pub fn flush_partial_prefix(&mut self) -> Option<Vec<u8>> {
        if self.is_holding_partial_prefix() {
            Some(std::mem::take(&mut self.held))
        } else {
            None
        }
    }
}

/// Returns the payload length and the terminator length (BEL is 1, ESC `\` is 2).
fn find_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes.iter().enumerate().find_map(|(i, &b)| match b {
        0x07 => Some((i, 1)),
        0x1b if bytes.get(i + 1) == Some(&b'\\') => Some((i, 2)),
        _ => None,
    })
}

fn parse_payload(payload: &[u8]) -> Option<Mark> {
    let text = std::str::from_utf8(payload).ok()?;
    if let Some(wrapped) = text.strip_prefix('@') {
        let (nonce, rest) = wrapped.split_once(';')?;
        let (host, inner) = rest.split_once(';')?;
        // A remote mark never wraps another remote mark.
        let mark = match parse_payload(inner.as_bytes())? {
            Mark::Remote { .. } => return None,
            mark => mark,
        };
        return Some(Mark::Remote {
            nonce: nonce.to_string(),
            host: percent_decode(host),
            mark: Box::new(mark),
        });
    }
    let (kind, rest) = text.split_once(';').unwrap_or((text, ""));
    match kind {
        "s" | "r" => {
            let (cwd, command) = rest.split_once(';').unwrap_or(("", rest));
            let cwd = Some(percent_decode(cwd)).filter(|cwd| !cwd.is_empty());
            let command = percent_decode(command);
            Some(if kind == "s" {
                Mark::Start { cwd, command }
            } else {
                Mark::Rejected { cwd, command }
            })
        }
        "x" => Some(Mark::StartUnrecorded),
        "e" => Some(Mark::End(rest.trim().parse().ok())),
        _ => None,
    }
}

/// Decodes `%XX` escapes. Anything that is not a valid escape stays as it is.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encodes a mark. The zsh hooks do this in shell code. Tests and tooling use this version.
pub fn encode(mark: &Mark) -> Vec<u8> {
    let mut out = PREFIX.to_vec();
    encode_payload(mark, &mut out);
    out.push(0x07);
    out
}

fn encode_payload(mark: &Mark, out: &mut Vec<u8>) {
    match mark {
        Mark::Remote { nonce, host, mark } => {
            out.push(b'@');
            out.extend_from_slice(nonce.as_bytes());
            out.push(b';');
            out.extend_from_slice(percent_encode(host).replace(';', "%3B").as_bytes());
            out.push(b';');
            encode_payload(mark, out);
        }
        Mark::Start { cwd, command } | Mark::Rejected { cwd, command } => {
            out.extend_from_slice(if matches!(mark, Mark::Start { .. }) {
                b"s;"
            } else {
                b"r;"
            });
            if let Some(cwd) = cwd {
                out.extend_from_slice(percent_encode(cwd).replace(';', "%3B").as_bytes());
            }
            out.push(b';');
            out.extend_from_slice(percent_encode(command).as_bytes());
        }
        Mark::StartUnrecorded => out.push(b'x'),
        Mark::End(status) => {
            out.extend_from_slice(b"e");
            if let Some(status) = status {
                out.extend_from_slice(format!(";{status}").as_bytes());
            }
        }
    }
}

fn percent_encode(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\x1b', "%1B")
        .replace('\x07', "%07")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(chunks: &[&[u8]]) -> Vec<Item> {
        let mut scanner = MarkScanner::new();
        let mut items: Vec<Item> = Vec::new();
        for chunk in chunks {
            for item in scanner.feed(chunk) {
                match (items.last_mut(), item) {
                    (Some(Item::Data(existing)), Item::Data(more)) => existing.extend(more),
                    (_, item) => items.push(item),
                }
            }
        }
        items
    }

    fn start(command: &str) -> Mark {
        Mark::Start {
            cwd: None,
            command: command.into(),
        }
    }

    fn data(text: &str) -> Item {
        Item::Data(text.as_bytes().to_vec())
    }

    #[test]
    fn a_rejected_line_is_its_own_kind_of_mark() {
        let mark = Mark::Rejected {
            cwd: Some("/tmp/a b".into()),
            command: "export X=<oops>".into(),
        };
        assert_eq!(scan_all(&[&encode(&mark)]), [Item::Mark(mark)]);
    }

    fn remote(mark: Mark) -> Mark {
        Mark::Remote {
            nonce: "ab12".into(),
            host: "prod; box".into(),
            mark: Box::new(mark),
        }
    }

    #[test]
    fn a_mark_from_another_machine_carries_its_nonce_and_host() {
        for inner in [
            Mark::Start {
                cwd: Some("~/a;b".into()),
                command: "echo 1;2".into(),
            },
            Mark::Rejected {
                cwd: None,
                command: "echo )".into(),
            },
            Mark::StartUnrecorded,
            Mark::End(Some(3)),
        ] {
            let mark = remote(inner);
            assert_eq!(scan_all(&[&encode(&mark)]), [Item::Mark(mark)]);
        }
    }

    #[test]
    fn a_remote_mark_without_a_host_or_a_mark_inside_is_dropped() {
        assert_eq!(scan_all(&[b"a\x1b]7733;ts;@n\x07b"]), [data("ab")]);
        assert_eq!(scan_all(&[b"a\x1b]7733;ts;@n;h\x07b"]), [data("ab")]);
        assert_eq!(scan_all(&[b"a\x1b]7733;ts;@n;h;zzz\x07b"]), [data("ab")]);
    }

    #[test]
    fn a_remote_mark_cannot_wrap_another_one() {
        let stream = b"\x1b]7733;ts;@n;h;@m;h;e;0\x07";
        assert_eq!(scan_all(&[stream]), []);
    }

    #[test]
    fn plain_data_passes_through() {
        assert_eq!(scan_all(&[b"hello"]), [data("hello")]);
    }

    #[test]
    fn marks_are_found_and_removed() {
        let mut stream = b"before".to_vec();
        stream.extend(encode(&start("ls -l")));
        stream.extend_from_slice(b"output");
        stream.extend(encode(&Mark::End(Some(2))));
        assert_eq!(
            scan_all(&[&stream]),
            [
                data("before"),
                Item::Mark(start("ls -l")),
                data("output"),
                Item::Mark(Mark::End(Some(2))),
            ]
        );
    }

    #[test]
    fn marks_split_across_chunks_are_still_found() {
        let encoded = encode(&start("echo hi"));
        for split in 1..encoded.len() {
            let (a, b) = encoded.split_at(split);
            assert_eq!(
                scan_all(&[a, b]),
                [Item::Mark(start("echo hi"))],
                "split at {split}"
            );
        }
    }

    #[test]
    fn byte_at_a_time_delivery_works() {
        let mut stream = b"x".to_vec();
        stream.extend(encode(&Mark::StartUnrecorded));
        stream.extend_from_slice(b"y");
        let chunks: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(
            scan_all(&chunks),
            [data("x"), Item::Mark(Mark::StartUnrecorded), data("y")]
        );
    }

    #[test]
    fn other_escape_sequences_are_untouched() {
        let stream = b"\x1b[31mred\x1b[0m\x1b]0;title\x07";
        assert_eq!(scan_all(&[stream]), [Item::Data(stream.to_vec())]);
    }

    #[test]
    fn near_miss_prefix_is_data() {
        let stream = b"\x1b]7733;other;x\x07";
        assert_eq!(scan_all(&[stream]), [Item::Data(stream.to_vec())]);
    }

    #[test]
    fn command_text_round_trips_with_special_bytes() {
        let command = "echo 100% \x1b[31m\x07 done\nsecond line";
        let encoded = encode(&start(command));
        assert_eq!(scan_all(&[&encoded]), [Item::Mark(start(command))]);
    }

    #[test]
    fn directory_round_trips_with_special_bytes() {
        let mark = Mark::Start {
            cwd: Some("/tmp/a;b%c\x1b d".into()),
            command: "echo ;x".into(),
        };
        assert_eq!(scan_all(&[&encode(&mark)]), [Item::Mark(mark)]);
    }

    #[test]
    fn missing_directory_is_none() {
        let stream = b"\x1b]7733;ts;s;;ls\x07";
        assert_eq!(scan_all(&[stream]), [Item::Mark(start("ls"))]);
    }

    #[test]
    fn st_terminator_is_accepted() {
        let stream = b"\x1b]7733;ts;e;0\x1b\\";
        assert_eq!(scan_all(&[stream]), [Item::Mark(Mark::End(Some(0)))]);
    }

    #[test]
    fn end_without_status_is_accepted() {
        assert_eq!(
            scan_all(&[&encode(&Mark::End(None))]),
            [Item::Mark(Mark::End(None))]
        );
    }

    #[test]
    fn unknown_mark_kinds_are_dropped() {
        let stream = b"a\x1b]7733;ts;zzz\x07b";
        assert_eq!(scan_all(&[stream]), [data("ab")]);
    }

    #[test]
    fn lone_escape_is_released_by_flush() {
        let mut scanner = MarkScanner::new();
        assert!(scanner.feed(b"\x1b]77").is_empty());
        assert!(scanner.is_holding_partial_prefix());
        assert_eq!(scanner.flush_partial_prefix(), Some(b"\x1b]77".to_vec()));
        assert!(!scanner.is_holding_partial_prefix());
    }

    #[test]
    fn a_mark_in_progress_is_not_flushed() {
        let mut scanner = MarkScanner::new();
        let encoded = encode(&start("ls"));
        assert!(scanner.feed(&encoded[..encoded.len() - 2]).is_empty());
        assert_eq!(scanner.flush_partial_prefix(), None);
        assert_eq!(
            scanner.feed(&encoded[encoded.len() - 2..]),
            [Item::Mark(start("ls"))]
        );
    }

    #[test]
    fn percent_decode_leaves_invalid_escapes_alone() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("a%25b"), "a%b");
        assert_eq!(percent_decode("%é"), "%é");
    }
}
