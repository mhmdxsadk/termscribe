//! The `doctor` health check.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dotfile;
use crate::ghostty;
use crate::ipc::{self, Request, Response, SOCKET_ENV};
use crate::zshrc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub status: Status,
    pub name: &'static str,
    pub detail: String,
}

impl Check {
    fn new(status: Status, name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            name,
            detail: detail.into(),
        }
    }
}

/// Runs every check against the current environment.
pub fn run() -> Vec<Check> {
    let mut checks = vec![check_binary(), check_zsh(), check_launch()];
    checks.extend(check_old_zshrc_line());
    checks.push(check_session());
    checks.push(check_clipboard());
    checks.extend(check_ghostty());
    checks
}

/// Formats the checks, one per line, followed by a summary line.
pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        let label = match check.status {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        let _ = writeln!(out, "{label:<5} {:<10} {}", check.name, check.detail);
    }
    let failed = count(checks, Status::Fail);
    let warned = count(checks, Status::Warn);
    let summary = match (failed, warned) {
        (0, 0) => "Everything looks good.".to_string(),
        (0, w) => format!("No problems, {w} {}.", plural(w, "warning")),
        (f, 0) => format!("{f} {} found.", plural(f, "problem")),
        (f, w) => format!(
            "{f} {} and {w} {}.",
            plural(f, "problem"),
            plural(w, "warning")
        ),
    };
    let _ = writeln!(out, "\n{summary}");
    out
}

/// True when a check failed. Warnings do not count.
pub fn has_failure(checks: &[Check]) -> bool {
    count(checks, Status::Fail) > 0
}

fn count(checks: &[Check], status: Status) -> usize {
    checks.iter().filter(|c| c.status == status).count()
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}

/// Finds an executable file called `name` in `PATH`.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn check_binary() -> Check {
    const NAME: &str = "binary";
    let version = env!("CARGO_PKG_VERSION");
    let Some(on_path) = find_in_path("termscribe") else {
        return Check::new(
            Status::Fail,
            NAME,
            "not on PATH. The .zshrc line would start a plain zsh. Add the install directory to PATH",
        );
    };
    let same = std::env::current_exe()
        .ok()
        .zip(on_path.canonicalize().ok())
        .is_some_and(|(running, found)| running.canonicalize().ok().as_ref() == Some(&found));
    if same {
        Check::new(
            Status::Ok,
            NAME,
            format!("{} (version {version})", on_path.display()),
        )
    } else {
        Check::new(
            Status::Warn,
            NAME,
            format!(
                "PATH runs {}, which is not this binary (version {version})",
                on_path.display()
            ),
        )
    }
}

fn check_zsh() -> Check {
    const NAME: &str = "zsh";
    let Some(zsh) = find_in_path("zsh") else {
        return Check::new(Status::Fail, NAME, "zsh not found on PATH");
    };
    let version = Command::new(&zsh)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| {
            let words: Vec<&str> = text.split_whitespace().take(2).collect();
            (words.len() == 2).then(|| words.join(" "))
        })
        .unwrap_or_else(|| "unknown version".into());
    let login = std::env::var("SHELL").unwrap_or_default();
    if login.ends_with("/zsh") {
        Check::new(Status::Ok, NAME, format!("{version} at {}", zsh.display()))
    } else {
        Check::new(
            Status::Warn,
            NAME,
            format!(
                "{version} at {}, but your login shell is {}. termscribe supports zsh only",
                zsh.display(),
                if login.is_empty() { "unset" } else { &login }
            ),
        )
    }
}

/// The command Ghostty is configured to run, and the file it comes from. Ghostty reads its
/// config files in order and the last `command` line wins.
fn ghostty_command() -> Option<(PathBuf, String)> {
    let mut found = None;
    for path in ghostty::config_paths() {
        if let Ok(content) = dotfile::read(&path) {
            if let Some(command) = ghostty::effective_command(&content) {
                found = Some((path, command.to_string()));
            }
        }
    }
    found
}

/// True when Ghostty is in use but its config does not start `termscribe relay`, so new tabs
/// are not captured.
pub fn ghostty_needs_setup() -> bool {
    needs_setup(ghostty::is_installed(), ghostty_command())
}

fn needs_setup(ghostty_present: bool, command: Option<(PathBuf, String)>) -> bool {
    ghostty_present && !command.is_some_and(|(_, command)| ghostty::is_relay_command(&command))
}

fn check_launch() -> Check {
    let in_session = std::env::var_os(SOCKET_ENV).is_some_and(|v| !v.is_empty());
    launch_check(in_session, ghostty::is_installed(), ghostty_command())
}

/// Checks that something starts the relay when a terminal opens. A shell that already runs in a
/// session proves it. Otherwise the answer is in Ghostty's config.
fn launch_check(
    in_session: bool,
    ghostty_present: bool,
    command: Option<(PathBuf, String)>,
) -> Check {
    const NAME: &str = "launch";
    if in_session {
        return Check::new(Status::Ok, NAME, "this terminal started termscribe relay");
    }
    match command {
        Some((path, command)) if ghostty::is_relay_command(&command) => Check::new(
            Status::Ok,
            NAME,
            format!("Ghostty runs `{command}` ({})", path.display()),
        ),
        Some((path, command)) => Check::new(
            Status::Fail,
            NAME,
            format!(
                "Ghostty runs `{command}` ({}), not termscribe. Run `termscribe setup`",
                path.display()
            ),
        ),
        None if ghostty_present => Check::new(
            Status::Fail,
            NAME,
            "Ghostty does not start termscribe. Run `termscribe setup`",
        ),
        None => Check::new(
            Status::Warn,
            NAME,
            "cannot tell if your terminal runs `termscribe relay`. Check its settings",
        ),
    }
}

fn check_old_zshrc_line() -> Option<Check> {
    let path = zshrc::zshrc_path().ok()?;
    let content = dotfile::read(&path).ok()?;
    old_zshrc_line_check(&path, &content)
}

/// Reports the line that termscribe 0.1.0 added to `.zshrc`, which is no longer needed.
fn old_zshrc_line_check(path: &Path, content: &str) -> Option<Check> {
    let line = zshrc::find_init_line(content)?;
    Some(Check::new(
        Status::Warn,
        "zshrc",
        format!(
            "line {line} of {} is from termscribe 0.1.0 and is not needed any more. \
             `termscribe setup` removes it",
            path.display()
        ),
    ))
}

fn check_session() -> Check {
    const NAME: &str = "session";
    if std::env::var_os("TERMSCRIBE_DISABLED").is_some_and(|v| !v.is_empty()) {
        return Check::new(
            Status::Warn,
            NAME,
            "TERMSCRIBE_DISABLED is set, so new shells skip the relay",
        );
    }
    let Some(socket) = std::env::var_os(SOCKET_ENV).filter(|v| !v.is_empty()) else {
        return Check::new(
            Status::Warn,
            NAME,
            "this shell is not in a termscribe session. Start a new terminal session to test it",
        );
    };
    session_check(ipc::request(Path::new(&socket), &Request::Ping))
}

/// Turns the relay's answer to a ping into a check. A relay that started under an older
/// termscribe does not know the ping request and answers with an error, so that case gets its own
/// message instead of a raw one.
fn session_check(reply: Result<Response, ipc::IpcError>) -> Check {
    const NAME: &str = "session";
    match reply {
        Ok(Response::Pong) => Check::new(Status::Ok, NAME, "the relay answers"),
        Ok(Response::Error(message)) if message.contains("unknown variant") => Check::new(
            Status::Warn,
            NAME,
            "this terminal session started under an older termscribe. \
             Start a new terminal session to use the current version",
        ),
        Ok(other) => Check::new(
            Status::Fail,
            NAME,
            format!("unexpected reply from the relay: {other:?}"),
        ),
        Err(error) => Check::new(Status::Fail, NAME, error.to_string()),
    }
}

fn check_clipboard() -> Check {
    const NAME: &str = "clipboard";
    if let Some(file) = std::env::var_os("TERMSCRIBE_CLIPBOARD_FILE").filter(|v| !v.is_empty()) {
        return Check::new(
            Status::Warn,
            NAME,
            format!(
                "TERMSCRIBE_CLIPBOARD_FILE writes copies to {}",
                Path::new(&file).display()
            ),
        );
    }
    if !cfg!(target_os = "macos") {
        return Check::new(Status::Fail, NAME, "only macOS is supported so far");
    }
    match find_in_path("pbcopy") {
        Some(path) => Check::new(Status::Ok, NAME, path.display().to_string()),
        None => Check::new(Status::Fail, NAME, "pbcopy not found on PATH"),
    }
}

/// Ghostty checks. Returns nothing outside Ghostty.
fn check_ghostty() -> Vec<Check> {
    let in_ghostty = std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
        || std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "ghostty");
    if !in_ghostty {
        return Vec::new();
    }
    let mut checks = Vec::new();

    if let Some(dir) = std::env::var_os("GHOSTTY_RESOURCES_DIR") {
        let integration = Path::new(&dir).join("shell-integration/zsh/ghostty-integration");
        checks.push(if integration.is_file() {
            Check::new(Status::Ok, "ghostty", "zsh integration found")
        } else {
            Check::new(
                Status::Warn,
                "ghostty",
                format!(
                    "{} is missing, so Cmd+K cannot move the transcript boundary",
                    integration.display()
                ),
            )
        });
    }

    let trigger = ghostty::copy_action();
    let configs = ghostty::config_paths();
    checks.push(match configs.iter().find_map(|p| std::fs::read_to_string(p).ok().filter(|c| has_keybind(c, &trigger))) {
        Some(_) => Check::new(Status::Ok, "keybind", format!("a Ghostty keybind sends {trigger}")),
        None => Check::new(
            Status::Warn,
            "keybind",
            format!("no Ghostty keybind sends {trigger}, so there is no copy shortcut. Run `termscribe setup`"),
        ),
    });
    checks
}

fn has_keybind(config: &str, trigger: &str) -> bool {
    config.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line.starts_with("keybind") && line.contains(trigger)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: Status) -> Check {
        Check::new(status, "x", "detail")
    }

    #[test]
    fn setup_is_needed_only_when_ghostty_does_not_start_the_relay() {
        let relay = || {
            Some((
                PathBuf::from("c"),
                "/opt/homebrew/bin/termscribe relay".into(),
            ))
        };
        let other = || Some((PathBuf::from("c"), "/bin/fish".into()));
        assert!(needs_setup(true, None));
        assert!(needs_setup(true, other()));
        assert!(!needs_setup(true, relay()));
        assert!(!needs_setup(false, None));
    }

    #[test]
    fn render_aligns_columns_and_summarises() {
        let out = render(&[
            Check::new(Status::Ok, "binary", "fine"),
            Check::new(Status::Fail, "zshrc", "missing"),
        ]);
        assert_eq!(
            out,
            "ok    binary     fine\nFAIL  zshrc      missing\n\n1 problem found.\n"
        );
    }

    #[test]
    fn summary_counts_failures_and_warnings() {
        let all_good = render(&[check(Status::Ok)]);
        assert!(all_good.ends_with("Everything looks good.\n"));
        let warned = render(&[check(Status::Warn), check(Status::Warn)]);
        assert!(warned.ends_with("No problems, 2 warnings.\n"));
        let both = render(&[
            check(Status::Fail),
            check(Status::Warn),
            check(Status::Fail),
        ]);
        assert!(both.ends_with("2 problems and 1 warning.\n"));
    }

    #[test]
    fn only_failures_fail_the_run() {
        assert!(!has_failure(&[check(Status::Ok), check(Status::Warn)]));
        assert!(has_failure(&[check(Status::Ok), check(Status::Fail)]));
    }

    #[test]
    fn launch_check_trusts_a_running_session() {
        let check = launch_check(true, false, None);
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn launch_check_reads_ghostty_s_command() {
        let path = PathBuf::from("/h/ghostty/config");
        let relay = launch_check(
            false,
            true,
            Some((path.clone(), "/opt/homebrew/bin/termscribe relay".into())),
        );
        assert_eq!(relay.status, Status::Ok);
        let other = launch_check(false, true, Some((path, "/bin/fish".into())));
        assert_eq!(other.status, Status::Fail);
        assert!(
            other.detail.contains("termscribe setup"),
            "{}",
            other.detail
        );
        let unset = launch_check(false, true, None);
        assert_eq!(unset.status, Status::Fail);
        assert!(
            unset.detail.contains("termscribe setup"),
            "{}",
            unset.detail
        );
        let elsewhere = launch_check(false, false, None);
        assert_eq!(elsewhere.status, Status::Warn);
    }

    #[test]
    fn the_old_zshrc_line_is_reported_only_when_present() {
        let path = Path::new("/h/.zshrc");
        assert!(old_zshrc_line_check(path, "export A=1\n").is_none());
        let check =
            old_zshrc_line_check(path, "a\neval \"$(termscribe init zsh)\"\n").expect("reported");
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("line 2"), "{}", check.detail);
        assert!(
            check.detail.contains("termscribe setup"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn session_check_explains_a_relay_from_an_older_version() {
        let old_relay = Response::Error(
            "bad request: unknown variant `Ping`, expected `Render` or `Clear`".into(),
        );
        let check = session_check(Ok(old_relay));
        assert_eq!(check.status, Status::Warn);
        assert!(
            check.detail.contains("older termscribe"),
            "{}",
            check.detail
        );
        assert_eq!(session_check(Ok(Response::Pong)).status, Status::Ok);
        let other = session_check(Ok(Response::Error("boom".into())));
        assert_eq!(other.status, Status::Fail);
        assert_eq!(session_check(Ok(Response::Cleared)).status, Status::Fail);
    }

    #[test]
    fn keybind_search_ignores_comments_and_other_keys() {
        let trigger = ghostty::copy_action();
        assert!(has_keybind("keybind = super+shift+c=csi:9000~\n", &trigger));
        assert!(has_keybind(
            "font-size = 14\n  keybind=a=csi:9000~",
            &trigger
        ));
        assert!(!has_keybind("# keybind = a=csi:9000~\n", &trigger));
        assert!(!has_keybind("keybind = a=csi:1~\n", &trigger));
        assert!(!has_keybind("title = csi:9000~\n", &trigger));
    }

    #[test]
    fn find_in_path_skips_non_executables() {
        let dir = tempfile::tempdir().expect("temp dir");
        let plain = dir.path().join("tool");
        std::fs::write(&plain, "").expect("write");
        assert!(!is_executable(&plain));
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable(&plain));
        assert!(!is_executable(dir.path()));
    }
}
