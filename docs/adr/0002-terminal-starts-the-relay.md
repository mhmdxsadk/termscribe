---
status: accepted
---

# The terminal starts the relay, and a ZDOTDIR shim loads the hooks

The terminal runs `termscribe relay` in place of the shell. The relay starts zsh as a login shell with `ZDOTDIR` pointing at a private directory of shim files. Each shim runs the user's own startup file of the same name, and the `.zshrc` shim loads termscribe's hooks after the user's `.zshrc`. Termscribe 0.1.0 instead asked the user to put `eval "$(termscribe init zsh)"` on the first line of `~/.zshrc`, and that line replaced the shell with the relay.

We changed because the `.zshrc` line caused three failures within a day of release. The line runs before the user's own `PATH` setup, so `termscribe` was not found and the shell started without a session. An older `termscribe` earlier in `PATH` shadowed the new one. And the line made zsh read `.zshrc` twice, once in the shell that `exec` replaced and once in the relay's shell. Placing the line last avoids the PATH failure but runs the whole `.zshrc` twice. Moving the user's Homebrew PATH setup to the top is not safe, because it can depend on variables set above it, and `remove` could not undo it.

## Considered options

- **Keep the `.zshrc` line and pin the binary's full path.** This fixed the PATH failure, and it was implemented and then dropped. The double read of `.zshrc` and the edit to the user's dotfile remained.
- **Put the line last in `.zshrc`.** The PATH is complete by then, but every terminal runs the whole file twice and repeats any output or side effect in it.
- **Move the Homebrew PATH line above the termscribe line.** This only works for the standard `brew shellenv` line, and it reorders the user's file in a way `remove` cannot reverse.
- **Terminal starts the relay, the relay sets `ZDOTDIR` (chosen).** This is the technique Ghostty and Kitty use for their own shell integration.

## Consequences

- `setup` writes `command = <path to termscribe> relay` into the Ghostty config. Other terminals need the same setting by hand, and the README says how. The user's `.zshrc` is no longer edited. `setup` and `remove` delete the old line if it is there.
- The hooks load after the user's `.zshrc`, so they see the final `PATH`, aliases and prompt setup. The shell starts once.
- The relay runs the user's `.zshenv`, `.zprofile` and `.zshrc` through the shim with `ZDOTDIR` set to the user's own value while each runs. Afterwards `ZDOTDIR` is the user's value again, so `.zlogin`, `.zlogout` and any nested shell use the user's files and ignore the shim. A `ZDOTDIR` set in the user's `.zshenv` is honoured.
- The relay is the terminal's first process, so no login shell has run before it. The relay starts zsh as a login shell so `.zprofile` runs.
- Each session writes `<pid>.zdot` next to its socket in the private directory and removes it on exit. The relay also removes files left by sessions whose process is gone.
- The relay starts a plain login shell, with a warning where it is not intentional, when it cannot start or the shell is not zsh.
- `termscribe init zsh` stays as a hidden command so that a `.zshrc` line from 0.1.0 does no harm. Inside a session it does nothing. Outside one it starts a relay, so a terminal set up the old way keeps working until `setup` removes the line.
- Ghostty's `command` setting applies to every new tab, window and split. Ghostty also stops injecting its own shell integration because it no longer launches a shell, so the hooks file loads it, as before.
