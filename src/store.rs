//! The in-memory history of one session.

use std::collections::VecDeque;

use crate::record::CommandRecord;
use crate::transcript::{Selection, Transcript};

/// Size limits for a session's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Finished commands to keep. The oldest are dropped first.
    pub max_commands: usize,
    /// Output bytes to keep per command. Later bytes are counted and dropped.
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_commands: 500,
            max_output_bytes: 1024 * 1024,
        }
    }
}

/// What the shell is doing right now, as far as the marks tell us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellState {
    /// No mark has arrived yet.
    Starting,
    /// The shell is at a prompt.
    Idle,
    /// A command is running.
    Running,
}

#[derive(Debug)]
struct Pending {
    /// `None` when the command must not be recorded (a leading space with `HIST_IGNORE_SPACE`).
    record: Option<CommandRecord>,
}

/// History for one session: finished commands, the command that is running now, and the
/// boundary.
#[derive(Debug)]
pub struct Store {
    limits: Limits,
    records: VecDeque<CommandRecord>,
    /// How many commands have finished since the session began, including dropped ones.
    finished_total: u64,
    /// Commands with an index below this are before the boundary.
    boundary: u64,
    pending: Option<Pending>,
    /// Move the boundary when the running command ends, because it cleared the screen.
    boundary_on_finish: bool,
    state: ShellState,
}

impl Store {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            records: VecDeque::new(),
            finished_total: 0,
            boundary: 0,
            pending: None,
            boundary_on_finish: false,
            state: ShellState::Starting,
        }
    }

    pub fn state(&self) -> ShellState {
        self.state
    }

    /// A command started. `command` is `None` when it must stay out of the history. `cwd` is the
    /// directory it started in, when the shell reported one.
    /// An earlier command that never reported its end is dropped.
    pub fn begin(&mut self, command: Option<String>, cwd: Option<String>) {
        self.pending = Some(Pending {
            record: command.map(|command| {
                let mut record = CommandRecord::new(command, Vec::new(), None);
                record.cwd = cwd;
                record
            }),
        });
        self.boundary_on_finish = false;
        self.state = ShellState::Running;
    }

    /// Output arrived while the current command ran.
    pub fn push_output(&mut self, bytes: &[u8]) {
        let Some(record) = self.pending.as_mut().and_then(|p| p.record.as_mut()) else {
            return;
        };
        let room = self
            .limits
            .max_output_bytes
            .saturating_sub(record.output.len());
        let keep = room.min(bytes.len());
        record.output.extend_from_slice(&bytes[..keep]);
        record.truncated_bytes += bytes.len() - keep;
    }

    /// The running command ended and the shell is back at a prompt.
    pub fn finish(&mut self, exit_code: Option<i32>) {
        self.state = ShellState::Idle;
        if let Some(Pending {
            record: Some(mut record),
        }) = self.pending.take()
        {
            record.exit_code = exit_code;
            self.records.push_back(record);
            self.finished_total += 1;
            while self.records.len() > self.limits.max_commands {
                self.records.pop_front();
            }
        }
        if std::mem::take(&mut self.boundary_on_finish) {
            self.set_boundary();
        }
    }

    /// The running command erased the screen. When it ends, the boundary moves past it, so the
    /// command that cleared the screen is not part of the next transcript.
    pub fn clear_on_finish(&mut self) {
        self.boundary_on_finish = true;
    }

    /// Forgets the running command. The `termscribe` command itself calls this, so it never
    /// appears in a transcript.
    pub fn discard_pending(&mut self) {
        self.pending = None;
    }

    /// Starts a new transcript. Later default transcripts hold only commands that finish from
    /// now on.
    pub fn set_boundary(&mut self) {
        self.boundary = self.finished_total;
    }

    pub fn select(&self, selection: Selection) -> Transcript {
        let oldest_index = self.finished_total - self.records.len() as u64;
        let skip = match selection {
            Selection::All => 0,
            Selection::SinceBoundary => self.boundary.saturating_sub(oldest_index) as usize,
            Selection::Last(n) => self.records.len().saturating_sub(n),
        };
        Transcript::new(self.records.iter().skip(skip).cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(store: &mut Store, command: &str, output: &str, code: i32) {
        store.begin(Some(command.to_string()), None);
        store.push_output(output.as_bytes());
        store.finish(Some(code));
    }

    fn commands(transcript: &Transcript) -> Vec<&str> {
        transcript
            .records
            .iter()
            .map(|r| r.command.as_str())
            .collect()
    }

    #[test]
    fn records_finished_commands_in_order() {
        let mut store = Store::new(Limits::default());
        run(&mut store, "a", "1", 0);
        run(&mut store, "b", "2", 1);
        let all = store.select(Selection::All);
        assert_eq!(commands(&all), ["a", "b"]);
        assert_eq!(all.records[1].exit_code, Some(1));
        assert_eq!(all.records[1].output, b"2");
    }

    #[test]
    fn last_n_ignores_the_boundary() {
        let mut store = Store::new(Limits::default());
        run(&mut store, "a", "", 0);
        run(&mut store, "b", "", 0);
        store.set_boundary();
        run(&mut store, "c", "", 0);
        assert_eq!(commands(&store.select(Selection::Last(2))), ["b", "c"]);
        assert_eq!(
            commands(&store.select(Selection::Last(10))),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn default_selection_starts_at_the_boundary() {
        let mut store = Store::new(Limits::default());
        run(&mut store, "a", "", 0);
        assert_eq!(commands(&store.select(Selection::SinceBoundary)), ["a"]);
        store.set_boundary();
        assert!(store.select(Selection::SinceBoundary).is_empty());
        run(&mut store, "b", "", 0);
        assert_eq!(commands(&store.select(Selection::SinceBoundary)), ["b"]);
        assert_eq!(commands(&store.select(Selection::All)), ["a", "b"]);
    }

    #[test]
    fn copying_twice_gives_the_same_result() {
        let mut store = Store::new(Limits::default());
        run(&mut store, "a", "", 0);
        assert_eq!(
            store.select(Selection::SinceBoundary),
            store.select(Selection::SinceBoundary)
        );
    }

    #[test]
    fn unfinished_command_is_excluded() {
        let mut store = Store::new(Limits::default());
        run(&mut store, "a", "", 0);
        store.begin(Some("sleep 100".into()), None);
        store.push_output(b"partial");
        assert_eq!(store.state(), ShellState::Running);
        assert_eq!(commands(&store.select(Selection::All)), ["a"]);
    }

    #[test]
    fn discarding_the_pending_command_keeps_it_out_and_keeps_the_boundary_right() {
        let mut store = Store::new(Limits::default());
        run(&mut store, "a", "", 0);
        store.begin(Some("termscribe --clear".into()), None);
        store.discard_pending();
        store.set_boundary();
        store.finish(Some(0));
        run(&mut store, "b", "", 0);
        assert_eq!(commands(&store.select(Selection::SinceBoundary)), ["b"]);
        assert_eq!(commands(&store.select(Selection::All)), ["a", "b"]);
    }

    #[test]
    fn unrecorded_commands_leave_no_trace_but_change_state() {
        let mut store = Store::new(Limits::default());
        store.begin(None, None);
        assert_eq!(store.state(), ShellState::Running);
        store.push_output(b"secret");
        store.finish(Some(0));
        assert_eq!(store.state(), ShellState::Idle);
        assert!(store.select(Selection::All).is_empty());
    }

    #[test]
    fn a_command_that_clears_the_screen_moves_the_boundary_past_itself() {
        let mut store = Store::new(Limits::default());
        run(&mut store, "a", "", 0);
        store.begin(Some("clear".into()), None);
        store.clear_on_finish();
        store.finish(Some(0));
        run(&mut store, "b", "", 0);
        assert_eq!(commands(&store.select(Selection::SinceBoundary)), ["b"]);
        assert_eq!(commands(&store.select(Selection::All)), ["a", "clear", "b"]);
    }

    #[test]
    fn the_clear_request_does_not_leak_into_the_next_command() {
        let mut store = Store::new(Limits::default());
        store.begin(Some("clear".into()), None);
        store.clear_on_finish();
        store.begin(Some("next".into()), None);
        store.finish(Some(0));
        assert_eq!(commands(&store.select(Selection::SinceBoundary)), ["next"]);
    }

    #[test]
    fn oldest_commands_are_dropped_past_the_limit() {
        let mut store = Store::new(Limits {
            max_commands: 2,
            ..Limits::default()
        });
        run(&mut store, "a", "", 0);
        store.set_boundary();
        run(&mut store, "b", "", 0);
        run(&mut store, "c", "", 0);
        run(&mut store, "d", "", 0);
        assert_eq!(commands(&store.select(Selection::All)), ["c", "d"]);
        assert_eq!(
            commands(&store.select(Selection::SinceBoundary)),
            ["c", "d"]
        );
    }

    #[test]
    fn output_past_the_limit_is_counted_not_stored() {
        let mut store = Store::new(Limits {
            max_output_bytes: 4,
            ..Limits::default()
        });
        store.begin(Some("big".into()), None);
        store.push_output(b"abc");
        store.push_output(b"defgh");
        store.finish(Some(0));
        let record = &store.select(Selection::All).records[0];
        assert_eq!(record.output, b"abcd");
        assert_eq!(record.truncated_bytes, 4);
    }

    #[test]
    fn a_new_begin_replaces_a_command_that_never_ended() {
        let mut store = Store::new(Limits::default());
        store.begin(Some("lost".into()), None);
        store.begin(Some("kept".into()), None);
        store.finish(Some(0));
        assert_eq!(commands(&store.select(Selection::All)), ["kept"]);
    }

    #[test]
    fn output_with_no_running_command_is_ignored() {
        let mut store = Store::new(Limits::default());
        store.push_output(b"stray");
        run(&mut store, "a", "x", 0);
        assert_eq!(store.select(Selection::All).records[0].output, b"x");
    }
}
