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
        "init", "on", "ssh", "status", "stop", "reload", "destroy", "provision",
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
fn on_without_config_reports_missing_toml() {
    let dir = tempdir().unwrap();
    let out = run_in(dir.path(), &["on"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dirtbag.toml"));
}

#[test]
fn status_in_fresh_project_reports_not_created() {
    let dir = tempdir().unwrap();
    run_in(dir.path(), &["init"]);
    let out = run_in(dir.path(), &["status"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("not created"));
}

#[test]
fn ssh_without_state_reports_no_state() {
    let dir = tempdir().unwrap();
    run_in(dir.path(), &["init"]);
    let out = run_in(dir.path(), &["ssh", "--", "true"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dirtbag on"));
}

/// Full lifecycle against a real VM. Ignored by default; needs tart + a Linux
/// image pull + Local Network permission.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_on_ssh_destroy() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("dirtbag.toml"),
        "image = \"ghcr.io/cirruslabs/ubuntu:latest\"\n\
         [resources]\ncpu = 2\nmemory = 2048\n\
         [[mount]]\nname = \"project\"\nsource = \".\"\ntarget = \"/opt/project\"\n\
         [[provision]]\nprivileged = true\ninline = '''\napt-get install -y -qq jq\n'''\n",
    )
    .unwrap();

    let on = run_in(dir.path(), &["on"]);
    assert!(on.status.success(), "on failed: {}", String::from_utf8_lossy(&on.stderr));

    // Provisioning ran.
    let jq = run_in(dir.path(), &["ssh", "--", "jq", "--version"]);
    assert!(jq.status.success());

    // Mount is live.
    let ls = run_in(dir.path(), &["ssh", "--", "ls", "/opt/project/dirtbag.toml"]);
    assert!(ls.status.success());

    let destroy = run_in(dir.path(), &["destroy"]);
    assert!(destroy.status.success());
    assert!(!dir.path().join(".dirtbag").exists());
}
