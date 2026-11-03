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

## Setup

The shell integration goes at the top of `~/.zshrc`. New terminal tabs then run their shell through termscribe.

```sh
eval "$(termscribe init zsh)"
```

## Usage

```sh
termscribe          # since the session started or the last clear
termscribe 3        # the last 3 commands
termscribe --all    # every command the session remembers
termscribe --clear  # make later copies skip everything so far
```

Other flags are listed in `termscribe --help`.

## Keyboard shortcut

If your terminal supports custom shortcuts, any key can trigger a copy by sending `ESC [ 9000 ~`. In [Ghostty](https://ghostty.org), for example:

```
keybind = super+alt+c=csi:9000~
```

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
