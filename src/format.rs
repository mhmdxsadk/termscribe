//! Markdown formatting for transcripts.
//!
//! The transcript is one fenced block. Each command starts with a `$` prompt line, and its output
//! follows with no decoration. The format favours few tokens for the agents that read it.

use crate::clean::{clean, collapse_repeats};
use crate::record::CommandRecord;
use crate::transcript::Transcript;

/// Shown inside the output block when a command drew a full-screen interface.
pub const ALT_SCREEN_NOTE: &str = "[full-screen program output omitted]";

/// Options that change how a transcript is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions {
    /// Add an `[exit N]` line after commands that failed.
    pub show_status: bool,
    /// Show the working directory on the first command and whenever it changes.
    pub show_cwd: bool,
    /// Keep ANSI escape sequences in the output.
    pub keep_ansi: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            show_status: true,
            show_cwd: true,
            keep_ansi: false,
        }
    }
}

/// Renders a transcript as Markdown. An empty transcript renders as an empty string.
pub fn format_markdown(transcript: &Transcript, options: &FormatOptions) -> String {
    if transcript.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut previous_cwd: Option<&str> = None;
    for record in &transcript.records {
        let prompt = match record.cwd.as_deref() {
            Some(cwd) if options.show_cwd && previous_cwd != Some(cwd) => format!("{cwd} $"),
            _ => "$".to_string(),
        };
        if record.cwd.is_some() {
            previous_cwd = record.cwd.as_deref();
        }
        push_command(&mut lines, &prompt, &record.command);
        push_output(&mut lines, record, options);
    }

    let body = lines.join("\n");
    let fence = "`".repeat(longest_backtick_run(&body).max(2) + 1);
    format!("{fence}\n{body}\n{fence}\n")
}

/// The command after its prompt. Extra lines of a multi-line command start with `>`.
fn push_command(lines: &mut Vec<String>, prompt: &str, command: &str) {
    let command = normalize_newlines(command);
    let mut parts = command.split('\n');
    lines.push(after_prompt(prompt, parts.next().unwrap_or("")));
    lines.extend(parts.map(|part| after_prompt(">", part)));
}

fn after_prompt(prompt: &str, text: &str) -> String {
    if text.is_empty() {
        prompt.to_string()
    } else {
        format!("{prompt} {text}")
    }
}

fn push_output(lines: &mut Vec<String>, record: &CommandRecord, options: &FormatOptions) {
    let cleaned = clean(&record.output, options.keep_ansi);
    if !cleaned.text.is_empty() {
        lines.push(collapse_repeats(&cleaned.text));
    }
    if cleaned.alt_screen {
        lines.push(ALT_SCREEN_NOTE.to_string());
    }
    if record.truncated_bytes > 0 {
        lines.push(format!("[… {} bytes truncated …]", record.truncated_bytes));
    }
    if options.show_status {
        if let Some(code) = record.exit_code.filter(|code| *code != 0) {
            lines.push(format!("[exit {code}]"));
        }
    }
}

/// Pasted multi-line commands arrive with carriage returns between lines.
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Replaces a leading home directory with `~`.
pub fn abbreviate_home(path: &str, home: Option<&str>) -> String {
    let home = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty());
    match home {
        Some(home) if path == home => "~".to_string(),
        Some(home) => match path.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_string(),
        },
        None => path.to_string(),
    }
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for c in text.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(records: Vec<CommandRecord>) -> String {
        format_markdown(&Transcript::new(records), &FormatOptions::default())
    }

    fn record(command: &str, output: &str) -> CommandRecord {
        CommandRecord::new(command, output, Some(0))
    }

    #[test]
    fn one_command() {
        assert_eq!(
            render(vec![record("echo hi", "hi\n")]),
            "```\n$ echo hi\nhi\n```\n"
        );
    }

    #[test]
    fn two_commands_share_one_block() {
        let markdown = render(vec![
            record(
                "git status",
                "On branch main\nnothing to commit, working tree clean\n",
            ),
            record("pwd", "/tmp/project\n"),
        ]);
        let expected = "\
```
$ git status
On branch main
nothing to commit, working tree clean
$ pwd
/tmp/project
```
";
        assert_eq!(markdown, expected);
    }

    #[test]
    fn command_without_output_has_only_its_line() {
        assert_eq!(
            render(vec![record("cd /tmp", ""), record("true", "\n\n")]),
            "```\n$ cd /tmp\n$ true\n```\n"
        );
    }

    #[test]
    fn empty_transcript_renders_nothing() {
        assert_eq!(render(vec![]), "");
    }

    #[test]
    fn blank_lines_inside_output_are_kept() {
        assert_eq!(
            render(vec![record("ls", "a\n\nb\n")]),
            "```\n$ ls\na\n\nb\n```\n"
        );
    }

    #[test]
    fn unicode_is_kept() {
        assert_eq!(
            render(vec![record("echo ✓", "✓ héllo 日本語\n")]),
            "```\n$ echo ✓\n✓ héllo 日本語\n```\n"
        );
    }

    #[test]
    fn ansi_is_stripped_by_default() {
        assert_eq!(
            render(vec![record("ls", "\x1b[31mred\x1b[0m\n")]),
            "```\n$ ls\nred\n```\n"
        );
    }

    #[test]
    fn ansi_is_kept_on_request() {
        let options = FormatOptions {
            keep_ansi: true,
            ..FormatOptions::default()
        };
        let transcript = Transcript::new(vec![record("ls", "\x1b[31mred\x1b[0m\n")]);
        assert_eq!(
            format_markdown(&transcript, &options),
            "```\n$ ls\n\x1b[31mred\x1b[0m\n```\n"
        );
    }

    #[test]
    fn output_with_triple_backticks_gets_a_longer_fence() {
        assert_eq!(
            render(vec![record(
                "cat README.md",
                "```rust\nfn main() {}\n```\n"
            )]),
            "````\n$ cat README.md\n```rust\nfn main() {}\n```\n````\n"
        );
    }

    #[test]
    fn backticks_in_a_command_also_lengthen_the_fence() {
        assert_eq!(
            render(vec![record("echo ````", "x\n")]),
            "`````\n$ echo ````\nx\n`````\n"
        );
    }

    #[test]
    fn multiline_command_continues_with_angle_brackets() {
        assert_eq!(
            render(vec![record(
                "for i in 1 2; do\n  echo $i\n\ndone",
                "1\n2\n"
            )]),
            "```\n$ for i in 1 2; do\n>   echo $i\n>\n> done\n1\n2\n```\n"
        );
    }

    #[test]
    fn pasted_commands_with_carriage_returns_become_plain_newlines() {
        let markdown = render(vec![record("head -3 f\r\nps -p 1\rls", "x\n")]);
        assert_eq!(markdown, "```\n$ head -3 f\n> ps -p 1\n> ls\nx\n```\n");
    }

    #[test]
    fn failed_commands_show_their_status_by_default() {
        let transcript = Transcript::new(vec![
            CommandRecord::new("ls /nope", "ls: /nope: No such file\n", Some(1)),
            CommandRecord::new("true", "", Some(0)),
            CommandRecord::new("sleep 9", "", Some(130)),
            CommandRecord::new("lost", "", None),
        ]);
        let expected = "\
```
$ ls /nope
ls: /nope: No such file
[exit 1]
$ true
$ sleep 9
[exit 130]
$ lost
```
";
        assert_eq!(
            format_markdown(&transcript, &FormatOptions::default()),
            expected
        );
    }

    #[test]
    fn status_can_be_hidden() {
        let options = FormatOptions {
            show_status: false,
            ..FormatOptions::default()
        };
        let transcript = Transcript::new(vec![CommandRecord::new("false", "", Some(1))]);
        assert_eq!(
            format_markdown(&transcript, &options),
            "```\n$ false\n```\n"
        );
    }

    #[test]
    fn directory_shows_on_the_first_command_and_when_it_changes() {
        let transcript = Transcript::new(vec![
            record("git status", "clean\n").in_dir("~/proj"),
            record("cd src", "").in_dir("~/proj"),
            record("ls", "main.rs\n").in_dir("~/proj/src"),
            record("pwd", "x\n").in_dir("~/proj/src"),
            record("cd ..", "").in_dir("~/proj/src"),
            record("ls", "src\n").in_dir("~/proj"),
        ]);
        let expected = "\
```
~/proj $ git status
clean
$ cd src
~/proj/src $ ls
main.rs
$ pwd
x
$ cd ..
~/proj $ ls
src
```
";
        assert_eq!(
            format_markdown(&transcript, &FormatOptions::default()),
            expected
        );
    }

    #[test]
    fn unknown_directory_does_not_reset_the_previous_one() {
        let transcript = Transcript::new(vec![
            record("a", "").in_dir("~/p"),
            record("b", ""),
            record("c", "").in_dir("~/p"),
        ]);
        assert_eq!(
            format_markdown(&transcript, &FormatOptions::default()),
            "```\n~/p $ a\n$ b\n$ c\n```\n"
        );
    }

    #[test]
    fn directory_can_be_hidden() {
        let options = FormatOptions {
            show_cwd: false,
            ..FormatOptions::default()
        };
        let transcript = Transcript::new(vec![record("ls", "x\n").in_dir("~/proj")]);
        assert_eq!(
            format_markdown(&transcript, &options),
            "```\n$ ls\nx\n```\n"
        );
    }

    #[test]
    fn interrupted_command_keeps_partial_output() {
        let interrupted =
            CommandRecord::new("ping localhost", "64 bytes from ::1\n^C\n", Some(130));
        assert_eq!(
            render(vec![interrupted]),
            "```\n$ ping localhost\n64 bytes from ::1\n^C\n[exit 130]\n```\n"
        );
    }

    #[test]
    fn alternate_screen_commands_get_a_note() {
        assert_eq!(
            render(vec![record("vim x", "\x1b[?1049hdrawing\x1b[?1049l")]),
            format!("```\n$ vim x\n{ALT_SCREEN_NOTE}\n```\n")
        );
    }

    #[test]
    fn truncation_is_reported() {
        let mut big = record("yes", "y\ny\n");
        big.truncated_bytes = 1234;
        assert_eq!(
            render(vec![big]),
            "```\n$ yes\ny\ny\n[… 1234 bytes truncated …]\n```\n"
        );
    }

    #[test]
    fn long_runs_of_repeated_lines_collapse() {
        let output = "tick tock tick tock\n".repeat(100);
        assert_eq!(
            render(vec![record("loop", &output)]),
            "```\n$ loop\ntick tock tick tock\n[previous line repeated 99 more times]\n```\n"
        );
    }

    #[test]
    fn very_large_output_formats_without_losing_lines() {
        let output: String = (0..200_000).map(|i| format!("line {i}\n")).collect();
        let markdown = render(vec![record("big", &output)]);
        assert_eq!(markdown.lines().count(), 200_000 + 3);
    }

    #[test]
    fn home_directory_becomes_a_tilde() {
        let home = Some("/Users/alex");
        assert_eq!(abbreviate_home("/Users/alex", home), "~");
        assert_eq!(abbreviate_home("/Users/alex/proj", home), "~/proj");
        assert_eq!(
            abbreviate_home("/Users/alex2/proj", home),
            "/Users/alex2/proj"
        );
        assert_eq!(abbreviate_home("/tmp", home), "/tmp");
        assert_eq!(
            abbreviate_home("/Users/alex/x", Some("/Users/alex/")),
            "~/x"
        );
        assert_eq!(abbreviate_home("/x", Some("/")), "/x");
        assert_eq!(abbreviate_home("/x", None), "/x");
    }
}
