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

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
