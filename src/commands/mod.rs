//! Subcommand handlers.

mod init;
mod lifecycle;
mod provision;
mod ssh;

use std::process::ExitCode;

use anyhow::Result;

use crate::cli::Command;

pub fn dispatch(command: Command) -> Result<ExitCode> {
    match command {
        Command::Init => init::run().map(ok),
        Command::Up => lifecycle::up().map(ok),
        Command::Ssh { cmd } => ssh::run(cmd),
        Command::Status => lifecycle::status().map(ok),
        Command::Down => lifecycle::down().map(ok),
        Command::Reload => lifecycle::reload().map(ok),
        Command::Destroy => lifecycle::destroy().map(ok),
        Command::Provision => provision::run().map(ok),
    }
}

fn ok(_: ()) -> ExitCode {
    ExitCode::SUCCESS
}
