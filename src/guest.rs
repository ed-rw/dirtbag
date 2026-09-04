//! Guest-OS abstraction.
//!
//! Tart shares directories over virtiofs, but the guest-side handling differs
//! per OS (Linux must mount manually; macOS auto-mounts). Lifecycle code talks
//! to a [`Guest`] so new guest types slot in without changes elsewhere. Linux
//! ships first.

use anyhow::{bail, Result};

use crate::ssh::Ssh;

pub trait Guest {
    /// Mount a virtiofs share (tag `tag`) at guest path `target`.
    fn mount_share(&self, ssh: &Ssh, tag: &str, target: &str, readonly: bool) -> Result<()>;
}

/// Pick a guest implementation for an image. Linux-only for now; the image ref
/// is where macOS/other detection will hook in later.
pub fn detect(_image: &str) -> Box<dyn Guest> {
    Box::new(LinuxGuest)
}

pub struct LinuxGuest;

impl Guest for LinuxGuest {
    fn mount_share(&self, ssh: &Ssh, tag: &str, target: &str, readonly: bool) -> Result<()> {
        let ro = if readonly { " -o ro" } else { "" };
        // Idempotent: create the mount point, skip if already mounted.
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
