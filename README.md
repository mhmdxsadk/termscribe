# termscribe

termscribe copies recent shell commands and their output to the clipboard as compact Markdown, meant for use with AI agents and LLMs.

```console
~/proj % git status
On branch main
nothing to commit, working tree clean
~/proj % cd src
~/proj/src % cat missing.txt
cat: missing.txt: No such file or directory
~/proj/src % termscribe
Copied 3 commands to the clipboard.
```

The copied Markdown:

````markdown
```
~/proj $ git status
On branch main
nothing to commit, working tree clean
$ cd src
~/proj/src $ cat missing.txt
cat: missing.txt: No such file or directory
[exit 1]
```
````

## How it works

Your terminal starts `termscribe relay` instead of your shell. The relay runs zsh on a pseudo-terminal and passes everything through unchanged, while small zsh hooks tell it where each command starts and ends. It keeps the commands and their output in memory, one history per tab, and drops them when the tab closes. Your zsh history is separate from that and works as usual: it uses your own history file, shared with any zsh running outside termscribe. When you run `termscribe`, it asks the relay for the recent commands, cleans up the output and copies it as Markdown.

## Installation

termscribe supports macOS. Install via Homebrew:

```sh
brew tap mhmdxsadk/tools
brew install termscribe
```

## Setup

termscribe works by starting your shell for you, so the terminal has to start termscribe instead of the shell. Your `~/.zshrc` stays as it is.

### Automatic

```sh
termscribe setup
```

This adds two lines to your Ghostty config. The first makes every new tab start termscribe, and the second makes Cmd+Alt+C copy the transcript. Reload the config with Cmd+Shift+, and open a new tab. To undo this, run `termscribe remove`.

### Manual

Add these lines to your Ghostty config, usually `~/.config/ghostty/config`:

```
# Run termscribe on top of the default shell when creating a new session
command = /opt/homebrew/bin/termscribe relay
# Bind Cmd+Alt+C to copy the transcript
keybind = super+alt+c=csi:9000~
```

Other terminals have a setting for the program to run in place of the shell. Set it to `/opt/homebrew/bin/termscribe relay`. Run `which termscribe` if your copy is somewhere else.

Reload the config and open a new tab. `termscribe doctor` checks that everything is in place.

## Usage

```sh
termscribe        # since the session started or the last clear
termscribe 3      # the last 3 commands
termscribe all    # every command the session remembers
termscribe clear  # make later copies skip everything so far
```

Every flag works with or without its dashes, so `termscribe clear` and `termscribe --clear` do the same thing. Other flags are listed in `termscribe --help`.

Cmd+Alt+C copies the transcript too, the same as a bare `termscribe`.

## Remote machines

To record what you run on another machine, connect with `termscribe ssh` instead of `ssh`:

```sh
termscribe ssh prod             # accepts the same arguments as ssh
termscribe ssh -p 2222 me@prod
```

Each command you run there becomes its own entry in the transcript, with the machine's name:

````markdown
```
~/proj $ termscribe ssh prod
prod:~ $ uptime
 14:02  up 3 days
$ cd srv
~/srv $ ls
app.log
```
````

Nothing is installed on the other machine. termscribe sends a small startup script through the connection, starts zsh with it, and removes it when the shell ends. Your remote `~/.zshrc` runs as usual. The remote machine needs zsh. Without it you get your normal shell, and the whole session is one entry, as with plain `ssh`. A connection that runs a command (`termscribe ssh prod uptime`) is also left alone.

Copy with Cmd+Alt+C, as everywhere else. Typing `termscribe` on the remote machine does not work, because the history lives on this machine. The remote shell shows no message when the shortcut copies.

The remote shell sends its marks through the connection together with a secret that only this session knows, so text that merely looks like a mark, such as a file you `cat`, is ignored. A hostile user on the same remote machine could read the secret from the process list.

To use it for every connection, add `alias ssh='termscribe ssh'` to your `.zshrc`.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
