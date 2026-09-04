//! Project-local machine state, stored in `.dirtbag/` (à la Vagrant's
//! `.vagrant/`).

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STATE_DIR: &str = ".dirtbag";
pub const STATE_FILE: &str = "state.toml";
pub const RUN_LOG: &str = "run.log";

/// Lifecycle phase of the project's VM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// No VM created yet.
    #[default]
    Absent,
    /// VM cloned but not running.
    Created,
    /// VM running (detached `tart run` tracked by `pid`).
    Running,
    /// VM created and previously stopped.
    Stopped,
}

/// Persisted state for a project's VM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct State {
    /// Resolved Tart VM name.
    pub vm_name: String,
    /// PID of the detached `tart run` process, when running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default)]
    pub phase: Phase,
    /// Fingerprint of the mount set the VM was last booted with, so `up` can
    /// detect config drift and suggest `dirtbag reload`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounts_hash: Option<String>,
}

impl State {
    pub fn new(vm_name: String) -> Self {
        Self {
            vm_name,
            pid: None,
            phase: Phase::Absent,
            mounts_hash: None,
        }
    }

    /// Load state from `<root>/.dirtbag/state.toml`, if it exists.
    pub fn load(root: &Path) -> Result<Option<Self>> {
        let path = state_file(root);
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let state = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(state))
    }

    /// Persist state to `<root>/.dirtbag/state.toml`, creating the dir.
    pub fn save(&self, root: &Path) -> Result<()> {
        let dir = state_dir(root);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("serializing state")?;
        let path = state_file(root);
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

pub fn state_dir(root: &Path) -> PathBuf {
    root.join(STATE_DIR)
}

pub fn state_file(root: &Path) -> PathBuf {
    state_dir(root).join(STATE_FILE)
}

pub fn run_log(root: &Path) -> PathBuf {
    state_dir(root).join(RUN_LOG)
}

/// Derive a stable VM name from a project directory: `dirtbag-<dir>-<hash>`.
///
/// The hash keeps names unique across identically-named directories. Once
/// resolved it is persisted in state, so it is stable for the project's life
/// regardless of future changes to this function.
pub fn default_vm_name(root: &Path) -> String {
    let base = root
        .file_name()
        .map(|s| sanitize(&s.to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vm".to_string());

    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    abs.hash(&mut hasher);
    let hash = hasher.finish();

    format!("dirtbag-{base}-{:08x}", (hash & 0xffff_ffff) as u32)
}

/// Keep `[A-Za-z0-9_-]`, collapse everything else to `-`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::new("dirtbag-demo-0badf00d".into());
        state.phase = Phase::Running;
        state.pid = Some(4321);
        state.save(dir.path()).unwrap();

        let loaded = State::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded, state);
        assert!(state_file(dir.path()).is_file());
    }

    #[test]
    fn load_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(State::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn default_name_is_deterministic_and_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let a = default_vm_name(dir.path());
        let b = default_vm_name(dir.path());
        assert_eq!(a, b);
        assert!(a.starts_with("dirtbag-"));
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize("my project!@#"), "my-project---");
    }
}
