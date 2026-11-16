//! `termscribe ssh`: connect to another machine and record what you run there.
//!
//! The relay lives on this machine, and so does the clipboard. The shell on the other machine
//! only has to tell the relay where each command starts and ends, and it can do that through the
//! connection itself, because the marks are ordinary escape sequences in its output. So this
//! starts `ssh` with a remote command that unpacks the hooks into a private temporary directory,
//! starts zsh with them (the same startup shim a local session uses) and removes the directory
//! when the shell ends. Nothing is installed on the other machine.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::ipc::SOCKET_ENV;
use crate::shell::{NONCE_ENV, bash_files, shell_word, shim_files};

/// What to do with the arguments given to `termscribe ssh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Open a shell on the machine and record it.
    Record {
        /// The arguments up to and including the destination.
        args: Vec<OsString>,
        /// The name for the machine in the transcript.
        host: String,
    },
    /// Run `ssh` as it is: a command was given, or there is no shell to record.
    Plain,
}

/// `ssh` options that take a value.
const OPTIONS_WITH_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";
/// `ssh` options that mean there is no interactive shell to record, or no connection at all.
const OPTIONS_WITHOUT_SHELL: &str = "NGQsTVWOfn";

/// Works out whether the arguments open an interactive shell, and on which machine.
pub fn plan(args: &[OsString]) -> Plan {
    let mut index = 0;
    while index < args.len() {
        let Some(arg) = args[index].to_str() else {
            return Plan::Plain;
        };
        index += 1;
        if arg == "--" {
            break;
        }
        let Some(flags) = arg.strip_prefix('-').filter(|flags| !flags.is_empty()) else {
            return destination(args, index - 1);
        };
        for (at, flag) in flags.char_indices() {
            if OPTIONS_WITHOUT_SHELL.contains(flag) {
                return Plan::Plain;
            }
            if OPTIONS_WITH_VALUE.contains(flag) {
                // The value is the rest of this word, or else the whole next word.
                if at + flag.len_utf8() == flags.len() {
                    index += 1;
                }
                break;
            }
        }
    }
    if index < args.len() {
        return destination(args, index);
    }
    Plan::Plain
}

/// `args[at]` is the destination. Anything after it is a command to run there.
fn destination(args: &[OsString], at: usize) -> Plan {
    if at + 1 != args.len() {
        return Plan::Plain;
    }
    let Some(host) = args[at]
        .to_str()
        .map(host_name)
        .filter(|host| !host.is_empty())
    else {
        return Plan::Plain;
    };
    Plan::Record {
        args: args.to_vec(),
        host,
    }
}

/// The part of `user@host` or `ssh://user@host:22` that names the machine.
fn host_name(destination: &str) -> String {
    let destination = destination.strip_prefix("ssh://").unwrap_or(destination);
    let destination = destination.rsplit('@').next().unwrap_or(destination);
    destination.trim_end_matches('/').to_string()
}

/// Runs `ssh` with the given arguments. Replaces this process, so `ssh` owns the terminal as
/// it would without termscribe. Returns only when that fails.
pub fn run(args: Vec<OsString>) -> i32 {
    let nonce = std::env::var(NONCE_ENV)
        .ok()
        .filter(|nonce| !nonce.is_empty());
    let in_session = std::env::var_os(SOCKET_ENV).is_some();
    let mut command = Command::new("ssh");
    match (plan(&args), nonce) {
        (Plan::Record { args, host }, Some(nonce)) if in_session => {
            command
                .arg("-t")
                .args(&args)
                .arg(remote_command(&host, &nonce));
        }
        (Plan::Record { .. }, _) => {
            eprintln!(
                "termscribe: this is not a termscribe session, so the connection is not recorded."
            );
            command.args(&args);
        }
        (Plan::Plain, _) => {
            command.args(&args);
        }
    }
    let error = command.exec();
    eprintln!("termscribe: could not run ssh: {error}");
    127
}

/// The command to run on the other machine, written so that any login shell can parse it.
///
/// It carries a base64 script that unpacks the hooks and starts zsh. The text is plain ASCII
/// without quotes, so it survives whichever shell the machine gives the user.
pub fn remote_command(host: &str, nonce: &str) -> String {
    format!(
        "sh -c '{LAUNCHER}' sh {}",
        base64(remote_script(host, nonce).as_bytes())
    )
}

/// Decodes the script and runs it. When decoding fails, the user still gets a shell.
const LAUNCHER: &str = r#"x=$(printf %s "$1" | base64 -d 2>/dev/null || printf %s "$1" | base64 -D 2>/dev/null); if [ -n "$x" ]; then eval "$x"; else echo "termscribe: could not start the recording on this machine." >&2; exec "${SHELL:-/bin/sh}"; fi"#;

/// The `sh` script that runs on the other machine.
fn remote_script(host: &str, nonce: &str) -> String {
    const END: &str = "TERMSCRIBE_END_OF_FILE";
    // The user's login shell is the one to record, when termscribe knows it. A shell with no
    // name falls back to whichever of the two is installed.
    let mut script = String::from(
        r#"pick=
case "${SHELL##*/}" in
  zsh|bash) pick=${SHELL##*/} ;;
  "") for s in zsh bash; do if command -v "$s" >/dev/null 2>&1; then pick=$s; break; fi; done ;;
esac
d=
if [ -n "$pick" ] && command -v "$pick" >/dev/null 2>&1; then
  d=$(mktemp -d "${TMPDIR:-/tmp}/termscribe.XXXXXX" 2>/dev/null)
fi
if [ -z "$d" ]; then
  echo "termscribe: it records zsh and bash, and the login shell on this machine is ${SHELL:-unknown}, so this session is one command in the transcript." >&2
  case "${SHELL##*/}" in bash|zsh|fish|ksh) exec "$SHELL" -l ;; esac
  exec "${SHELL:-/bin/sh}"
fi
trap 'rm -rf "$d"' EXIT
trap 'exit 129' HUP
trap 'exit 143' TERM
trap '' INT
"#,
    );
    for file in shim_files().into_iter().chain(bash_files()) {
        assert!(!file.content.lines().any(|line| line == END));
        script.push_str(&format!(
            "cat > \"$d/{}\" <<'{END}'\n{}{END}\n",
            file.name,
            ensure_newline(&file.content)
        ));
    }
    let host = shell_word(host);
    script.push_str(&format!(
        r#"case $pick in
  zsh) ZDOTDIR=$d TERMSCRIBE_USER_ZDOTDIR=${{ZDOTDIR-}} TERMSCRIBE_REMOTE={nonce} TERMSCRIBE_HOST={host} zsh -l ;;
  bash) TERMSCRIBE_REMOTE={nonce} TERMSCRIBE_HOST={host} bash --rcfile "$d/termscribe.bashrc" -i ;;
esac
exit $?
"#
    ));
    script
}

fn ensure_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

/// Standard base64 with padding. The remote side decodes it with `base64 -d`.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn host_of(words: &[&str]) -> Option<String> {
        match plan(&args(words)) {
            Plan::Record { host, .. } => Some(host),
            Plan::Plain => None,
        }
    }

    #[test]
    fn a_bare_destination_is_recorded() {
        assert_eq!(host_of(&["prod"]).as_deref(), Some("prod"));
        assert_eq!(
            host_of(&["me@prod.example.com"]).as_deref(),
            Some("prod.example.com")
        );
        assert_eq!(
            host_of(&["ssh://me@prod:2222/"]).as_deref(),
            Some("prod:2222")
        );
    }

    #[test]
    fn options_come_before_the_destination() {
        assert_eq!(host_of(&["-p", "2222", "prod"]).as_deref(), Some("prod"));
        assert_eq!(host_of(&["-p2222", "-A", "prod"]).as_deref(), Some("prod"));
        assert_eq!(host_of(&["-Ap", "2222", "prod"]).as_deref(), Some("prod"));
        assert_eq!(
            host_of(&["-o", "User=me", "-v", "prod"]).as_deref(),
            Some("prod")
        );
        assert_eq!(
            host_of(&["-i", "key", "--", "prod"]).as_deref(),
            Some("prod")
        );
    }

    #[test]
    fn a_remote_command_is_run_as_it_is() {
        assert_eq!(host_of(&["prod", "uptime"]), None);
        assert_eq!(host_of(&["-p", "22", "prod", "ls", "-l"]), None);
    }

    #[test]
    fn options_that_leave_no_shell_to_record_run_as_they_are() {
        for flags in [
            ["-N", "prod"],
            ["-T", "prod"],
            ["-V", ""],
            ["-W", "h:1"],
            ["-f", "prod"],
        ] {
            assert_eq!(host_of(&flags), None, "{flags:?}");
        }
        assert_eq!(host_of(&["-vN", "prod"]), None);
        assert_eq!(host_of(&["-O", "check", "prod"]), None);
    }

    #[test]
    fn no_destination_means_plain_ssh() {
        assert_eq!(host_of(&[]), None);
        assert_eq!(host_of(&["-v"]), None);
        assert_eq!(host_of(&["-p", "22"]), None);
    }

    #[test]
    fn base64_matches_the_standard_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_remote_command_is_plain_words_that_any_shell_reads_the_same() {
        let command = remote_command("prod", "abc123");
        let (launcher, payload) = command.split_once("' sh ").expect("launcher");
        assert!(launcher.starts_with("sh -c '"));
        assert!(
            !launcher[7..].contains('\''),
            "the launcher has a quote in it"
        );
        assert!(
            payload
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)),
            "{payload}"
        );
    }

    #[test]
    fn the_script_carries_the_secret_the_host_and_every_hook_file() {
        let script = remote_script("my host", "abc123");
        assert!(script.contains("TERMSCRIBE_REMOTE=abc123"));
        assert!(script.contains("TERMSCRIBE_HOST='my host'"));
        for name in [
            ".zshenv",
            ".zprofile",
            ".zshrc",
            "termscribe.zsh",
            "termscribe.bashrc",
        ] {
            assert!(script.contains(&format!("\"$d/{name}\"")), "{name}");
        }
    }

    #[test]
    fn the_script_parses_in_strict_shells() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("remote.sh");
        std::fs::write(&path, remote_script("prod", "abc123")).expect("write");
        // `sh` is dash on many servers. Shells that are not installed are skipped.
        for shell in ["sh", "dash", "bash", "ksh"] {
            if let Ok(status) = Command::new(shell).arg("-n").arg(&path).status() {
                assert!(status.success(), "{shell} cannot parse the script");
            }
        }
    }
}
