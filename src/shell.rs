//! Shell integration scripts.

use clap::ValueEnum;

use crate::input::COPY_TRIGGER;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Zsh,
}

/// The script that `termscribe init <shell>` prints.
pub fn init_script(shell: Shell) -> String {
    match shell {
        Shell::Zsh => ZSH.replace("@TRIGGER@", &zsh_key(COPY_TRIGGER)),
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

const ZSH: &str = r#"# termscribe shell integration for zsh.
# Put this line at the very top of ~/.zshrc:
#   eval "$(termscribe init zsh)"
if [[ -o interactive && -t 0 && -t 1 ]]; then
  if [[ -z $TERMSCRIBE_SOCKET && -z $TERMSCRIBE_DISABLED ]]; then
    # Not inside a relay yet. Replace this shell with one. The relay falls back to a plain
    # shell by itself when it cannot start.
    if (( $+commands[termscribe] )); then
      exec termscribe relay
    fi
  elif [[ -n $TERMSCRIBE_SOCKET ]]; then
    # Only the first shell of a session records commands. Nested shells run inside the
    # output of the command that started them.
    [[ -n $TERMSCRIBE_OWNER ]] || export TERMSCRIBE_OWNER=$$
    if [[ $TERMSCRIBE_OWNER == $$ && -z $_termscribe_loaded ]]; then
      typeset -g _termscribe_loaded=1

      # Ghostty loads its zsh integration only into the shell it starts itself, and the relay
      # replaced that shell. Load it here. Without it Ghostty cannot tell where the prompt is,
      # so Cmd+K stops telling the shell to redraw, and prompt jumping stops working.
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
        message=$(command termscribe 2>&1)
        zle -M -- "$message"
      }
      zle -N _termscribe_copy_widget
      bindkey -M emacs '@TRIGGER@' _termscribe_copy_widget 2>/dev/null
      bindkey -M viins '@TRIGGER@' _termscribe_copy_widget 2>/dev/null
      bindkey -M vicmd '@TRIGGER@' _termscribe_copy_widget 2>/dev/null

      preexec_functions+=(_termscribe_preexec)
      precmd_functions=(_termscribe_precmd $precmd_functions)
    fi
    # Short name for the command, unless something else already uses it.
    if (( ! $+commands[ts] && ! $+functions[ts] && ! $+aliases[ts] )); then
      ts() { command termscribe "$@"; }
    fi
  fi
fi
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_binds_the_copy_trigger() {
        let script = init_script(Shell::Zsh);
        assert!(script.contains(r"'\e[9000~'"));
        assert!(!script.contains("@TRIGGER@"));
    }
}
