//! Choosing which command records belong in a transcript.

use serde::{Deserialize, Serialize};

use crate::record::CommandRecord;

/// Which part of a session's history to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Selection {
    /// Every command that finished after the latest boundary.
    SinceBoundary,
    /// The most recent `n` commands, ignoring the boundary.
    Last(usize),
    /// Every retained command, ignoring the boundary.
    All,
}

/// An ordered selection of command records from one session, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    pub records: Vec<CommandRecord>,
}

impl Transcript {
    pub fn new(records: Vec<CommandRecord>) -> Self {
        Self { records }
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }
}
