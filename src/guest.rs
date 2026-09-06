//! Guest-OS abstraction.
//!
//! Tart shares directories over virtiofs. The guest side differs per OS: Linux
//! mounts a share manually; macOS mounts it automatically. Lifecycle code uses
//! the [`Guest`] trait, so you can add guest types without other changes. Linux
//! is first.

use anyhow::{bail, Result};

use crate::ssh::Ssh;

pub trait Guest {
    /// Mount a virtiofs share (tag `tag`) at guest path `target`.
    fn mount_share(&self, ssh: &Ssh, tag: &str, target: &str, readonly: bool) -> Result<()>;

    /// Whether provisioning has already run on this VM. The record lives in the
    /// guest, so it belongs to the VM and cannot desync from it.
    fn is_provisioned(&self, ssh: &Ssh) -> Result<bool>;

    /// Record that provisioning finished, and flush it to disk.
    fn mark_provisioned(&self, ssh: &Ssh) -> Result<()>;
}

/// Pick a guest for an image. Linux only for now. Later, use the image ref to
/// detect macOS and other guests.
pub fn detect(_image: &str) -> Box<dyn Guest> {
    Box::new(LinuxGuest)
}

pub struct LinuxGuest;

const PROVISIONED_DIR: &str = "/var/lib/dirtbag";
const PROVISIONED_MARKER: &str = "/var/lib/dirtbag/provisioned";

impl Guest for LinuxGuest {
    fn mount_share(&self, ssh: &Ssh, tag: &str, target: &str, readonly: bool) -> Result<()> {
        let ro = if readonly { " -o ro" } else { "" };
        // Make the mount point. Skip the mount if it is already mounted.
        let script = format!(
            r#"set -e
sudo mkdir -p "{target}"
if mountpoint -q "{target}"; then exit 0; fi
sudo mount -t virtiofs{ro} "{tag}" "{target}""#
        );
        let (code, out) = ssh.exec_capture(&script)?;
        if code != 0 {
            bail!("mounting `{tag}` at {target} failed (exit {code}): {}", out.trim());
        }
        Ok(())
    }

    fn is_provisioned(&self, ssh: &Ssh) -> Result<bool> {
        let (code, _) = ssh.exec_capture(&format!("test -f {PROVISIONED_MARKER}"))?;
        Ok(code == 0)
    }

    fn mark_provisioned(&self, ssh: &Ssh) -> Result<()> {
        // `sync` makes the marker (and the provisioning writes) durable at once.
        let script = format!(
            "set -e\nsudo mkdir -p {PROVISIONED_DIR}\nsudo touch {PROVISIONED_MARKER}\nsync"
        );
        let (code, out) = ssh.exec_capture(&script)?;
        if code != 0 {
            bail!("could not record provisioning (exit {code}): {}", out.trim());
        }
        Ok(())
    }
}
