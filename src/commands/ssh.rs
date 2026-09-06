use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::config::Project;
use crate::ssh::{Ssh, SSH_PORT};
use crate::state::State;
use crate::tart::Tart;

/// `dirtbag ssh [-- CMD...]` — interactive shell, or run a command.
///
/// Returns the remote exit status as the process exit code.
pub fn run(cmd: Vec<String>) -> Result<ExitCode> {
    let project = Project::discover_cwd()?;
    let state = State::load(&project.root)?
        .context("no dirtbag state; run `dirtbag on` first")?;
    let tart = Tart::locate()?;
    let ip = tart
        .ip(&state.vm_name)?
        .context("VM has no IP — is it running? try `dirtbag on`")?;

    let ssh = Ssh::connect(
        &ip,
        SSH_PORT,
        &project.config.ssh.user,
        &project.config.ssh.password,
    )?;

    let code = if cmd.is_empty() {
        ssh.shell()?
    } else {
        ssh.exec_streaming(&cmd.join(" "))?
    };

    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}
