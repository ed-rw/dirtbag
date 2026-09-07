use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::{CONFIG_FILE, Config, share_name_for};

/// Build the starter config. `name`/`target` are filled from the project
/// directory so the mount reads clearly, though both may be omitted (they
/// default to the same values).
fn template(share: &str) -> String {
    format!(
        r#"# dirtbag sandbox configuration. Run `dirtbag up` to build this VM.

# Base image to clone. cirruslabs publishes Linux images (ubuntu, debian,
# fedora) with version tags — see the available tags at
# https://github.com/cirruslabs/tart/pkgs/container/ubuntu
image = "ghcr.io/cirruslabs/ubuntu:latest"

# Resources default to cpu = 2 and memory = 4096 (MiB); omit [resources] to
# accept them. `disk` (GiB, grow-only) has no default.
# [resources]
# cpu    = 2
# memory = 4096
# disk   = 50

# Share the project directory into the guest (live).
[[mount]]
name   = "{share}"
source = "."
target = "/opt/{share}"

# Copy a file in once at `up` time (not kept in sync).
# Copy targets must be writable by the ssh user (copies run over SCP, no sudo).
# [[copy]]
# source = "./secrets.env"
# target = "/home/admin/.env"

# Provisioning steps run once, in order, over SSH to configure the machine.
# Use `path` for a script file, or `inline` for a snippet. `privileged = true`
# runs via sudo. Re-run them any time with `dirtbag provision`.
[[provision]]
path       = "scripts/setup.sh"
privileged = true

# on-boot steps run every time the machine boots, after provisioning — for
# preparing the machine to be used (starting services, agents, tunnels). Same
# shape as [[provision]].
# [[on-boot]]
# inline = "systemctl --user start my-agent"

# SSH defaults to admin:admin (the Tart image default); omit [ssh] to accept it.
# [ssh]
# user     = "admin"
# password = "admin"
"#
    )
}

const SETUP_SH: &str = r#"#!/usr/bin/env bash
set -euo pipefail

# Provisioning script — customize for your sandbox.
apt-get update
apt-get install -y build-essential git

echo "dirtbag: provisioning complete"
"#;

pub fn run(file: Option<&Path>) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    // `--file` scaffolds a differently-named config. `join` absorbs an absolute
    // path and resolves a relative one against the cwd.
    let config_path = match file {
        Some(path) => cwd.join(path),
        None => cwd.join(CONFIG_FILE),
    };
    if config_path.exists() {
        bail!("{} already exists", config_path.display());
    }
    let root = config_path.parent().unwrap_or(&cwd);
    let file_name = config_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| CONFIG_FILE.to_string());

    let contents = template(&share_name_for(root));
    // Make sure the template is valid before you write it.
    Config::parse(&contents).context("internal: init template failed to validate")?;

    std::fs::write(&config_path, &contents)
        .with_context(|| format!("writing {}", config_path.display()))?;
    write_if_absent(&root.join("scripts").join("setup.sh"), SETUP_SH)?;

    println!("Created {file_name} and scripts/setup.sh");
    println!("Next: edit {file_name}, then run `dirtbag up`.");
    Ok(())
}

fn write_if_absent(path: &Path, contents: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
