//! Copy files into the guest and run config steps (`[[provision]]`, `[[on-boot]]`) over SSH.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use base64::Engine;
use tracing::{info, warn};

use crate::config::{Project, Step};
use crate::ssh::Ssh;

/// Copy each `[[copy]]` file into the guest (one-shot; not kept in sync).
pub fn run_copies(ssh: &Ssh, project: &Project) -> Result<()> {
    for c in &project.config.copies {
        let src = project.resolve_host_path(&c.source);
        let data = std::fs::read(&src).with_context(|| format!("reading {src}"))?;
        ssh.upload(&data, &c.target, 0o644)
            .with_context(|| format!("copying {src} -> {}", c.target))?;
        info!(target = %c.target, "copied file");
    }
    Ok(())
}

/// Run each `[[provision]]` step in dependency order (see [`run_steps_over_ssh`]).
pub fn run_provisions(ssh: &Ssh, project: &Project) -> Result<()> {
    run_steps_over_ssh(ssh, project, &project.config.provisions, "provision")
}

/// Run each `[[on-boot]]` step in dependency order (see [`run_steps_over_ssh`]).
pub fn run_on_boot(ssh: &Ssh, project: &Project) -> Result<()> {
    run_steps_over_ssh(ssh, project, &project.config.on_boots, "on-boot")
}

/// Run each `[[on-shutdown]]` step in dependency order, ending with the implicit
/// `sync` (see [`run_steps_over_ssh`]).
pub fn run_on_shutdown(ssh: &Ssh, project: &Project) -> Result<()> {
    run_steps_over_ssh(
        ssh,
        project,
        &project.config.on_shutdown_steps(),
        "on-shutdown",
    )
}

/// How one step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ran,
    Failed,
    Skipped,
}

/// Run a list of script steps in order over SSH. A step is skipped when a step
/// it `needs` did not succeed; skips propagate down the chain. Every runnable
/// step is attempted — a failure does not halt the list — but the run reports an
/// error at the end if any step failed. `label` names the section in log lines
/// and the error message.
fn run_steps_over_ssh(ssh: &Ssh, project: &Project, steps: &[Step], label: &str) -> Result<()> {
    let outcomes = resolve_step_outcomes(steps, label, |step| exec_step(ssh, project, step));
    let failed = outcomes.iter().filter(|o| **o == Outcome::Failed).count();
    if failed > 0 {
        bail!("{failed} {label} step(s) failed");
    }
    Ok(())
}

/// Walk the steps in order and decide each one's [`Outcome`], honoring `needs`.
/// `run` executes a runnable step and returns its exit code. Kept free of SSH so
/// the dependency logic is unit-testable.
fn resolve_step_outcomes(
    steps: &[Step],
    label: &str,
    mut run: impl FnMut(&Step) -> Result<i32>,
) -> Vec<Outcome> {
    let mut succeeded: HashMap<&str, bool> = HashMap::new();
    let mut outcomes = Vec::with_capacity(steps.len());
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let unmet: Vec<&str> = step
            .needs
            .iter()
            .filter(|dep| succeeded.get(dep.as_str()).copied() != Some(true))
            .map(String::as_str)
            .collect();

        let outcome = if !unmet.is_empty() {
            warn!(kind = label, step = n, needs = ?unmet, "skipping step; dependency did not succeed");
            Outcome::Skipped
        } else {
            info!(kind = label, step = n, "running step");
            match run(step) {
                Ok(0) => Outcome::Ran,
                Ok(code) => {
                    warn!(kind = label, step = n, exit = code, "step failed");
                    Outcome::Failed
                }
                Err(e) => {
                    warn!(kind = label, step = n, error = %format!("{e:#}"), "step failed");
                    Outcome::Failed
                }
            }
        };

        if let Some(id) = step.id.as_deref() {
            succeeded.insert(id, outcome == Outcome::Ran);
        }
        outcomes.push(outcome);
    }
    outcomes
}

/// Run one step's script over SSH and return its exit code.
fn exec_step(ssh: &Ssh, project: &Project, step: &Step) -> Result<i32> {
    let script = script_text(project, step)?;
    let shell = step.shell.as_deref().unwrap_or("bash");
    let runner = if step.privileged {
        format!("sudo {shell}")
    } else {
        shell.to_string()
    };
    // Send the script as base64 to prevent shell syntax conflicts.
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    let cmd = format!("echo '{b64}' | base64 -d | {runner}");
    ssh.exec_streaming(&cmd)
}

fn script_text(project: &Project, step: &Step) -> Result<String> {
    if let Some(inline) = &step.inline {
        return Ok(inline.clone());
    }
    let path = step
        .path
        .as_ref()
        .expect("config validation guarantees inline or path");
    let full = project.resolve_host_path(path);
    std::fs::read_to_string(&full).with_context(|| format!("reading provision script {full}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: Option<&str>, needs: &[&str]) -> Step {
        Step {
            id: id.map(str::to_string),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            inline: Some("true".to_string()),
            path: None,
            shell: None,
            privileged: false,
        }
    }

    /// Run the steps, failing any step whose id is in `fail`.
    fn run_failing(steps: &[Step], fail: &[&str]) -> Vec<Outcome> {
        resolve_step_outcomes(steps, "test", |s| {
            let failed = s.id.as_deref().is_some_and(|id| fail.contains(&id));
            Ok(if failed { 1 } else { 0 })
        })
    }

    #[test]
    fn runs_all_steps_when_none_fail() {
        let steps = [step(Some("a"), &[]), step(None, &["a"])];
        assert_eq!(run_failing(&steps, &[]), [Outcome::Ran, Outcome::Ran]);
    }

    #[test]
    fn a_failure_does_not_halt_independent_steps() {
        let steps = [step(Some("a"), &[]), step(None, &[])];
        // The second step does not need `a`, so it still runs.
        assert_eq!(run_failing(&steps, &["a"]), [Outcome::Failed, Outcome::Ran]);
    }

    #[test]
    fn skips_step_whose_dependency_failed() {
        let steps = [step(Some("a"), &[]), step(None, &["a"])];
        assert_eq!(
            run_failing(&steps, &["a"]),
            [Outcome::Failed, Outcome::Skipped]
        );
    }

    #[test]
    fn skips_propagate_down_the_chain() {
        let steps = [
            step(Some("a"), &[]),
            step(Some("b"), &["a"]),
            step(None, &["b"]),
        ];
        // `a` fails -> `b` skipped -> the last step skipped because `b` did not
        // succeed.
        assert_eq!(
            run_failing(&steps, &["a"]),
            [Outcome::Failed, Outcome::Skipped, Outcome::Skipped]
        );
    }

    #[test]
    fn step_needing_two_deps_runs_only_when_both_succeed() {
        let steps = [
            step(Some("a"), &[]),
            step(Some("b"), &[]),
            step(None, &["a", "b"]),
        ];
        assert_eq!(
            run_failing(&steps, &["b"]),
            [Outcome::Ran, Outcome::Failed, Outcome::Skipped]
        );
    }
}
