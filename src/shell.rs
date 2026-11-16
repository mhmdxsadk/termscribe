//! The zsh startup shim and the session hooks.
//!
//! The relay starts zsh with `ZDOTDIR` pointing at a private directory of shim files. Each shim
//! runs the user's own startup file of the same name, so the user's `.zshenv`, `.zprofile` and
//! `.zshrc` run as usual. After the user's `.zshrc` the shim loads the session hooks, so they
//! come last and see the finished `PATH`, aliases and prompt setup.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use clap::ValueEnum;

use crate::input::COPY_TRIGGER;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Zsh,
}

/// The environment variable that carries the user's own `ZDOTDIR` to the shim. Empty or unset
/// means the user had none.
pub const USER_ZDOTDIR_ENV: &str = "TERMSCRIBE_USER_ZDOTDIR";
/// The environment variable that carries the path of the termscribe binary to the hooks.
pub const BIN_ENV: &str = "TERMSCRIBE_BIN";
/// The environment variable that carries the session's secret to the shell. `termscribe ssh`
/// passes it on to the shell on the other machine.
pub const NONCE_ENV: &str = "TERMSCRIBE_NONCE";

/// Makes a secret for one session: 128 random bits as hex.
pub fn new_nonce() -> io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// One file of the shim directory.
pub struct ShimFile {
    pub name: &'static str,
    pub content: String,
}

/// Every file the relay writes into the shim directory.
pub fn shim_files() -> Vec<ShimFile> {
    vec![
        ShimFile {
            name: ".zshenv",
            content: ZSHENV.to_string(),
        },
        ShimFile {
            name: ".zprofile",
            content: ZPROFILE.to_string(),
        },
        ShimFile {
            name: ".zshrc",
            content: ZSHRC.to_string(),
        },
        ShimFile {
            name: "termscribe.zsh",
            content: HOOKS.replace("@TRIGGER@", &zsh_key(COPY_TRIGGER)),
        },
    ]
}

/// The file that starts bash on another machine (`termscribe ssh`). bash needs no shim
/// directory: `bash --rcfile` reads this file instead of `~/.bashrc`, and the file runs the
/// user's own startup files the way a login shell would.
pub fn bash_files() -> Vec<ShimFile> {
    vec![ShimFile {
        name: "termscribe.bashrc",
        content: REMOTE_BASHRC.to_string(),
    }]
}

/// Writes the shim files into `dir`. The directory is private to the user, and it replaces
/// whatever an earlier session with the same process id left there.
pub fn write_shim(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    DirBuilder::new().mode(0o700).create(dir)?;
    for file in shim_files() {
        fs::write(dir.join(file.name), file.content)?;
    }
    Ok(())
}

/// What `termscribe init zsh` prints. Termscribe 0.1.0 asked users to evaluate this from
/// `~/.zshrc`. Now the terminal starts the relay, so the script does nothing inside a session.
/// A shell that no relay started still gets one, so a terminal that was set up the old way keeps
/// working until `termscribe setup` removes the line.
pub fn legacy_init_script(shell: Shell) -> String {
    match shell {
        Shell::Zsh => LEGACY_ZSH.to_string(),
    }
}

const LEGACY_ZSH: &str = r#"# termscribe no longer needs this line. Run `termscribe setup` to remove it.
if [[ -o interactive && -t 0 && -t 1 && -z $TERMSCRIBE_SOCKET && -z $TERMSCRIBE_DISABLED ]] \
   && (( $+commands[termscribe] )); then
  exec termscribe relay
fi
"#;

/// Quotes a word for a shell when it holds anything but plain path characters.
pub fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+@:~".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Writes bytes in the form `bindkey` expects, for example `\e[9000~`.
fn zsh_key(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x1b => "\\e".to_string(),
            b => (b as char).to_string(),
        })
        .collect()
}

// Each shim file leaves `ZDOTDIR` as the user's own value while it runs the user's file, so that
// file sees what it would see without termscribe. Then it points `ZDOTDIR` back at the shim
// directory so that zsh finds the next shim file. The `.zshrc` shim is the last one, and it
// leaves `ZDOTDIR` as the user's own value for good.

const ZSHENV: &str = r#"# termscribe zsh startup shim. termscribe writes this file for each terminal session and removes
# it when the session ends. It runs your own .zshenv.
typeset -g _termscribe_shim_dir=$ZDOTDIR
if [[ -n $TERMSCRIBE_USER_ZDOTDIR ]]; then
  ZDOTDIR=$TERMSCRIBE_USER_ZDOTDIR
else
  unset ZDOTDIR
fi
[[ -r ${ZDOTDIR:-$HOME}/.zshenv ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
# Your .zshenv may move ZDOTDIR. Your other startup files are then in the new place.
export TERMSCRIBE_USER_ZDOTDIR=${ZDOTDIR-}
ZDOTDIR=$_termscribe_shim_dir
"#;

const ZPROFILE: &str = r#"# termscribe zsh startup shim. It runs your own .zprofile.
if [[ -n $TERMSCRIBE_USER_ZDOTDIR ]]; then
  ZDOTDIR=$TERMSCRIBE_USER_ZDOTDIR
else
  unset ZDOTDIR
fi
[[ -r ${ZDOTDIR:-$HOME}/.zprofile ]] && source "${ZDOTDIR:-$HOME}/.zprofile"
ZDOTDIR=$_termscribe_shim_dir
"#;

const ZSHRC: &str = r#"# termscribe zsh startup shim. It runs your own .zshrc, then loads the termscribe hooks.
if [[ -n $TERMSCRIBE_USER_ZDOTDIR ]]; then
  ZDOTDIR=$TERMSCRIBE_USER_ZDOTDIR
else
  unset ZDOTDIR
fi
# zsh read /etc/zshrc while ZDOTDIR was the shim directory. On macOS that file sets HISTFILE from
# ZDOTDIR, so the history would be read from and saved to a directory that is deleted when the
# session ends. Point it at the place a shell without termscribe would use.
if [[ $HISTFILE == "$_termscribe_shim_dir"/* ]]; then
  HISTFILE=${ZDOTDIR:-$HOME}/${HISTFILE#"$_termscribe_shim_dir"/}
fi
[[ -r ${ZDOTDIR:-$HOME}/.zshrc ]] && source "${ZDOTDIR:-$HOME}/.zshrc"
# From here on ZDOTDIR is yours again, so zsh reads your .zlogin and .zlogout from your own
# directory and any shell you start inside this one ignores the shim.
if [[ -r $_termscribe_shim_dir/termscribe.zsh ]]; then
  source "$_termscribe_shim_dir/termscribe.zsh"
fi
unset _termscribe_shim_dir TERMSCRIBE_USER_ZDOTDIR
"#;

const HOOKS: &str = r#"# termscribe session hooks. termscribe writes this file for each terminal session.
# Inside a session, TERMSCRIBE_SOCKET names the relay. On another machine, `termscribe ssh`
# sets TERMSCRIBE_REMOTE (the session's secret) and TERMSCRIBE_HOST instead. That shell only
# sends marks through the connection, and the local relay does the rest.
if [[ -z $_termscribe_loaded ]] && [[ -n $TERMSCRIBE_SOCKET || -n $TERMSCRIBE_REMOTE ]]; then
  typeset -g _termscribe_loaded=1
  typeset -g _termscribe_remote=
  [[ -z $TERMSCRIBE_SOCKET ]] && _termscribe_remote=1

  if [[ -z $_termscribe_remote ]]; then
    typeset -g _termscribe_bin=${TERMSCRIBE_BIN:-termscribe}
    [[ -x $_termscribe_bin ]] || _termscribe_bin=termscribe

    # Ghostty loads its zsh integration only into a shell that it starts itself, and the relay
    # started this one. Load it here. Without it Ghostty cannot tell where the prompt is, so
    # Cmd+K stops telling the shell to redraw, and prompt jumping stops working.
    if [[ -n $GHOSTTY_RESOURCES_DIR ]] && (( ! $+_ghostty_state )) \
       && [[ -r $GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration ]]; then
      source "$GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration"
    fi
  fi

  typeset -g _termscribe_line=

  _termscribe_mark() {
    if [[ -n $_termscribe_remote ]]; then
      print -rn -- $'\e]7733;ts;@'"$TERMSCRIBE_REMOTE;$_termscribe_host;$1"$'\a' >/dev/tty 2>/dev/null
    else
      print -rn -- $'\e]7733;ts;'"$1"$'\a' >/dev/tty 2>/dev/null
    fi
  }

  # Escapes text for a mark. The result is in REPLY, so no subshell is needed.
  _termscribe_encode() {
    REPLY=${1//\%/%25}
    REPLY=${REPLY//$'\e'/%1B}
    REPLY=${REPLY//$'\a'/%07}
  }

  typeset -g _termscribe_host=
  if [[ -n $_termscribe_remote ]]; then
    _termscribe_encode "$TERMSCRIBE_HOST"
    _termscribe_host=${REPLY//;/%3B}
  fi

  # The mark payload `<dir>;<command>` for the current directory and the given text. On another
  # machine the local relay cannot know where home is, so the directory is written with `~`.
  _termscribe_where_and_what() {
    local place=$PWD
    if [[ -n $_termscribe_remote && -n $HOME && ( $PWD == "$HOME" || $PWD == "$HOME"/* ) ]]; then
      place="~${PWD#"$HOME"}"
    fi
    _termscribe_encode "$place"
    local dir=${REPLY//;/%3B}
    _termscribe_encode "$1"
    REPLY="$dir;$REPLY"
  }

  # zsh calls this for every line it reads, before it runs the line. `preexec` does not run for
  # a line zsh rejects, such as a syntax error, so the precmd below uses this to tell that
  # a line was read and never started.
  _termscribe_addhistory() {
    _termscribe_line=$1
    return 0
  }

  _termscribe_preexec() {
    _termscribe_line=
    if [[ -o histignorespace && $1 == ' '* ]]; then
      _termscribe_mark x
    else
      _termscribe_where_and_what "$1"
      _termscribe_mark "s;$REPLY"
    fi
  }

  _termscribe_precmd() {
    local code=$?
    if [[ -n $_termscribe_line ]]; then
      local line=${_termscribe_line%$'\n'}
      _termscribe_line=
      # The line was read and no command started. The relay has the error message that zsh
      # printed, and ignores the line when there was none (a comment, for example). zsh gives
      # no status for a rejected line, so say 1.
      if [[ -n ${line//[[:space:]]/} ]] && ! [[ -o histignorespace && $line == ' '* ]]; then
        _termscribe_where_and_what "$line"
        _termscribe_mark "r;$REPLY"
        code=1
      fi
    fi
    _termscribe_mark "e;$code"
  }

  zshaddhistory_functions=(_termscribe_addhistory $zshaddhistory_functions)
  preexec_functions+=(_termscribe_preexec)
  precmd_functions=(_termscribe_precmd $precmd_functions)

  if [[ -z $_termscribe_remote ]]; then
    # The copy shortcut reaches the shell only at a prompt. Show the result with zle -M.
    _termscribe_copy_widget() {
      local message
      message=$(command "$_termscribe_bin" 2>&1)
      zle -M -- "$message"
    }
    zle -N _termscribe_copy_widget
    bindkey -M emacs '@TRIGGER@' _termscribe_copy_widget 2>/dev/null
    bindkey -M viins '@TRIGGER@' _termscribe_copy_widget 2>/dev/null
    bindkey -M vicmd '@TRIGGER@' _termscribe_copy_widget 2>/dev/null

    # Short name for the command, unless something else already uses it.
    if (( ! $+commands[ts] && ! $+functions[ts] && ! $+aliases[ts] )); then
      ts() { command "$_termscribe_bin" "$@"; }
    fi
  fi
fi
"#;

/// The startup file for bash on another machine. It starts like a login shell, then adds the
/// hooks. bash has no hook that runs before a command and knows the whole line, so a DEBUG trap
/// marks the start of the first command after each prompt, and `PROMPT_COMMAND` marks the end.
/// The shell's own history supplies the text of the line.
const REMOTE_BASHRC: &str = r##"# termscribe startup file for bash. `termscribe ssh` writes it for this session and removes it
# when the session ends. It runs your own startup files as a login shell would.
if [ -r /etc/profile ]; then . /etc/profile; fi
for _termscribe_file in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
  if [ -r "$_termscribe_file" ]; then . "$_termscribe_file"; break; fi
done
unset _termscribe_file

if [ -n "$TERMSCRIBE_REMOTE" ] && [ -z "$_termscribe_loaded" ]; then
  if [ -n "$(trap -p DEBUG)" ]; then
    echo "termscribe: your startup files already set a DEBUG trap, so commands on this machine are not recorded one by one." >&2
  else
    _termscribe_loaded=1
    # 1 while the shell waits at a prompt, so the next command is the start of a new line.
    _termscribe_ready=0
    _termscribe_status=0
    _termscribe_histnum=

    _termscribe_mark() {
      printf '\033]7733;ts;@%s;%s;%s\007' "$TERMSCRIBE_REMOTE" "$_termscribe_host" "$1" >/dev/tty 2>/dev/null
    }

    # Escapes text for a mark. The result is in REPLY.
    _termscribe_encode() {
      REPLY=${1//\%/%25}
      REPLY=${REPLY//$'\033'/%1B}
      REPLY=${REPLY//$'\007'/%07}
    }

    _termscribe_encode "$TERMSCRIBE_HOST"
    _termscribe_host=${REPLY//;/%3B}

    # The mark payload `<dir>;<command>`. The directory is written with `~` for home, because
    # the relay cannot know where home is on this machine.
    _termscribe_where_and_what() {
      local place=$PWD rest
      if [ -n "$HOME" ] && { [ "$PWD" = "$HOME" ] || [[ $PWD == "$HOME"/* ]]; }; then
        rest=${PWD#"$HOME"}
        place="~$rest"
      fi
      _termscribe_encode "$place"
      local dir=${REPLY//;/%3B}
      _termscribe_encode "$1"
      REPLY="$dir;$REPLY"
    }

    # Runs before every simple command. Only the first one after a prompt starts a command.
    _termscribe_debug() {
      [ "$_termscribe_ready" = 1 ] || return 0
      [ -z "$COMP_LINE" ] || return 0
      _termscribe_ready=0
      local entry num text
      entry=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
      entry=${entry#"${entry%%[![:space:]]*}"}
      num=${entry%%[!0-9]*}
      text=${entry#"$num"}
      text=${text:2}
      if [ -z "$num" ]; then
        # No history to read, so the first command of the line is the best there is.
        text=$BASH_COMMAND
      elif [ "$num" = "$_termscribe_histnum" ] && [[ $HISTCONTROL == *ignorespace* || $HISTCONTROL == *ignoreboth* ]]; then
        # The line did not enter the history, either because it is a repeat or because it starts
        # with a space. A repeat starts like the previous line. Anything else is a private line.
        case $text in
          "$BASH_COMMAND"*) ;;
          *) case $BASH_COMMAND in "$text"*) ;; *) _termscribe_mark x; return 0 ;; esac ;;
        esac
      fi
      _termscribe_histnum=$num
      _termscribe_where_and_what "$text"
      _termscribe_mark "s;$REPLY"
      return 0
    }

    # PROMPT_COMMAND runs ours first, to read the status before anything changes it and to send
    # the end mark before your own prompt commands print anything, and last, to arm the trap
    # for the next line.
    _termscribe_first() {
      _termscribe_status=$?
      _termscribe_mark "e;$_termscribe_status"
      return $_termscribe_status
    }
    _termscribe_last() {
      _termscribe_ready=1
    }
    if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
      PROMPT_COMMAND=(_termscribe_first "${PROMPT_COMMAND[@]}" _termscribe_last)
    else
      PROMPT_COMMAND="_termscribe_first"$'\n'"${PROMPT_COMMAND}"$'\n'"_termscribe_last"
    fi
    trap '_termscribe_debug' DEBUG
  fi
fi
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> String {
        shim_files()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no shim file {name}"))
            .content
    }

    #[test]
    fn hooks_bind_the_copy_trigger() {
        let hooks = file("termscribe.zsh");
        assert!(hooks.contains(r"'\e[9000~'"));
        assert!(!hooks.contains("@TRIGGER@"));
    }

    #[test]
    fn the_bash_startup_file_parses_and_reads_the_remote_variables() {
        let files = bash_files();
        assert_eq!(files.len(), 1);
        assert!(files[0].content.contains("TERMSCRIBE_REMOTE"));
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("termscribe.bashrc");
        std::fs::write(&path, &files[0].content).expect("write");
        if let Ok(status) = std::process::Command::new("bash")
            .arg("-n")
            .arg(&path)
            .status()
        {
            assert!(status.success());
        }
    }

    #[test]
    fn the_shim_has_one_file_per_startup_stage() {
        let names: Vec<_> = shim_files().into_iter().map(|f| f.name).collect();
        assert_eq!(names, [".zshenv", ".zprofile", ".zshrc", "termscribe.zsh"]);
    }

    #[test]
    fn the_shim_reads_the_environment_names_the_relay_sets() {
        assert!(file(".zshenv").contains(USER_ZDOTDIR_ENV));
        assert!(file("termscribe.zsh").contains(BIN_ENV));
    }

    #[test]
    fn every_zsh_file_parses() {
        let dir = tempfile::tempdir().expect("temp dir");
        for shim in shim_files() {
            let path = dir.path().join(shim.name);
            std::fs::write(&path, shim.content).expect("write");
            let status = std::process::Command::new("zsh")
                .arg("-n")
                .arg(&path)
                .status();
            if let Ok(status) = status {
                assert!(status.success(), "{} does not parse", shim.name);
            }
        }
    }

    #[test]
    fn write_shim_creates_a_private_directory_with_every_file() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().expect("temp dir");
        let dir = parent.path().join("1.zdot");
        write_shim(&dir).expect("write shim");
        let mode = fs::metadata(&dir).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        for name in [".zshenv", ".zprofile", ".zshrc", "termscribe.zsh"] {
            assert!(dir.join(name).is_file(), "{name} missing");
        }
        // Writing again replaces what is there.
        fs::write(dir.join("stale"), "x").expect("stale");
        write_shim(&dir).expect("write shim again");
        assert!(!dir.join("stale").exists());
    }

    #[test]
    fn shell_word_quotes_only_when_needed() {
        assert_eq!(
            shell_word("/opt/homebrew/bin/termscribe"),
            "/opt/homebrew/bin/termscribe"
        );
        assert_eq!(shell_word("/my dir/termscribe"), "'/my dir/termscribe'");
        assert_eq!(shell_word("/it's/termscribe"), r"'/it'\''s/termscribe'");
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn the_legacy_init_script_starts_a_relay_only_outside_a_session() {
        let script = legacy_init_script(Shell::Zsh);
        assert!(script.starts_with("# termscribe no longer needs this line"));
        assert!(script.contains("-z $TERMSCRIBE_SOCKET"));
        assert!(script.contains("exec termscribe relay"));
    }

    #[test]
    fn the_legacy_init_script_parses() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("init.zsh");
        std::fs::write(&path, legacy_init_script(Shell::Zsh)).expect("write");
        if let Ok(status) = std::process::Command::new("zsh")
            .arg("-n")
            .arg(&path)
            .status()
        {
            assert!(status.success());
        }
    }
}
