//! End-to-end tests. Each one runs the real relay with a real zsh on a pseudo-terminal.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use termscribe::format::{ALT_SCREEN_NOTE, FormatOptions};
use termscribe::input::COPY_TRIGGER;
use termscribe::ipc::{Request, Response, request};
use termscribe::transcript::Selection;

const PROMPT: &str = "PROMPT> ";
const WAIT: Duration = Duration::from_secs(15);

struct Session {
    _home: tempfile::TempDir,
    clipboard_file: PathBuf,
    socket: PathBuf,
    output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

fn zsh_available() -> bool {
    Path::new("/bin/zsh").exists()
}

/// How the test starts the session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// Run `termscribe relay` directly.
    Relay,
    /// Run a plain interactive zsh and let `.zshrc` start the relay, as a real user would.
    FromZshrc,
}

impl Session {
    fn start() -> Self {
        Self::start_with(Launch::Relay)
    }

    fn start_with(launch: Launch) -> Self {
        Self::start_full(launch, &[])
    }

    fn start_full(launch: Launch, extra_env: &[(&str, &str)]) -> Self {
        let home = tempfile::tempdir().expect("temp dir");
        // Resolve symlinks so that HOME, the working directory and zsh's `$PWD` all agree.
        let home_path = home.path().canonicalize().expect("canonical temp dir");
        std::fs::write(
            home_path.join(".zshrc"),
            format!("PROMPT='{PROMPT}'\nRPROMPT=\neval \"$(termscribe init zsh)\"\n"),
        )
        .expect("write zshrc");
        let clipboard_file = home_path.join("clipboard.txt");

        let bin = PathBuf::from(env!("CARGO_BIN_EXE_termscribe"));
        let bin_dir = bin.parent().expect("bin dir").to_path_buf();
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut command = match launch {
            Launch::Relay => {
                let mut command = CommandBuilder::new(&bin);
                command.arg("relay");
                command
            }
            Launch::FromZshrc => {
                let mut command = CommandBuilder::new("/bin/zsh");
                command.arg("-i");
                command
            }
        };
        command.env("SHELL", "/bin/zsh");
        command.env("ZDOTDIR", &home_path);
        command.env("HOME", &home_path);
        command.env("PWD", &home_path);
        command.cwd(&home_path);
        command.env("PATH", path);
        command.env("TERM", "xterm-256color");
        command.env("TERMSCRIBE_CLIPBOARD_FILE", &clipboard_file);
        for name in [
            "TERMSCRIBE_SOCKET",
            "TERMSCRIBE_OWNER",
            "TERMSCRIBE_DISABLED",
            "TERM_PROGRAM",
            "GHOSTTY_RESOURCES_DIR",
        ] {
            command.env_remove(name);
        }
        for (name, value) in extra_env {
            command.env(name, value);
        }
        let child = pair.slave.spawn_command(command).expect("spawn relay");
        drop(pair.slave);

        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = pair.master.try_clone_reader().expect("reader");
        thread::spawn({
            let output = Arc::clone(&output);
            move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    output.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        });
        let writer = pair.master.take_writer().expect("writer");

        let mut session = Self {
            _home: home,
            clipboard_file,
            socket: PathBuf::new(),
            output,
            writer,
            child,
            _master: pair.master,
        };
        session.wait_for_text(PROMPT, 0);
        let reply = session.run("echo SOCK=$TERMSCRIBE_SOCKET");
        let at = reply.rfind("SOCK=/").expect("socket path in output") + "SOCK=".len();
        let socket: String = reply[at..]
            .chars()
            .take_while(|c| !c.is_control())
            .collect();
        session.socket = PathBuf::from(socket);
        session
    }

    fn len(&self) -> usize {
        self.output.lock().unwrap().len()
    }

    fn text_from(&self, start: usize) -> String {
        let output = self.output.lock().unwrap();
        String::from_utf8_lossy(&output[start.min(output.len())..]).into_owned()
    }

    /// Waits until `needle` appears after byte `start`. Returns the text up to and including it.
    fn wait_for_text(&self, needle: &str, start: usize) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.text_from(start);
            if let Some(at) = text.find(needle) {
                return text[..at + needle.len()].to_string();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}; saw {text:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    /// Types a command and waits for the next prompt. Returns what the terminal showed.
    fn run(&mut self, command: &str) -> String {
        let start = self.len();
        self.send(format!("{command}\n").as_bytes());
        // The first prompt after the echoed command marks completion.
        let echoed = self.wait_for_text("\n", start);
        let shown = self.wait_for_text(PROMPT, start + echoed.len());
        // Give the relay a moment to handle the end mark that precedes the prompt.
        thread::sleep(Duration::from_millis(30));
        shown
    }

    fn markdown(&self, selection: Selection) -> String {
        self.markdown_with(selection, true, true)
    }

    fn markdown_with(&self, selection: Selection, show_status: bool, show_cwd: bool) -> String {
        let reply = request(
            &self.socket,
            &Request::Render {
                selection,
                show_status,
                show_cwd,
                keep_ansi: false,
            },
        )
        .expect("render request");
        match reply {
            Response::Rendered { markdown, .. } => markdown,
            other => panic!("unexpected reply {other:?}"),
        }
    }

    fn clipboard(&self) -> Option<String> {
        std::fs::read_to_string(&self.clipboard_file).ok()
    }

    fn wait_for_clipboard(&self) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(text) = self.clipboard() {
                if !text.is_empty() {
                    return text;
                }
            }
            assert!(Instant::now() < deadline, "clipboard was never written");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.writer.write_all(b"exit\n");
        let _ = self.writer.flush();
        thread::sleep(Duration::from_millis(100));
        let _ = self.child.kill();
    }
}

macro_rules! require_zsh {
    () => {
        if !zsh_available() {
            eprintln!("skipping: /bin/zsh not found");
            return;
        }
    };
}

#[test]
fn records_commands_and_their_output() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo hello");
    s.run("printf 'a\\nb\\n'");
    s.run("true");
    let markdown = s.markdown(Selection::SinceBoundary);
    let expected = "\
```
~ $ echo SOCK=$TERMSCRIBE_SOCKET
SOCK=@PATH@
$ echo hello
hello
$ printf 'a\\nb\\n'
a
b
$ true
```
";
    let expected = expected.replace("@PATH@", &s.socket.display().to_string());
    assert_eq!(markdown, expected);
}

#[test]
fn output_without_a_trailing_newline_is_kept_whole() {
    require_zsh!();
    let mut s = Session::start();
    s.run("printf hello");
    assert!(
        s.markdown(Selection::Last(1))
            .ends_with("$ printf hello\nhello\n```\n"),
        "got {:?}",
        s.markdown(Selection::Last(1))
    );
}

#[test]
fn failed_and_interrupted_commands_show_their_status() {
    require_zsh!();
    let mut s = Session::start();
    s.run("ls /definitely/not/here");
    let start = s.len();
    s.send(b"sleep 30\n");
    s.wait_for_text("\n", start);
    thread::sleep(Duration::from_millis(300));
    s.send(&[0x03]);
    s.wait_for_text(PROMPT, start + 10);
    thread::sleep(Duration::from_millis(50));
    let markdown = s.markdown(Selection::Last(2));
    assert!(markdown.contains("$ ls /definitely/not/here"), "{markdown}");
    assert!(markdown.contains("No such file or directory"), "{markdown}");
    assert!(
        markdown.contains("[exit 1]") || markdown.contains("[exit 2]"),
        "{markdown}"
    );
    assert!(markdown.contains("$ sleep 30"), "{markdown}");
    assert!(markdown.contains("[exit 130]"), "{markdown}");
    let quiet = s.markdown_with(Selection::Last(2), false, true);
    assert!(!quiet.contains("[exit"), "{quiet}");
}

#[test]
fn the_working_directory_shows_when_it_changes() {
    require_zsh!();
    let mut s = Session::start();
    s.run("mkdir 'a;b%c d'");
    s.run("cd 'a;b%c d'");
    s.run("echo here");
    let markdown = s.markdown(Selection::Last(3));
    assert_eq!(
        markdown,
        "```\n~ $ mkdir 'a;b%c d'\n$ cd 'a;b%c d'\n~/a;b%c d $ echo here\nhere\n```\n"
    );
    let plain = s.markdown_with(Selection::Last(3), true, false);
    assert_eq!(
        plain,
        "```\n$ mkdir 'a;b%c d'\n$ cd 'a;b%c d'\n$ echo here\nhere\n```\n"
    );
}

#[test]
fn full_screen_output_is_replaced_by_a_note() {
    require_zsh!();
    let mut s = Session::start();
    s.run("printf 'before\\n\\033[?1049h\\x53ECRET\\033[?1049lafter\\n'");
    let markdown = s.markdown(Selection::Last(1));
    assert!(markdown.contains("before\nafter"), "{markdown}");
    assert!(markdown.contains(ALT_SCREEN_NOTE), "{markdown}");
    assert!(!markdown.contains("SECRET"), "{markdown}");
}

#[test]
fn progress_bars_keep_only_their_last_state() {
    require_zsh!();
    let mut s = Session::start();
    s.run("printf '10%%\\r50%%\\r100%%\\ndone\\n'");
    let markdown = s.markdown(Selection::Last(1));
    assert!(markdown.ends_with("100%\ndone\n```\n"), "{markdown}");
}

#[test]
fn commands_starting_with_a_space_are_not_recorded_when_histignorespace_is_set() {
    require_zsh!();
    let mut s = Session::start();
    s.run("setopt histignorespace");
    s.run(" echo top-secret");
    s.run("echo visible");
    let markdown = s.markdown(Selection::All);
    assert!(!markdown.contains("top-secret"), "{markdown}");
    assert!(markdown.contains("$ echo visible"), "{markdown}");
}

#[test]
fn the_termscribe_command_never_appears_in_a_transcript() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo one");
    let shown = s.run("termscribe --stdout");
    assert!(shown.contains("$ echo one"), "{shown}");
    s.run("echo two");
    let markdown = s.markdown(Selection::All);
    assert!(!markdown.contains("termscribe --stdout"), "{markdown}");
    assert!(markdown.contains("$ echo two"), "{markdown}");
}

#[test]
fn clear_starts_a_new_transcript_without_forgetting_history() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo old");
    s.run("termscribe --clear");
    s.run("echo new");
    let since = s.markdown(Selection::SinceBoundary);
    assert!(since.contains("$ echo new"), "{since}");
    assert!(!since.contains("echo old"), "{since}");
    assert!(s.markdown(Selection::All).contains("$ echo old"));
    assert!(s.markdown(Selection::Last(5)).contains("$ echo old"));
}

#[test]
fn clearing_the_screen_at_a_prompt_starts_a_new_transcript() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo old");
    let start = s.len();
    // Ghostty sends a form feed after Cmd+K clears the screen at a prompt. Ctrl+L sends the same.
    s.send(&[0x0c]);
    s.wait_for_text(PROMPT, start);
    thread::sleep(Duration::from_millis(50));
    s.run("echo new");
    let since = s.markdown(Selection::SinceBoundary);
    assert!(since.contains("$ echo new"), "{since}");
    assert!(!since.contains("echo old"), "{since}");
    assert!(s.markdown(Selection::All).contains("$ echo old"));
    assert!(s.markdown(Selection::Last(5)).contains("$ echo old"));
}

#[test]
fn the_clear_command_starts_a_new_transcript_and_is_not_part_of_it() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo old");
    s.run("clear");
    s.run("echo new");
    let since = s.markdown(Selection::SinceBoundary);
    assert_eq!(since, "```\n~ $ echo new\nnew\n```\n");
    assert!(s.markdown(Selection::All).contains("$ clear"));
}

#[test]
fn full_screen_programs_erasing_their_own_screen_do_not_move_the_boundary() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo kept");
    s.run("printf '\\033[?1049h\\033[2J\\033[?1049l'");
    let since = s.markdown(Selection::SinceBoundary);
    assert!(since.contains("$ echo kept"), "{since}");
}

#[test]
fn nested_shells_fold_into_the_outer_command() {
    require_zsh!();
    let mut s = Session::start();
    s.run("zsh -i");
    s.run("echo from inside");
    s.run("exit");
    let markdown = s.markdown(Selection::Last(1));
    assert!(markdown.starts_with("```\n~ $ zsh -i"), "{markdown}");
    assert!(markdown.contains("from inside"), "{markdown}");
}

#[test]
fn sessions_do_not_share_commands() {
    require_zsh!();
    let mut a = Session::start();
    let mut b = Session::start();
    a.run("echo only-in-a");
    b.run("echo only-in-b");
    let from_a = a.markdown(Selection::All);
    let from_b = b.markdown(Selection::All);
    assert!(from_a.contains("only-in-a") && !from_a.contains("only-in-b"));
    assert!(from_b.contains("only-in-b") && !from_b.contains("only-in-a"));
    assert_ne!(a.socket, b.socket);
}

#[test]
fn the_shortcut_at_a_prompt_copies_and_shows_a_message() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo copy-me");
    let start = s.len();
    s.send(COPY_TRIGGER);
    s.wait_for_text("Copied", start);
    let clipboard = s.wait_for_clipboard();
    assert!(clipboard.contains("$ echo copy-me"), "{clipboard}");
    // The shortcut leaves nothing in the shell's input line or history.
    let markdown = s.markdown(Selection::All);
    assert!(!markdown.contains("$ termscribe"), "{markdown}");
}

#[test]
fn the_shortcut_during_a_command_copies_quietly() {
    require_zsh!();
    let mut s = Session::start();
    s.run("echo before-sleep");
    let start = s.len();
    s.send(b"sleep 2\n");
    s.wait_for_text("\n", start);
    thread::sleep(Duration::from_millis(300));
    let before_shortcut = s.len();
    s.send(COPY_TRIGGER);
    let clipboard = s.wait_for_clipboard();
    assert!(clipboard.contains("$ echo before-sleep"), "{clipboard}");
    assert!(!clipboard.contains("sleep 2"), "{clipboard}");
    assert!(!s.text_from(before_shortcut).contains("Copied"));
    s.wait_for_text(PROMPT, start + 10);
}

#[test]
fn ghostty_integration_is_loaded_inside_the_relay_and_stays_out_of_transcripts() {
    require_zsh!();
    let resources = "/Applications/Ghostty.app/Contents/Resources/ghostty";
    if !Path::new(resources)
        .join("shell-integration/zsh/ghostty-integration")
        .exists()
    {
        eprintln!("skipping: Ghostty is not installed");
        return;
    }
    let mut s = Session::start_full(Launch::Relay, &[("GHOSTTY_RESOURCES_DIR", resources)]);
    let shown = s.run("echo ${+_ghostty_state}");
    assert!(
        shown.contains("]133;C\x07"),
        "Ghostty integration did not load: {shown:?}"
    );
    let shown = s.run("echo marks-pass-through");
    // Ghostty's own prompt marks still reach the terminal.
    assert!(shown.contains("\x1b]133;"), "no OSC 133 in {shown:?}");
    let markdown = s.markdown(Selection::Last(1));
    assert_eq!(
        markdown,
        "```\n~ $ echo marks-pass-through\nmarks-pass-through\n```\n"
    );
}

#[test]
fn zshrc_line_starts_the_relay_and_hooks() {
    require_zsh!();
    let mut s = Session::start_with(Launch::FromZshrc);
    s.run("echo via-rc");
    let markdown = s.markdown(Selection::Last(1));
    assert_eq!(markdown, "```\n~ $ echo via-rc\nvia-rc\n```\n");
    // The `ts` shortcut function exists because nothing else uses that name.
    let shown = s.run("type ts");
    assert!(
        shown.contains("function") || shown.contains("ts is"),
        "{shown}"
    );
}

#[test]
fn format_options_are_available_to_tests() {
    // Keeps the public API used by the tests honest.
    let _ = FormatOptions::default();
}
