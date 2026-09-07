//! Subcommand handlers.

mod init;
mod lifecycle;
mod provision;
mod ssh;
mod version;

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;

use crate::cli::Command;

/// Run a command. `file` is the optional `--file` override; every command
/// resolves its project through it (see [`crate::config::Project::find`]).
pub fn dispatch(command: Command, file: Option<&Path>) -> Result<ExitCode> {
    match command {
        Command::Init => init::run(file).map(ok),
        Command::Up => lifecycle::up(file).map(ok),
        Command::Ssh { cmd } => ssh::run(file, cmd),
        Command::Status => lifecycle::status(file).map(ok),
        Command::Down => lifecycle::down(file).map(ok),
        Command::Reload => lifecycle::reload(file).map(ok),
        Command::Destroy => lifecycle::destroy(file).map(ok),
        Command::Provision => provision::run(file).map(ok),
        Command::Version => version::run().map(ok),
    }
}

fn ok(_: ()) -> ExitCode {
    ExitCode::SUCCESS
}
