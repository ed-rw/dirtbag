//! VM lifecycle commands: `on`, `status`, `stop`, `reload`, `destroy`.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tracing::{info, warn};

use crate::config::Project;
use crate::process;
use crate::ssh::{Ssh, SSH_PORT};
use crate::state::{self, Phase, State};
use crate::tart::{dir_flag, run_args, DirShare, Tart};

const IP_TIMEOUT: Duration = Duration::from_secs(180);
const SSH_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

fn discover() -> Result<Project> {
    Project::discover_cwd()
}

/// Load the project state, or make a new one with a resolved VM name.
fn load_or_init_state(project: &Project) -> Result<State> {
    if let Some(state) = State::load(&project.root)? {
        return Ok(state);
    }
    let name = project
        .config
        .name
        .clone()
        .unwrap_or_else(|| state::default_vm_name(&project.root));
    Ok(State::new(name))
}

fn is_running(tart: &Tart, name: &str) -> Result<bool> {
    Ok(tart.get(name)?.map(|v| v.running).unwrap_or(false))
}

pub fn on() -> Result<()> {
    let project = discover()?;
    let tart = Tart::locate()?;
    let mut state = load_or_init_state(&project)?;
    let name = state.vm_name.clone();

    for (mount, path) in project.missing_mount_sources() {
        warn!(mount = %mount, path = %path, "mount source does not exist on host");
    }

    let dirs = project.dir_shares();
    let fingerprint = mounts_fingerprint(&dirs);

    if tart.get(&name)?.is_none() {
        info!(image = %project.config.image, vm = %name, "cloning base image");
        tart.clone(&project.config.image, &name)
            .with_context(|| format!("cloning {} to {name}", project.config.image))?;
        // Give the VM a unique MAC. `tart ip` resolves by MAC, so two clones of
        // the same image would otherwise get the same IP and shadow each other.
        tart.set_random_mac(&name).context("assigning a unique MAC")?;
        state.phase = Phase::Created;
        state.save(&project.root)?;
    }

    // Tart accepts resource changes only while the VM is stopped.
    if !is_running(&tart, &name)? {
        tart.set(&name, &project.config.resources.to_tart())
            .context("applying resource settings")?;
    }

    let just_started = if is_running(&tart, &name)? {
        // Tart fixes the mounts at run time. A reload reattaches changed mounts.
        if state.mounts_hash.as_deref().is_some_and(|h| h != fingerprint) {
            warn!("mount configuration changed since boot — run `dirtbag reload` to apply");
        }
        info!(vm = %name, "already running");
        false
    } else {
        let args = run_args(&name, &dirs, true);
        let pid = process::spawn_detached(tart.bin(), &args, &state::run_log(&project.root))?;
        state.pid = Some(pid);
        state.phase = Phase::Running;
        state.mounts_hash = Some(fingerprint);
        state.save(&project.root)?;
        info!(vm = %name, pid, "started detached tart run");
        true
    };

    let ip = wait_for_ip(&tart, &name, &state)?;

    if just_started {
        configure_boot(&project, &ip, &mut state)?;
    }

    println!("VM `{name}` is up at {ip}");
    Ok(())
}

/// Configure the guest after a boot. Mount the shares every time. Copy files
/// and run the provisioners only the first time.
fn configure_boot(project: &Project, ip: &str, state: &mut State) -> Result<()> {
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
    for m in &project.config.mounts {
        guest.mount_share(&ssh, &m.name, &m.target, m.readonly)?;
        info!(tag = %m.name, target = %m.target, "mounted share");
    }

    // Provision a VM one time. Use `dirtbag provision` to run the steps again.
    if !state.provisioned {
        crate::provision::run_copies(&ssh, project)?;
        crate::provision::run_provisions(&ssh, project)?;
        state.provisioned = true;
        state.save(&project.root)?;
    }
    Ok(())
}

/// Order-independent fingerprint of the mount set. It shows when a running VM
/// no longer matches the config.
fn mounts_fingerprint(dirs: &[DirShare]) -> String {
    use std::hash::{Hash, Hasher};
    let mut flags: Vec<String> = dirs.iter().map(dir_flag).collect();
    flags.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    flags.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// `dirtbag reload` — stop the VM (if running) and bring it back on so
/// mount/resource changes take effect.
pub fn reload() -> Result<()> {
    let project = discover()?;
    if State::load(&project.root)?.is_some() {
        stop()?;
    }
    on()
}

fn wait_for_ip(tart: &Tart, name: &str, state: &State) -> Result<String> {
    let start = Instant::now();
    loop {
        if let Some(ip) = tart.ip(name)? {
            return Ok(ip);
        }
        // Stop early if our tart process is dead.
        if let Some(pid) = state.pid {
            if !process::is_alive(pid) {
                bail!(
                    "tart run (pid {pid}) exited before the VM got an IP; see .dirtbag/run.log"
                );
            }
        }
        if start.elapsed() > IP_TIMEOUT {
            bail!("timed out after {}s waiting for `{name}` to get an IP", IP_TIMEOUT.as_secs());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

pub fn stop() -> Result<()> {
    let project = discover()?;
    let tart = Tart::locate()?;
    let mut state = State::load(&project.root)?
        .context("no dirtbag state for this project; run `dirtbag on` first")?;

    if is_running(&tart, &state.vm_name)? {
        // Tart stops the VM by a hard power-off, so flush the guest filesystem
        // first. Otherwise unsynced writes from this session are lost.
        if let Err(e) = sync_guest(&tart, &project, &state.vm_name) {
            warn!("could not flush the guest before stop: {e:#}");
        }
        info!(vm = %state.vm_name, "stopping");
        tart.stop(&state.vm_name).context("stopping VM")?;
    }
    if let Some(pid) = state.pid.take() {
        process::terminate(pid);
    }
    state.phase = Phase::Stopped;
    state.save(&project.root)?;
    println!("VM `{}` stopped", state.vm_name);
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

    if let Some(mut state) = State::load(&project.root)? {
        if is_running(&tart, &state.vm_name)? {
            tart.stop(&state.vm_name).context("stopping VM before delete")?;
        }
        if let Some(pid) = state.pid.take() {
            process::terminate(pid);
        }
        if tart.get(&state.vm_name)?.is_some() {
            info!(vm = %state.vm_name, "deleting");
            tart.delete(&state.vm_name).context("deleting VM")?;
        }
        println!("VM `{}` destroyed", state.vm_name);
    } else {
        println!("Nothing to destroy (no dirtbag state).");
    }

    let dir = state::state_dir(&project.root);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("removing {}", dir.display()))?;
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
    let Some(state) = State::load(&project.root)? else {
        println!("not created — run `dirtbag on`");
        return Ok(());
    };
    let tart = Tart::locate()?;

    let vm = tart.get(&state.vm_name)?;
    let (tart_state, running) = match &vm {
        Some(v) => (v.state.as_str(), v.running),
        None => ("absent", false),
    };
    let ip = if running {
        tart.ip(&state.vm_name)?.unwrap_or_default()
    } else {
        String::new()
    };

    println!("name:  {}", state.vm_name);
    println!("phase: {tart_state}");
    if let Some(pid) = state.pid {
        let live = if process::is_alive(pid) { "alive" } else { "dead" };
        println!("pid:   {pid} ({live})");
    }
    if !ip.is_empty() {
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
