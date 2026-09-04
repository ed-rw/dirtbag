use anyhow::{Context, Result};

use crate::config::Project;
use crate::ssh::{Ssh, SSH_PORT};
use crate::state::State;
use crate::tart::Tart;

/// `dirtbag provision` — re-run provisioners against the running VM.
pub fn run() -> Result<()> {
    let project = Project::discover_cwd()?;
    let state = State::load(&project.root)?
        .context("no dirtbag state; run `dirtbag up` first")?;
    let tart = Tart::locate()?;
    let ip = tart
        .ip(&state.vm_name)?
        .context("VM has no IP — is it running? try `dirtbag up`")?;

    let ssh = Ssh::connect(
        &ip,
        SSH_PORT,
        &project.config.ssh.user,
        &project.config.ssh.password,
    )?;
    crate::provision::run_provisions(&ssh, &project)?;
    println!("provisioning complete");
    Ok(())
}
