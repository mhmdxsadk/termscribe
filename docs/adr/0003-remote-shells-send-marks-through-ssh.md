---
status: accepted
---

# Record remote shells with marks sent through the connection

`termscribe ssh host` starts `ssh` with a remote command that unpacks the same zsh startup shim a local session uses into a private temporary directory, starts the user's login shell with it (zsh or bash), and removes the directory afterwards. The remote hooks write the usual marks to the terminal, wrapped as `@<nonce>;<host>;<mark>`. The marks travel through the connection like any output, and the local relay, which is still the only process that keeps history and uses the clipboard, records each remote command as its own record with a host.

## Considered options

- **Run a relay on the remote machine and forward its socket back.** It needs a termscribe binary on every host, and the clipboard would still have to be reached from there.
- **Parse the remote prompt on the local side.** It is fragile and gives no exit codes or directories.
- **Install the hooks in the remote `~/.zshrc`.** It needs a one-time edit on each host. The wrapper needs none.

## Consequences

- Nothing is installed on the remote machine, but it needs `sh`, `base64`, `mktemp` and a login shell that is zsh or bash. With another shell the user gets it as it is and one long entry, as before. The launcher runs the user's login shell and never swaps it for another that happens to be installed, because that would change aliases and startup files.
- bash has no hook that runs before a command and sees the whole line. A DEBUG trap marks the first command after each prompt, `PROMPT_COMMAND` marks the end, and `history 1` supplies the text. `bash --rcfile` reads the termscribe file, which runs the user's startup files the way a login shell would. bash cannot see a line it rejects, so those are not recorded. When `HISTCONTROL` ignores lines, a repeat of the previous line is recorded, and a private line is guessed by comparing it with `BASH_COMMAND`. A DEBUG trap that the user's files already set is not overridden.
- A remote shell started this way has no `termscribe` command. The copy shortcut works because the relay handles it locally.
- The relay creates a secret for each session (`TERMSCRIBE_NONCE`) and drops remote marks that do not carry it, so output that merely looks like a mark cannot add fake commands. The secret is visible in the remote process list, so this does not defend against another user of the remote machine.
- The first remote mark ends the local `termscribe ssh host` entry, which keeps what it printed until then. The next local mark ends the remote part. An `exit` typed on the remote machine takes the exit status of `ssh`.
- A command is not recorded as remote when `ssh` runs something else or never gets a shell, because `termscribe ssh` leaves those alone.
- Inside Ghostty, the relay writes OSC 133 prompt marks (`D;<status>` and `A` at a remote prompt, `C` when a remote command starts) next to the remote marks. Ghostty sends Cmd+K to the shell as a form feed only when it believes the cursor is at a prompt, and it learns that from these marks, which the remote shell does not send. Without them Ghostty clears the screen itself and the relay never sees Cmd+K, so the transcript keeps everything before it.
