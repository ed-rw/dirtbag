//! VM lifecycle commands: `up`, `status`, `down`, `reload`, `destroy`.
//!
//! dirtbag keeps no host-side state file. Tart is the source of truth for
//! whether a VM exists and is running; the guest records whether it has been
//! provisioned; and the VM name is derived from the project (see
//! [`Project::vm_name`]).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::config::Project;
use crate::forward::{self, Forward};
use crate::process;
use crate::ssh::{SSH_PORT, Ssh};
use crate::tart::{Tart, Vm, run_args};

const IP_TIMEOUT: Duration = Duration::from_secs(180);
const SSH_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Longer than the supervisor waits for a bind address to exist.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(90);
const FORWARD_POLL: Duration = Duration::from_millis(250);

fn is_running(tart: &Tart, name: &str) -> Result<bool> {
    Ok(tart.get(name)?.map(|v| v.running).unwrap_or(false))
}

pub fn up(file: Option<&Path>, instance: Option<&str>) -> Result<()> {
    let project = Project::find(file, instance)?;
    let tart = Tart::locate()?;
    let name = project.vm_name();

    for (mount, path) in project.missing_mount_sources()? {
        warn!(mount = %mount, path = %path, "mount source does not exist on host");
    }

    // Check the host ports before the clone, so a busy port leaves no new VM.
    if !is_running(&tart, &name)? {
        forward::reserve(&project.config.network.forward)?;
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
        warn_forward_drift(&project, &name)?;
        None
    } else {
        Some(start_supervisor(&project, &tart, &name)?)
    };

    let ip = wait_for_ip(&tart, &name, pid, &project.run_log())?;

    // Configure the guest only when we just booted it.
    if let Some(pid) = pid {
        wait_for_forwards(&name, pid, &project.run_log())?;
        configure_boot(&project, &ip)?;
    }

    println!("VM `{name}` is up at {ip}");
    for f in running_forwards(&name)? {
        println!("forward {}", forward::label(&f));
    }
    Ok(())
}

/// Start the VM detached, under a supervisor that forwards the host ports.
/// Give each auto forward a free host port first.
fn start_supervisor(project: &Project, tart: &Tart, name: &str) -> Result<u32> {
    std::fs::create_dir_all(project.dirtbag_dir())
        .with_context(|| format!("creating {}", project.dirtbag_dir().display()))?;
    let forwards = forward::reserve(&project.config.network.forward)?;
    let exe = std::env::current_exe().context("finding the dirtbag binary")?;
    let args = forward::supervise_args(
        name,
        &forwards,
        &tart.bin().to_string_lossy(),
        &run_args(name, &project.dir_shares()?, true),
    );
    let pid = process::spawn_detached(&exe, &args, &project.run_log())?;
    info!(vm = %name, pid, "started detached VM supervisor");
    Ok(pid)
}

/// The forwards of the supervisor for `name`, or none if no supervisor runs.
fn running_forwards(name: &str) -> Result<Vec<Forward>> {
    Ok(forward::supervisor_for(name)?
        .map(|s| s.forwards)
        .unwrap_or_default())
}

/// Forwards change only when the supervisor starts. Warn if the running ones
/// do not match the config.
fn warn_forward_drift(project: &Project, name: &str) -> Result<()> {
    let want = &project.config.network.forward;
    let running = running_forwards(name)?;
    let same = want.len() == running.len() && want.iter().zip(&running).all(|(w, r)| w.matches(r));
    if !same {
        warn!(
            "the running port forwards do not match the config; run `dirtbag reload` to apply them"
        );
    }
    Ok(())
}

/// Configure the guest after a boot. Mount the shares and run the `[[on-boot]]`
/// steps every time. Copy files and run the provisioners only the first time,
/// tracked by a marker in the guest.
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

    // Prepare the machine on every boot, after provisioning.
    crate::provision::run_on_boot(&ssh, project)?;
    Ok(())
}

/// `dirtbag reload` — bring the VM down (if running) and back up so mount and
/// resource changes take effect.
pub fn reload(file: Option<&Path>, instance: Option<&str>) -> Result<()> {
    down(file, instance)?;
    up(file, instance)
}

/// Wait until the supervisor listens on each forward. It binds an address
/// that the VM network creates only after the boot. If a bind fails, the
/// supervisor stops the VM and exits.
fn wait_for_forwards(name: &str, pid: u32, log: &Path) -> Result<()> {
    let forwards = running_forwards(name)?;
    let start = Instant::now();
    loop {
        let waiting: Vec<_> = forwards
            .iter()
            .filter(|f| !forward::is_listening(f))
            .collect();
        if !process::is_alive(pid) {
            bail!(
                "the VM supervisor (pid {pid}) exited, so the port forwards are not available; see {}",
                log.display()
            );
        }
        if waiting.is_empty() {
            return Ok(());
        }
        if start.elapsed() > FORWARD_TIMEOUT {
            let labels: Vec<_> = waiting.iter().map(|f| f.to_string()).collect();
            bail!(
                "timed out waiting for forwards {}; see {}",
                labels.join(", "),
                log.display()
            );
        }
        std::thread::sleep(FORWARD_POLL);
    }
}

fn wait_for_ip(tart: &Tart, name: &str, pid: Option<u32>, log: &Path) -> Result<String> {
    let start = Instant::now();
    loop {
        if let Some(ip) = tart.ip(name)? {
            return Ok(ip);
        }
        // Stop early if the VM process we started has died.
        if let Some(pid) = pid
            && !process::is_alive(pid)
        {
            bail!(
                "the VM process (pid {pid}) exited before the VM got an IP; see {}",
                log.display()
            );
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

pub fn down(file: Option<&Path>, instance: Option<&str>) -> Result<()> {
    let project = Project::find(file, instance)?;
    let tart = Tart::locate()?;
    let name = project.vm_name();

    if is_running(&tart, &name)? {
        // Tart stops the VM by a hard power-off, so wind the guest down first.
        // Otherwise unsynced writes from this session are lost.
        if let Err(e) = shutdown_guest(&tart, &project, &name) {
            warn!("could not wind the guest down before stop: {e:#}");
        }
        info!(vm = %name, "stopping");
        tart.stop(&name).context("stopping VM")?;
        println!("VM `{name}` is down");
    } else {
        println!("VM `{name}` is not running");
    }
    Ok(())
}

/// Wind the guest down over SSH before a stop. Run the `[[on-shutdown]]` steps,
/// the last of which is always `sync`, so a hard power-off keeps the writes.
fn shutdown_guest(tart: &Tart, project: &Project, name: &str) -> Result<()> {
    let ip = tart.ip(name)?.context("VM has no IP")?;
    let ssh = Ssh::connect(
        &ip,
        SSH_PORT,
        &project.config.ssh.user,
        &project.config.ssh.password,
    )?;
    crate::provision::run_on_shutdown(&ssh, project)
}

pub fn destroy(file: Option<&Path>, instance: Option<&str>) -> Result<()> {
    let project = Project::find(file, instance)?;
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

    // Remove only this sandbox's own run log, so a sibling that shares the
    // directory keeps its state. Tidy the shared `.dirtbag/` away once it holds
    // nothing more (`remove_dir` succeeds only on an empty directory).
    let log = project.run_log();
    if log.exists() {
        std::fs::remove_file(&log).with_context(|| format!("removing {}", log.display()))?;
    }
    let _ = std::fs::remove_dir(project.dirtbag_dir());
    Ok(())
}

pub fn status(file: Option<&Path>, instance: Option<&str>) -> Result<()> {
    // With an explicit `--file`, report that project and surface any load error.
    // Without one, discover from the cwd and fall back to a global VM listing.
    let project = if file.is_some() {
        Project::find(file, instance)?
    } else {
        let cwd = std::env::current_dir().context("resolving current directory")?;
        match Project::discover(&cwd) {
            Ok(project) => project.with_instance(instance)?,
            Err(_) => return global_status(),
        }
    };
    if project.instance.is_some() {
        project_status(&project)
    } else {
        instances_status(&project)
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
    for f in running_forwards(&name)? {
        println!("forward: {}", forward::label(&f));
    }
    Ok(())
}

/// List the config's base VM and all of its instances.
fn instances_status(project: &Project) -> Result<()> {
    let tart = Tart::locate()?;
    let vms: Vec<Vm> = tart
        .list()?
        .into_iter()
        .filter(|vm| project.owns_vm(&vm.name))
        .collect();
    if vms.is_empty() {
        println!("not created — run `dirtbag up`");
        return Ok(());
    }
    print_vms(&tart, &vms)
}

/// Fallback used outside a project: list all local Tart VMs.
fn global_status() -> Result<()> {
    let tart = Tart::locate()?;
    let vms = tart.list()?;
    if vms.is_empty() {
        println!("No Tart VMs found.");
        return Ok(());
    }
    print_vms(&tart, &vms)
}

fn print_vms(tart: &Tart, vms: &[Vm]) -> Result<()> {
    let supervisors = forward::supervisors()?;
    println!("{:<28} {:<10} {:<16} FORWARDS", "NAME", "STATE", "IP");
    for vm in vms {
        let ip = if vm.running {
            tart.ip(&vm.name)?.unwrap_or_default()
        } else {
            String::new()
        };
        let forwards = supervisors
            .iter()
            .find(|s| s.vm == vm.name)
            .map(|s| {
                s.forwards
                    .iter()
                    .map(forward::label)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!("{:<28} {:<10} {:<16} {}", vm.name, vm.state, ip, forwards);
    }
    Ok(())
}
