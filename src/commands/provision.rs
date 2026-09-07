use std::path::Path;

use anyhow::{Context, Result};

use crate::config::Project;
use crate::ssh::{SSH_PORT, Ssh};
use crate::tart::Tart;

/// `dirtbag provision` — re-run provisioners against the running VM.
pub fn run(file: Option<&Path>) -> Result<()> {
    let project = Project::find(file)?;
    let tart = Tart::locate()?;
    let ip = tart
        .ip(&project.vm_name())?
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
