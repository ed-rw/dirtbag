//! File copy-in and provisioning over SSH.

use anyhow::{bail, Context, Result};
use base64::Engine;
use tracing::info;

use crate::config::{Project, Provision};
use crate::ssh::Ssh;

/// Copy each `[[copy]]` file into the guest (one-shot; not kept in sync).
pub fn run_copies(ssh: &Ssh, project: &Project) -> Result<()> {
    for c in &project.config.copies {
        let src = project.resolve_host_path(&c.source);
        let data = std::fs::read(&src).with_context(|| format!("reading {src}"))?;
        ssh.upload(&data, &c.target, 0o644)
            .with_context(|| format!("copying {src} -> {}", c.target))?;
        info!(target = %c.target, "copied file");
    }
    Ok(())
}

/// Run each `[[provision]]` step in order. Stop at the first non-zero exit.
pub fn run_provisions(ssh: &Ssh, project: &Project) -> Result<()> {
    for (i, step) in project.config.provisions.iter().enumerate() {
        let n = i + 1;
        let script = script_text(project, step)?;
        let shell = step.shell.as_deref().unwrap_or("bash");
        let runner = if step.privileged {
            format!("sudo {shell}")
        } else {
            shell.to_string()
        };
        // Send the script as base64 to prevent shell syntax conflicts.
        let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
        let cmd = format!("echo '{b64}' | base64 -d | {runner}");

        info!(step = n, "provisioning");
        let code = ssh.exec_streaming(&cmd)?;
        if code != 0 {
            bail!("provision step #{n} failed (exit {code})");
        }
    }
    Ok(())
}

fn script_text(project: &Project, step: &Provision) -> Result<String> {
    if let Some(inline) = &step.inline {
        return Ok(inline.clone());
    }
    let path = step
        .path
        .as_ref()
        .expect("config validation guarantees inline or path");
    let full = project.resolve_host_path(path);
    std::fs::read_to_string(&full).with_context(|| format!("reading provision script {full}"))
}
