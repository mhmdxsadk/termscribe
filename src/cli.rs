//! The `termscribe` command line.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::clipboard::system_clipboard;
use crate::doctor;
use crate::ghostty::{self, Change, KeybindChange};
use crate::ipc::{self, Request, Response, SOCKET_ENV};
use crate::relay;
use crate::shell::{Shell, legacy_init_script};
use crate::transcript::Selection;
use crate::zshrc;

#[derive(Debug, Parser)]
#[command(
    name = "termscribe",
    version,
    about = "Copy a clean Markdown transcript of recent shell commands and their output",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Copy the last N commands instead of everything since the boundary
    #[arg(value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    count: Option<u32>,

    /// Copy every command the session still remembers
    #[arg(long, conflicts_with_all = ["count", "clear"])]
    all: bool,

    /// Start a new transcript from now on. The terminal is not cleared
    #[arg(long, conflicts_with_all = ["count", "all"])]
    clear: bool,

    /// Print the Markdown instead of copying it
    #[arg(long)]
    stdout: bool,

    /// Print nothing on success
    #[arg(long, short)]
    quiet: bool,

    /// Leave out the `[exit N]` line after commands that failed
    #[arg(long)]
    no_status: bool,

    /// Leave out the working directory
    #[arg(long)]
    no_cwd: bool,

    /// Keep ANSI escape sequences in the output
    #[arg(long)]
    raw_ansi: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Make Ghostty start every new terminal through termscribe
    #[command(long_flag = "setup")]
    Setup,
    /// Undo setup
    #[command(long_flag = "remove")]
    Remove,
    /// Check that termscribe is installed and working
    #[command(long_flag = "doctor")]
    Doctor,
    /// Run your shell inside termscribe. Set your terminal to run this instead of the shell
    Relay {
        /// The shell to run. Defaults to your login shell, which must be zsh
        #[arg(long)]
        shell: Option<String>,
    },
    /// What termscribe 0.1.0 asked you to evaluate in ~/.zshrc
    #[command(hide = true)]
    Init {
        #[arg(value_enum)]
        shell: Shell,
    },
}

/// Flags that also work as bare words, so `termscribe clear` means `termscribe --clear`. The
/// maintenance commands are subcommands and accept both forms by themselves.
const BARE_FLAGS: &[&str] = &[
    "all",
    "clear",
    "stdout",
    "quiet",
    "no-status",
    "no-cwd",
    "raw-ansi",
    "version",
];

/// Rewrites each argument that is exactly a flag name into the dashed flag. The first item is
/// the program name and stays as it is. A count such as `3` never matches a flag name.
pub fn accept_bare_flags<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    args.into_iter()
        .enumerate()
        .map(|(index, arg)| match arg.to_str() {
            Some(word) if index > 0 && BARE_FLAGS.contains(&word) => format!("--{word}").into(),
            _ => arg,
        })
        .collect()
}

/// The help text, with flags shown by their bare names. Clap prints `--all`. This prints `all`,
/// with the short form after it where one exists. Every name and description comes from the
/// definitions above, so the text cannot drift from the real flags.
fn bare_help(command: &clap::Command) -> String {
    let mut command = command.clone();
    command.build();

    let mut commands: Vec<(String, String)> = Vec::new();
    for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
        let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
        commands.push((sub.get_name().to_string(), about));
    }
    let mut arguments: Vec<(String, String)> = Vec::new();
    for arg in command.get_positionals() {
        let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
        let name = arg
            .get_value_names()
            .and_then(|names| names.first())
            .map_or_else(|| arg.get_id().to_string(), ToString::to_string);
        arguments.push((name, help));
    }
    let mut options: Vec<(String, String)> = Vec::new();
    for arg in command
        .get_arguments()
        .filter(|arg| arg.get_long().is_some())
    {
        let mut label = arg.get_long().unwrap_or_default().to_string();
        if let Some(short) = arg.get_short() {
            label.push_str(&format!(", -{short}"));
        }
        let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
        options.push((label, help));
    }

    let mut out = String::new();
    if let Some(about) = command.get_about() {
        out.push_str(&format!("{about}\n\n"));
    }
    out.push_str("Usage: termscribe [OPTIONS] [N]\n       termscribe <COMMAND>\n");
    for (title, rows) in [
        ("Commands", &commands),
        ("Arguments", &arguments),
        ("Options", &options),
    ] {
        let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
        out.push_str(&format!("\n{title}:\n"));
        for (label, help) in rows {
            out.push_str(&format!("  {label:<width$}  {help}\n"));
        }
    }
    out.push_str("\nOptions also work with leading dashes, for example --all.\n");
    out
}

fn parse_cli(args: Vec<OsString>) -> Result<Cli, clap::Error> {
    let command = Cli::command();
    let command = command.clone().override_help(bare_help(&command));
    let matches = command.try_get_matches_from(accept_bare_flags(args))?;
    Cli::from_arg_matches(&matches)
}

pub fn main() -> ExitCode {
    let cli = parse_cli(std::env::args_os().collect()).unwrap_or_else(|error| error.exit());
    match cli.command {
        Some(Command::Init { shell }) => {
            print!("{}", legacy_init_script(shell));
            ExitCode::SUCCESS
        }
        Some(Command::Relay { shell }) => {
            let code = relay::run(shell);
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Some(Command::Setup) => finish(run_setup()),
        Some(Command::Remove) => finish(run_remove()),
        Some(Command::Doctor) => {
            let checks = doctor::run();
            print!("{}", doctor::render(&checks));
            if doctor::has_failure(&checks) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        None => finish(run_copy(&cli)),
    }
}

fn finish(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("termscribe: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Where the terminal should find this binary. A path the user typed in full wins. Otherwise it
/// is the first `termscribe` in `PATH`, which stays valid after an upgrade, unlike the binary's
/// own location inside Homebrew's versioned folder.
fn program_path() -> Result<PathBuf> {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| doctor::find_in_path("termscribe"))
        .or_else(|| std::env::current_exe().ok())
        .context("could not find where termscribe is installed")
}

fn run_setup() -> Result<()> {
    let program = program_path()?;
    let line = ghostty::relay_line(&program)?;
    if !ghostty::is_installed() {
        bail!(
            "Ghostty was not found. In your terminal's settings, run `{} relay` instead of \
             your shell",
            program.display()
        );
    }
    let config = ghostty::config_to_edit()?;
    let shown = config.display();
    let done = ghostty::setup(&config, &line)?;
    match done.command {
        Change::Added => println!("Added `{line}` to {shown}."),
        Change::Updated => println!("Updated the termscribe command in {shown}."),
        Change::Unchanged => println!("{shown} already starts termscribe."),
    }
    match &done.keybind {
        KeybindChange::Added => println!(
            "Added `{}` to {shown}, so Cmd+Alt+C copies the transcript.",
            ghostty::keybind_line()
        ),
        KeybindChange::Unchanged => {}
        KeybindChange::Taken(action) => println!(
            "Left Cmd+Alt+C alone, because it is already bound to `{action}` in {shown}. \
             Bind any key to `{}` to copy with a shortcut.",
            ghostty::copy_action()
        ),
    }
    remove_old_zshrc_line()?;
    let changed = done.command != Change::Unchanged || done.keybind == KeybindChange::Added;
    if changed {
        println!(
            "Reload the Ghostty config (Cmd+Shift+,) or restart Ghostty, then open a new tab."
        );
    }
    Ok(())
}

fn run_remove() -> Result<()> {
    let mut removed = 0;
    for config in ghostty::config_paths() {
        let done = ghostty::remove(&config)?;
        if done.command > 0 {
            println!("Removed the termscribe command from {}.", config.display());
        }
        if done.keybind > 0 {
            println!("Removed the copy shortcut from {}.", config.display());
        }
        removed += done.command + done.keybind;
    }
    removed += remove_old_zshrc_line()?;
    if removed == 0 {
        println!("termscribe is not set up, so there is nothing to remove.");
    } else {
        println!(
            "Terminals that are already open keep their session until you close them. \
             Reload the Ghostty config (Cmd+Shift+,) to stop new tabs from using termscribe."
        );
    }
    Ok(())
}

/// Takes out the line that termscribe 0.1.0 added to `~/.zshrc`. Returns how many it removed.
fn remove_old_zshrc_line() -> Result<usize> {
    let path = zshrc::zshrc_path()?;
    let removed = zshrc::remove(&path)?;
    if removed > 0 {
        println!(
            "Removed the old termscribe line from {}. It is not needed any more.",
            path.display()
        );
    }
    Ok(removed)
}

fn session_socket() -> Result<PathBuf> {
    match std::env::var_os(SOCKET_ENV) {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => bail!(
            "this shell is not running inside a termscribe session. Run `termscribe setup`, \
             then open a new terminal tab"
        ),
    }
}

fn run_copy(cli: &Cli) -> Result<()> {
    let socket = session_socket()?;

    if cli.clear {
        match ipc::request(&socket, &Request::Clear)? {
            Response::Cleared => {
                if !cli.quiet {
                    println!("Started a new transcript.");
                }
                return Ok(());
            }
            Response::Error(message) => bail!("{message}"),
            other => bail!("unexpected reply from the session: {other:?}"),
        }
    }

    let selection = if cli.all {
        Selection::All
    } else if let Some(count) = cli.count {
        Selection::Last(count as usize)
    } else {
        Selection::SinceBoundary
    };
    let request = Request::Render {
        selection,
        show_status: !cli.no_status,
        show_cwd: !cli.no_cwd,
        keep_ansi: cli.raw_ansi,
    };
    let (markdown, commands) = match ipc::request(&socket, &request)? {
        Response::Rendered { markdown, commands } => (markdown, commands),
        Response::Error(message) => bail!("{message}"),
        other => bail!("unexpected reply from the session: {other:?}"),
    };

    if cli.stdout {
        std::io::stdout()
            .write_all(markdown.as_bytes())
            .context("could not write to stdout")?;
        return Ok(());
    }
    if commands == 0 {
        if !cli.quiet {
            if doctor::ghostty_needs_setup() {
                println!(
                    "termscribe is not set up in Ghostty yet, so new tabs are not captured. \
                     Run `termscribe setup`."
                );
            } else {
                println!("No commands to copy.");
            }
        }
        return Ok(());
    }
    system_clipboard()
        .copy(&markdown)
        .context("could not copy to the clipboard")?;
    if !cli.quiet {
        let noun = if commands == 1 { "command" } else { "commands" };
        println!("Copied {commands} {noun} to the clipboard.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let args = std::iter::once("termscribe").chain(args.iter().copied());
        Cli::try_parse_from(accept_bare_flags(args.map(OsString::from)))
    }

    fn command(args: &[&str]) -> Option<Command> {
        parse(args).expect("parses").command
    }

    #[test]
    fn maintenance_commands_work_with_and_without_dashes() {
        assert!(matches!(command(&["setup"]), Some(Command::Setup)));
        assert!(matches!(command(&["--setup"]), Some(Command::Setup)));
        assert!(matches!(command(&["remove"]), Some(Command::Remove)));
        assert!(matches!(command(&["--remove"]), Some(Command::Remove)));
        assert!(matches!(command(&["doctor"]), Some(Command::Doctor)));
        assert!(matches!(command(&["--doctor"]), Some(Command::Doctor)));
    }

    #[test]
    fn maintenance_commands_refuse_company() {
        assert!(parse(&["setup", "--quiet"]).is_err());
        assert!(parse(&["--setup", "--all"]).is_err());
        assert!(parse(&["doctor", "3"]).is_err());
        assert!(parse(&["setup", "remove"]).is_err());
    }

    #[test]
    fn help_shows_bare_names_for_every_flag_and_command() {
        let help = bare_help(&Cli::command());
        let listed = |name: &str| {
            help.lines().any(|line| {
                line.strip_prefix("  ")
                    .and_then(|rest| rest.strip_prefix(name))
                    .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with(','))
            })
        };
        for name in [
            "all",
            "clear",
            "stdout",
            "quiet",
            "no-status",
            "no-cwd",
            "raw-ansi",
            "setup",
            "remove",
            "doctor",
            "relay",
            "N",
        ] {
            assert!(listed(name), "{name} missing:\n{help}");
        }
        assert!(
            help.lines()
                .all(|line| !line.trim_start().starts_with("--")),
            "{help}"
        );
        assert!(!help.contains("init"), "{help}");
    }

    #[test]
    fn copy_flags_still_parse() {
        let cli = parse(&["--all", "--stdout"]).expect("copy flags");
        assert!(cli.all && cli.stdout && cli.command.is_none());
        assert_eq!(parse(&["5"]).expect("count").count, Some(5));
    }

    #[test]
    fn every_flag_works_without_dashes() {
        let cli = parse(&["all", "stdout", "quiet"]).expect("bare flags");
        assert!(cli.all && cli.stdout && cli.quiet);
        let cli = parse(&["no-status", "no-cwd", "raw-ansi"]).expect("bare flags");
        assert!(cli.no_status && cli.no_cwd && cli.raw_ansi);
        assert!(parse(&["clear"]).expect("clear").clear);
    }

    #[test]
    fn bare_and_dashed_flags_mix_and_keep_their_rules() {
        let cli = parse(&["3", "stdout", "--quiet"]).expect("mixed");
        assert_eq!(cli.count, Some(3));
        assert!(cli.stdout && cli.quiet);
        assert!(parse(&["all", "clear"]).is_err());
        assert!(parse(&["3", "all"]).is_err());
    }

    #[test]
    fn the_program_name_is_never_rewritten() {
        let args = accept_bare_flags(["clear", "clear"].map(OsString::from));
        assert_eq!(args, ["clear", "--clear"].map(OsString::from));
    }
}
