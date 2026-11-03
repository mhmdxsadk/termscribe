//! The `termscribe` command line.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::clipboard::system_clipboard;
use crate::ipc::{self, Request, Response, SOCKET_ENV};
use crate::relay;
use crate::shell::{Shell, init_script};
use crate::transcript::Selection;

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

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Init { shell }) => {
            print!("{}", init_script(shell));
            ExitCode::SUCCESS
        }
        Some(Command::Relay { shell }) => {
            let code = relay::run(shell);
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        None => match run_copy(&cli) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("termscribe: {error:#}");
                ExitCode::FAILURE
            }
        },
    }
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
