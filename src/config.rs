//! `dirtbag.toml` model, discovery, validation, and path resolution.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::tart::{self, DirShare};

pub const CONFIG_FILE: &str = "dirtbag.toml";
pub const DIRTBAG_DIR: &str = ".dirtbag";
pub const RUN_LOG: &str = "run.log";

/// Stem of the default [`CONFIG_FILE`]. A config file with this stem keeps the
/// original name and state paths, so it does not disturb existing sandboxes.
const DEFAULT_STEM: &str = "dirtbag";

/// A parsed config file with the project root that contains it.
#[derive(Debug, Clone)]
pub struct Project {
    /// Directory containing the config file; all relative paths resolve here.
    pub root: PathBuf,
    /// The config file itself. Its stem disambiguates sibling sandboxes that
    /// share a directory (see [`Project::vm_name`] and [`Project::run_log`]).
    pub config_path: PathBuf,
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

    /// Steps that configure the machine. Run once, on first boot.
    #[serde(default, rename = "provision")]
    pub provisions: Vec<Step>,

    /// Steps that prepare the machine. Run on every boot.
    #[serde(default, rename = "on-boot")]
    pub on_boots: Vec<Step>,

    /// Steps that wind the machine down. Run before every stop, ahead of the
    /// implicit final `sync`.
    #[serde(default, rename = "on-shutdown")]
    pub on_shutdowns: Vec<Step>,

    #[serde(default)]
    pub ssh: Ssh,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Resources {
    #[serde(default = "default_cpu")]
    pub cpu: u32,
    /// Memory in MiB.
    #[serde(default = "default_memory")]
    pub memory: u32,
    /// Disk size in GiB (grow-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<u32>,
}

fn default_cpu() -> u32 {
    2
}
fn default_memory() -> u32 {
    4096
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            cpu: default_cpu(),
            memory: default_memory(),
            disk: None,
        }
    }
}

impl Resources {
    pub fn to_tart(&self) -> tart::Resources {
        tart::Resources {
            cpu: Some(self.cpu),
            memory_mib: Some(self.memory),
            disk_gb: self.disk,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Mount {
    /// Share name and virtiofs tag. Defaults to the project directory name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub source: String,
    /// Absolute guest mount point. Defaults to `/opt/<name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub readonly: bool,
}

/// A [`Mount`] with its name and target defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMount {
    /// Share name and virtiofs tag.
    pub name: String,
    /// Config-relative host path.
    pub source: String,
    /// Absolute guest mount point.
    pub target: String,
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Copy {
    pub source: String,
    pub target: String,
}

/// One script step: exactly one of `inline` or `path`. Shared by `[[provision]]`,
/// `[[on-boot]]`, and `[[on-shutdown]]`.
///
/// A step may name itself with `id` and depend on earlier steps with `needs`.
/// A step whose dependency did not succeed is skipped, not run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Step {
    /// Optional name, so later steps can depend on this one with `needs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Ids of earlier steps this one depends on. Skip this step if any of them
    /// did not succeed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
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
        // Mounts need the project directory to resolve their defaults, so they
        // are validated in `Project::mounts`.
        for c in &self.copies {
            if !Path::new(&c.target).is_absolute() {
                bail!("copy target `{}` must be an absolute guest path", c.target);
            }
        }
        validate_steps(&self.provisions, "provision")?;
        validate_steps(&self.on_boots, "on-boot")?;
        validate_steps(&self.on_shutdowns, "on-shutdown")?;
        Ok(())
    }

    /// The on-shutdown steps to run, with the always-last `sync` appended.
    ///
    /// `sync` flushes the guest filesystem so a hard power-off keeps the writes.
    /// It is a plain step with no `needs`, so it runs even when an earlier step
    /// failed.
    pub fn on_shutdown_steps(&self) -> Vec<Step> {
        let mut steps = self.on_shutdowns.clone();
        steps.push(Step {
            id: None,
            needs: Vec::new(),
            inline: Some("sync".to_string()),
            path: None,
            shell: None,
            privileged: false,
        });
        steps
    }
}

/// Each step needs exactly one of `inline` or `path`. Every `needs` entry must
/// name an earlier step's `id`, and ids must be unique. `label` names the config
/// section in error messages.
fn validate_steps(steps: &[Step], label: &str) -> Result<()> {
    let mut ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (i, s) in steps.iter().enumerate() {
        let n = i + 1;
        match (&s.inline, &s.path) {
            (Some(_), Some(_)) => bail!("[[{label}]] #{n}: set only one of `inline` or `path`"),
            (None, None) => bail!("[[{label}]] #{n}: needs either `inline` or `path`"),
            _ => {}
        }
        // Depend only on earlier steps, so a step's outcome is known when a
        // later step asks about it.
        for dep in &s.needs {
            if !ids.contains(dep.as_str()) {
                bail!(
                    "[[{label}]] #{n}: `needs` refers to `{dep}`, which is not an earlier step id"
                );
            }
        }
        if let Some(id) = s.id.as_deref() {
            if id.trim().is_empty() {
                bail!("[[{label}]] #{n}: `id` must not be empty");
            }
            if !ids.insert(id) {
                bail!("[[{label}]] #{n}: duplicate step id `{id}`");
            }
        }
    }
    Ok(())
}

impl Project {
    /// Load an explicit config file when `file` is given, otherwise discover
    /// `dirtbag.toml` from the current working directory.
    pub fn find(file: Option<&Path>) -> Result<Self> {
        match file {
            Some(path) => Self::load(path),
            None => Self::discover_cwd(),
        }
    }

    /// Discover the project from the current working directory.
    pub fn discover_cwd() -> Result<Self> {
        let cwd = std::env::current_dir().context("resolving current directory")?;
        Self::discover(&cwd)
    }

    /// Find the project. Search from `start` up through the parent directories
    /// for `dirtbag.toml`.
    pub fn discover(start: &Path) -> Result<Self> {
        let path = find_config(start).with_context(|| {
            format!(
                "no {CONFIG_FILE} found in {} or any parent",
                start.display()
            )
        })?;
        Self::load(&path)
    }

    /// Load the project from an explicit config file path. The file's parent
    /// directory becomes the project root that relative paths resolve against.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        // Resolve to an absolute path so the root and the derived VM name do not
        // depend on the current directory. The read above proves the file
        // exists, so join the cwd only if `canonicalize` fails.
        let config_path = std::fs::canonicalize(path).unwrap_or_else(|_| {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        });
        let root = config_path
            .parent()
            .expect("an absolute config path has a parent")
            .to_path_buf();
        let config = Config::parse(&text)?;
        Ok(Self {
            root,
            config_path,
            config,
        })
    }

    /// The mounts with defaults filled in and validated. `name` defaults to the
    /// project directory name; `target` defaults to `/opt/<name>`.
    pub fn mounts(&self) -> Result<Vec<ResolvedMount>> {
        let default_name = self.default_share_name();
        let mut seen_names = std::collections::HashSet::new();
        let mut seen_targets = std::collections::HashSet::new();
        let mut resolved = Vec::with_capacity(self.config.mounts.len());
        for m in &self.config.mounts {
            let name = m.name.clone().unwrap_or_else(|| default_name.clone());
            if name.trim().is_empty() {
                bail!("a [[mount]] `name` must not be empty");
            }
            let target = m.target.clone().unwrap_or_else(|| format!("/opt/{name}"));
            if !Path::new(&target).is_absolute() {
                bail!("mount `{name}` target must be an absolute guest path");
            }
            if !seen_names.insert(name.clone()) {
                bail!(
                    "duplicate mount name `{name}` — names become virtiofs tags and must be unique"
                );
            }
            if !seen_targets.insert(target.clone()) {
                bail!("duplicate mount target `{target}`");
            }
            resolved.push(ResolvedMount {
                name,
                source: m.source.clone(),
                target,
                readonly: m.readonly,
            });
        }
        Ok(resolved)
    }

    /// Resolve mounts to absolute-host-path [`DirShare`]s for `tart run`.
    pub fn dir_shares(&self) -> Result<Vec<DirShare>> {
        Ok(self
            .mounts()?
            .into_iter()
            .map(|m| DirShare {
                name: m.name,
                path: self.resolve_host_path(&m.source),
                readonly: m.readonly,
            })
            .collect())
    }

    /// Mount sources that do not exist on the host, as (name, resolved path).
    /// Tart needs the host path to exist, so warn about these before boot.
    pub fn missing_mount_sources(&self) -> Result<Vec<(String, String)>> {
        Ok(self
            .mounts()?
            .into_iter()
            .filter_map(|m| {
                let resolved = self.resolve_host_path(&m.source);
                (!Path::new(&resolved).exists()).then_some((m.name, resolved))
            })
            .collect())
    }

    /// Default share name and virtiofs tag: the sanitized project directory name.
    fn default_share_name(&self) -> String {
        share_name_for(&self.root)
    }

    /// Resolve a config-relative host path to an absolute string.
    pub fn resolve_host_path(&self, p: &str) -> String {
        let joined = self.root.join(p);
        std::fs::canonicalize(&joined)
            .unwrap_or(joined)
            .to_string_lossy()
            .into_owned()
    }

    /// The Tart VM name: the configured `name`, or one derived from the project
    /// path. dirtbag stores no name — tart is the source of truth for the VM.
    pub fn vm_name(&self) -> String {
        self.config
            .name
            .clone()
            .unwrap_or_else(|| derive_vm_name(&self.root, self.config_stem()))
    }

    /// The project-local `.dirtbag/` directory (holds the run log).
    pub fn dirtbag_dir(&self) -> PathBuf {
        self.root.join(DIRTBAG_DIR)
    }

    /// The detached `tart run` log file. A non-default config file gets its own
    /// log (`<stem>.run.log`) so sibling sandboxes do not clobber each other.
    pub fn run_log(&self) -> PathBuf {
        let name = match self.config_stem() {
            DEFAULT_STEM => RUN_LOG.to_string(),
            stem => format!("{stem}.{RUN_LOG}"),
        };
        self.dirtbag_dir().join(name)
    }

    /// The config file's stem — `dirtbag` for the default `dirtbag.toml`,
    /// otherwise the file name without its final extension. It keeps sibling
    /// sandboxes in one directory distinct.
    fn config_stem(&self) -> &str {
        self.config_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(DEFAULT_STEM)
    }
}

/// Derive a stable VM name from a project directory: `dirtbag-<dir>-<hash>`.
///
/// The hash is taken over the canonical path, so the name is unique per project
/// directory and stable as long as the directory is not moved. Set `name` in
/// the config to pin a VM across moves.
///
/// A non-default `stem` (any config file other than `dirtbag.toml`) folds into
/// both the base and the hash, so two config files in one directory derive
/// different names. The default stem changes nothing, so earlier VM names stay
/// stable.
fn derive_vm_name(root: &Path, stem: &str) -> String {
    let mut base = root
        .file_name()
        .map(|s| sanitize(&s.to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vm".to_string());

    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    abs.hash(&mut hasher);

    if stem != DEFAULT_STEM {
        base = format!("{base}-{}", sanitize(stem));
        stem.hash(&mut hasher);
    }

    format!(
        "dirtbag-{base}-{:08x}",
        (hasher.finish() & 0xffff_ffff) as u32
    )
}

/// The default share name for a project directory: its sanitized name, or
/// `project` when the directory has no usable name. Shared with `init`.
pub fn share_name_for(root: &Path) -> String {
    root.file_name()
        .map(|s| sanitize(&s.to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

/// Keep `[A-Za-z0-9_-]`. Replace all other characters with `-`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
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

[[on-boot]]
id = "warm"
inline = "echo booted"

[[on-boot]]
needs = ["warm"]
path = "scripts/start.sh"
privileged = true

[[on-shutdown]]
inline = "echo bye"

[ssh]
user = "admin"
password = "admin"
"#;

    #[test]
    fn parses_full_config() {
        let c = Config::parse(FULL).unwrap();
        assert_eq!(c.name.as_deref(), Some("demo"));
        assert_eq!(c.resources.cpu, 4);
        assert_eq!(c.mounts.len(), 2);
        assert!(c.mounts[1].readonly);
        assert_eq!(c.copies.len(), 1);
        assert_eq!(c.provisions.len(), 1);
        assert!(c.provisions[0].privileged);
        assert_eq!(c.on_boots.len(), 2);
        assert_eq!(c.on_boots[0].inline.as_deref(), Some("echo booted"));
        assert_eq!(c.on_boots[0].id.as_deref(), Some("warm"));
        assert!(c.on_boots[1].privileged);
        assert_eq!(c.on_boots[1].needs, ["warm"]);
        assert_eq!(c.on_shutdowns.len(), 1);
        assert_eq!(c.on_shutdowns[0].inline.as_deref(), Some("echo bye"));
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
    fn resources_default_when_omitted() {
        let c = Config::parse("image = \"x\"").unwrap();
        assert_eq!(c.resources.cpu, 2);
        assert_eq!(c.resources.memory, 4096);
        assert_eq!(c.resources.disk, None);
    }

    #[test]
    fn partial_resources_keep_defaults() {
        let c = Config::parse("image = \"x\"\n[resources]\ncpu = 8\n").unwrap();
        assert_eq!(c.resources.cpu, 8);
        assert_eq!(c.resources.memory, 4096);
    }

    #[test]
    fn mount_name_and_target_default_to_dir_name() {
        let project = project_at("/tmp/myproj", "image = \"x\"\n[[mount]]\nsource = \".\"\n");
        let mounts = project.mounts().unwrap();
        assert_eq!(mounts[0].name, "myproj");
        assert_eq!(mounts[0].target, "/opt/myproj");
    }

    #[test]
    fn round_trips_through_toml() {
        let c = Config::parse(FULL).unwrap();
        let serialized = toml::to_string(&c).unwrap();
        let again = Config::parse(&serialized).unwrap();
        assert_eq!(c, again);
    }

    /// A project rooted at `root` with the default `dirtbag.toml` config file.
    fn project_at(root: &str, config: &str) -> Project {
        let root = PathBuf::from(root);
        Project {
            config_path: root.join(CONFIG_FILE),
            root,
            config: Config::parse(config).unwrap(),
        }
    }

    fn project_with(config: &str) -> Project {
        project_at("/tmp/proj", config)
    }

    #[test]
    fn rejects_relative_mount_target() {
        let err =
            project_with("image=\"x\"\n[[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"rel/path\"\n")
                .mounts()
                .unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn rejects_provision_with_both_inline_and_path() {
        let err = Config::parse("image = \"x\"\n[[provision]]\ninline=\"echo\"\npath=\"s.sh\"\n")
            .unwrap_err();
        assert!(err.to_string().contains("only one"));
    }

    #[test]
    fn rejects_empty_provision() {
        let err = Config::parse("image = \"x\"\n[[provision]]\nshell=\"bash\"\n").unwrap_err();
        assert!(err.to_string().contains("either"));
    }

    #[test]
    fn rejects_empty_on_boot() {
        let err = Config::parse("image = \"x\"\n[[on-boot]]\nshell=\"bash\"\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("on-boot"));
        assert!(msg.contains("either"));
    }

    #[test]
    fn rejects_on_boot_with_both_inline_and_path() {
        let err = Config::parse("image = \"x\"\n[[on-boot]]\ninline=\"echo\"\npath=\"s.sh\"\n")
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("on-boot"));
        assert!(msg.contains("only one"));
    }

    #[test]
    fn on_boots_default_empty() {
        let c = Config::parse("image = \"x\"").unwrap();
        assert!(c.on_boots.is_empty());
    }

    #[test]
    fn rejects_empty_on_shutdown() {
        let err = Config::parse("image = \"x\"\n[[on-shutdown]]\nshell=\"bash\"\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("on-shutdown"));
        assert!(msg.contains("either"));
    }

    #[test]
    fn on_shutdowns_default_empty() {
        let c = Config::parse("image = \"x\"").unwrap();
        assert!(c.on_shutdowns.is_empty());
    }

    #[test]
    fn on_shutdown_steps_append_sync() {
        let c = Config::parse("image=\"x\"\n[[on-shutdown]]\ninline=\"echo bye\"\n").unwrap();
        let steps = c.on_shutdown_steps();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].inline.as_deref(), Some("echo bye"));
        assert_eq!(steps[1].inline.as_deref(), Some("sync"));
    }

    #[test]
    fn on_shutdown_steps_are_just_sync_by_default() {
        let steps = Config::parse("image=\"x\"").unwrap().on_shutdown_steps();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].inline.as_deref(), Some("sync"));
    }

    #[test]
    fn accepts_needs_referring_to_an_earlier_id() {
        let c = Config::parse(
            "image=\"x\"\n\
             [[provision]]\nid=\"a\"\ninline=\"echo a\"\n\
             [[provision]]\nneeds=[\"a\"]\ninline=\"echo b\"\n",
        )
        .unwrap();
        assert_eq!(c.provisions[1].needs, ["a"]);
    }

    #[test]
    fn rejects_needs_on_unknown_id() {
        let err = Config::parse("image=\"x\"\n[[provision]]\nneeds=[\"ghost\"]\ninline=\"echo\"\n")
            .unwrap_err();
        assert!(err.to_string().contains("not an earlier step id"));
    }

    #[test]
    fn rejects_needs_on_a_later_step() {
        // A step may depend only on an earlier step, so a forward reference fails.
        let err = Config::parse(
            "image=\"x\"\n\
             [[provision]]\nneeds=[\"b\"]\ninline=\"echo a\"\n\
             [[provision]]\nid=\"b\"\ninline=\"echo b\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not an earlier step id"));
    }

    #[test]
    fn rejects_self_referential_needs() {
        let err =
            Config::parse("image=\"x\"\n[[provision]]\nid=\"a\"\nneeds=[\"a\"]\ninline=\"echo\"\n")
                .unwrap_err();
        assert!(err.to_string().contains("not an earlier step id"));
    }

    #[test]
    fn rejects_duplicate_step_id() {
        let err = Config::parse(
            "image=\"x\"\n\
             [[provision]]\nid=\"a\"\ninline=\"echo\"\n\
             [[provision]]\nid=\"a\"\ninline=\"echo\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate step id"));
    }

    #[test]
    fn rejects_duplicate_mount_names() {
        let err = project_with(
            "image=\"x\"\n\
             [[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"/a\"\n\
             [[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"/b\"\n",
        )
        .mounts()
        .unwrap_err();
        assert!(err.to_string().contains("duplicate mount name"));
    }

    #[test]
    fn rejects_duplicate_mount_targets() {
        let err = project_with(
            "image=\"x\"\n\
             [[mount]]\nname=\"p\"\nsource=\".\"\ntarget=\"/same\"\n\
             [[mount]]\nname=\"q\"\nsource=\".\"\ntarget=\"/same\"\n",
        )
        .mounts()
        .unwrap_err();
        assert!(err.to_string().contains("duplicate mount target"));
    }

    #[test]
    fn rejects_empty_image() {
        assert!(
            Config::parse("image = \"\"")
                .unwrap_err()
                .to_string()
                .contains("image")
        );
    }

    #[test]
    fn vm_name_prefers_config_name() {
        let project = project_at("/tmp/whatever", "image = \"x\"\nname = \"pinned\"");
        assert_eq!(project.vm_name(), "pinned");
    }

    #[test]
    fn vm_name_derives_deterministically_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let project = project_at(&dir.path().to_string_lossy(), "image = \"x\"");
        let name = project.vm_name();
        assert_eq!(name, project.vm_name());
        assert!(name.starts_with("dirtbag-"));
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn a_non_default_config_file_derives_a_distinct_vm_name_and_run_log() {
        let root = PathBuf::from("/tmp/proj");
        let default = project_at("/tmp/proj", "image = \"x\"");
        let sibling = Project {
            root: root.clone(),
            config_path: root.join("dirtbag.dev.toml"),
            config: Config::parse("image = \"x\"").unwrap(),
        };
        // Same directory, but the sibling file gets its own VM and its own log.
        assert_ne!(default.vm_name(), sibling.vm_name());
        assert_eq!(default.run_log(), root.join(".dirtbag/run.log"));
        assert_eq!(sibling.run_log(), root.join(".dirtbag/dirtbag.dev.run.log"));
    }

    #[test]
    fn run_log_is_under_dirtbag_dir() {
        let project = project_at("/tmp/proj", "image = \"x\"");
        assert_eq!(
            project.run_log(),
            PathBuf::from("/tmp/proj/.dirtbag/run.log")
        );
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(sanitize("my project!@#"), "my-project---");
    }
}
