---
status: accepted
---

# Record remote shells with marks sent through the connection

`termscribe ssh host` starts `ssh` with a remote command that unpacks the same zsh startup shim a local session uses into a private temporary directory, starts zsh with it, and removes the directory afterwards. The remote hooks write the usual marks to the terminal, wrapped as `@<nonce>;<host>;<mark>`. The marks travel through the connection like any output, and the local relay, which is still the only process that keeps history and uses the clipboard, records each remote command as its own record with a host.

## Considered options

- **Run a relay on the remote machine and forward its socket back.** It needs a termscribe binary on every host, and the clipboard would still have to be reached from there.
- **Parse the remote prompt on the local side.** It is fragile and gives no exit codes or directories.
- **Install the hooks in the remote `~/.zshrc`.** It needs a one-time edit on each host. The wrapper needs none.

## Consequences

- Nothing is installed on the remote machine, but it needs `sh`, `base64`, `mktemp` and zsh. Without zsh the user gets their normal shell and one long entry, as before.
- A remote shell started this way has no `termscribe` command. The copy shortcut works because the relay handles it locally.
- The relay creates a secret for each session (`TERMSCRIBE_NONCE`) and drops remote marks that do not carry it, so output that merely looks like a mark cannot add fake commands. The secret is visible in the remote process list, so this does not defend against another user of the remote machine.
- The first remote mark ends the local `termscribe ssh host` entry, which keeps what it printed until then. The next local mark ends the remote part. An `exit` typed on the remote machine takes the exit status of `ssh`.
- A command is not recorded as remote when `ssh` runs something else or never gets a shell, because `termscribe ssh` leaves those alone.
