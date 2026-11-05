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
[[ -r ${ZDOTDIR:-$HOME}/.zshrc ]] && source "${ZDOTDIR:-$HOME}/.zshrc"
# From here on ZDOTDIR is yours again, so zsh reads your .zlogin and .zlogout from your own
# directory and any shell you start inside this one ignores the shim.
if [[ -r $_termscribe_shim_dir/termscribe.zsh ]]; then
  source "$_termscribe_shim_dir/termscribe.zsh"
fi
unset _termscribe_shim_dir TERMSCRIBE_USER_ZDOTDIR
"#;

const HOOKS: &str = r#"# termscribe session hooks. termscribe writes this file for each terminal session.
if [[ -n $TERMSCRIBE_SOCKET && -z $_termscribe_loaded ]]; then
  typeset -g _termscribe_loaded=1
  typeset -g _termscribe_bin=${TERMSCRIBE_BIN:-termscribe}
  [[ -x $_termscribe_bin ]] || _termscribe_bin=termscribe

  # Ghostty loads its zsh integration only into a shell that it starts itself, and the relay
  # started this one. Load it here. Without it Ghostty cannot tell where the prompt is, so Cmd+K
  # stops telling the shell to redraw, and prompt jumping stops working.
  if [[ -n $GHOSTTY_RESOURCES_DIR ]] && (( ! $+_ghostty_state )) \
     && [[ -r $GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration ]]; then
    source "$GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration"
  fi

  _termscribe_mark() {
    print -rn -- $'\e]7733;ts;'"$1"$'\a' >/dev/tty 2>/dev/null
  }

  _termscribe_preexec() {
    if [[ -o histignorespace && $1 == ' '* ]]; then
      _termscribe_mark x
    else
      local cmd=${1//\%/%25}
      cmd=${cmd//$'\e'/%1B}
      cmd=${cmd//$'\a'/%07}
      local dir=${PWD//\%/%25}
      dir=${dir//$'\e'/%1B}
      dir=${dir//$'\a'/%07}
      dir=${dir//;/%3B}
      _termscribe_mark "s;$dir;$cmd"
    fi
  }

  _termscribe_precmd() {
    local code=$?
    _termscribe_mark "e;$code"
  }

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

  preexec_functions+=(_termscribe_preexec)
  precmd_functions=(_termscribe_precmd $precmd_functions)

  # Short name for the command, unless something else already uses it.
  if (( ! $+commands[ts] && ! $+functions[ts] && ! $+aliases[ts] )); then
    ts() { command "$_termscribe_bin" "$@"; }
  fi
fi
"#;

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
