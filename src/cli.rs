//! The `termscribe` command line.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::clipboard::system_clipboard;
use crate::doctor;
use crate::ipc::{self, Request, Response, SOCKET_ENV};
use crate::relay;
use crate::shell::{Shell, init_script};
use crate::transcript::Selection;
use crate::zshrc::{self, Change};

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
    /// Add `eval "$(termscribe init zsh)"` as the first line of ~/.zshrc
    #[command(long_flag = "setup")]
    Setup,
    /// Remove `eval "$(termscribe init zsh)"` from ~/.zshrc
    #[command(long_flag = "remove")]
    Remove,
    /// Check that termscribe is installed and working
    #[command(long_flag = "doctor")]
    Doctor,
    /// Print the shell integration to evaluate from your shell's startup file
    Init {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Run the shell inside a relay. The shell integration calls this
    #[command(hide = true)]
    Relay {
        /// The shell program to run
        #[arg(long)]
        shell: Option<String>,
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
            print!("{}", init_script(shell));
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

fn run_setup() -> Result<()> {
    let path = zshrc::zshrc_path()?;
    let shown = path.display();
    match zshrc::setup(&path)? {
        Change::Added => println!("Added the termscribe line to the top of {shown}."),
        Change::Moved => println!("Moved the termscribe line to the top of {shown}."),
        Change::Unchanged => {
            println!("{shown} already starts with the termscribe line.");
            return Ok(());
        }
    }
    if doctor::find_in_path("termscribe").is_none() {
        eprintln!(
            "warning: termscribe is not on PATH, so new shells will start without it. \
             Add its install directory to PATH."
        );
    }
    println!("Open a new terminal to start a session.");
    Ok(())
}

fn run_remove() -> Result<()> {
    let path = zshrc::zshrc_path()?;
    let shown = path.display();
    if zshrc::remove(&path)? == 0 {
        println!("No termscribe line in {shown}.");
    } else {
        println!(
            "Removed the termscribe line from {shown}. Terminals that are already open keep \
             their session until you close them."
        );
    }
    Ok(())
}

fn session_socket() -> Result<PathBuf> {
    match std::env::var_os(SOCKET_ENV) {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => bail!(
            "this shell is not running inside a termscribe session. Add \
             `eval \"$(termscribe init zsh)\"` to the top of ~/.zshrc and open a new terminal"
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
            println!("No commands to copy.");
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
            "init",
            "N",
        ] {
            assert!(listed(name), "{name} missing:\n{help}");
        }
        assert!(
            help.lines()
                .all(|line| !line.trim_start().starts_with("--")),
            "{help}"
        );
        assert!(!help.contains("relay"), "{help}");
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
