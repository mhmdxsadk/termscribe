//! Points Ghostty at `termscribe relay`, so every new terminal starts the relay and the relay
//! starts the shell.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::dotfile;
use crate::shell::shell_word;

/// What `setup` did to the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Updated,
    Unchanged,
}

/// Every place Ghostty reads its config from, in the order it reads them.
pub fn config_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(xdg).join("ghostty"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(home);
        dirs.push(home.join(".config/ghostty"));
        dirs.push(home.join("Library/Application Support/com.mitchellh.ghostty"));
    }
    dirs.iter()
        .flat_map(|dir| [dir.join("config"), dir.join("config.ghostty")])
        .collect()
}

/// The config file to edit: the first one that exists, otherwise a new one in the XDG location.
pub fn config_to_edit() -> Result<PathBuf> {
    if let Some(existing) = config_paths().into_iter().find(|path| path.is_file()) {
        return Ok(existing);
    }
    config_paths()
        .into_iter()
        .next()
        .context("neither XDG_CONFIG_HOME nor HOME is set, so Ghostty's config cannot be located")
}

/// True when Ghostty seems to be installed or in use.
pub fn is_installed() -> bool {
    std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
        || Path::new("/Applications/Ghostty.app").exists()
        || config_paths().iter().any(|path| path.is_file())
}

/// The value of a `command = ...` line, with surrounding quotes removed.
fn command_value(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    if key.trim() != "command" {
        return None;
    }
    let value = value.trim();
    Some(
        value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value),
    )
}

/// True when a `command` value starts the termscribe relay, for example
/// `/opt/homebrew/bin/termscribe relay`.
pub fn is_relay_command(value: &str) -> bool {
    value.contains("termscribe") && value.split_whitespace().any(|word| word == "relay")
}

/// The command value of the last `command` line, which is the one Ghostty uses.
pub fn effective_command(content: &str) -> Option<&str> {
    content
        .lines()
        .filter_map(command_value)
        .next_back()
        .filter(|value| !value.is_empty())
}

/// The line for a binary at `program`.
pub fn relay_line(program: &Path) -> Result<String> {
    let program = program
        .to_str()
        .context("the path to termscribe is not valid UTF-8")?;
    Ok(format!("command = {} relay", shell_word(program)))
}

/// Returns the config with `line` as its command, or an error when something else is already
/// the command. Arguments the user added after `relay` are kept.
pub fn with_relay_command(content: &str, line: &str) -> Result<(String, Change)> {
    let wanted = command_value(line).unwrap_or_default();
    match effective_command(content) {
        None => {
            let mut out = content.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(line);
            out.push('\n');
            Ok((out, Change::Added))
        }
        Some(current) if is_relay_command(current) => {
            let tail = current.split_once(" relay").map_or("", |(_, tail)| tail);
            let line = format!("{line}{tail}");
            if current == command_value(&line).unwrap_or_default() {
                return Ok((content.to_string(), Change::Unchanged));
            }
            let mut replaced = false;
            let lines: Vec<String> = content
                .lines()
                .rev()
                .map(|existing| {
                    if !replaced && command_value(existing).is_some_and(is_relay_command) {
                        replaced = true;
                        line.clone()
                    } else {
                        existing.to_string()
                    }
                })
                .collect();
            let mut out = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
            out.push('\n');
            Ok((out, Change::Updated))
        }
        Some(current) => bail!(
            "Ghostty already runs `{current}` as its command, so setup left it alone. \
             Termscribe starts your shell itself and supports zsh only. If `{current}` is zsh, \
             replace that line with `command = {wanted} --shell {current}`"
        ),
    }
}

/// Returns the config without any relay `command` line, and how many lines were dropped.
pub fn without_relay_command(content: &str) -> (String, usize) {
    let mut removed = 0;
    let kept: String = content
        .split_inclusive('\n')
        .filter(|line| {
            let hit = command_value(line).is_some_and(is_relay_command);
            removed += usize::from(hit);
            !hit
        })
        .collect();
    (kept, removed)
}

/// Makes Ghostty run the relay. Creates the config when it is missing.
pub fn setup(path: &Path, line: &str) -> Result<Change> {
    let (content, change) = with_relay_command(&dotfile::read(path)?, line)?;
    if change != Change::Unchanged {
        dotfile::write(path, &content)?;
    }
    Ok(change)
}

/// Removes the relay command from the config. Returns how many lines were removed. A missing
/// file is not created.
pub fn remove(path: &Path) -> Result<usize> {
    let (content, removed) = without_relay_command(&dotfile::read(path)?);
    if removed > 0 {
        dotfile::write(path, &content)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const LINE: &str = "command = /opt/homebrew/bin/termscribe relay";

    #[test]
    fn reads_command_lines_and_ignores_the_rest() {
        assert_eq!(command_value("command = zsh"), Some("zsh"));
        assert_eq!(command_value("  command=\"zsh -l\"  "), Some("zsh -l"));
        assert_eq!(command_value("command ="), Some(""));
        assert_eq!(command_value("# command = zsh"), None);
        assert_eq!(command_value("initial-command = zsh"), None);
        assert_eq!(command_value("font-size = 14"), None);
    }

    #[test]
    fn recognises_the_relay_command() {
        assert!(is_relay_command("/opt/homebrew/bin/termscribe relay"));
        assert!(is_relay_command(
            "'/my dir/termscribe' relay --shell /bin/zsh"
        ));
        assert!(!is_relay_command("/bin/zsh"));
        assert!(!is_relay_command("termscribe --help"));
    }

    #[test]
    fn the_last_command_line_wins_and_an_empty_one_resets() {
        assert_eq!(effective_command("command = a\ncommand = b\n"), Some("b"));
        assert_eq!(effective_command("command = a\ncommand =\n"), None);
        assert_eq!(effective_command("font-size = 14\n"), None);
    }

    #[test]
    fn relay_line_quotes_awkward_paths() {
        let plain = relay_line(Path::new("/opt/homebrew/bin/termscribe")).expect("line");
        assert_eq!(plain, LINE);
        let spaced = relay_line(Path::new("/my dir/termscribe")).expect("line");
        assert_eq!(spaced, "command = '/my dir/termscribe' relay");
    }

    #[test]
    fn setup_appends_to_an_existing_config() {
        let (out, change) = with_relay_command("font-size = 14\n", LINE).expect("setup");
        assert_eq!(out, format!("font-size = 14\n{LINE}\n"));
        assert_eq!(change, Change::Added);
    }

    #[test]
    fn setup_adds_the_missing_newline_and_fills_an_empty_file() {
        let (out, _) = with_relay_command("font-size = 14", LINE).expect("setup");
        assert_eq!(out, format!("font-size = 14\n{LINE}\n"));
        let (out, _) = with_relay_command("", LINE).expect("setup");
        assert_eq!(out, format!("{LINE}\n"));
    }

    #[test]
    fn setup_is_a_no_op_when_already_done() {
        let original = format!("a = 1\n{LINE}\nb = 2\n");
        let (out, change) = with_relay_command(&original, LINE).expect("setup");
        assert_eq!(out, original);
        assert_eq!(change, Change::Unchanged);
    }

    #[test]
    fn setup_updates_a_relay_line_that_points_elsewhere_and_keeps_its_arguments() {
        let original = "a = 1\ncommand = /old/termscribe relay --shell /bin/zsh\nb = 2\n";
        let (out, change) = with_relay_command(original, LINE).expect("setup");
        assert_eq!(out, format!("a = 1\n{LINE} --shell /bin/zsh\nb = 2\n"),);
        assert_eq!(change, Change::Updated);
    }

    #[test]
    fn setup_refuses_to_replace_another_command() {
        let error = with_relay_command("command = /opt/homebrew/bin/fish\n", LINE).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("/opt/homebrew/bin/fish"), "{message}");
        assert!(message.contains("--shell"), "{message}");
    }

    #[test]
    fn setup_goes_after_a_reset_command() {
        let (out, change) = with_relay_command("command = fish\ncommand =\n", LINE).expect("setup");
        assert_eq!(out, format!("command = fish\ncommand =\n{LINE}\n"));
        assert_eq!(change, Change::Added);
    }

    #[test]
    fn removal_drops_only_relay_lines() {
        let (out, removed) =
            without_relay_command(&format!("a = 1\n{LINE}\ncommand = fish\n# {LINE}\n"));
        assert_eq!(out, format!("a = 1\ncommand = fish\n# {LINE}\n"));
        assert_eq!(removed, 1);
    }

    #[test]
    fn files_are_created_edited_and_cleaned() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("ghostty/config");
        assert_eq!(remove(&path).expect("remove"), 0);
        assert!(!path.exists());
        assert_eq!(setup(&path, LINE).expect("setup"), Change::Added);
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            format!("{LINE}\n")
        );
        assert_eq!(setup(&path, LINE).expect("setup"), Change::Unchanged);
        assert_eq!(remove(&path).expect("remove"), 1);
        assert_eq!(fs::read_to_string(&path).expect("read"), "");
    }
}
