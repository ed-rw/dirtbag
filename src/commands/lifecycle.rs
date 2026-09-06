//! VM lifecycle commands: `up`, `status`, `down`, `reload`, `destroy`.
//!
//! dirtbag keeps no host-side state file. Tart is the source of truth for
//! whether a VM exists and is running; the guest records whether it has been
//! provisioned; and the VM name is derived from the project (see
//! [`Project::vm_name`]).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::config::Project;
use crate::process;
use crate::ssh::{SSH_PORT, Ssh};
use crate::tart::{Tart, run_args};

const IP_TIMEOUT: Duration = Duration::from_secs(180);
const SSH_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

fn discover() -> Result<Project> {
    Project::discover_cwd()
}

fn is_running(tart: &Tart, name: &str) -> Result<bool> {
    Ok(tart.get(name)?.map(|v| v.running).unwrap_or(false))
}

pub fn up() -> Result<()> {
    let project = discover()?;
    let tart = Tart::locate()?;
    let name = project.vm_name();

    for (mount, path) in project.missing_mount_sources()? {
        warn!(mount = %mount, path = %path, "mount source does not exist on host");
    }

    if tart.get(&name)?.is_none() {
        info!(image = %project.config.image, vm = %name, "cloning base image");
        tart.clone(&project.config.image, &name)
            .with_context(|| format!("cloning {} to {name}", project.config.image))?;
        // Give the VM a unique MAC. `tart ip` resolves by MAC, so two clones of
        // the same image would otherwise get the same IP and shadow each other.
        tart.set_random_mac(&name)
            .context("assigning a unique MAC")?;
    }

    // Tart accepts resource changes only while the VM is stopped.
    if !is_running(&tart, &name)? {
        tart.set(&name, &project.config.resources.to_tart())
            .context("applying resource settings")?;
    }

    // `pid` is set only when we start the VM this run; it drives the boot
    // fast-fail below and is never persisted (a saved PID can be reused).
    let pid = if is_running(&tart, &name)? {
        info!(vm = %name, "already running");
        None
    } else {
        std::fs::create_dir_all(project.dirtbag_dir())
            .with_context(|| format!("creating {}", project.dirtbag_dir().display()))?;
        let args = run_args(&name, &project.dir_shares()?, true);
        let pid = process::spawn_detached(tart.bin(), &args, &project.run_log())?;
        info!(vm = %name, pid, "started detached tart run");
        Some(pid)
    };

    let ip = wait_for_ip(&tart, &name, pid)?;

    // Configure the guest only when we just booted it.
    if pid.is_some() {
        configure_boot(&project, &ip)?;
    }

    println!("VM `{name}` is up at {ip}");
    Ok(())
}

/// Configure the guest after a boot. Mount the shares every time. Copy files and
/// run the provisioners only the first time, tracked by a marker in the guest.
fn configure_boot(project: &Project, ip: &str) -> Result<()> {
    info!(%ip, "waiting for ssh");
    let ssh = Ssh::connect_ready(
        ip,
        SSH_PORT,
        &project.config.ssh.user,
        &project.config.ssh.password,
        SSH_TIMEOUT,
    )?;

    // The guest loses its mounts on reboot, so mount the shares on every boot.
    let guest = crate::guest::detect(&project.config.image);
    for m in project.mounts()? {
        guest.mount_share(&ssh, &m.name, &m.target, m.readonly)?;
        info!(tag = %m.name, target = %m.target, "mounted share");
    }

    // Provision a VM one time. Use `dirtbag provision` to run the steps again.
    if !guest.is_provisioned(&ssh)? {
        crate::provision::run_copies(&ssh, project)?;
        crate::provision::run_provisions(&ssh, project)?;
        guest.mark_provisioned(&ssh)?;
    }
    Ok(())
}

/// `dirtbag reload` — bring the VM down (if running) and back up so mount and
/// resource changes take effect.
pub fn reload() -> Result<()> {
    down()?;
    up()
}

fn wait_for_ip(tart: &Tart, name: &str, pid: Option<u32>) -> Result<String> {
    let start = Instant::now();
    loop {
        if let Some(ip) = tart.ip(name)? {
            return Ok(ip);
        }
        // Stop early if the VM process we started has died.
        if let Some(pid) = pid
            && !process::is_alive(pid)
        {
            bail!("tart run (pid {pid}) exited before the VM got an IP; see .dirtbag/run.log");
        }
        if start.elapsed() > IP_TIMEOUT {
            bail!(
                "timed out after {}s waiting for `{name}` to get an IP",
                IP_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

pub fn down() -> Result<()> {
    let project = discover()?;
    let tart = Tart::locate()?;
    let name = project.vm_name();

    if is_running(&tart, &name)? {
        // Tart stops the VM by a hard power-off, so flush the guest filesystem
        // first. Otherwise unsynced writes from this session are lost.
        if let Err(e) = sync_guest(&tart, &project, &name) {
            warn!("could not flush the guest before stop: {e:#}");
        }
        info!(vm = %name, "stopping");
        tart.stop(&name).context("stopping VM")?;
        println!("VM `{name}` is down");
    } else {
        println!("VM `{name}` is not running");
    }
    Ok(())
}

/// Flush the guest filesystem over SSH so a hard power-off keeps the writes.
fn sync_guest(tart: &Tart, project: &Project, name: &str) -> Result<()> {
    let ip = tart.ip(name)?.context("VM has no IP")?;
    let ssh = Ssh::connect(
        &ip,
        SSH_PORT,
        &project.config.ssh.user,
        &project.config.ssh.password,
    )?;
    let (code, out) = ssh.exec_capture("sync")?;
    if code != 0 {
        bail!("guest `sync` failed (exit {code}): {}", out.trim());
    }
    Ok(())
}

pub fn destroy() -> Result<()> {
    let project = discover()?;
    let tart = Tart::locate()?;
    let name = project.vm_name();

    if is_running(&tart, &name)? {
        tart.stop(&name).context("stopping VM before delete")?;
    }
    if tart.get(&name)?.is_some() {
        info!(vm = %name, "deleting");
        tart.delete(&name).context("deleting VM")?;
        println!("VM `{name}` destroyed");
    } else {
        println!("VM `{name}` does not exist");
    }

    let dir = project.dirtbag_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    match Project::discover(&cwd) {
        Ok(project) => project_status(&project),
        Err(_) => global_status(),
    }
}

fn project_status(project: &Project) -> Result<()> {
    let tart = Tart::locate()?;
    let name = project.vm_name();
    let Some(vm) = tart.get(&name)? else {
        println!("not created — run `dirtbag up`");
        return Ok(());
    };

    println!("name:  {name}");
    println!("state: {}", vm.state);
    if vm.running
        && let Some(ip) = tart.ip(&name)?
    {
        println!("ip:    {ip}");
    }
    Ok(())
}

/// Fallback used outside a project: list all local Tart VMs.
fn global_status() -> Result<()> {
    let tart = Tart::locate()?;
    let vms = tart.list()?;
    if vms.is_empty() {
        println!("No Tart VMs found.");
        return Ok(());
    }
    println!("{:<28} {:<10} IP", "NAME", "STATE");
    for vm in &vms {
        let ip = if vm.running {
            tart.ip(&vm.name)?.unwrap_or_default()
        } else {
            String::new()
        };
        println!("{:<28} {:<10} {}", vm.name, vm.state, ip);
    }
    Ok(())
}
