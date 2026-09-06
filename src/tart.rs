//! Wrapper around the `tart` CLI.
//!
//! The argument builders and output parsers are free functions. You can test
//! them without tart or a VM. The [`Tart`] struct runs the commands.

use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::error::TartError;

/// A VM from `tart list --format json`. serde ignores the other JSON fields
/// (Disk, Size, Accessed, …).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Vm {
    pub name: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub running: bool,
}

/// A host directory to share into the guest via `tart run --dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirShare {
    /// Share/mount-tag name (used as the virtiofs tag and guest mount label).
    pub name: String,
    /// Absolute host path.
    pub path: String,
    pub readonly: bool,
}

/// VM resource overrides applied via `tart set`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resources {
    pub cpu: Option<u32>,
    pub memory_mib: Option<u32>,
    pub disk_gb: Option<u32>,
}

impl Resources {
    fn is_empty(&self) -> bool {
        self.cpu.is_none() && self.memory_mib.is_none() && self.disk_gb.is_none()
    }
}

pub fn list_args() -> Vec<String> {
    vec!["list".into(), "--format".into(), "json".into()]
}

pub fn ip_args(name: &str) -> Vec<String> {
    vec!["ip".into(), name.into()]
}

pub fn clone_args(source: &str, name: &str) -> Vec<String> {
    vec!["clone".into(), source.into(), name.into()]
}

pub fn stop_args(name: &str) -> Vec<String> {
    vec!["stop".into(), name.into()]
}

pub fn delete_args(name: &str) -> Vec<String> {
    vec!["delete".into(), name.into()]
}

pub fn set_args(name: &str, res: &Resources) -> Vec<String> {
    let mut a = vec!["set".into(), name.into()];
    if let Some(cpu) = res.cpu {
        a.push("--cpu".into());
        a.push(cpu.to_string());
    }
    if let Some(mem) = res.memory_mib {
        a.push("--memory".into());
        a.push(mem.to_string());
    }
    if let Some(disk) = res.disk_gb {
        a.push("--disk-size".into());
        a.push(disk.to_string());
    }
    a
}

/// Build one `--dir=...` flag for a share.
///
/// The flag sets a per-share `tag=<name>` and uses no name prefix. A name
/// prefix makes Tart put the files in a subdirectory with that name. Without a
/// prefix, the guest mounts the share contents directly at the target with
/// `mount -t virtiofs <tag> <target>`.
pub fn dir_flag(share: &DirShare) -> String {
    let mut opts = Vec::new();
    if share.readonly {
        opts.push("ro".to_string());
    }
    opts.push(format!("tag={}", share.name));
    format!("--dir={}:{}", share.path, opts.join(","))
}

pub fn run_args(name: &str, dirs: &[DirShare], no_graphics: bool) -> Vec<String> {
    let mut a = vec!["run".into()];
    if no_graphics {
        a.push("--no-graphics".into());
    }
    for d in dirs {
        a.push(dir_flag(d));
    }
    a.push(name.into());
    a
}

pub fn parse_list(json: &str) -> Result<Vec<Vm>, TartError> {
    serde_json::from_str(json).map_err(|source| TartError::Parse {
        cmd: "list".into(),
        source,
    })
}

/// A located `tart` executable.
#[derive(Debug, Clone)]
pub struct Tart {
    bin: PathBuf,
}

impl Tart {
    /// Locate `tart` on `PATH`.
    pub fn locate() -> Result<Self, TartError> {
        let bin = which::which("tart").map_err(|_| TartError::NotFound)?;
        Ok(Self { bin })
    }

    pub fn bin(&self) -> &PathBuf {
        &self.bin
    }

    fn output(&self, args: &[String]) -> Result<std::process::Output, TartError> {
        tracing::debug!(args = ?args, "tart");
        Ok(Command::new(&self.bin).args(args).output()?)
    }

    /// Run a command. Return stdout, or a [`TartError::Command`] on failure.
    fn checked(&self, args: &[String]) -> Result<String, TartError> {
        let out = self.output(args)?;
        if !out.status.success() {
            return Err(TartError::Command {
                cmd: args.join(" "),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn list(&self) -> Result<Vec<Vm>, TartError> {
        parse_list(&self.checked(&list_args())?)
    }

    /// Look up a single VM by name, if it exists locally.
    pub fn get(&self, name: &str) -> Result<Option<Vm>, TartError> {
        Ok(self.list()?.into_iter().find(|v| v.name == name))
    }

    /// Get a VM's IP. Return `Ok(None)` when the VM is stopped or has no DHCP
    /// lease yet. Tart exits non-zero in that case, which is not an error here.
    pub fn ip(&self, name: &str) -> Result<Option<String>, TartError> {
        let out = self.output(&ip_args(name))?;
        if !out.status.success() {
            return Ok(None);
        }
        let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!ip.is_empty()).then_some(ip))
    }

    pub fn clone(&self, source: &str, name: &str) -> Result<(), TartError> {
        self.checked(&clone_args(source, name)).map(drop)
    }

    pub fn set(&self, name: &str, res: &Resources) -> Result<(), TartError> {
        if res.is_empty() {
            return Ok(());
        }
        self.checked(&set_args(name, res)).map(drop)
    }

    pub fn stop(&self, name: &str) -> Result<(), TartError> {
        self.checked(&stop_args(name)).map(drop)
    }

    pub fn delete(&self, name: &str) -> Result<(), TartError> {
        self.checked(&delete_args(name)).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_flag_readwrite_sets_tag() {
        let share = DirShare {
            name: "project".into(),
            path: "/Users/ed/code/x".into(),
            readonly: false,
        };
        assert_eq!(dir_flag(&share), "--dir=/Users/ed/code/x:tag=project");
    }

    #[test]
    fn dir_flag_readonly_prepends_ro() {
        let share = DirShare {
            name: "src".into(),
            path: "/tmp/src".into(),
            readonly: true,
        };
        assert_eq!(dir_flag(&share), "--dir=/tmp/src:ro,tag=src");
    }

    #[test]
    fn run_args_headless_places_name_last() {
        let dirs = vec![DirShare {
            name: "project".into(),
            path: "/tmp/p".into(),
            readonly: false,
        }];
        assert_eq!(
            run_args("vm1", &dirs, true),
            vec!["run", "--no-graphics", "--dir=/tmp/p:tag=project", "vm1"]
        );
    }

    #[test]
    fn set_args_only_includes_provided_fields() {
        let res = Resources {
            cpu: Some(4),
            memory_mib: None,
            disk_gb: Some(50),
        };
        assert_eq!(
            set_args("vm1", &res),
            vec!["set", "vm1", "--cpu", "4", "--disk-size", "50"]
        );
    }

    #[test]
    fn clone_and_lifecycle_args() {
        assert_eq!(clone_args("ghcr.io/x:latest", "vm1"), vec!["clone", "ghcr.io/x:latest", "vm1"]);
        assert_eq!(stop_args("vm1"), vec!["stop", "vm1"]);
        assert_eq!(delete_args("vm1"), vec!["delete", "vm1"]);
        assert_eq!(list_args(), vec!["list", "--format", "json"]);
        assert_eq!(ip_args("vm1"), vec!["ip", "vm1"]);
    }

    #[test]
    fn parse_list_reads_real_schema() {
        // Captured from `tart list --format json` (tart 2.32.1).
        let json = r#"[
          {
            "Disk" : 50,
            "Source" : "local",
            "Accessed" : "2026-09-04T13:34:42Z",
            "Name" : "_dirtbag_probe",
            "Size" : 0,
            "State" : "stopped",
            "Running" : false
          }
        ]"#;
        let vms = parse_list(json).unwrap();
        assert_eq!(vms.len(), 1);
        assert_eq!(
            vms[0],
            Vm {
                name: "_dirtbag_probe".into(),
                source: "local".into(),
                state: "stopped".into(),
                running: false,
            }
        );
    }

    #[test]
    fn parse_list_empty() {
        assert_eq!(parse_list("[\n\n]").unwrap(), vec![]);
    }
}
