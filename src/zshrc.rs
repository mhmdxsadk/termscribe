//! The `~/.zshrc` line that termscribe 0.1.0 asked users to add.
//!
//! Termscribe no longer needs it. `setup` and `remove` take it out, and `doctor` reports it.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::dotfile;

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

/// True for `eval "$(termscribe init zsh)"` and its usual variants, such as a full path.
fn is_init_line(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("eval") && line.contains("termscribe") && line.contains(" init zsh")
}

/// The 1-based line number of the first old init line, if the content has one.
pub fn find_init_line(content: &str) -> Option<usize> {
    content
        .lines()
        .position(is_init_line)
        .map(|index| index + 1)
}

/// Returns the content without any old init line, and how many lines were dropped.
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

/// Removes the old init line from the file. Returns how many lines were removed. A missing file
/// is not created.
pub fn remove(path: &Path) -> Result<usize> {
    let (content, removed) = without_init_line(&dotfile::read(path)?);
    if removed > 0 {
        dotfile::write(path, &content)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const LINE: &str = r#"eval "$(termscribe init zsh)""#;

    #[test]
    fn recognises_the_line_in_its_usual_forms() {
        assert!(is_init_line(LINE));
        assert!(is_init_line("  eval \"$(termscribe init zsh)\"  "));
        assert!(is_init_line(
            "eval \"$(/opt/homebrew/bin/termscribe init zsh)\""
        ));
        assert!(is_init_line(r#"eval "$('/my dir/termscribe' init zsh)""#));
        assert!(!is_init_line("# eval \"$(termscribe init zsh)\""));
        assert!(!is_init_line("eval \"$(starship init zsh)\""));
        assert!(!is_init_line("alias ts=termscribe"));
    }

    #[test]
    fn find_reports_the_line_number() {
        assert_eq!(find_init_line(""), None);
        assert_eq!(find_init_line("export A=1\n"), None);
        assert_eq!(find_init_line(&format!("{LINE}\nexport A=1\n")), Some(1));
        assert_eq!(find_init_line(&format!("a\n\n{LINE}\n")), Some(3));
    }

    #[test]
    fn removal_drops_only_the_line() {
        let (content, removed) = without_init_line(&format!("{LINE}\nexport A=1\n# note\n"));
        assert_eq!(content, "export A=1\n# note\n");
        assert_eq!(removed, 1);
    }

    #[test]
    fn removal_without_the_line_changes_nothing() {
        let (content, removed) = without_init_line("export A=1\n");
        assert_eq!(content, "export A=1\n");
        assert_eq!(removed, 0);
    }

    #[test]
    fn removal_drops_every_copy_and_keeps_crlf_endings() {
        let (content, removed) = without_init_line(&format!("a\r\n{LINE}\r\nb\r\n{LINE}"));
        assert_eq!(content, "a\r\nb\r\n");
        assert_eq!(removed, 2);
    }

    #[test]
    fn remove_rewrites_the_file_and_skips_a_missing_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".zshrc");
        assert_eq!(remove(&path).expect("remove"), 0);
        assert!(!path.exists());
        fs::write(&path, format!("{LINE}\nexport A=1\n")).expect("seed");
        assert_eq!(remove(&path).expect("remove"), 1);
        assert_eq!(fs::read_to_string(&path).expect("read"), "export A=1\n");
    }
}
