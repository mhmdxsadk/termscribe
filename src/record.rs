//! The stored description of one finished command.

/// One finished command and what it printed.
///
/// `output` holds the raw bytes the terminal received while the command ran. Cleaning happens
/// later, at copy time, so `--raw-ansi` can still see the escape sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    /// The command exactly as the user submitted it.
    pub command: String,
    /// Raw output bytes, possibly cut short by the per-command size limit.
    pub output: Vec<u8>,
    /// How many output bytes were dropped because of the size limit.
    pub truncated_bytes: usize,
    /// The shell's exit status, when it is known.
    pub exit_code: Option<i32>,
    /// The directory the command ran in, when the shell reported it. The relay already
    /// replaced the home directory with `~`.
    pub cwd: Option<String>,
}

impl CommandRecord {
    pub fn new(
        command: impl Into<String>,
        output: impl Into<Vec<u8>>,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            command: command.into(),
            output: output.into(),
            truncated_bytes: 0,
            exit_code,
            cwd: None,
        }
    }

    /// Sets the directory the command ran in.
    pub fn in_dir(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}
