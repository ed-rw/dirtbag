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
        Command::On => lifecycle::on().map(ok),
        Command::Ssh { cmd } => ssh::run(cmd),
        Command::Status => lifecycle::status().map(ok),
        Command::Stop => lifecycle::stop().map(ok),
        Command::Reload => lifecycle::reload().map(ok),
        Command::Destroy => lifecycle::destroy().map(ok),
        Command::Provision => provision::run().map(ok),
    }
}

fn ok(_: ()) -> ExitCode {
    ExitCode::SUCCESS
}
