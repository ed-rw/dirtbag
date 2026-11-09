//! The `update` command: replace this binary with the latest GitHub release.
//!
//! It uses the system `curl`, `tar` and `shasum`, which macOS supplies, so
//! dirtbag needs no HTTP or TLS crate.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::info;

const REPO: &str = "ed-rw/dirtbag";
const TARGET: &str = "aarch64-apple-darwin";

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
}

/// Download the latest release and install it over the running binary, unless
/// this binary is already at that version or newer. `/releases/latest` skips
/// pre-releases.
pub fn run() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let release: Release = serde_json::from_slice(&curl(&[&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    )])?)
    .context("parsing the latest release from GitHub")?;
    let latest = release.tag_name.trim_start_matches('v');

    if !is_newer(latest, current) {
        println!("dirtbag {current} is up to date (latest release is {latest})");
        return Ok(());
    }

    let exe = current_exe()?;
    info!(from = current, to = latest, path = %exe.display(), "updating");

    let tmp = tempfile::tempdir().context("creating a temporary directory")?;
    let bin = download(latest, tmp.path())?;
    install(&bin, &exe)?;

    println!("updated dirtbag {current} -> {latest} ({})", exe.display());
    Ok(())
}

/// Download and verify the release archive in `dir`, unpack it, and return
/// the path of the `dirtbag` binary in it.
fn download(version: &str, dir: &Path) -> Result<PathBuf> {
    let name = format!("dirtbag-{version}-{TARGET}");
    let archive = format!("{name}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");

    for file in [archive.clone(), format!("{archive}.sha256")] {
        let out = dir.join(&file);
        curl(&["-o", &out.to_string_lossy(), &format!("{base}/{file}")])?;
    }

    // The checksum file names the archive by its bare file name, so run the
    // check in `dir`.
    run_tool(
        Command::new("shasum")
            .args(["-a", "256", "-c", &format!("{archive}.sha256")])
            .current_dir(dir),
        "verifying the release checksum",
    )?;
    run_tool(
        Command::new("tar")
            .args(["-xzf", &archive])
            .current_dir(dir),
        "unpacking the release",
    )?;

    let bin = dir.join(&name).join("dirtbag");
    if !bin.is_file() {
        bail!("the release archive has no {name}/dirtbag");
    }
    Ok(bin)
}

/// Replace `exe` with `bin`. Copy to a file next to `exe` and then rename it,
/// so the swap is atomic and a failed copy leaves the old binary intact.
fn install(bin: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .context("the dirtbag binary has no parent directory")?;
    let staged = dir.join(".dirtbag.update");
    let result = std::fs::copy(bin, &staged)
        .and_then(|_| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        })
        .and_then(|_| std::fs::rename(&staged, exe));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&staged);
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            bail!(
                "no permission to write {}; run `sudo dirtbag update`",
                exe.display()
            );
        }
        return Err(e).with_context(|| format!("installing to {}", exe.display()));
    }
    Ok(())
}

/// The path of the running binary, with symlinks resolved, so the update
/// replaces the real file and not a link to it.
fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding the dirtbag binary")?;
    std::fs::canonicalize(&exe).with_context(|| format!("resolving {}", exe.display()))
}

/// Fetch a URL with `curl` and return the body. Fail on an HTTP error.
fn curl(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "User-Agent: dirtbag"])
        .args(args)
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!(
            "curl {} failed: {}",
            args.last().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn run_tool(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd.output().with_context(|| what.to_string())?;
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// True if `latest` is a newer `MAJOR.MINOR.PATCH` than `current`. If a version
/// does not parse, it is newer only when it differs. Thus a bad tag does not
/// block an update, and an equal tag does not start a second update.
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => latest != current,
    }
}

/// Parse `MAJOR.MINOR.PATCH`. Ignore a `-pre` or `+build` suffix.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.2.1"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
    }

    #[test]
    fn ignores_pre_release_suffixes() {
        assert_eq!(parse_version("0.3.0-rc1"), Some((0, 3, 0)));
        assert_eq!(parse_version("0.3"), None);
        assert_eq!(parse_version("0.3.0.1"), None);
    }

    #[test]
    fn unparsable_versions_update_only_when_different() {
        assert!(is_newer("nightly", "0.2.1"));
        assert!(!is_newer("nightly", "nightly"));
    }
}
