# Termscribe design note

Date: 2026-11-02. Status: accepted by the user. First version implemented; see the implementation notes at the end.

## Goal

One shortcut or one command puts a clean Markdown transcript of recent shell commands and their output on the clipboard. The user does not clear the terminal, select text, or clean the result.

## Research findings

Facts below were checked on the target machine (Ghostty 1.4.0, zsh 5.9) unless marked otherwise.

### Ghostty

- Ghostty injects shell integration for bash, elvish, fish, nushell and zsh when it launches them directly ([docs](https://ghostty.org/docs/features/shell-integration)). Detection uses the basename of the launched command.
- The zsh script (`Contents/Resources/ghostty/shell-integration/zsh/ghostty-integration`) uses `precmd` and `preexec` and emits OSC 133. It writes `A;cl=line` and `B` inside `PS1`, a bare `C` from `preexec`, and `D;<status>` from `precmd`. It also emits `P;k=s` and `P;k=i` for continuation lines, and OSC 7 for the working directory. It writes to a separate `/dev/tty` descriptor so redirection cannot swallow the marks.
- The `C` mark carries no command text. The command text sits on screen between `B` and `C`.
- The script is GPLv3 (derived from Kitty). Termscribe writes its own hooks and copies none of it.
- Ghostty uses the zones for `jump_to_prompt` and click-to-move-cursor. Release 1.3.0 rewrote its OSC 133 handling ([notes](https://ghostty.org/docs/install/release-notes/1-3-0)).

### What an external program can get from Ghostty

| Mechanism | Result |
| --- | --- |
| IPC or CLI query | None found. |
| AppleScript (preview, `sdef /Applications/Ghostty.app`) | Terminal id, title, working directory, pid, tty. Commands `input text` and `perform action`. No zones, no text readback. |
| `write_scrollback_file`, `write_screen_file`, `write_selection_file` | Write a temp file, then copy, paste or open its path ([reference](https://ghostty.org/docs/config/keybind/reference)). Docs do not say whether semantic information survives. Not tested, because the design does not need it. |
| `jump_to_prompt` | Moves the viewport only. |
| A copy-command-output action | Not present in 1.4.0 (`ghostty +list-actions`). |

### OSC 133

OSC 133 is a de facto standard from FinalTerm, now implemented by Kitty, WezTerm, iTerm2 and others. `A` starts a prompt, `B` ends it and starts user input, `C` starts command output, and `D;status` ends it ([Kitty](https://sw.kovidgoyal.net/kitty/shell-integration/)). Kitty's `C` can carry the command line (`cmdline=`). The marks travel inside the output stream, so only the terminal or a process on the PTY can read them. A shell-side hook cannot.

### Conclusion

Terminal-native capture is not viable for an external program. Ghostty keeps the zones and offers no read path. Scrollback export is also lossy. A capture layer on our side of the terminal is required.

## Options compared

| Option | Output fidelity | Terminal-neutral | Disturbs the session | Verdict |
| --- | --- | --- | --- | --- |
| Terminal-native (Ghostty export) | Lossy | No | Clobbers the clipboard | Rejected |
| Hooks plus per-command tee | Good | Yes | Breaks TUIs, `isatty`, ordering | Rejected |
| Hooks only | None | Yes | No | Cannot capture output |
| PTY relay plus hooks | Full byte stream | Yes | One forwarding process | Recommended |

See `docs/adr/0001-pty-relay-capture.md`.

## Recommended architecture

```text
Ghostty  <-->  Relay (owns PTY master)  <-->  zsh (PTY slave)
                 |  ^
        marks    |  |  keybinding sequence (intercepted, never forwarded)
                 v  |
            Store (memory)  <--- Unix socket (0600) <--- `termscribe` CLI
```

Data flow for a copy request.

```text
Capture (relay + marks) -> CommandRecord -> Transcript (selection)
  -> Cleaner (output text) -> MarkdownFormatter -> Clipboard (trait)
```

### Components

- **Relay.** Spawns the shell on a PTY, forwards bytes in both directions, mirrors window size, signals and job control, and exits with the shell's status. It passes all unknown escape sequences through. It removes only termscribe's own marks.
- **Marks.** The zsh hooks write private OSC sequences to the controlling terminal. `preexec` sends the working directory and the exact command text (`$1`, encoded so newlines and control bytes are safe). `precmd` sends the exit status and signals that the shell is idle. The relay uses the idle signal to decide when printing a message is safe. The OSC number is chosen at implementation time. Termscribe does not depend on Ghostty's own OSC 133 marks, which pass through untouched.
- **Store.** In-memory ring of `CommandRecord` (500 commands, 1 MiB of output each, with a `[… N bytes truncated …]` marker). Holds the Boundary.
- **Socket.** One Unix socket per Session, path in `$TERMSCRIBE_SOCKET`, mode `0600` in a `0700` directory. The relay deletes it on exit.
- **Keybinding.** Ghostty binds `cmd+shift+c` to send a private sequence. The relay swallows it, builds the Transcript itself, copies it, and prints one confirmation line only if the shell is idle. Ghostty's `csi:` keybind action exists in 1.4.0. The exact binding line is verified during implementation.
- **CLI.** `termscribe [N] [--all] [--clear] [--stdout] [--quiet] [--no-status] [--no-cwd] [--raw-ansi]` and `termscribe init zsh`. Inside a Session it talks to the relay over the socket. Outside one it exits with an explanatory error.
- **Clipboard trait.** `pbcopy` first. Linux and Windows implementations add later without touching the core.

### Output cleaning

The relay stores the raw bytes between the start and end marks. The Cleaner runs at copy time.

1. Remove CSI, OSC and other escape sequences (unless `--raw-ansi`).
2. Treat `\r` as "return to line start", so the last rewrite of a progress line wins. Handle `\b`.
3. Normalize `\r\n` to `\n`.
4. If the command entered the alternate screen (`?1049`, `?1047`, `?47`), drop the text drawn there and note it with a placeholder.
5. Trim trailing whitespace per line and trailing blank lines.

Known risk. zsh's `PROMPT_SP` prints a `%` and padding after output that lacks a trailing newline, before `precmd` runs. Ghostty's script documents the same failure. Tests must cover it, and the Cleaner may need to recognise the pattern.

### Markdown format

The whole transcript is one fenced block, written to use few tokens. The fence is one backtick longer than the longest backtick run anywhere inside, with a minimum of three, and has no info string.

- Each command is a line that starts with `$ `. A multi-line command continues on lines that start with `> `.
- The command's output follows, with no decoration. A command with no output has only its own line.
- The working directory prefixes the prompt (`~/proj $ git status`) on the first command and on any command whose directory differs from the previous one. A command with no known directory keeps the previous one. `$HOME` is written as `~`. `--no-cwd` removes it.
- A failed command gets an `[exit N]` line after its output. `--no-status` removes it.
- A run of identical output lines collapses to the line plus `[previous line repeated N more times]`, only when that saves bytes.
- Output is never cut by line count. The 1 MiB per-command memory limit still applies and still leaves a truncation marker.

```
~/proj $ git status
On branch main
nothing to commit, working tree clean
$ cd src
~/proj/src $ cat missing.txt
cat: missing.txt: No such file or directory
[exit 1]
```

### Edge cases

| Case | Behaviour |
| --- | --- |
| Nested shell, `ssh` | Folded into the outer command. |
| Ctrl+C | Exit code 130, partial output kept. |
| Background output at an idle prompt | Dropped. |
| Pipeline | One command, status is `$?`. |
| Multi-line command | Stored exactly as submitted. |
| Still running when copying | Excluded. |
| Huge output | Capped per command, marker inserted. |
| Clear screen at a prompt (Cmd+K, Ctrl+L) | Moves the boundary. |
| `clear` command | Moves the boundary past itself when it ends. |
| Full-screen program erasing its own screen | Ignored. |
| Several panes | Each has its own relay, socket and Store. |
| Relay cannot start | Plain zsh plus one warning line (fail open). |

### Privacy

When zsh's `HIST_IGNORE_SPACE` option is set, a command that starts with a space is not recorded, and neither is its output. Without the option, every command is recorded.

History lives in memory and disappears with the pane. Commands and output can still contain secrets, and the clipboard is shared, so the user decides what to paste.

## Code layout (single crate, library plus binary)

`record`, `transcript`, `clean`, `format`, `clipboard`, `capture` (relay, PTY, marks, input interception), `ipc`, `cli`. The first five modules have no I/O dependencies and carry the unit tests.

## Testing

- Unit tests for `format`, `clean` and `transcript`. Cases: one and several commands, no output, failed command, multiline output, Unicode, ANSI, triple backticks, backticks in commands, very large output, interrupted commands.
- Integration tests that start a real zsh under the relay in a test PTY and check the resulting Transcript. Cover Ctrl+C, a TUI-like alternate-screen program, a missing trailing newline, and two concurrent sessions.

## Distribution

In scope now. `git init`, a GitHub Actions CI workflow (fmt, clippy, test on macOS), MIT `LICENSE`, `.gitignore`, `rustfmt.toml`, README, and release-please (release type `rust`) for version bumps and GitHub Releases from Conventional Commits.

Out of scope. Homebrew formula, crates.io publishing, and prebuilt binaries. Attaching binaries to a release later needs a build job. Events created with the default `GITHUB_TOKEN` do not trigger other workflows, so that job must run inside the release workflow or use a separate token.

## Implementation notes

Details settled while building the first version.

- **Marks.** `ESC ] 7733 ; ts ; <kind> [; <payload>] BEL`. Kinds are `s;<cwd>;<command>` (start, with `%`, ESC and BEL percent-encoded in both parts, and `;` also in the directory), `x` (start, unrecorded) and `e;<status>` (end, shell at a prompt). The zsh hooks write them to `/dev/tty` with no forks. `preexec` is appended to `preexec_functions` and `precmd` is prepended to `precmd_functions`. The first keeps other hooks' output out of the capture, and the second reads `$?` before anything changes it.
- **Shortcut.** The terminal sends `CSI 9000 ~`. At a prompt the relay forwards it, and a zle widget runs `termscribe` and shows the reply with `zle -M`, which redraws safely. During a command the relay swallows it and copies on a background thread without printing. A lone ESC is never delayed. A partial sequence waits at most 15 ms.
- **`PROMPT_SP`.** zsh 5.9 prints `%`, padding spaces, `\r`, a space and `\r` after output, before `precmd`. The Cleaner removes that tail, because otherwise the padding overwrites an unterminated last line.
- **Nested shells.** The first shell exports `TERMSCRIBE_OWNER=$$`. A shell whose `$$` differs installs no hooks, so its output folds into the outer command. `exec zsh` keeps the PID and keeps recording.
- **Clearing the screen.** At a prompt, Ghostty erases the display and sends a form feed to the shell (checked in Ghostty's current source, not against 1.4.0). The relay moves the boundary when it sees a form feed on the keyboard stream while the shell is idle. Cmd+K during a running command sends nothing to the shell, so it changes nothing. The `clear` command is found by watching its output for `ESC [ 2 J` or `ESC [ 3 J` outside the alternate screen, and the boundary moves when the command ends.
- **Ghostty's own integration.** Ghostty loads its zsh integration only into the shell it starts, and the relay replaces that shell. Without it Ghostty never sees OSC 133 prompt marks, `cursorIsAtPrompt` stays false, and Cmd+K sends no form feed. The init script sources `$GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration` inside the relay's shell when that variable is set. Ghostty's marks pass through the relay untouched.
- **The `termscribe` command.** Every request from the CLI drops the command that is running, so `ts`, `termscribe --clear` and the shortcut widget never enter a transcript.
- **Home directory.** The relay shortens the directory with its own `$HOME` when it handles the start mark, so the stored record already holds `~/proj`.
- **Truncation.** Only the first 1 MiB of a command's output is kept, and the marker reports the dropped bytes. Keeping the tail as well is a possible later change.
- **Fail open.** The relay execs a plain shell with `TERMSCRIBE_DISABLED=1` when it has no terminal or cannot set up the pty.
- **Test hook.** `TERMSCRIBE_CLIPBOARD_FILE` writes the clipboard text to a file. The integration tests use it so they never touch the real clipboard.
- **Not yet verified.** The Ghostty keybinding `keybind = super+shift+c=csi:9000~` has not been tried in a real Ghostty window.
