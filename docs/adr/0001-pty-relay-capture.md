---
status: accepted
---

# Capture command output with a PTY relay

Termscribe starts the user's shell inside its own pseudo-terminal and forwards every byte unchanged between the real terminal and the shell. Small zsh hooks send private marks into that byte stream at the start and end of each command, and the relay cuts the output out between them. We chose this because no terminal exposes command output to an outside program, and a hook-only design cannot see output without redirecting it.

## Considered options

- **Ask the terminal.** Ghostty 1.4.0 keeps OSC 133 zones internally and exposes no API to read them. Its `write_scrollback_file` and `write_screen_file` actions only write a temp file and copy, paste or open its path. Scrollback loses data to line wrapping, the scrollback limit, and the alternate screen. The approach is also Ghostty-only.
- **Hooks plus a per-command tee.** This breaks `isatty` checks, TUIs, and stderr ordering. The original brief ruled it out.
- **Hooks only.** `preexec` and `precmd` give command text, exit status and timing, but no output.

## Consequences

- Every pane runs one extra process between the terminal and the shell. The relay must forward resize events, signals and job control correctly, and it must fail open to a plain shell if it cannot start.
- The relay sees one merged stream, so standard output and standard error cannot be told apart.
- The relay can intercept the keybinding's escape sequence before the shell sees it, so the shortcut works while a program is running.
- The design works in any terminal, because the relay and the hooks depend only on standard escape sequences.
