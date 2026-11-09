use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// A CLI for managing Tart VMs as disposable dev sandboxes.
#[derive(Debug, Parser)]
#[command(name = "dirtbag", version, about, long_about = None)]
pub struct Cli {
    /// Increase logging verbosity (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Use this config file instead of discovering `dirtbag.toml`. Lets a
    /// directory hold several sandboxes side by side.
    #[arg(short, long, global = true, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Select a named instance of the config. One config file can run several
    /// VMs side by side. Each VM is named `<vm-name>-<INSTANCE>`.
    #[arg(short, long, global = true, value_name = "INSTANCE")]
    pub instance: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scaffold a starter dirtbag.toml in the current directory.
    Init,

    /// Create and start the VM: clone, configure, run, mount, copy, provision.
    Up,

    /// Open an interactive shell in the VM, or run a command in it.
    Ssh {
        /// Command to run (after `--`); omit for an interactive shell.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },

    /// Show the VM's status (name, state, IP). Without `--instance`, list every
    /// instance of the config.
    Status,

    /// Gracefully stop the VM.
    Down,

    /// Restart the VM to apply mount/resource changes (down + up).
    Reload,

    /// Stop and delete the VM and its local state.
    Destroy,

    /// Re-run provisioners against the running VM.
    Provision,

    /// Show the dirtbag and tart versions.
    Version,

    /// Download the latest release from GitHub and install it over this binary.
    Update,
}
