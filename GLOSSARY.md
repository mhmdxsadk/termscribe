# Termscribe

Termscribe turns recent shell commands and their output into a compact Markdown transcript on the clipboard.

## Language

**Session**:
The history of one interactive terminal pane, from the moment its shell starts until that shell exits. Sessions never share commands.
_Avoid_: Tab, window, terminal

**Command**:
One line (or multi-line entry) the user submitted to the shell, together with everything the shell printed in response.
_Avoid_: Job, process, run

**Command Record**:
The stored description of one Command. It holds the exact command text, the working directory it started in, its output, and its exit status.
_Avoid_: Entry, event, log line

**Output**:
The text a Command printed to the terminal, as the user saw it. Standard output and standard error are not told apart.
_Avoid_: Result, stdout

**Relay**:
The termscribe process that stands between a terminal pane and its shell for one Session and keeps that Session's Command Records. The terminal starts it with `termscribe relay` in place of the shell.
_Avoid_: Proxy, wrapper, daemon

**Transcript**:
An ordered selection of Command Records from one Session, ready to be formatted.
_Avoid_: History, log, buffer

**Boundary**:
A point in a Session. A default Transcript contains only Commands that started after the latest Boundary. Clearing the screen and `termscribe --clear` move it.
_Avoid_: Checkpoint, reset, mark
