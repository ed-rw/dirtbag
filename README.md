# dirtbag

![OG dirtbag](https://encrypted-tbn0.gstatic.com/images?q=tbn:ANd9GcQIjOEsYxxTxENLI7y7o2Zo-SEzKI2kv7g0hij55QSoHA&s=10)

A CLI for managing [Tart](https://tart.run) VMs as disposable dev
sandboxes — declare a sandbox in `dirtbag.toml`, then `dirtbag up` to get a
running, mounted, provisioned VM you can `ssh` into and `destroy` when done.

Built for giving coding agents (and people) clean, reproducible, throwaway
environments to build and test in, isolated from the host.

```console
$ dirtbag init          # scaffold dirtbag.toml + scripts/setup.sh
$ dirtbag up            # clone → configure → boot headless → mount → copy → provision
$ dirtbag ssh           # drop into the VM
$ dirtbag ssh -- make test
$ dirtbag down          # stop it
$ dirtbag destroy       # delete the VM and local state
```

## Requirements

- An **Apple-Silicon Mac** (Tart uses Apple's Virtualization.framework).
- **Tart** installed: `brew install cirruslabs/cli/tart`.
- **Rust** (to build): `cargo build --release`.
- macOS **Local Network permission** for your terminal — see
  [Troubleshooting](#troubleshooting).

Linux guests are supported today; the guest layer is behind a trait so macOS and
other guests can be added later.

## Install

```console
$ cargo build --release
$ cp target/release/dirtbag /usr/local/bin/   # or anywhere on PATH
```

## Quick start

```console
$ mkdir my-sandbox && cd my-sandbox
$ dirtbag init
Created dirtbag.toml and scripts/setup.sh
$ dirtbag up
VM `dirtbag-my-sandbox-1a2b3c4d` is up at 192.168.64.3
$ dirtbag ssh -- uname -a
Linux ubuntu 7.0.0-... aarch64 GNU/Linux
$ dirtbag destroy
```

## Configuration — `dirtbag.toml`

`dirtbag` walks up from the current directory to find `dirtbag.toml`; all
relative paths resolve against that file's directory. Pass `-f <PATH>` to use a
different config file (see [Commands](#commands)).

```toml
# VM name. Optional — defaults to `dirtbag-<dir>-<hash>`, derived from the
# project path. Set it to pin the VM across directory moves.
name  = "my-sandbox"

# Base image to clone (OCI ref or a local VM name). Required. cirruslabs
# publishes ubuntu/debian/fedora images; tags at
# https://github.com/cirruslabs/tart/pkgs/container/ubuntu
image = "ghcr.io/cirruslabs/ubuntu:latest"

# Optional — defaults to cpu = 2, memory = 4096. Omit the whole section to
# accept the defaults. `disk` has no default.
[resources]
cpu    = 4       # number of vCPUs
memory = 8192    # memory in MiB
disk   = 50      # disk size in GiB — grow-only (Tart cannot shrink a disk)

# Live directory share (Tart --dir, virtiofs). Repeatable.
# `dirtbag init` fills name/target from the project directory; both are
# optional and default to that name and /opt/<name>.
[[mount]]
name     = "project"        # virtiofs tag
source   = "."              # host path, relative to dirtbag.toml (required)
target   = "/opt/project"   # absolute guest mount point
readonly = false

# One-shot copy-in over SCP at `up` time (not kept in sync). Repeatable.
# Copy targets must be writable by the ssh user (SCP runs without sudo).
[[copy]]
source = "./secrets.env"
target = "/home/admin/.env"  # absolute guest path

# Provisioning steps run ONCE to configure the machine, in order, over SSH.
# Repeatable. Each step has exactly one of `inline` or `path`.
[[provision]]
id     = "packages"                 # optional name, so later steps can `needs` it
inline = '''
apt-get update
apt-get install -y build-essential
'''
# path       = "scripts/setup.sh"  # a script file, relative to dirtbag.toml
# shell      = "bash"               # interpreter (default: bash)
privileged = true                   # run via sudo

# A step can depend on earlier steps by id. It is skipped (not run) if any
# dependency did not succeed, and the skip propagates to steps that need it.
[[provision]]
needs  = ["packages"]
inline = "make -C /opt/project"

# on-boot steps run EVERY time the machine boots, after provisioning — to
# prepare it for use (start services, agents, tunnels). Same shape as
# [[provision]]; repeatable.
[[on-boot]]
inline = "systemctl --user start my-agent"
# path       = "scripts/start.sh"
# privileged = true

# on-shutdown steps run EVERY time the machine stops (`down`/`reload`), before
# the VM powers off — to flush state or drain work. dirtbag always runs `sync`
# as the final step, so you never need to add it. Same shape as [[provision]];
# repeatable.
[[on-shutdown]]
inline = "systemctl --user stop my-agent"

# Optional — defaults to admin:admin (the Tart image default).
[ssh]
user     = "admin"
password = "admin"
```

Notes:
- **Mounts are attached at boot.** Changing `[[mount]]` on a running VM takes
  effect after `dirtbag reload`.
- **`[[provision]]` / `[[on-boot]]` / `[[on-shutdown]]`.** Provisioning
  configures the machine and runs once (first boot); on-boot prepares the machine
  for use and runs on every boot, after provisioning; on-shutdown winds it down
  and runs before every stop. All run when dirtbag drives the lifecycle
  (`up`/`down`/`reload`), not on a reboot or shutdown initiated inside the guest.
- **Step dependencies.** Give a step an `id`, then `needs = ["that-id"]` on a
  later step in the same list. Every runnable step is attempted in order — a
  failure does not halt the list — but a step is skipped when a step it `needs`
  did not succeed, and that skip propagates to anything that needs it. `up` and
  `provision` still exit non-zero if any step failed.
- On **Linux guests** dirtbag mounts each share inside the guest
  (`mount -t virtiofs <tag> <target>`); the mount point is created with `sudo`.
- **Inline scripts** use TOML literal strings (`'''…'''`), so shell content is
  taken verbatim — no escaping of `$`, quotes, or backslashes.

## Commands

| Command | Description |
|---|---|
| `dirtbag init` | Scaffold a starter `dirtbag.toml` and `scripts/setup.sh`. |
| `dirtbag up` | Clone (if needed) → apply resources → boot headless & detached → wait for SSH → mount shares → copy files → provision (first boot only) → run on-boot steps. Re-running on a running VM is a no-op. |
| `dirtbag ssh [-- CMD…]` | Interactive shell, or run `CMD` in the VM (its exit status is propagated). |
| `dirtbag status` | Show the VM's name, state, and IP. Outside a project, lists all Tart VMs. |
| `dirtbag down` | Run on-shutdown steps → `sync` the guest → stop the VM. |
| `dirtbag reload` | Bring the VM down and back up to apply mount/resource changes. |
| `dirtbag provision` | Re-run the provisioners against the running VM. |
| `dirtbag destroy` | Stop and delete the VM, and remove its `.dirtbag/` state (a sibling `-f` sandbox keeps its own). |

Global flags:

- `-v` / `-vv` increase logging (or set `RUST_LOG`).
- `-f` / `--file <PATH>` selects a config file instead of discovering
  `dirtbag.toml`. This lets one directory hold several sandboxes side by side —
  e.g. `dirtbag -f dirtbag.gpu.toml up`. A non-default file gets its own VM
  (its name folds in the file stem) and its own `.dirtbag/<stem>.run.log`, so
  siblings never collide. `dirtbag -f dirtbag.gpu.toml init` scaffolds a new
  file under that name.

## How it works

- **Detached boot.** `tart run` runs in the foreground for the life of the VM,
  so dirtbag spawns `tart run --no-graphics` in its own session (`setsid`), with
  output redirected to `.dirtbag/run.log`, and records the PID. The VM keeps
  running after `dirtbag up` returns.
- **No host state file.** Tart is the source of truth for whether a VM exists
  and is running; the VM name is derived from the project (`config.name`, or a
  hash of the project path). The project-local `.dirtbag/` directory holds only
  the run log.
- **Access is over SSH** (libssh2). The same session handles the boot readiness
  probe, `ssh -- CMD`, the interactive PTY shell, SCP copy-in, and running
  provisioners.
- **Provision once.** `up` runs the copies and provisioners only on a VM's first
  boot, tracked by a marker *inside the guest* (`/var/lib/dirtbag/provisioned`),
  so it belongs to the VM and cannot desync. Later boots re-mount the shares but
  skip provisioning. Use `dirtbag provision` to run the steps again.
- **Prepare on every boot.** `[[on-boot]]` steps run over SSH on every boot that
  dirtbag drives (`up` on a stopped VM, and `reload`), after provisioning. Use
  them to start services or agents the sandbox needs to be usable.
- **Down winds the guest down.** `tart stop` is a hard power-off, so before it
  `dirtbag down` runs the `[[on-shutdown]]` steps over SSH. The last step is
  always an implicit `sync`; without it, writes from the session are lost on the
  next boot. `sync` has no `needs`, so it runs even after a user step failed, and
  the whole run is best-effort — a failure is logged and the VM still stops.
- **Guests** implement a `Guest` trait so per-OS differences (e.g. Linux needing
  a manual virtiofs mount) live in one place.

## Troubleshooting

**`dirtbag up` hangs at "waiting for ssh", or SSH fails with "No route to
host".** On macOS 15+/26, apps need **Local Network** permission to reach the
Tart NAT bridge (`192.168.64.0/24`). Grant it under **System Settings → Privacy
& Security → Local Network** for your terminal app, then retry. A VM getting an
IP is *not* proof it's reachable — this permission gates the actual connection.

**Files show up in a nested subdirectory / mount looks wrong.** Mounts are set at
`tart run` time. If you changed `[[mount]]` while the VM was running, run
`dirtbag reload`.

**`mount source does not exist on host`.** Tart requires the host path to exist
before boot; create it (or fix `source`) and re-run.

## Development

```console
$ cargo test                 # unit + no-VM integration tests
$ cargo test -- --ignored    # full end-to-end test (needs tart + a real VM)
$ cargo clippy --all-targets
```

## License

MIT
