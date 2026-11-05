//! The private socket between the `termscribe` command and its session's relay.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::format::{FormatOptions, format_markdown};
use crate::store::Store;
use crate::transcript::Selection;

/// The environment variable that holds the socket path inside a session.
pub const SOCKET_ENV: &str = "TERMSCRIBE_SOCKET";

const MAX_LINE: u64 = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Unix socket paths are limited to about 104 bytes on macOS.
const MAX_SOCKET_PATH: usize = 100;

pub type SharedStore = Arc<Mutex<Store>>;

pub fn lock(store: &SharedStore) -> MutexGuard<'_, Store> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Render a transcript. The caller is a command running in the session. When the transcript
    /// holds commands, the relay forgets the caller. When it is empty, the caller stays in the
    /// history.
    Render {
        selection: Selection,
        show_status: bool,
        show_cwd: bool,
        keep_ansi: bool,
    },
    /// Move the boundary to now.
    Clear,
    /// Check that the session answers. Like `Render`, the caller is a command running in the
    /// session, so the relay forgets that command.
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Rendered { markdown: String, commands: usize },
    Cleared,
    Pong,
    Error(String),
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("could not reach the session at {path}: {source}")]
    Connect { path: PathBuf, source: io::Error },
    #[error("lost the connection to the session: {0}")]
    Io(#[from] io::Error),
    #[error("the session sent an unreadable reply: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Renders a selection from the store. Returns the Markdown and how many commands it holds.
pub fn render(
    store: &SharedStore,
    selection: Selection,
    options: &FormatOptions,
) -> (String, usize) {
    let transcript = lock(store).select(selection);
    (format_markdown(&transcript, options), transcript.len())
}

/// Answers one request.
pub fn handle(store: &SharedStore, request: Request) -> Response {
    match request {
        Request::Render {
            selection,
            show_status,
            show_cwd,
            keep_ansi,
        } => {
            let options = FormatOptions {
                show_status,
                show_cwd,
                keep_ansi,
            };
            let (markdown, commands) = render(store, selection, &options);
            // A call that finds nothing to copy is an ordinary command, so it stays in the
            // history. Only a call that hands over a transcript is left out of the next one.
            if commands > 0 {
                lock(store).discard_pending();
            }
            Response::Rendered { markdown, commands }
        }
        Request::Clear => {
            let mut store = lock(store);
            store.discard_pending();
            store.set_boundary();
            Response::Cleared
        }
        Request::Ping => {
            lock(store).discard_pending();
            Response::Pong
        }
    }
}

/// Accepts clients until the listener fails. Each client gets its own thread, so a slow client
/// cannot stall the relay.
pub fn serve(listener: UnixListener, store: SharedStore) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let store = Arc::clone(&store);
        thread::spawn(move || {
            let _ = serve_client(stream, &store);
        });
    }
}

fn serve_client(stream: UnixStream, store: &SharedStore) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_LINE)).read_line(&mut line)?;
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => handle(store, request),
        Err(error) => Response::Error(format!("bad request: {error}")),
    };
    let mut writer = &stream;
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Sends one request to the session at `socket` and waits for the reply.
pub fn request(socket: &Path, request: &Request) -> Result<Response, IpcError> {
    let stream = UnixStream::connect(socket).map_err(|source| IpcError::Connect {
        path: socket.to_path_buf(),
        source,
    })?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut writer = &stream;
    serde_json::to_writer(&mut writer, request)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

/// Creates this session's socket in a private directory.
///
/// The directory is `termscribe-<uid>` under the runtime or temp directory, mode 0700 and
/// owned by the current user. The socket itself is mode 0600.
pub fn bind_socket() -> io::Result<(UnixListener, PathBuf)> {
    let uid = nix::unistd::getuid().as_raw();
    let mut dir = runtime_base().join(format!("termscribe-{uid}"));
    let file = format!("{}.sock", std::process::id());
    if dir.join(&file).as_os_str().len() > MAX_SOCKET_PATH {
        dir = PathBuf::from("/tmp").join(format!("termscribe-{uid}"));
    }
    prepare_dir(&dir, uid)?;
    let path = dir.join(file);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok((listener, path))
}

/// Removes the sockets and shim directories that sessions left behind when they died without
/// cleaning up. A session's files are named after its process id, as `<pid>.sock` and
/// `<pid>.zdot`. Files whose process still runs are kept.
pub fn remove_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((stem, extension)) = name.to_str().and_then(|n| n.rsplit_once('.')) else {
            continue;
        };
        if !matches!(extension, "sock" | "zdot") {
            continue;
        }
        let Ok(pid) = stem.parse::<i32>() else {
            continue;
        };
        let gone = matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
        if gone {
            let path = entry.path();
            let _ = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

fn runtime_base() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

fn prepare_dir(dir: &Path, uid: u32) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} must be a private directory owned by you", dir.display()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Limits;

    fn store_with(commands: &[(&str, &str)]) -> SharedStore {
        let mut store = Store::new(Limits::default());
        for (command, output) in commands {
            store.begin(Some((*command).to_string()), None);
            store.push_output(output.as_bytes());
            store.finish(Some(0));
        }
        Arc::new(Mutex::new(store))
    }

    fn render_request(selection: Selection) -> Request {
        Request::Render {
            selection,
            show_status: true,
            show_cwd: true,
            keep_ansi: false,
        }
    }

    #[test]
    fn render_drops_the_calling_command() {
        let store = store_with(&[("a", "1\n")]);
        lock(&store).begin(Some("termscribe".into()), None);
        let Response::Rendered { markdown, commands } =
            handle(&store, render_request(Selection::All))
        else {
            panic!("expected a rendered reply");
        };
        assert_eq!(commands, 1);
        assert_eq!(markdown, "```\n$ a\n1\n```\n");
        lock(&store).finish(Some(0));
        assert_eq!(lock(&store).select(Selection::All).len(), 1);
    }

    #[test]
    fn an_empty_render_keeps_the_calling_command() {
        let store = store_with(&[]);
        lock(&store).begin(Some("termscribe".into()), None);
        let Response::Rendered { commands, .. } = handle(&store, render_request(Selection::All))
        else {
            panic!("expected a rendered reply");
        };
        assert_eq!(commands, 0);
        lock(&store).finish(Some(0));
        let kept = lock(&store).select(Selection::All);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.records[0].command, "termscribe");
    }

    #[test]
    fn clear_moves_the_boundary_without_forgetting_history() {
        let store = store_with(&[("a", ""), ("b", "")]);
        assert_eq!(handle(&store, Request::Clear), Response::Cleared);
        let Response::Rendered { commands, .. } =
            handle(&store, render_request(Selection::SinceBoundary))
        else {
            panic!("expected a rendered reply");
        };
        assert_eq!(commands, 0);
        let Response::Rendered { commands, .. } = handle(&store, render_request(Selection::All))
        else {
            panic!("expected a rendered reply");
        };
        assert_eq!(commands, 2);
    }

    #[test]
    fn ping_forgets_only_the_calling_command() {
        let store = store_with(&[("a", "")]);
        lock(&store).begin(Some("termscribe doctor".into()), None);
        assert_eq!(handle(&store, Request::Ping), Response::Pong);
        lock(&store).finish(Some(0));
        let Response::Rendered { commands, .. } =
            handle(&store, render_request(Selection::SinceBoundary))
        else {
            panic!("expected a rendered reply");
        };
        assert_eq!(commands, 1);
    }

    #[test]
    fn requests_travel_over_a_real_socket() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let store = store_with(&[("echo hi", "hi\n")]);
        thread::spawn({
            let store = Arc::clone(&store);
            move || serve(listener, store)
        });
        let reply = request(&path, &render_request(Selection::Last(1))).expect("reply");
        assert_eq!(
            reply,
            Response::Rendered {
                markdown: "```\n$ echo hi\nhi\n```\n".into(),
                commands: 1
            }
        );
    }

    #[test]
    fn garbage_requests_get_an_error_reply() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        thread::spawn({
            let store = store_with(&[]);
            move || serve(listener, store)
        });
        let mut stream = UnixStream::connect(&path).expect("connect");
        stream.write_all(b"not json\n").expect("write");
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).expect("read");
        assert!(matches!(
            serde_json::from_str::<Response>(&line),
            Ok(Response::Error(_))
        ));
    }

    #[test]
    fn stale_session_files_are_removed_and_live_ones_kept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let live = std::process::id();
        // No process has this id: it is above the largest id macOS and Linux hand out.
        let dead = 2_000_000_000u32;
        for name in [
            format!("{dead}.sock"),
            format!("{live}.sock"),
            "notes.txt".into(),
        ] {
            fs::write(dir.path().join(name), "").expect("seed file");
        }
        for name in [format!("{dead}.zdot"), format!("{live}.zdot")] {
            fs::create_dir(dir.path().join(name)).expect("seed dir");
        }
        remove_stale(dir.path());
        assert!(!dir.path().join(format!("{dead}.sock")).exists());
        assert!(!dir.path().join(format!("{dead}.zdot")).exists());
        assert!(dir.path().join(format!("{live}.sock")).exists());
        assert!(dir.path().join(format!("{live}.zdot")).exists());
        assert!(dir.path().join("notes.txt").exists());
    }

    #[test]
    fn connecting_to_a_missing_socket_is_a_clear_error() {
        let error = request(Path::new("/nonexistent/x.sock"), &Request::Clear).unwrap_err();
        assert!(matches!(error, IpcError::Connect { .. }));
    }
}
