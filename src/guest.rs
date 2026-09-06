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
}

/// Pick a guest for an image. Linux only for now. Later, use the image ref to
/// detect macOS and other guests.
pub fn detect(_image: &str) -> Box<dyn Guest> {
    Box::new(LinuxGuest)
}

pub struct LinuxGuest;

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
}
