use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::{Config, CONFIG_FILE};

const TEMPLATE: &str = r#"# dirtbag sandbox configuration.
# Docs: https://tart.run  —  run `dirtbag on` to build this VM.

image = "ghcr.io/cirruslabs/ubuntu:latest"

[resources]
cpu    = 2
memory = 4096   # MiB
# disk = 50     # GiB (grow only)

# Share the project directory into the guest (live).
[[mount]]
name   = "project"
source = "."
target = "/opt/project"

# Copy a file in once at `on` time (not kept in sync).
# Copy targets must be writable by the ssh user (copies run over SCP, no sudo).
# [[copy]]
# source = "./secrets.env"
# target = "/home/admin/.env"

# Provisioning steps run in order over SSH. Use `path` for a script file,
# or `inline` for a snippet. `privileged = true` runs via sudo.
[[provision]]
path       = "scripts/setup.sh"
privileged = true

[ssh]
user     = "admin"
password = "admin"
"#;

const SETUP_SH: &str = r#"#!/usr/bin/env bash
set -euo pipefail

# Provisioning script — customize for your sandbox.
apt-get update
apt-get install -y build-essential git

echo "dirtbag: provisioning complete"
"#;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let config_path = cwd.join(CONFIG_FILE);
    if config_path.exists() {
        bail!("{CONFIG_FILE} already exists in {}", cwd.display());
    }

    // Sanity-check the template stays valid as the schema evolves.
    Config::parse(TEMPLATE).context("internal: init template failed to validate")?;

    std::fs::write(&config_path, TEMPLATE)
        .with_context(|| format!("writing {}", config_path.display()))?;
    write_if_absent(&cwd.join("scripts").join("setup.sh"), SETUP_SH)?;

    println!("Created {CONFIG_FILE} and scripts/setup.sh");
    println!("Next: edit dirtbag.toml, then run `dirtbag on`.");
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
