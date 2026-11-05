//! Reading and rewriting the user's config files without damaging them.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Context, Result};

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

/// Replaces the file through a temporary file in the same directory, so that a failure never
/// leaves half a file. A symlink is edited at its target, and the parent directory is created
/// when it is missing.
pub fn write(path: &Path, content: &str) -> Result<()> {
    let target = match fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => {
            return Err(error).with_context(|| format!("could not resolve {}", path.display()));
        }
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".into());
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
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_missing_file_reads_as_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(read(&dir.path().join("none")).expect("read"), "");
    }

    #[test]
    fn writing_creates_the_file_and_its_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a/b/config");
        write(&path, "x\n").expect("write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "x\n");
    }

    #[test]
    fn writing_keeps_permissions_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config");
        fs::write(&path, "old\n").expect("seed");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        write(&path, "new\n").expect("write");
        let mode = fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(fs::read_dir(dir.path()).expect("list").count(), 1);
    }

    #[test]
    fn a_symlink_is_edited_at_its_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("dotfiles-config");
        let link = dir.path().join("config");
        fs::write(&target, "old\n").expect("seed");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        write(&link, "new\n").expect("write");
        assert!(fs::symlink_metadata(&link).expect("meta").is_symlink());
        assert_eq!(fs::read_to_string(&target).expect("read"), "new\n");
    }

    #[test]
    fn a_file_that_is_not_utf8_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config");
        fs::write(&path, [0xff, 0xfe, b'\n']).expect("seed");
        assert!(read(&path).is_err());
    }
}
