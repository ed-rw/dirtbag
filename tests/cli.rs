//! CLI integration tests.
//!
//! These drive the built `dirtbag` binary. The default set needs no VM (config
//! discovery, `init`, argument errors). The end-to-end test is `#[ignore]`d
//! because it needs an Apple-Silicon host with `tart`, network access, and
//! macOS Local Network permission; run it with `cargo test -- --ignored`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

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

/// Write a stub `tart` that reports no VMs: `list` gives an empty set, `get`
/// reports that the VM does not exist, and every other subcommand fails. A real
/// `tart` does the same for a VM that was never created.
fn stub_tart() -> TempDir {
    let dir = tempdir().unwrap();
    let bin = dir.path().join("tart");
    std::fs::write(
        &bin,
        "#!/bin/sh\ncase \"$1\" in\n\
         list) echo '[]' ;;\n\
         get) echo \"the specified VM \\\"$2\\\" does not exist\" >&2; exit 2 ;;\n\
         *) exit 1 ;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// Run dirtbag against [`stub_tart`] instead of the host's `tart`. Tests of the
/// "no VM yet" paths need a `tart` on PATH to reach the message they assert,
/// but no real one: a CI runner has none, and a developer's own VMs must not
/// change the result.
fn run_with_stub_tart(dir: &Path, args: &[&str]) -> Output {
    let stub = stub_tart();
    let path = match std::env::var_os("PATH") {
        Some(p) => format!("{}:{}", stub.path().display(), p.to_string_lossy()),
        None => stub.path().display().to_string(),
    };
    dirtbag()
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
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
        "version",
        "update",
    ] {
        assert!(text.contains(cmd), "help missing `{cmd}`:\n{text}");
    }
}

/// `version` prints the crate version and works outside a project.
#[test]
fn version_reports_dirtbag_and_tart() {
    let dir = tempdir().unwrap();
    let out = run_in(dir.path(), &["version"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("dirtbag {}", env!("CARGO_PKG_VERSION"))),
        "unexpected version output:\n{text}"
    );
    assert!(text.contains("tart"), "no tart line:\n{text}");
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
    let out = run_with_stub_tart(dir.path(), &["status"]);
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
    let out = run_with_stub_tart(dir.path(), &["ssh", "--", "true"]);
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

#[test]
fn instance_option_rejects_unsafe_names() {
    let dir = tempdir().unwrap();
    run_in(dir.path(), &["init"]);
    let out = run_with_stub_tart(dir.path(), &["--instance", "a/b", "status"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("instance `a/b`"));
}

#[test]
fn status_of_a_missing_instance_reports_not_created() {
    let dir = tempdir().unwrap();
    run_in(dir.path(), &["init"]);
    let out = run_with_stub_tart(dir.path(), &["-i", "two", "status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("not created"));
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
    assert_ok(&destroy, "destroy");
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
    assert_ok(&run_in(dir.path(), &["destroy"]), "destroy");
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
    assert_ok(&run_gpu(&["destroy"]), "destroy");
    assert!(
        !dir.path().join(".dirtbag").exists(),
        "the shared state dir should be gone once both sandboxes are destroyed"
    );
}

/// Two instances of one config file run as separate VMs. Each has its own
/// name, run log and guest disk, and `status` without `-i` lists both.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_instances_of_one_config_are_independent() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("dirtbag.toml"),
        "image = \"ghcr.io/cirruslabs/ubuntu:latest\"\n\
         [resources]\ncpu = 2\nmemory = 2048\n\
         [[mount]]\nname = \"project\"\nsource = \".\"\ntarget = \"/opt/project\"\n",
    )
    .unwrap();
    let run_two = |args: &[&str]| {
        let a: Vec<&str> = ["-i", "two"].iter().chain(args).copied().collect();
        run_in(dir.path(), &a)
    };

    let up_base = run_in(dir.path(), &["up"]);
    assert!(
        up_base.status.success(),
        "base up failed: {}",
        String::from_utf8_lossy(&up_base.stderr)
    );
    let up_two = run_two(&["up"]);
    assert!(
        up_two.status.success(),
        "instance up failed: {}",
        String::from_utf8_lossy(&up_two.stderr)
    );

    let name_base = vm_name_from_up(&up_base);
    let name_two = vm_name_from_up(&up_two);
    assert_eq!(name_two, format!("{name_base}-two"));
    assert!(dir.path().join(".dirtbag/run.log").is_file());
    assert!(dir.path().join(".dirtbag/two.run.log").is_file());

    // Both instances mount the same host directory.
    assert!(
        run_two(&["ssh", "--", "ls", "/opt/project/dirtbag.toml"])
            .status
            .success(),
        "instance missing the shared mount"
    );

    // A guest file in one instance does not appear in the other, so each
    // instance has its own disk.
    assert!(
        run_two(&["ssh", "--", "touch", "/tmp/only-two"])
            .status
            .success()
    );
    assert!(
        !run_in(dir.path(), &["ssh", "--", "ls", "/tmp/only-two"])
            .status
            .success(),
        "base VM should not see the instance's guest file"
    );

    let status = run_in(dir.path(), &["status"]);
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_out.contains(&name_base) && status_out.contains(&name_two),
        "status should list both instances:\n{status_out}"
    );

    // Destroy the instance. The base VM keeps running with its log intact.
    assert_ok(&run_two(&["destroy"]), "destroy");
    assert!(
        String::from_utf8_lossy(&run_two(&["status"]).stdout).contains("not created"),
        "instance should be gone after destroy"
    );
    assert!(!dir.path().join(".dirtbag/two.run.log").exists());
    assert!(dir.path().join(".dirtbag/run.log").is_file());
    assert!(run_in(dir.path(), &["ssh", "--", "true"]).status.success());

    assert_ok(&run_in(dir.path(), &["destroy"]), "destroy");
    assert!(
        !dir.path().join(".dirtbag").exists(),
        "the state dir should be gone once both instances are destroyed"
    );
}

/// A config with port forwards, on the cached Ubuntu image.
fn forward_config(forwards: &str) -> String {
    format!(
        "image = \"ghcr.io/cirruslabs/ubuntu:latest\"\n\
         [resources]\ncpu = 2\nmemory = 2048\n\
         [network]\nforward = [{forwards}]\n"
    )
}

/// The host ports from the `forward 127.0.0.1:PORT -> GUEST` lines of `up`.
fn forwarded_ports(out: &Output) -> Vec<u16> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("forward "))
        .map(|l| {
            let host = l.split(" -> ").next().unwrap();
            host.rsplit(':').next().unwrap().parse().unwrap()
        })
        .collect()
}

/// Read the SSH banner through a forwarded host port. The guest's sshd
/// listens on port 22, so a banner proves the relay reaches the guest.
fn ssh_banner(port: u16) -> String {
    use std::io::Read;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap();
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// A free host port, released again for the test to use.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Auto ports (`0:GUEST`) give each instance its own host port, and each one
/// reaches its own guest.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_auto_forwards_give_each_instance_a_port() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("dirtbag.toml"), forward_config("\"0:22\"")).unwrap();

    let up_base = run_in(dir.path(), &["up"]);
    assert!(
        up_base.status.success(),
        "base up failed: {}",
        String::from_utf8_lossy(&up_base.stderr)
    );
    let up_two = run_in(dir.path(), &["-i", "two", "up"]);
    assert!(
        up_two.status.success(),
        "instance up failed: {}",
        String::from_utf8_lossy(&up_two.stderr)
    );

    let (base_ports, two_ports) = (forwarded_ports(&up_base), forwarded_ports(&up_two));
    assert_eq!(base_ports.len(), 1, "{base_ports:?}");
    assert_eq!(two_ports.len(), 1, "{two_ports:?}");
    assert_ne!(base_ports[0], two_ports[0]);
    for port in [base_ports[0], two_ports[0]] {
        let banner = ssh_banner(port);
        assert!(banner.starts_with("SSH-2.0"), "port {port}: {banner:?}");
    }

    let status = String::from_utf8_lossy(&run_in(dir.path(), &["status"]).stdout).into_owned();
    for port in [base_ports[0], two_ports[0]] {
        assert!(
            status.contains(&format!("127.0.0.1:{port} -> 22")),
            "status should show port {port}:\n{status}"
        );
    }

    let two_status =
        String::from_utf8_lossy(&run_in(dir.path(), &["-i", "two", "status"]).stdout).into_owned();
    assert!(
        two_status.contains(&format!("forward: 127.0.0.1:{} -> 22", two_ports[0])),
        "{two_status}"
    );

    // A changed forward list applies only on reload, so `up` warns.
    std::fs::write(
        dir.path().join("dirtbag.toml"),
        forward_config("\"0:22\", \"0:80\""),
    )
    .unwrap();
    let again = run_in(dir.path(), &["up"]);
    assert!(
        again.status.success(),
        "up after the config change failed: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("dirtbag reload"),
        "stderr: {}",
        String::from_utf8_lossy(&again.stderr)
    );

    assert_ok(
        &run_in(dir.path(), &["-i", "two", "destroy"]),
        "instance destroy",
    );
    assert_ok(&run_in(dir.path(), &["destroy"]), "base destroy");
}

/// A fixed host port belongs to one VM at a time. A second instance fails
/// before it clones, and `down` releases the port.
#[test]
#[ignore = "needs Apple-Silicon host, tart, and network; run with --ignored"]
fn e2e_fixed_forward_conflicts_fail_fast_and_down_releases_the_port() {
    let dir = tempdir().unwrap();
    let port = free_port();
    std::fs::write(
        dir.path().join("dirtbag.toml"),
        forward_config(&format!("\"{port}:22\"")),
    )
    .unwrap();

    let up = run_in(dir.path(), &["up"]);
    assert!(
        up.status.success(),
        "up failed: {}",
        String::from_utf8_lossy(&up.stderr)
    );
    let name = vm_name_from_up(&up);
    assert!(ssh_banner(port).starts_with("SSH-2.0"));
    assert!(
        String::from_utf8_lossy(&run_in(dir.path(), &["status"]).stdout)
            .contains(&format!("127.0.0.1:{port} -> 22"))
    );

    let clash = run_in(dir.path(), &["-i", "two", "up"]);
    assert!(
        !clash.status.success(),
        "a second instance must not take the port"
    );
    assert!(
        String::from_utf8_lossy(&clash.stderr).contains("not available"),
        "stderr: {}",
        String::from_utf8_lossy(&clash.stderr)
    );
    let list = Command::new("tart").arg("list").output().unwrap();
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains(&format!("{name}-two")),
        "the failed instance should not have been cloned"
    );

    assert!(run_in(dir.path(), &["down"]).status.success());
    let released = (0..10).any(|_| {
        port_is_free(port) || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            false
        }
    });
    assert!(released, "down should release host port {port}");

    // The forward comes back on the next boot.
    assert!(run_in(dir.path(), &["up"]).status.success());
    assert!(ssh_banner(port).starts_with("SSH-2.0"));
    assert_ok(&run_in(dir.path(), &["destroy"]), "destroy");
}

/// Assert that a dirtbag run succeeded. Show its stderr if it did not.
fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&out.stderr)
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

    assert_ok(&run_in(dir.path(), &["destroy"]), "destroy");
}
