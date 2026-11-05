//! The relay: runs the shell on a pseudo-terminal, forwards bytes both ways, and records the
//! output of each command.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, bail};
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::openpty;
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{Pid, isatty, read, write};

use crate::clipboard::{Clipboard, system_clipboard};
use crate::format::{FormatOptions, abbreviate_home};
use crate::input::{COPY_TRIGGER, InputFilter, InputItem};
use crate::ipc::{self, SOCKET_ENV, SharedStore};
use crate::marks::{Item, Mark, MarkScanner};
use crate::screen::ClearDetector;
use crate::shell::{BIN_ENV, USER_ZDOTDIR_ENV, write_shim};
use crate::store::{Limits, ShellState, Store};
use crate::transcript::Selection;

/// Ctrl+L. Ghostty sends it to the shell after Cmd+K clears the screen at a prompt.
const FORM_FEED: u8 = 0x0c;
const READ_SIZE: usize = 64 * 1024;
/// Stop reading the keyboard while this many bytes wait to be written to the shell.
const MAX_QUEUED_INPUT: usize = 1024 * 1024;
/// How long a partial escape sequence may wait for its remaining bytes.
const PARTIAL_WAIT_MS: u16 = 15;
/// How long to keep draining output after the shell exits.
const DRAIN_WAIT_MS: u16 = 50;

mod ioctls {
    nix::ioctl_read_bad!(get_winsize, libc::TIOCGWINSZ, libc::winsize);
    nix::ioctl_write_ptr_bad!(set_winsize, libc::TIOCSWINSZ, libc::winsize);
}

/// Runs a relay around `shell` and returns the exit code for the process.
///
/// The terminal starts this instead of a shell, so the process is a login shell's stand-in. If
/// the relay cannot start, or the shell is not one termscribe supports, this replaces the process
/// with a plain login shell instead, so a problem here never leaves the user without a shell.
pub fn run(shell: Option<String>) -> i32 {
    let shell = shell
        .or_else(|| std::env::var("TERMSCRIBE_SHELL").ok())
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_string());
    if std::env::var_os("TERMSCRIBE_DISABLED").is_some_and(|value| !value.is_empty()) {
        return exec_plain_shell(&shell);
    }
    if shell_name(&shell) != "zsh" {
        eprintln!(
            "termscribe: only zsh is supported, and your shell is {shell}. \
             Starting it without termscribe."
        );
        return exec_plain_shell(&shell);
    }
    match Relay::start(&shell) {
        Ok(relay) => match relay.run() {
            Ok(code) => code,
            Err(error) => {
                eprintln!("termscribe: relay stopped: {error:#}");
                1
            }
        },
        Err(error) => {
            eprintln!("termscribe: {error:#}. Starting a plain shell instead.");
            exec_plain_shell(&shell)
        }
    }
}

fn shell_name(shell: &str) -> &str {
    Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell)
}

/// A shell's name with a leading dash tells it to behave as a login shell.
fn login_name(shell: &str) -> String {
    format!("-{}", shell_name(shell))
}

/// Replaces this process with the shell, as the terminal would have started it. Returns only
/// when that fails.
fn exec_plain_shell(shell: &str) -> i32 {
    let error = Command::new(shell).arg0(login_name(shell)).exec();
    eprintln!("termscribe: could not start {shell}: {error}");
    127
}

/// Puts the terminal in raw mode and restores it when dropped.
struct RawMode {
    original: Termios,
}

impl RawMode {
    fn enable(original: Termios) -> Result<Self> {
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, &raw)
            .context("could not switch the terminal to raw mode")?;
        Ok(Self { original })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, &self.original);
    }
}

/// Signals the loop reacts to.
struct Signals {
    wake: UnixStream,
    winch: Arc<AtomicBool>,
    child: Arc<AtomicBool>,
    terminate: Arc<AtomicBool>,
}

impl Signals {
    fn install() -> Result<Self> {
        let (wake, writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        let winch = Arc::new(AtomicBool::new(false));
        let child = Arc::new(AtomicBool::new(false));
        let terminate = Arc::new(AtomicBool::new(false));
        let registrations = [
            (libc::SIGWINCH, &winch),
            (libc::SIGCHLD, &child),
            (libc::SIGTERM, &terminate),
            (libc::SIGHUP, &terminate),
        ];
        for (signal, flag) in registrations {
            signal_hook::flag::register(signal, Arc::clone(flag))?;
            signal_hook::low_level::pipe::register(signal, writer.try_clone()?)?;
        }
        Ok(Self {
            wake,
            winch,
            child,
            terminate,
        })
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        while matches!(read(&self.wake, &mut buf), Ok(n) if n > 0) {}
    }
}

struct Relay {
    master: OwnedFd,
    child: Child,
    store: SharedStore,
    clipboard: Arc<dyn Clipboard>,
    socket_path: PathBuf,
    shim_dir: PathBuf,
    signals: Signals,
    scanner: MarkScanner,
    filter: InputFilter,
    clear_detector: ClearDetector,
    to_child: Vec<u8>,
    stdin_open: bool,
    exit_status: Option<ExitStatus>,
    _raw_mode: RawMode,
}

impl Relay {
    fn start(shell: &str) -> Result<Self> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        if !isatty(stdin.as_fd()).unwrap_or(false) || !isatty(stdout.as_fd()).unwrap_or(false) {
            bail!("the relay needs a terminal on stdin and stdout");
        }

        let original = tcgetattr(stdin.as_fd()).context("could not read terminal settings")?;
        let winsize = current_winsize();
        let pty = openpty(winsize.as_ref(), Some(&original)).context("could not open a pty")?;
        fcntl(&pty.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
            .context("could not make the pty nonblocking")?;

        let (listener, socket_path) = ipc::bind_socket().context("could not create the socket")?;
        if let Some(dir) = socket_path.parent() {
            ipc::remove_stale(dir);
        }
        let shim_dir = socket_path.with_extension("zdot");
        write_shim(&shim_dir).context("could not write the shell startup files")?;
        let store: SharedStore = Arc::new(Mutex::new(Store::new(Limits::default())));
        thread::spawn({
            let store = Arc::clone(&store);
            move || ipc::serve(listener, store)
        });

        let signals = Signals::install().context("could not install signal handlers")?;

        let mut command = Command::new(shell);
        command
            .arg0(login_name(shell))
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave))
            .env(SOCKET_ENV, &socket_path)
            .env("ZDOTDIR", &shim_dir)
            .env(
                USER_ZDOTDIR_ENV,
                std::env::var_os("ZDOTDIR").unwrap_or_default(),
            );
        if let Ok(exe) = std::env::current_exe() {
            command.env(BIN_ENV, exe);
        }
        // SAFETY: the closure only calls setsid and ioctl, which are async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()?;
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .with_context(|| format!("could not start {shell}"))?;
        drop(command);

        let raw_mode = RawMode::enable(original)?;
        Ok(Self {
            master: pty.master,
            child,
            store,
            clipboard: Arc::from(system_clipboard()),
            socket_path,
            shim_dir,
            signals,
            scanner: MarkScanner::new(),
            filter: InputFilter::new(),
            clear_detector: ClearDetector::new(),
            to_child: Vec::new(),
            stdin_open: true,
            exit_status: None,
            _raw_mode: raw_mode,
        })
    }

    fn run(mut self) -> Result<i32> {
        let result = self.event_loop();
        let status = match self.exit_status.take() {
            Some(status) => status,
            None => {
                let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGHUP);
                self.child.wait()?
            }
        };
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_dir_all(&self.shim_dir);
        result?;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }

    fn event_loop(&mut self) -> Result<()> {
        let mut buf = vec![0u8; READ_SIZE];
        loop {
            let stdin = io::stdin();
            let want_stdin = self.stdin_open && self.to_child.len() < MAX_QUEUED_INPUT;
            let mut master_flags = PollFlags::POLLIN;
            if !self.to_child.is_empty() {
                master_flags |= PollFlags::POLLOUT;
            }
            let mut fds = vec![
                PollFd::new(self.signals.wake.as_fd(), PollFlags::POLLIN),
                PollFd::new(self.master.as_fd(), master_flags),
            ];
            if want_stdin {
                fds.push(PollFd::new(stdin.as_fd(), PollFlags::POLLIN));
            }

            let timeout = if self.exit_status.is_some() {
                PollTimeout::from(DRAIN_WAIT_MS)
            } else if self.scanner.is_holding_partial_prefix() || self.filter.is_holding() {
                PollTimeout::from(PARTIAL_WAIT_MS)
            } else {
                PollTimeout::NONE
            };
            let ready = match poll(&mut fds, timeout) {
                Ok(n) => n,
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(error).context("poll failed"),
            };

            let revents: Vec<PollFlags> = fds
                .iter()
                .map(|fd| fd.revents().unwrap_or(PollFlags::empty()))
                .collect();
            drop(fds);

            if ready == 0 {
                if self.exit_status.is_some() {
                    return Ok(());
                }
                self.release_held_bytes()?;
                continue;
            }

            if !revents[0].is_empty() {
                self.signals.drain();
                self.handle_signals()?;
            }
            if revents[1].contains(PollFlags::POLLOUT) {
                self.flush_to_child()?;
            }
            if revents[1].intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
                match read(&self.master, &mut buf) {
                    Ok(0) | Err(Errno::EIO) => return Ok(()),
                    Ok(n) => self.handle_output(&buf[..n])?,
                    Err(Errno::EAGAIN | Errno::EINTR) => {}
                    Err(error) => return Err(error).context("could not read from the shell"),
                }
            }
            if want_stdin
                && revents[2]
                    .intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            {
                match read(&stdin, &mut buf) {
                    Ok(0) | Err(Errno::EIO) => self.stdin_open = false,
                    Ok(n) => self.handle_input(&buf[..n])?,
                    Err(Errno::EAGAIN | Errno::EINTR) => {}
                    Err(error) => return Err(error).context("could not read the keyboard"),
                }
            }
        }
    }

    fn handle_signals(&mut self) -> Result<()> {
        if self.signals.winch.swap(false, Ordering::SeqCst) {
            if let Some(winsize) = current_winsize() {
                // SAFETY: the fd is the open pty master and `winsize` is a valid winsize.
                let _ = unsafe { ioctls::set_winsize(self.master.as_raw_fd(), &winsize) };
            }
        }
        if self.signals.child.swap(false, Ordering::SeqCst) {
            if let Some(status) = self.child.try_wait()? {
                self.exit_status = Some(status);
            }
        }
        if self.signals.terminate.swap(false, Ordering::SeqCst) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGHUP);
        }
        Ok(())
    }

    /// Output from the shell: show it, and record it when a command is running.
    fn handle_output(&mut self, bytes: &[u8]) -> Result<()> {
        for item in self.scanner.feed(bytes) {
            match item {
                Item::Data(data) => self.show_and_record(&data)?,
                Item::Mark(mark) => self.apply_mark(mark),
            }
        }
        Ok(())
    }

    fn show_and_record(&mut self, data: &[u8]) -> Result<()> {
        write_all_blocking(io::stdout().as_fd(), data)
            .context("could not write to the terminal")?;
        let mut store = ipc::lock(&self.store);
        store.push_output(data);
        if store.state() == ShellState::Running && self.clear_detector.feed(data) {
            store.clear_on_finish();
        }
        Ok(())
    }

    fn apply_mark(&mut self, mark: Mark) {
        let mut store = ipc::lock(&self.store);
        match mark {
            Mark::Start { cwd, command } => {
                self.clear_detector.reset();
                let command = command.trim_end_matches(['\n', '\r']).to_string();
                let home = std::env::var("HOME").ok();
                let cwd = cwd.map(|cwd| abbreviate_home(&cwd, home.as_deref()));
                store.begin(Some(command), cwd);
            }
            Mark::StartUnrecorded => {
                self.clear_detector.reset();
                store.begin(None, None);
            }
            Mark::End(code) => store.finish(code),
        }
    }

    /// Keyboard input: forward it, except for the copy shortcut.
    fn handle_input(&mut self, bytes: &[u8]) -> Result<()> {
        for item in self.filter.feed(bytes) {
            match item {
                InputItem::Data(data) => {
                    self.note_screen_clear_at_prompt(&data);
                    self.to_child.extend_from_slice(&data);
                }
                InputItem::CopyShortcut => self.on_copy_shortcut(),
            }
        }
        self.flush_to_child()
    }

    /// Cmd+K in Ghostty and Ctrl+L both reach the shell as a form feed. At a prompt they clear
    /// the screen, so they also start a new transcript.
    fn note_screen_clear_at_prompt(&self, input: &[u8]) {
        if input.contains(&FORM_FEED) {
            let mut store = ipc::lock(&self.store);
            if store.state() == ShellState::Idle {
                store.set_boundary();
            }
        }
    }

    /// At a prompt, hand the shortcut to the shell so it can show the result with `zle -M`.
    /// Anywhere else, copy quietly, because printing could corrupt a running program's screen.
    fn on_copy_shortcut(&mut self) {
        let state = ipc::lock(&self.store).state();
        if state == ShellState::Idle {
            self.to_child.extend_from_slice(COPY_TRIGGER);
        } else {
            let store = Arc::clone(&self.store);
            let clipboard = Arc::clone(&self.clipboard);
            thread::spawn(move || {
                let (markdown, commands) =
                    ipc::render(&store, Selection::SinceBoundary, &FormatOptions::default());
                if commands > 0 {
                    let _ = clipboard.copy(&markdown);
                }
            });
        }
    }

    /// Releases bytes that waited for the rest of an escape sequence that never came.
    fn release_held_bytes(&mut self) -> Result<()> {
        if let Some(data) = self.scanner.flush_partial_prefix() {
            self.show_and_record(&data)?;
        }
        if let Some(data) = self.filter.flush() {
            self.to_child.extend_from_slice(&data);
            self.flush_to_child()?;
        }
        Ok(())
    }

    fn flush_to_child(&mut self) -> Result<()> {
        while !self.to_child.is_empty() {
            match write(&self.master, &self.to_child) {
                Ok(0) => break,
                Ok(n) => {
                    self.to_child.drain(..n);
                }
                Err(Errno::EAGAIN) => break,
                Err(Errno::EINTR) => {}
                // The shell is gone. The child-exit path ends the loop.
                Err(Errno::EIO) => {
                    self.to_child.clear();
                    break;
                }
                Err(error) => return Err(error).context("could not write to the shell"),
            }
        }
        Ok(())
    }
}

fn current_winsize() -> Option<libc::winsize> {
    let mut winsize = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: stdin is open for the life of the process and `winsize` is a valid out pointer.
    let result = unsafe { ioctls::get_winsize(io::stdin().as_raw_fd(), &mut winsize) };
    (result.is_ok() && winsize.ws_row > 0 && winsize.ws_col > 0).then_some(winsize)
}

fn write_all_blocking(fd: BorrowedFd<'_>, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        match write(fd, data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(Errno::EINTR) => {}
            Err(Errno::EAGAIN) => {
                let mut fds = [PollFd::new(fd, PollFlags::POLLOUT)];
                let _ = poll(&mut fds, PollTimeout::NONE);
            }
            Err(error) => return Err(io::Error::from(error)),
        }
    }
    Ok(())
}
