//! Turning raw terminal output into the text the user saw.

/// Output after cleaning.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CleanedOutput {
    /// The visible text, with no trailing blank lines.
    pub text: String,
    /// True when the command drew on the alternate screen (vim, htop, less).
    /// Whatever it drew there is not part of `text`.
    pub alt_screen: bool,
}

/// Cleans raw output.
///
/// With `keep_ansi` false, escape sequences are removed and carriage returns, backspaces and
/// "erase line" sequences are applied, so a progress bar leaves only its last state. Text drawn
/// on the alternate screen is dropped. With `keep_ansi` true, only line endings are normalized
/// and the escape sequences stay in the text.
pub fn clean(raw: &[u8], keep_ansi: bool) -> CleanedOutput {
    let raw = strip_prompt_sp(raw);
    let text = String::from_utf8_lossy(raw);
    if keep_ansi {
        let normalized = text.replace("\r\n", "\n");
        return CleanedOutput {
            text: trim_end_blank(&normalized),
            alt_screen: false,
        };
    }
    render(&text)
}

fn trim_end_blank(text: &str) -> String {
    text.trim_end_matches(['\n', '\r', ' ', '\t']).to_string()
}

/// Removes the end-of-line marker that zsh prints before `precmd` when `PROMPT_SP` is on.
///
/// The bytes look like `%` + spaces + `\r` + space + `\r`, optionally wrapped in SGR colour
/// sequences. They sit at the very end of a command's output and are not part of it.
fn strip_prompt_sp(raw: &[u8]) -> &[u8] {
    let mut end = raw.len();
    let take_byte = |end: &mut usize, byte: u8| -> bool {
        if *end > 0 && raw[*end - 1] == byte {
            *end -= 1;
            true
        } else {
            false
        }
    };
    let take_spaces = |end: &mut usize| -> bool {
        let start = *end;
        while *end > 0 && raw[*end - 1] == b' ' {
            *end -= 1;
        }
        *end < start
    };
    if !(take_byte(&mut end, b'\r')
        && take_spaces(&mut end)
        && take_byte(&mut end, b'\r')
        && take_spaces(&mut end))
    {
        return raw;
    }
    end = strip_trailing_sgr(raw, end);
    if !(take_byte(&mut end, b'%') || take_byte(&mut end, b'#')) {
        return raw;
    }
    end = strip_trailing_sgr(raw, end);
    &raw[..end]
}

/// Moves `end` back over any `ESC [ ... m` sequences that finish at `end`.
fn strip_trailing_sgr(raw: &[u8], mut end: usize) -> usize {
    loop {
        if end == 0 || raw[end - 1] != b'm' {
            return end;
        }
        let mut start = end - 1;
        while start > 0 && (raw[start - 1].is_ascii_digit() || raw[start - 1] == b';') {
            start -= 1;
        }
        if start >= 2 && raw[start - 1] == b'[' && raw[start - 2] == 0x1b {
            end = start - 2;
        } else {
            return end;
        }
    }
}

/// A minimal line model. It tracks one line and a cursor column, which is all that carriage
/// returns, backspaces and erase-in-line need.
struct Screen {
    lines: Vec<String>,
    line: Vec<char>,
    col: usize,
}

impl Screen {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            line: Vec::new(),
            col: 0,
        }
    }

    fn put(&mut self, c: char) {
        if self.col < self.line.len() {
            self.line[self.col] = c;
        } else {
            while self.line.len() < self.col {
                self.line.push(' ');
            }
            self.line.push(c);
        }
        self.col += 1;
    }

    fn newline(&mut self) {
        let line: String = self.line.iter().collect();
        self.lines.push(line.trim_end().to_string());
        self.line.clear();
        self.col = 0;
    }

    fn erase_in_line(&mut self, mode: usize) {
        match mode {
            0 => self.line.truncate(self.col),
            1 => {
                let upto = self.col.min(self.line.len());
                for cell in &mut self.line[..upto] {
                    *cell = ' ';
                }
            }
            _ => self.line.clear(),
        }
    }

    fn finish(mut self) -> String {
        if !self.line.is_empty() {
            self.newline();
        }
        while self.lines.last().is_some_and(|line| line.is_empty()) {
            self.lines.pop();
        }
        self.lines.join("\n")
    }
}

fn render(text: &str) -> CleanedOutput {
    let chars: Vec<char> = text.chars().collect();
    let mut screen = Screen::new();
    let mut alt_screen = false;
    let mut in_alt = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\x1b' => i = escape(&chars, i, &mut screen, &mut in_alt, &mut alt_screen),
            _ if in_alt => {}
            '\r' => screen.col = 0,
            '\n' => screen.newline(),
            '\x08' => screen.col = screen.col.saturating_sub(1),
            '\t' => screen.put('\t'),
            c if c.is_control() => {}
            c => screen.put(c),
        }
    }
    CleanedOutput {
        text: screen.finish(),
        alt_screen,
    }
}

/// Handles the escape sequence that starts right after an ESC at `chars[i - 1]`.
/// Returns the index of the first character after the sequence.
fn escape(
    chars: &[char],
    mut i: usize,
    screen: &mut Screen,
    in_alt: &mut bool,
    seen_alt: &mut bool,
) -> usize {
    let Some(&kind) = chars.get(i) else {
        return i;
    };
    i += 1;
    match kind {
        '[' => {
            let params_start = i;
            while i < chars.len() && ('\u{20}'..='\u{3f}').contains(&chars[i]) {
                i += 1;
            }
            let Some(&final_char) = chars.get(i) else {
                return i;
            };
            let params: String = chars[params_start..i].iter().collect();
            i += 1;
            csi(&params, final_char, screen, in_alt, seen_alt);
            i
        }
        // OSC, DCS, SOS, PM, APC end at BEL or ST (ESC \).
        ']' | 'P' | 'X' | '^' | '_' => {
            while i < chars.len() {
                match chars[i] {
                    '\x07' => return i + 1,
                    '\x1b' if chars.get(i + 1) == Some(&'\\') => return i + 2,
                    _ => i += 1,
                }
            }
            i
        }
        // Character-set selection and similar take one more character.
        '(' | ')' | '*' | '+' | '#' | '%' => (i + 1).min(chars.len()),
        _ => i,
    }
}

fn csi(
    params: &str,
    final_char: char,
    screen: &mut Screen,
    in_alt: &mut bool,
    seen_alt: &mut bool,
) {
    if let Some(private) = params.strip_prefix('?') {
        if matches!(final_char, 'h' | 'l')
            && private
                .split(';')
                .any(|p| matches!(p, "1049" | "1047" | "47"))
        {
            *in_alt = final_char == 'h';
            *seen_alt = true;
        }
        return;
    }
    if *in_alt {
        return;
    }
    let first = params
        .split(';')
        .next()
        .and_then(|p| p.parse::<usize>().ok());
    match final_char {
        'K' => screen.erase_in_line(first.unwrap_or(0)),
        'C' => screen.col += first.unwrap_or(1).max(1),
        'D' => screen.col = screen.col.saturating_sub(first.unwrap_or(1).max(1)),
        'G' => screen.col = first.unwrap_or(1).max(1) - 1,
        _ => {}
    }
}

/// Replaces a long run of identical lines with the line once and a count marker. A run collapses
/// only when the marker is shorter than the lines it replaces, so the change always saves
/// bytes.
pub fn collapse_repeats(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let mut run = 1;
        while lines.get(i + run) == Some(&line) {
            run += 1;
        }
        let extra = run - 1;
        let marker = format!("[previous line repeated {extra} more times]");
        if extra >= 2 && extra * (line.len() + 1) > marker.len() + 1 {
            out.push(line.to_string());
            out.push(marker);
        } else {
            out.extend(std::iter::repeat_n(line.to_string(), run));
        }
        i += run;
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_runs_of_one_line_collapse() {
        let text = "start\n".to_string() + &"downloading chunk\n".repeat(50) + "end";
        assert_eq!(
            collapse_repeats(&text),
            "start\ndownloading chunk\n[previous line repeated 49 more times]\nend"
        );
    }

    #[test]
    fn short_runs_stay_because_the_marker_would_cost_more() {
        let text = "y\n".repeat(10) + "done";
        assert_eq!(collapse_repeats(&text), text);
        let text = "same line\n".repeat(3) + "done";
        assert_eq!(collapse_repeats(&text), text);
    }

    #[test]
    fn collapse_breaks_at_a_different_line() {
        let long = "a long repeated line of output";
        let text = format!("{long}\n{long}\n{long}\n{long}\nother\n{long}\n{long}\n{long}\n{long}");
        assert_eq!(
            collapse_repeats(&text),
            format!(
                "{long}\n[previous line repeated 3 more times]\nother\n{long}\n[previous line repeated 3 more times]"
            )
        );
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(collapse_repeats(""), "");
    }

    fn text(raw: &str) -> String {
        clean(raw.as_bytes(), false).text
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(text("hello\nworld\n"), "hello\nworld");
    }

    #[test]
    fn strips_colour_sequences() {
        assert_eq!(text("\x1b[31mred\x1b[0m plain\n"), "red plain");
    }

    #[test]
    fn strips_osc_sequences_with_bel_and_st() {
        assert_eq!(text("a\x1b]0;title\x07b\x1b]7;file:///x\x1b\\c\n"), "abc");
    }

    #[test]
    fn crlf_becomes_lf() {
        assert_eq!(text("one\r\ntwo\r\n"), "one\ntwo");
    }

    #[test]
    fn carriage_return_keeps_last_rewrite() {
        assert_eq!(text("10%\r50%\r100%\ndone\n"), "100%\ndone");
    }

    #[test]
    fn shorter_rewrite_keeps_the_tail_of_the_longer_line() {
        assert_eq!(text("abcdef\rxy\n"), "xycdef");
    }

    #[test]
    fn erase_line_after_return_clears_the_old_text() {
        assert_eq!(text("working...\r\x1b[Kdone\n"), "done");
        assert_eq!(text("working...\r\x1b[2Kdone\n"), "done");
    }

    #[test]
    fn backspace_overwrites() {
        assert_eq!(text("ab\x08c\n"), "ac");
    }

    #[test]
    fn cursor_forward_pads_with_spaces() {
        assert_eq!(text("a\x1b[3Cb\n"), "a   b");
    }

    #[test]
    fn unicode_is_preserved() {
        assert_eq!(text("héllo ✓ 日本語 🎉\n"), "héllo ✓ 日本語 🎉");
    }

    #[test]
    fn invalid_utf8_becomes_replacement_character() {
        assert_eq!(clean(b"a\xffb\n", false).text, "a\u{fffd}b");
    }

    #[test]
    fn trailing_blank_lines_and_spaces_are_removed() {
        assert_eq!(text("a  \n\n\n"), "a");
        assert_eq!(text("a\n   \n"), "a");
    }

    #[test]
    fn leading_blank_lines_are_kept() {
        assert_eq!(text("\nfirst\n"), "\nfirst");
    }

    #[test]
    fn empty_output_is_empty() {
        assert_eq!(clean(b"", false), CleanedOutput::default());
        assert_eq!(text("\n\n"), "");
    }

    #[test]
    fn alternate_screen_text_is_dropped_and_flagged() {
        let raw = "before\n\x1b[?1049hFULL SCREEN\x1b[?1049lafter\n";
        let cleaned = clean(raw.as_bytes(), false);
        assert_eq!(cleaned.text, "before\nafter");
        assert!(cleaned.alt_screen);
    }

    #[test]
    fn unterminated_alternate_screen_drops_the_rest() {
        let raw = "before\n\x1b[?1049hdraw draw draw";
        let cleaned = clean(raw.as_bytes(), false);
        assert_eq!(cleaned.text, "before");
        assert!(cleaned.alt_screen);
    }

    #[test]
    fn ordinary_private_modes_do_not_count_as_alt_screen() {
        let cleaned = clean(b"\x1b[?2004hok\x1b[?2004l\n", false);
        assert_eq!(cleaned.text, "ok");
        assert!(!cleaned.alt_screen);
    }

    #[test]
    fn prompt_sp_marker_is_removed_after_output_without_newline() {
        let raw = format!("hello%{}\r \r", " ".repeat(38));
        assert_eq!(text(&raw), "hello");
    }

    #[test]
    fn prompt_sp_marker_is_removed_after_output_with_newline() {
        let raw = format!("hi\r\n%{}\r \r", " ".repeat(38));
        assert_eq!(text(&raw), "hi");
    }

    #[test]
    fn prompt_sp_marker_with_colour_wrapping_is_removed() {
        let raw = format!(
            "hello\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m{}\r \r",
            " ".repeat(38)
        );
        assert_eq!(text(&raw), "hello");
    }

    #[test]
    fn real_percent_sign_in_output_survives() {
        assert_eq!(text("progress 100%\n"), "progress 100%");
    }

    #[test]
    fn keep_ansi_leaves_escape_sequences() {
        let cleaned = clean(b"\x1b[31mred\x1b[0m\r\nnext\r\n", true);
        assert_eq!(cleaned.text, "\x1b[31mred\x1b[0m\nnext");
    }

    #[test]
    fn very_large_output_is_handled() {
        let line = "x".repeat(99) + "\n";
        let raw = line.repeat(50_000);
        let cleaned = clean(raw.as_bytes(), false);
        assert_eq!(cleaned.text.lines().count(), 50_000);
    }
}
