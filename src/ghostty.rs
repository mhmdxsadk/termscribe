//! Points Ghostty at `termscribe relay`, so every new terminal starts the relay and the relay
//! starts the shell.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::dotfile;
use crate::input::COPY_TRIGGER;
use crate::shell::shell_word;

/// The key that `setup` binds to the copy shortcut.
const KEYBIND_TRIGGER: &str = "super+alt+c";
/// The comment that opens the block `setup` appends to the config. `remove` takes out only
/// lines inside that block, so a keybind the user wrote themselves stays.
const MARKER: &str = "# termscribe";
/// The comment termscribe 0.2.1 wrote above its keybind. `remove` still understands it.
const OLD_KEYBIND_MARKER: &str = "# termscribe: copy shortcut";

/// What `setup` did to the `command` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Updated,
    Unchanged,
}

/// What `setup` did about the copy shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindChange {
    Added,
    /// Some key already sends the copy sequence.
    Unchanged,
    /// The key is bound to another action, which `setup` left alone. Holds that action.
    Taken(String),
}

/// What `setup` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupResult {
    pub command: Change,
    pub keybind: KeybindChange,
}

/// How many lines `remove` took out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveResult {
    pub command: usize,
    pub keybind: usize,
}

/// The keybind action that sends the copy sequence, for example `csi:9000~`.
pub fn copy_action() -> String {
    let tail = COPY_TRIGGER.strip_prefix(b"\x1b[").unwrap_or(COPY_TRIGGER);
    format!("csi:{}", String::from_utf8_lossy(tail))
}

/// The line that binds the copy shortcut.
pub fn keybind_line() -> String {
    format!("keybind = {KEYBIND_TRIGGER}={}", copy_action())
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

/// Checks the config's command. An existing relay line is rewritten in place and reported as
/// `Updated`. When no command is set, the content comes back unchanged and `Added` tells the
/// caller to append `line`. Another command is an error. Arguments the user added after `relay`
/// are kept.
pub fn update_relay_command(content: &str, line: &str) -> Result<(String, Change)> {
    let wanted = command_value(line).unwrap_or_default();
    match effective_command(content) {
        None => Ok((content.to_string(), Change::Added)),
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

/// Splits a `keybind = <trigger>=<action>` line. Comments and other keys give `None`.
fn keybind_parts(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    if key.trim() != "keybind" {
        return None;
    }
    let (trigger, action) = value.trim().split_once('=')?;
    Some((trigger.trim(), action.trim()))
}

/// A trigger without Ghostty's prefixes such as `global:` and `unconsumed:`.
fn bare_trigger(mut trigger: &str) -> &str {
    while let Some(rest) = ["all:", "global:", "unconsumed:", "performable:"]
        .iter()
        .find_map(|prefix| trigger.strip_prefix(prefix))
    {
        trigger = rest;
    }
    trigger
}

/// Decides what to do about the copy shortcut. Nothing changes when some key already sends the
/// copy sequence, or when `super+alt+c` already does something else.
pub fn keybind_change(content: &str) -> KeybindChange {
    let action = copy_action();
    let mut taken = None;
    for line in content.lines() {
        let Some((trigger, bound)) = keybind_parts(line) else {
            continue;
        };
        if bound == action {
            return KeybindChange::Unchanged;
        }
        if bare_trigger(trigger) == KEYBIND_TRIGGER {
            taken = Some(bound.to_string());
        }
    }
    match taken {
        Some(bound) => KeybindChange::Taken(bound),
        None => KeybindChange::Added,
    }
}

/// Appends the termscribe block: an empty line, the `# termscribe` comment, then `lines`. The
/// empty line is left out when the config is empty or already ends with one.
pub fn with_block(content: &str, lines: &[String]) -> String {
    let mut out = content.to_string();
    if lines.is_empty() {
        return out;
    }
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        let last_line_is_empty = out.lines().next_back().is_some_and(|l| l.trim().is_empty());
        if !last_line_is_empty {
            out.push('\n');
        }
    }
    out.push_str(MARKER);
    out.push('\n');
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Returns the config without the lines `setup` added, and how many of each were dropped.
///
/// A `# termscribe` comment takes the relay command and copy keybind lines that follow it, and
/// the empty line `setup` put above it. The comment from 0.2.1 takes its keybind. A relay
/// command outside any block goes too. A keybind the user wrote themselves stays.
pub fn without_termscribe(content: &str) -> (String, RemoveResult) {
    let action = copy_action();
    let is_command = |line: &str| command_value(line).is_some_and(is_relay_command);
    let is_keybind = |line: &str| keybind_parts(line).is_some_and(|(_, bound)| bound == action);
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = RemoveResult {
        command: 0,
        keybind: 0,
    };
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let comment = line.trim();
        if comment == MARKER || comment == OLD_KEYBIND_MARKER {
            let (mut commands, mut keybinds) = (0, 0);
            let mut end = index + 1;
            while let Some(next) = lines.get(end) {
                if is_command(next) {
                    commands += 1;
                } else if is_keybind(next) {
                    keybinds += 1;
                } else {
                    break;
                }
                end += 1;
            }
            if commands + keybinds > 0 {
                removed.command += commands;
                removed.keybind += keybinds;
                if comment == MARKER && kept.last().is_some_and(|prev| prev.trim().is_empty()) {
                    kept.pop();
                }
                index = end;
                continue;
            }
        }
        if is_command(line) {
            removed.command += 1;
        } else {
            kept.push(line);
        }
        index += 1;
    }
    (kept.concat(), removed)
}

/// Makes Ghostty run the relay and binds the copy shortcut. Whatever it adds goes in one block
/// at the end of the config. Creates the config when it is missing.
pub fn setup(path: &Path, line: &str) -> Result<SetupResult> {
    let original = dotfile::read(path)?;
    let (content, command) = update_relay_command(&original, line)?;
    let keybind = keybind_change(&content);
    let mut block = Vec::new();
    if command == Change::Added {
        block.push(line.to_string());
    }
    if keybind == KeybindChange::Added {
        block.push(keybind_line());
    }
    let content = with_block(&content, &block);
    if content != original {
        dotfile::write(path, &content)?;
    }
    Ok(SetupResult { command, keybind })
}

/// Removes the relay command and the keybind that `setup` added. A keybind the user wrote
/// themselves stays. A missing file is not created.
pub fn remove(path: &Path) -> Result<RemoveResult> {
    let original = dotfile::read(path)?;
    let (content, removed) = without_termscribe(&original);
    if removed.command + removed.keybind > 0 {
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
    fn a_missing_command_is_reported_as_added_and_left_to_the_caller() {
        let (out, change) = update_relay_command("font-size = 14\n", LINE).expect("update");
        assert_eq!(out, "font-size = 14\n");
        assert_eq!(change, Change::Added);
        let (_, change) =
            update_relay_command("command = fish\ncommand =\n", LINE).expect("update");
        assert_eq!(change, Change::Added);
    }

    #[test]
    fn the_block_goes_after_an_empty_line_at_the_end() {
        let lines = [LINE.to_string(), KEYBIND.to_string()];
        let out = with_block("font-size = 14\n", &lines);
        assert_eq!(
            out,
            format!("font-size = 14\n\n{MARKER}\n{LINE}\n{KEYBIND}\n")
        );
    }

    #[test]
    fn the_block_adds_a_missing_newline_and_skips_a_second_empty_line() {
        let lines = [LINE.to_string()];
        let out = with_block("font-size = 14", &lines);
        assert_eq!(out, format!("font-size = 14\n\n{MARKER}\n{LINE}\n"));
        let out = with_block("font-size = 14\n\n", &lines);
        assert_eq!(out, format!("font-size = 14\n\n{MARKER}\n{LINE}\n"));
        assert_eq!(with_block("", &lines), format!("{MARKER}\n{LINE}\n"));
        // A file that is only an empty line, or whose last line is only spaces, already ends
        // with an empty line.
        assert_eq!(with_block("\n", &lines), format!("\n{MARKER}\n{LINE}\n"));
        assert_eq!(
            with_block("a = 1\n  \n", &lines),
            format!("a = 1\n  \n{MARKER}\n{LINE}\n")
        );
        assert_eq!(
            with_block("a = 1\n  ", &lines),
            format!("a = 1\n  \n{MARKER}\n{LINE}\n")
        );
        assert_eq!(with_block("a = 1\n", &[]), "a = 1\n");
    }

    #[test]
    fn update_is_a_no_op_when_already_done() {
        let original = format!("a = 1\n{LINE}\nb = 2\n");
        let (out, change) = update_relay_command(&original, LINE).expect("update");
        assert_eq!(out, original);
        assert_eq!(change, Change::Unchanged);
    }

    #[test]
    fn update_rewrites_a_relay_line_that_points_elsewhere_and_keeps_its_arguments() {
        let original = "a = 1\ncommand = /old/termscribe relay --shell /bin/zsh\nb = 2\n";
        let (out, change) = update_relay_command(original, LINE).expect("update");
        assert_eq!(out, format!("a = 1\n{LINE} --shell /bin/zsh\nb = 2\n"),);
        assert_eq!(change, Change::Updated);
    }

    #[test]
    fn setup_refuses_to_replace_another_command() {
        let error = update_relay_command("command = /opt/homebrew/bin/fish\n", LINE).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("/opt/homebrew/bin/fish"), "{message}");
        assert!(message.contains("--shell"), "{message}");
    }

    const KEYBIND: &str = "keybind = super+alt+c=csi:9000~";
    const OLD_MARKER: &str = "# termscribe: copy shortcut";

    #[test]
    fn the_copy_action_and_keybind_match_the_trigger() {
        assert_eq!(copy_action(), "csi:9000~");
        assert_eq!(keybind_line(), KEYBIND);
    }

    #[test]
    fn reads_keybind_lines_and_ignores_the_rest() {
        assert_eq!(
            keybind_parts("keybind = super+alt+c=csi:9000~"),
            Some(("super+alt+c", "csi:9000~"))
        );
        assert_eq!(keybind_parts("  keybind=a=text:x"), Some(("a", "text:x")));
        assert_eq!(keybind_parts("# keybind = a=csi:9000~"), None);
        assert_eq!(keybind_parts("title = a=b"), None);
        assert_eq!(keybind_parts("keybind = clear"), None);
        assert_eq!(bare_trigger("global:unconsumed:super+alt+c"), "super+alt+c");
    }

    #[test]
    fn keybind_is_added_when_nothing_sends_the_sequence() {
        assert_eq!(keybind_change("font-size = 14"), KeybindChange::Added);
        assert_eq!(keybind_change(""), KeybindChange::Added);
    }

    #[test]
    fn keybind_is_left_alone_when_any_key_sends_the_sequence() {
        for original in [
            format!("{KEYBIND}\n"),
            "keybind = super+shift+c=csi:9000~\n".to_string(),
            "keybind = global:f5=csi:9000~\n".to_string(),
        ] {
            assert_eq!(keybind_change(&original), KeybindChange::Unchanged);
        }
    }

    #[test]
    fn keybind_is_skipped_when_the_key_does_something_else() {
        assert_eq!(
            keybind_change("keybind = super+alt+c=text:hello\n"),
            KeybindChange::Taken("text:hello".into())
        );
    }

    #[test]
    fn a_commented_out_keybind_does_not_count() {
        assert_eq!(
            keybind_change("# keybind = super+alt+c=csi:9000~\n"),
            KeybindChange::Added
        );
    }

    #[test]
    fn removal_takes_the_block_and_the_empty_line_above_it() {
        let ours = format!("a = 1\n\n{MARKER}\n{LINE}\n{KEYBIND}\n");
        let (out, removed) = without_termscribe(&ours);
        assert_eq!(out, "a = 1\n");
        assert_eq!((removed.command, removed.keybind), (1, 1));
    }

    #[test]
    fn removal_takes_a_block_that_holds_one_line() {
        let (out, removed) = without_termscribe(&format!("a = 1\n\n{MARKER}\n{LINE}\n"));
        assert_eq!(out, "a = 1\n");
        assert_eq!((removed.command, removed.keybind), (1, 0));
        let (out, removed) = without_termscribe(&format!("a = 1\n\n{MARKER}\n{KEYBIND}\n"));
        assert_eq!(out, "a = 1\n");
        assert_eq!((removed.command, removed.keybind), (0, 1));
    }

    #[test]
    fn removal_keeps_lines_that_follow_the_block() {
        let (out, _) = without_termscribe(&format!("a = 1\n\n{MARKER}\n{LINE}\nb = 2\n"));
        assert_eq!(out, "a = 1\nb = 2\n");
    }

    #[test]
    fn removal_still_takes_the_comment_from_0_2_1() {
        let old = format!("a = 1\n{LINE}\n{OLD_MARKER}\n{KEYBIND}\nb = 2\n");
        let (out, removed) = without_termscribe(&old);
        assert_eq!(out, "a = 1\nb = 2\n");
        assert_eq!((removed.command, removed.keybind), (1, 1));
    }

    #[test]
    fn removal_drops_only_relay_lines_and_keeps_other_comments() {
        let (out, removed) =
            without_termscribe(&format!("a = 1\n{LINE}\ncommand = fish\n# {LINE}\n"));
        assert_eq!(
            out,
            "a = 1\ncommand = fish\n# {LINE}\n".replace("{LINE}", LINE)
        );
        assert_eq!((removed.command, removed.keybind), (1, 0));
        let stranded = format!("{MARKER}\nfont-size = 14\n");
        assert_eq!(without_termscribe(&stranded).0, stranded);
    }

    #[test]
    fn removal_leaves_a_keybind_the_user_wrote() {
        let theirs = format!("a = 1\n{KEYBIND}\n");
        let (out, removed) = without_termscribe(&theirs);
        assert_eq!(out, theirs);
        assert_eq!((removed.command, removed.keybind), (0, 0));
    }

    #[test]
    fn files_are_created_edited_and_cleaned() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("ghostty/config");
        let nothing = remove(&path).expect("remove");
        assert_eq!((nothing.command, nothing.keybind), (0, 0));
        assert!(!path.exists());

        let done = setup(&path, LINE).expect("setup");
        assert_eq!(done.command, Change::Added);
        assert_eq!(done.keybind, KeybindChange::Added);
        let written = fs::read_to_string(&path).expect("read");
        assert_eq!(written, format!("{MARKER}\n{LINE}\n{KEYBIND}\n"));

        let again = setup(&path, LINE).expect("setup");
        assert_eq!(again.command, Change::Unchanged);
        assert_eq!(again.keybind, KeybindChange::Unchanged);
        assert_eq!(fs::read_to_string(&path).expect("read"), written);

        let undone = remove(&path).expect("remove");
        assert_eq!((undone.command, undone.keybind), (1, 1));
        assert_eq!(fs::read_to_string(&path).expect("read"), "");
    }

    #[test]
    fn setup_and_remove_restore_an_existing_config() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config");
        let original = "font-size = 14\ntheme = dark\n";
        fs::write(&path, original).expect("seed");
        setup(&path, LINE).expect("setup");
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            format!("{original}\n{MARKER}\n{LINE}\n{KEYBIND}\n")
        );
        remove(&path).expect("remove");
        assert_eq!(fs::read_to_string(&path).expect("read"), original);
    }

    #[test]
    fn setup_adds_only_the_keybind_when_the_command_is_already_there() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config");
        fs::write(&path, format!("{LINE}\nfont-size = 14\n")).expect("seed");
        let done = setup(&path, LINE).expect("setup");
        assert_eq!(done.command, Change::Unchanged);
        assert_eq!(done.keybind, KeybindChange::Added);
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            format!("{LINE}\nfont-size = 14\n\n{MARKER}\n{KEYBIND}\n")
        );
    }

    #[test]
    fn a_keybind_the_user_wrote_survives_setup_and_remove() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config");
        fs::write(&path, format!("{KEYBIND}\n")).expect("seed");
        let done = setup(&path, LINE).expect("setup");
        assert_eq!(done.keybind, KeybindChange::Unchanged);
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            format!("{KEYBIND}\n\n{MARKER}\n{LINE}\n")
        );
        let undone = remove(&path).expect("remove");
        assert_eq!((undone.command, undone.keybind), (1, 0));
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            format!("{KEYBIND}\n")
        );
    }
}
