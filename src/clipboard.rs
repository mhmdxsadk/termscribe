//! Clipboard access behind a trait, so the core stays platform-neutral.

use std::io::Write;
use std::process::{Command, Stdio};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("no clipboard backend is available on this platform")]
    Unsupported,
    #[error("could not run `{program}`: {source}")]
    Spawn {
        program: &'static str,
        source: std::io::Error,
    },
    #[error("could not write to `{program}`: {source}")]
    Write {
        program: &'static str,
        source: std::io::Error,
    },
    #[error("`{program}` exited with {status}")]
    Failed {
        program: &'static str,
        status: std::process::ExitStatus,
    },
}

/// Something that can put text on the system clipboard.
pub trait Clipboard: Send + Sync {
    fn copy(&self, text: &str) -> Result<(), ClipboardError>;
}

/// macOS clipboard through `pbcopy`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PbCopy;

impl Clipboard for PbCopy {
    fn copy(&self, text: &str) -> Result<(), ClipboardError> {
        const PROGRAM: &str = "pbcopy";
        let mut child = Command::new(PROGRAM)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|source| ClipboardError::Spawn {
                program: PROGRAM,
                source,
            })?;
        let write_result = child
            .stdin
            .take()
            .map(|mut stdin| stdin.write_all(text.as_bytes()))
            .unwrap_or(Ok(()));
        let status = child.wait().map_err(|source| ClipboardError::Spawn {
            program: PROGRAM,
            source,
        })?;
        write_result.map_err(|source| ClipboardError::Write {
            program: PROGRAM,
            source,
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(ClipboardError::Failed {
                program: PROGRAM,
                status,
            })
        }
    }
}

/// A clipboard for platforms without a backend yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unsupported;

impl Clipboard for Unsupported {
    fn copy(&self, _text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unsupported)
    }
}

/// Writes the text to a file instead of the clipboard. Tests and debugging use this through the
/// `TERMSCRIBE_CLIPBOARD_FILE` environment variable.
#[derive(Debug, Clone)]
pub struct FileClipboard {
    path: std::path::PathBuf,
}

impl FileClipboard {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Clipboard for FileClipboard {
    fn copy(&self, text: &str) -> Result<(), ClipboardError> {
        std::fs::write(&self.path, text).map_err(|source| ClipboardError::Write {
            program: "clipboard file",
            source,
        })
    }
}

/// The clipboard for the current platform.
pub fn system_clipboard() -> Box<dyn Clipboard> {
    if let Some(path) = std::env::var_os("TERMSCRIBE_CLIPBOARD_FILE").filter(|p| !p.is_empty()) {
        Box::new(FileClipboard::new(path))
    } else if cfg!(target_os = "macos") {
        Box::new(PbCopy)
    } else {
        Box::new(Unsupported)
    }
}
