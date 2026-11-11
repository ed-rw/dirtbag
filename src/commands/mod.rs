//! Subcommand handlers.

mod init;
mod lifecycle;
mod provision;
mod ssh;
mod supervise;
mod update;
mod version;

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;

use crate::cli::Command;

/// Run a command. `file` is the optional `--file` override. `instance` is the
/// optional `--instance` label. Each VM command uses both to find its project
/// (see [`crate::config::Project::find`]).
pub fn dispatch(command: Command, file: Option<&Path>, instance: Option<&str>) -> Result<ExitCode> {
    match command {
        Command::Init => init::run(file).map(ok),
        Command::Up => lifecycle::up(file, instance).map(ok),
        Command::Ssh { cmd } => ssh::run(file, instance, cmd),
        Command::Status => lifecycle::status(file, instance).map(ok),
        Command::Down => lifecycle::down(file, instance).map(ok),
        Command::Reload => lifecycle::reload(file, instance).map(ok),
        Command::Destroy => lifecycle::destroy(file, instance).map(ok),
        Command::Provision => provision::run(file, instance).map(ok),
        Command::Version => version::run().map(ok),
        Command::Update => update::run().map(ok),
        Command::Supervise {
            vm,
            forwards,
            child,
        } => supervise::run(&vm, &forwards, &child),
    }
}

fn ok(_: ()) -> ExitCode {
    ExitCode::SUCCESS
}
