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

#[test]
fn init_with_file_scaffolds_named_config() {
    let dir = tempdir().unwrap();
    let out = run_in(dir.path(), &["--file", "dirtbag.dev.toml", "init"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.path().join("dirtbag.dev.toml").is_file());
    // init leaves the default file untouched, so both live side by side.
    assert!(!dir.path().join("dirtbag.toml").exists());
    assert!(String::from_utf8_lossy(&out.stdout).contains("dirtbag.dev.toml"));
}

#[test]
fn file_option_selects_a_sibling_config() {
    let dir = tempdir().unwrap();
    // Two sandboxes in one directory. `up` on the named one must read it, not
    // the default, so a bad image in the sibling is what fails.
    run_in(dir.path(), &["init"]);
    std::fs::write(dir.path().join("dirtbag.dev.toml"), "image = \"\"\n").unwrap();

    let out = run_in(dir.path(), &["--file", "dirtbag.dev.toml", "up"]);
    assert!(!out.status.success());
    // Validation rejects the empty image. This proves dirtbag read the sibling.
    assert!(String::from_utf8_lossy(&out.stderr).contains("image"));
}

#[test]
fn file_option_reports_a_missing_file() {
    let dir = tempdir().unwrap();
    let out = run_in(dir.path(), &["--file", "nope.toml", "status"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope.toml"));
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

/// Two sandboxes in one directory (`dirtbag.toml` + a `-f` sibling) are fully
/// independent VMs. Destroy one; the other keeps running with its state intact.
/// Ignored by default; needs tart + a Linux image pull.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_sibling_sandboxes_are_independent() {
    let dir = tempdir().unwrap();
    // Same image (pulled once), but a distinct mount tag per sandbox so an `ls`
    // inside each VM proves it booted from its own config.
    let config = |tag: &str| {
        format!(
            "image = \"ghcr.io/cirruslabs/ubuntu:latest\"\n\
             [resources]\ncpu = 2\nmemory = 2048\n\
             [[mount]]\nname = \"{tag}\"\nsource = \".\"\ntarget = \"/opt/{tag}\"\n",
        )
    };
    std::fs::write(dir.path().join("dirtbag.toml"), config("main")).unwrap();
    std::fs::write(dir.path().join("dirtbag.gpu.toml"), config("gpu")).unwrap();

    let gpu = ["--file", "dirtbag.gpu.toml"];
    let with = |args: &[&str], extra: &[&str]| -> Vec<String> {
        args.iter().chain(extra).map(|s| s.to_string()).collect()
    };
    let run_gpu = |args: &[&str]| {
        let a = with(&gpu, args);
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        run_in(dir.path(), &a)
    };

    // Boot both sandboxes from the one directory.
    let up_main = run_in(dir.path(), &["up"]);
    assert!(
        up_main.status.success(),
        "main up failed: {}",
        String::from_utf8_lossy(&up_main.stderr)
    );
    let up_gpu = run_gpu(&["up"]);
    assert!(
        up_gpu.status.success(),
        "gpu up failed: {}",
        String::from_utf8_lossy(&up_gpu.stderr)
    );

    // They are two different VMs, each with its own detached run log.
    let name_main = vm_name_from_up(&up_main);
    let name_gpu = vm_name_from_up(&up_gpu);
    assert_ne!(
        name_main, name_gpu,
        "siblings must derive distinct VM names"
    );
    assert!(dir.path().join(".dirtbag/run.log").is_file());
    assert!(dir.path().join(".dirtbag/dirtbag.gpu.run.log").is_file());

    // Each VM mounted the tag from its own config, not the sibling's.
    assert!(
        run_gpu(&["ssh", "--", "ls", "/opt/gpu/dirtbag.gpu.toml"])
            .status
            .success(),
        "gpu VM missing its own mount"
    );
    assert!(
        !run_gpu(&["ssh", "--", "ls", "/opt/main"]).status.success(),
        "gpu VM should not have the main sandbox's mount"
    );

    // Destroy the default; the sibling keeps running with its log intact.
    assert!(run_in(dir.path(), &["destroy"]).status.success());
    assert!(
        String::from_utf8_lossy(&run_in(dir.path(), &["status"]).stdout).contains("not created"),
        "main VM should be gone after destroy"
    );
    assert!(
        dir.path().join(".dirtbag/dirtbag.gpu.run.log").is_file(),
        "destroying the default must not remove the sibling's run log"
    );
    let status_gpu = run_gpu(&["status"]);
    let gpu_out = String::from_utf8_lossy(&status_gpu.stdout);
    assert!(gpu_out.contains(&name_gpu), "sibling VM should still exist");
    assert!(
        !gpu_out.contains("not created"),
        "sibling VM should still be up:\n{gpu_out}"
    );

    // The sibling still works end to end, then destroys cleanly.
    assert!(run_gpu(&["ssh", "--", "true"]).status.success());
    assert!(run_gpu(&["destroy"]).status.success());
    assert!(
        !dir.path().join(".dirtbag").exists(),
        "the shared state dir should be gone once both sandboxes are destroyed"
    );
}

/// Extract the VM name from an `up` success line: ``VM `name` is up at IP``.
fn vm_name_from_up(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split('`')
        .nth(1)
        .unwrap_or_else(|| panic!("no VM name in up output:\n{stdout}"))
        .to_string()
}

/// A VM provisions one time. A down then a restart re-mounts the shares but
/// does not run the provisioners again.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_provision_runs_once_across_restarts() {
    let dir = tempdir().unwrap();
    // Each provisioner run appends a line to one guest file; each on-boot run
    // appends to another. The line counts show provision runs once while
    // on-boot runs on every boot. The on-shutdown list also exercises
    // dependency skipping: a failed step's dependent must not run, while the
    // implicit `sync` (no deps) still does.
    std::fs::write(
        dir.path().join("dirtbag.toml"),
        "image = \"ghcr.io/cirruslabs/ubuntu:latest\"\n\
         [resources]\ncpu = 2\nmemory = 2048\n\
         [[mount]]\nname = \"project\"\nsource = \".\"\ntarget = \"/opt/project\"\n\
         [[provision]]\nprivileged = true\ninline = '''\necho ran >> /etc/dirtbag-provisions\n'''\n\
         [[on-boot]]\nprivileged = true\ninline = '''\necho ran >> /etc/dirtbag-onboot\n'''\n\
         [[on-shutdown]]\nprivileged = true\ninline = '''\necho ran >> /etc/dirtbag-onshutdown\n'''\n\
         [[on-shutdown]]\nid = \"drain\"\ninline = '''\nexit 1\n'''\n\
         [[on-shutdown]]\nneeds = [\"drain\"]\nprivileged = true\ninline = '''\ntouch /etc/dirtbag-skipped\n'''\n",
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

    // The on-shutdown steps ran on the one `down` between the two boots. The
    // `sync` (no deps) ran too, so this file survived the hard power-off.
    assert_eq!(
        count_shutdowns(dir.path()),
        1,
        "on-shutdown steps must run on every stop"
    );

    // The step that needs the failed `drain` was skipped, so it never created
    // its marker file.
    let skipped = run_in(dir.path(), &["ssh", "--", "ls", "/etc/dirtbag-skipped"]);
    assert!(
        !skipped.status.success(),
        "step depending on a failed step must be skipped"
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
