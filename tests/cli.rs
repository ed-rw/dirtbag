//! CLI integration tests.
//!
//! These drive the built `dirtbag` binary. The default set needs no VM (config
//! discovery, `init`, argument errors). The end-to-end test is `#[ignore]`d
//! because it needs an Apple-Silicon host with `tart`, network access, and
//! macOS Local Network permission; run it with `cargo test -- --ignored`.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

fn dirtbag() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dirtbag"))
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    dirtbag()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawning dirtbag")
}

#[test]
fn help_lists_all_commands() {
    let out = dirtbag().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for cmd in [
        "init",
        "up",
        "ssh",
        "status",
        "down",
        "reload",
        "destroy",
        "provision",
    ] {
        assert!(text.contains(cmd), "help missing `{cmd}`:\n{text}");
    }
}

#[test]
fn init_scaffolds_then_refuses_overwrite() {
    let dir = tempdir().unwrap();

    let first = run_in(dir.path(), &["init"]);
    assert!(first.status.success());
    assert!(dir.path().join("dirtbag.toml").is_file());
    assert!(dir.path().join("scripts").join("setup.sh").is_file());

    let second = run_in(dir.path(), &["init"]);
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already exists"));
}

#[test]
fn up_without_config_reports_missing_toml() {
    let dir = tempdir().unwrap();
    let out = run_in(dir.path(), &["up"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dirtbag.toml"));
}

#[test]
fn status_in_fresh_project_reports_not_created() {
    let dir = tempdir().unwrap();
    run_in(dir.path(), &["init"]);
    let out = run_in(dir.path(), &["status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("not created"));
}

#[test]
fn ssh_without_state_reports_no_state() {
    let dir = tempdir().unwrap();
    run_in(dir.path(), &["init"]);
    let out = run_in(dir.path(), &["ssh", "--", "true"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dirtbag up"));
}

/// Full lifecycle against a real VM. Ignored by default; needs tart + a Linux
/// image pull + Local Network permission.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_up_ssh_destroy() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("dirtbag.toml"),
        "image = \"ghcr.io/cirruslabs/ubuntu:latest\"\n\
         [resources]\ncpu = 2\nmemory = 2048\n\
         [[mount]]\nname = \"project\"\nsource = \".\"\ntarget = \"/opt/project\"\n\
         [[provision]]\nprivileged = true\ninline = '''\napt-get install -y -qq jq\n'''\n",
    )
    .unwrap();

    let up = run_in(dir.path(), &["up"]);
    assert!(
        up.status.success(),
        "up failed: {}",
        String::from_utf8_lossy(&up.stderr)
    );

    // Provisioning ran.
    let jq = run_in(dir.path(), &["ssh", "--", "jq", "--version"]);
    assert!(jq.status.success());

    // Mount is live.
    let ls = run_in(
        dir.path(),
        &["ssh", "--", "ls", "/opt/project/dirtbag.toml"],
    );
    assert!(ls.status.success());

    let destroy = run_in(dir.path(), &["destroy"]);
    assert!(destroy.status.success());
    assert!(!dir.path().join(".dirtbag").exists());
}

/// A VM provisions one time. A down then a restart re-mounts the shares but
/// does not run the provisioners again.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_provision_runs_once_across_restarts() {
    let dir = tempdir().unwrap();
    // Each provisioner run appends a line to one guest file; each on-boot run
    // appends to another. The line counts show provision runs once while
    // on-boot runs on every boot.
    std::fs::write(
        dir.path().join("dirtbag.toml"),
        "image = \"ghcr.io/cirruslabs/ubuntu:latest\"\n\
         [resources]\ncpu = 2\nmemory = 2048\n\
         [[mount]]\nname = \"project\"\nsource = \".\"\ntarget = \"/opt/project\"\n\
         [[provision]]\nprivileged = true\ninline = '''\necho ran >> /etc/dirtbag-provisions\n'''\n\
         [[on-boot]]\nprivileged = true\ninline = '''\necho ran >> /etc/dirtbag-onboot\n'''\n\
         [[on-shutdown]]\nprivileged = true\ninline = '''\necho ran >> /etc/dirtbag-onshutdown\n'''\n",
    )
    .unwrap();

    let count_lines = |dir: &std::path::Path, path: &str| -> usize {
        let out = run_in(dir, &["ssh", "--", "cat", path]);
        assert!(
            out.status.success(),
            "cat {path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).lines().count()
    };
    let count_runs = |dir: &std::path::Path| count_lines(dir, "/etc/dirtbag-provisions");
    let count_boots = |dir: &std::path::Path| count_lines(dir, "/etc/dirtbag-onboot");
    let count_shutdowns = |dir: &std::path::Path| count_lines(dir, "/etc/dirtbag-onshutdown");

    let first = run_in(dir.path(), &["up"]);
    assert!(
        first.status.success(),
        "up failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        count_runs(dir.path()),
        1,
        "provisioners should run once on first boot"
    );
    assert_eq!(
        count_boots(dir.path()),
        1,
        "on-boot steps should run on the first boot"
    );

    assert!(run_in(dir.path(), &["down"]).status.success());

    let second = run_in(dir.path(), &["up"]);
    assert!(
        second.status.success(),
        "second up failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    // The share is mounted again after the restart.
    let ls = run_in(
        dir.path(),
        &["ssh", "--", "ls", "/opt/project/dirtbag.toml"],
    );
    assert!(ls.status.success(), "mount missing after restart");

    // The provisioners did not run a second time.
    assert_eq!(
        count_runs(dir.path()),
        1,
        "provisioners must not run again on restart"
    );

    // The on-boot steps run again on the second boot.
    assert_eq!(
        count_boots(dir.path()),
        2,
        "on-boot steps must run on every boot"
    );

    // The on-shutdown steps ran on the one `down` between the two boots.
    assert_eq!(
        count_shutdowns(dir.path()),
        1,
        "on-shutdown steps must run on every stop"
    );

    // `dirtbag provision` runs them again on demand.
    assert!(run_in(dir.path(), &["provision"]).status.success());
    assert_eq!(
        count_runs(dir.path()),
        2,
        "explicit provision should run the steps again"
    );

    assert!(run_in(dir.path(), &["destroy"]).status.success());
}
