//! `dirtbag.toml` model, discovery, validation, and path resolution.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::tart::{self, DirShare};

pub const CONFIG_FILE: &str = "dirtbag.toml";

/// A parsed `dirtbag.toml` together with the project root it was found in.
#[derive(Debug, Clone)]
pub struct Project {
    /// Directory containing `dirtbag.toml`; all relative paths resolve here.
    pub root: PathBuf,
    pub config: Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Config {
    /// Explicit VM name. If absent, dirtbag derives it from the project directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Base OCI/local image to clone.
    pub image: String,

    #[serde(default)]
    pub resources: Resources,

    #[serde(default, rename = "mount")]
    pub mounts: Vec<Mount>,

    #[serde(default, rename = "copy")]
    pub copies: Vec<Copy>,

    #[serde(default, rename = "provision")]
    pub provisions: Vec<Provision>,

    #[serde(default)]
    pub ssh: Ssh,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Resources {
    pub cpu: Option<u32>,
    /// Memory in MiB.
    pub memory: Option<u32>,
    /// Disk size in GiB.
    pub disk: Option<u32>,
}

impl Resources {
    pub fn to_tart(&self) -> tart::Resources {
        tart::Resources {
            cpu: self.cpu,
            memory_mib: self.memory,
            disk_gb: self.disk,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Mount {
    pub name: String,
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Copy {
    pub source: String,
    pub target: String,
}

/// One provisioning step: exactly one of `inline` or `path`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Provision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default)]
    pub privileged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Ssh {
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_password")]
    pub password: String,
}

fn default_user() -> String {
    "admin".into()
}
fn default_password() -> String {
    "admin".into()
}

impl Default for Ssh {
    fn default() -> Self {
        Self {
            user: default_user(),
            password: default_password(),
        }
    }
}

impl Config {
    /// Parse a config from TOML text and validate it.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).context("parsing dirtbag.toml")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.image.trim().is_empty() {
            bail!("`image` must not be empty");
        }
        let mut seen_names = std::collections::HashSet::new();
        let mut seen_targets = std::collections::HashSet::new();
        for m in &self.mounts {
            if m.name.trim().is_empty() {
                bail!("each [[mount]] needs a non-empty `name`");
            }
            if !Path::new(&m.target).is_absolute() {
                bail!("mount `{}` target must be an absolute guest path", m.name);
            }
            if !seen_names.insert(&m.name) {
                bail!(
                    "duplicate mount name `{}` — names become virtiofs tags and must be unique",
                    m.name
                );
            }
            if !seen_targets.insert(&m.target) {
                bail!("duplicate mount target `{}`", m.target);
            }
        }
        for c in &self.copies {
            if !Path::new(&c.target).is_absolute() {
                bail!("copy target `{}` must be an absolute guest path", c.target);
            }
        }
        for (i, p) in self.provisions.iter().enumerate() {
            match (&p.inline, &p.path) {
                (Some(_), Some(_)) => {
                    bail!("[[provision]] #{}: set only one of `inline` or `path`", i + 1)
                }
                (None, None) => {
                    bail!("[[provision]] #{}: needs either `inline` or `path`", i + 1)
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl Project {
    /// Discover the project from the current working directory.
    pub fn discover_cwd() -> Result<Self> {
        let cwd = std::env::current_dir().context("resolving current directory")?;
        Self::discover(&cwd)
    }

    /// Find the project. Search from `start` up through the parent directories
    /// for `dirtbag.toml`.
    pub fn discover(start: &Path) -> Result<Self> {
        let path = find_config(start)
            .with_context(|| format!("no {CONFIG_FILE} found in {} or any parent", start.display()))?;
        let root = path
            .parent()
            .expect("config path has a parent")
            .to_path_buf();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config = Config::parse(&text)?;
        Ok(Self { root, config })
    }

    /// Resolve mounts to absolute-host-path [`DirShare`]s for `tart run`.
    pub fn dir_shares(&self) -> Vec<DirShare> {
        self.config
            .mounts
            .iter()
            .map(|m| DirShare {
                name: m.name.clone(),
                path: self.resolve_host_path(&m.source),
                readonly: m.readonly,
            })
            .collect()
    }

    /// Mount sources that do not exist on the host, as (name, resolved path).
    /// Tart needs the host path to exist, so warn about these before boot.
    pub fn missing_mount_sources(&self) -> Vec<(String, String)> {
        self.config
            .mounts
            .iter()
            .filter_map(|m| {
                let resolved = self.resolve_host_path(&m.source);
                (!Path::new(&resolved).exists()).then(|| (m.name.clone(), resolved))
            })
            .collect()
    }

    /// Resolve a config-relative host path to an absolute string.
    pub fn resolve_host_path(&self, p: &str) -> String {
        let joined = self.root.join(p);
        std::fs::canonicalize(&joined)
            .unwrap_or(joined)
            .to_string_lossy()
            .into_owned()
    }
}

/// Search from `start` up through the parent directories for a `dirtbag.toml`.
fn find_config(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
name  = "demo"
image = "ghcr.io/cirruslabs/ubuntu:latest"

[resources]
cpu = 4
memory = 8192
disk = 50

[[mount]]
name = "project"
source = "."
target = "/home/admin/project"

[[mount]]
name = "cache"
source = "../cache"
target = "/home/admin/.cache"
readonly = true

[[copy]]
source = "./secrets.env"
target = "/home/admin/.env"

[[provision]]
inline = '''
set -euo pipefail
echo hi
'''
privileged = true

[ssh]
user = "admin"
password = "admin"
"#;

    #[test]
    fn parses_full_config() {
        let c = Config::parse(FULL).unwrap();
        assert_eq!(c.name.as_deref(), Some("demo"));
        assert_eq!(c.resources.cpu, Some(4));
        assert_eq!(c.mounts.len(), 2);
        assert!(c.mounts[1].readonly);
        assert_eq!(c.copies.len(), 1);
        assert_eq!(c.provisions.len(), 1);
        assert!(c.provisions[0].privileged);
        assert_eq!(c.ssh.user, "admin");
    }

    #[test]
    fn ssh_defaults_when_omitted() {
        let c = Config::parse("image = \"x\"").unwrap();
        assert_eq!(c.ssh.user, "admin");
        assert_eq!(c.ssh.password, "admin");
        assert!(c.mounts.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let c = Config::parse(FULL).unwrap();
        let serialized = toml::to_string(&c).unwrap();
        let again = Config::parse(&serialized).unwrap();
        assert_eq!(c, again);
    }

    #[test]
    fn rejects_relative_mount_target() {
        let err = Config::parse(
            "image = \"x\"\n[[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"rel/path\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn rejects_provision_with_both_inline_and_path() {
        let err = Config::parse(
            "image = \"x\"\n[[provision]]\ninline=\"echo\"\npath=\"s.sh\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("only one"));
    }

    #[test]
    fn rejects_empty_provision() {
        let err = Config::parse("image = \"x\"\n[[provision]]\nshell=\"bash\"\n").unwrap_err();
        assert!(err.to_string().contains("either"));
    }

    #[test]
    fn rejects_duplicate_mount_names() {
        let err = Config::parse(
            "image=\"x\"\n\
             [[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"/a\"\n\
             [[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"/b\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate mount name"));
    }

    #[test]
    fn rejects_duplicate_mount_targets() {
        let err = Config::parse(
            "image=\"x\"\n\
             [[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"/same\"\n\
             [[mount]]\nname=\"q\"\nsource=\".\"\ntarget=\"/same\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate mount target"));
    }

    #[test]
    fn rejects_empty_image() {
        assert!(Config::parse("image = \"\"").unwrap_err().to_string().contains("image"));
    }
}
