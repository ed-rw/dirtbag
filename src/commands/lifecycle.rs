//! VM lifecycle commands: `up`, `status`, `halt`, `destroy`.

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

/// Resolve (and persist on first use) the VM name for a project.
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

pub fn up() -> Result<()> {
    let project = discover()?;
    let tart = Tart::locate()?;
    let mut state = load_or_init_state(&project)?;
    let name = state.vm_name.clone();

    // Warn about mount sources that don't exist — Tart requires them to.
    for (mount, path) in project.missing_mount_sources() {
        warn!(mount = %mount, path = %path, "mount source does not exist on host");
    }

    let dirs = project.dir_shares();
    let fingerprint = mounts_fingerprint(&dirs);

    // 1. Clone from the base image if the VM doesn't exist yet.
    if tart.get(&name)?.is_none() {
        info!(image = %project.config.image, vm = %name, "cloning base image");
        tart.clone(&project.config.image, &name)
            .with_context(|| format!("cloning {} to {name}", project.config.image))?;
        state.phase = Phase::Created;
        state.save(&project.root)?;
    }

    // 2. Apply resource settings (only valid while stopped).
    if !is_running(&tart, &name)? {
        tart.set(&name, &project.config.resources.to_tart())
            .context("applying resource settings")?;
    }

    // 3. Start the VM detached, unless tart already reports it running.
    let just_started = if is_running(&tart, &name)? {
        // Mounts are fixed at `tart run` time; if they changed, a reload is
        // needed to reattach them.
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

    // 4. Wait for the VM to obtain an IP.
    let ip = wait_for_ip(&tart, &name, &state)?;

    // 5. On first boot only: wait for SSH, mount shares, copy files, provision.
    // Re-running `up` on an already-running VM is a no-op (use `dirtbag reload`
    // to reattach changed mounts, or `dirtbag provision` to re-provision).
    if just_started {
        info!(vm = %name, %ip, "waiting for ssh");
        let ssh = Ssh::connect_ready(
            &ip,
            SSH_PORT,
            &project.config.ssh.user,
            &project.config.ssh.password,
            SSH_TIMEOUT,
        )?;

        let guest = crate::guest::detect(&project.config.image);
        for m in &project.config.mounts {
            guest.mount_share(&ssh, &m.name, &m.target, m.readonly)?;
            info!(tag = %m.name, target = %m.target, "mounted share");
        }
        crate::provision::run_copies(&ssh, &project)?;
        crate::provision::run_provisions(&ssh, &project)?;
    }

    println!("VM `{name}` is up at {ip}");
    Ok(())
}

/// A stable fingerprint of the mount set, order-independent, used to detect
/// when a running VM's shares no longer match the config.
fn mounts_fingerprint(dirs: &[DirShare]) -> String {
    use std::hash::{Hash, Hasher};
    let mut flags: Vec<String> = dirs.iter().map(dir_flag).collect();
    flags.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    flags.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// `dirtbag reload` — stop the VM (if running) and bring it back up so
/// mount/resource changes take effect.
pub fn reload() -> Result<()> {
    let project = discover()?;
    if State::load(&project.root)?.is_some() {
        halt()?;
    }
    up()
}

fn wait_for_ip(tart: &Tart, name: &str, state: &State) -> Result<String> {
    let start = Instant::now();
    loop {
        if let Some(ip) = tart.ip(name)? {
            return Ok(ip);
        }
        // If we own the process and it died, fail fast with the log hint.
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

pub fn halt() -> Result<()> {
    let project = discover()?;
    let tart = Tart::locate()?;
    let mut state = State::load(&project.root)?
        .context("no dirtbag state for this project; run `dirtbag up` first")?;

    if is_running(&tart, &state.vm_name)? {
        info!(vm = %state.vm_name, "stopping");
        tart.stop(&state.vm_name).context("stopping VM")?;
    }
    if let Some(pid) = state.pid.take() {
        process::terminate(pid);
    }
    state.phase = Phase::Stopped;
    state.save(&project.root)?;
    println!("VM `{}` halted", state.vm_name);
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
        println!("not created — run `dirtbag up`");
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
