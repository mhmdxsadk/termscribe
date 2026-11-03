//! Adds the shell integration line to `~/.zshrc` and takes it out again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The line that starts a session. It must come first, before anything that could fail or print.
pub const INIT_LINE: &str = r#"eval "$(termscribe init zsh)""#;

/// Where the line sits in a `.zshrc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Missing,
    First,
    /// The line exists, but not on the first line. Holds the 1-based line number.
    Later(usize),
}

/// What `setup` did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Moved,
    Unchanged,
}

/// The user's `.zshrc`: `$ZDOTDIR/.zshrc` when `ZDOTDIR` is set, otherwise `$HOME/.zshrc`.
pub fn zshrc_path() -> Result<PathBuf> {
    let dir = ["ZDOTDIR", "HOME"]
        .iter()
        .find_map(|name| std::env::var_os(name).filter(|value| !value.is_empty()));
    match dir {
        Some(dir) => Ok(PathBuf::from(dir).join(".zshrc")),
        None => bail!("neither ZDOTDIR nor HOME is set, so ~/.zshrc cannot be located"),
    }
}

fn is_init_line(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("eval") && line.contains("termscribe init zsh")
}

pub fn inspect(content: &str) -> Presence {
    match content.lines().position(is_init_line) {
        None => Presence::Missing,
        Some(0) => Presence::First,
        Some(index) => Presence::Later(index + 1),
    }
}

/// Returns the content with exactly one init line, on the first line.
pub fn with_init_line(content: &str) -> (String, Change) {
    let matches = content.lines().filter(|line| is_init_line(line)).count();
    if matches == 1 && inspect(content) == Presence::First {
        return (content.to_string(), Change::Unchanged);
    }
    let (rest, _) = without_init_line(content);
    let change = if matches == 0 {
        Change::Added
    } else {
        Change::Moved
    };
    (format!("{INIT_LINE}\n{rest}"), change)
}

/// Returns the content without any init line, and how many lines were dropped.
pub fn without_init_line(content: &str) -> (String, usize) {
    let mut removed = 0;
    let kept: String = content
        .split_inclusive('\n')
        .filter(|line| {
            let hit = is_init_line(line);
            removed += usize::from(hit);
            !hit
        })
        .collect();
    (kept, removed)
}

/// Reads the file. A missing file reads as empty. A file that is not UTF-8 is an error, so that
/// a rewrite never damages it.
pub fn read(path: &Path) -> Result<String> {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .with_context(|| format!("{} is not valid UTF-8, left unchanged", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Puts the init line on the first line of the file, creating the file when it is missing.
pub fn setup(path: &Path) -> Result<Change> {
    let (content, change) = with_init_line(&read(path)?);
    if change != Change::Unchanged {
        write(path, &content)?;
    }
    Ok(change)
}

/// Removes the init line. Returns how many lines were removed.
pub fn remove(path: &Path) -> Result<usize> {
    let (content, removed) = without_init_line(&read(path)?);
    if removed > 0 {
        write(path, &content)?;
    }
    Ok(removed)
}

/// Replaces the file through a temporary file in the same directory, so that a failure never
/// leaves half a `.zshrc`. A symlinked `.zshrc` is edited at its target.
fn write(path: &Path, content: &str) -> Result<()> {
    let target = match fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => {
            return Err(error).with_context(|| format!("could not resolve {}", path.display()));
        }
    };
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".zshrc".into());
    let temp = target.with_file_name(format!(".{name}.termscribe-{}", std::process::id()));
    let result = (|| -> io::Result<()> {
        fs::write(&temp, content)?;
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&temp, meta.permissions())?;
        }
        fs::rename(&temp, &target)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| format!("could not write {}", target.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_line_in_its_usual_forms() {
        assert!(is_init_line(INIT_LINE));
        assert!(is_init_line("  eval \"$(termscribe init zsh)\"  "));
        assert!(is_init_line(
            "eval \"$(/opt/homebrew/bin/termscribe init zsh)\""
        ));
        assert!(!is_init_line("# eval \"$(termscribe init zsh)\""));
        assert!(!is_init_line("eval \"$(starship init zsh)\""));
        assert!(!is_init_line("alias ts=termscribe"));
    }

    #[test]
    fn inspect_reports_where_the_line_is() {
        assert_eq!(inspect(""), Presence::Missing);
        assert_eq!(inspect("export A=1\n"), Presence::Missing);
        assert_eq!(
            inspect(&format!("{INIT_LINE}\nexport A=1\n")),
            Presence::First
        );
        assert_eq!(
            inspect(&format!("export A=1\n\n{INIT_LINE}\n")),
            Presence::Later(3)
        );
    }

    #[test]
    fn setup_adds_the_line_at_the_top() {
        let (content, change) = with_init_line("export A=1\n");
        assert_eq!(content, format!("{INIT_LINE}\nexport A=1\n"));
        assert_eq!(change, Change::Added);
    }

    #[test]
    fn setup_fills_an_empty_file() {
        let (content, change) = with_init_line("");
        assert_eq!(content, format!("{INIT_LINE}\n"));
        assert_eq!(change, Change::Added);
    }

    #[test]
    fn setup_handles_a_last_line_without_a_newline() {
        let (content, _) = with_init_line("export A=1");
        assert_eq!(content, format!("{INIT_LINE}\nexport A=1"));
    }

    #[test]
    fn setup_leaves_a_correct_file_alone() {
        let original = format!("{INIT_LINE}\nexport A=1\n");
        let (content, change) = with_init_line(&original);
        assert_eq!(content, original);
        assert_eq!(change, Change::Unchanged);
    }

    #[test]
    fn setup_moves_a_line_from_further_down() {
        let (content, change) = with_init_line(&format!("export A=1\n{INIT_LINE}\nexport B=2\n"));
        assert_eq!(content, format!("{INIT_LINE}\nexport A=1\nexport B=2\n"));
        assert_eq!(change, Change::Moved);
    }

    #[test]
    fn setup_collapses_duplicates() {
        let (content, change) = with_init_line(&format!("{INIT_LINE}\nx\n{INIT_LINE}\n"));
        assert_eq!(content, format!("{INIT_LINE}\nx\n"));
        assert_eq!(change, Change::Moved);
    }

    #[test]
    fn setup_twice_changes_nothing_the_second_time() {
        let (once, _) = with_init_line("export A=1\n");
        let (twice, change) = with_init_line(&once);
        assert_eq!(once, twice);
        assert_eq!(change, Change::Unchanged);
    }

    #[test]
    fn remove_removes_only_the_line() {
        let (content, removed) = without_init_line(&format!("{INIT_LINE}\nexport A=1\n# note\n"));
        assert_eq!(content, "export A=1\n# note\n");
        assert_eq!(removed, 1);
    }

    #[test]
    fn remove_without_the_line_changes_nothing() {
        let (content, removed) = without_init_line("export A=1\n");
        assert_eq!(content, "export A=1\n");
        assert_eq!(removed, 0);
    }

    #[test]
    fn remove_undoes_setup() {
        let original = "export A=1\n\nalias l=ls\n";
        let (added, _) = with_init_line(original);
        assert_eq!(without_init_line(&added).0, original);
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let (content, removed) = without_init_line(&format!("a\r\n{INIT_LINE}\r\nb\r\n"));
        assert_eq!(content, "a\r\nb\r\n");
        assert_eq!(removed, 1);
    }

    #[test]
    fn setup_creates_a_missing_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".zshrc");
        assert_eq!(setup(&path).expect("setup"), Change::Added);
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            format!("{INIT_LINE}\n")
        );
    }

    #[test]
    fn remove_does_not_create_a_missing_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".zshrc");
        assert_eq!(remove(&path).expect("remove"), 0);
        assert!(!path.exists());
    }

    #[test]
    fn writing_keeps_permissions_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".zshrc");
        fs::write(&path, "export A=1\n").expect("seed");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        setup(&path).expect("setup");
        let mode = fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(fs::read_dir(dir.path()).expect("list").count(), 1);
    }

    #[test]
    fn a_symlinked_zshrc_is_edited_at_its_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("dotfiles-zshrc");
        let link = dir.path().join(".zshrc");
        fs::write(&target, "export A=1\n").expect("seed");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        setup(&link).expect("setup");
        assert!(fs::symlink_metadata(&link).expect("meta").is_symlink());
        assert_eq!(
            fs::read_to_string(&target).expect("read"),
            format!("{INIT_LINE}\nexport A=1\n")
        );
    }

    #[test]
    fn a_file_that_is_not_utf8_is_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".zshrc");
        fs::write(&path, [0xff, 0xfe, b'\n']).expect("seed");
        assert!(setup(&path).is_err());
        assert_eq!(fs::read(&path).expect("read"), vec![0xff, 0xfe, b'\n']);
    }
}
