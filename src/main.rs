mod cli;
mod commands;
mod config;
mod error;
mod forward;
mod guest;
mod process;
mod provision;
mod ssh;
mod tart;
mod tcp;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    // The supervisor relays TCP, so a peer that closes must not stop it.
    if !matches!(cli.command, cli::Command::Supervise { .. }) {
        restore_sigpipe();
    }
    init_tracing(cli.verbose);

    match commands::dispatch(cli.command, cli.file.as_deref(), cli.instance.as_deref()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Rust ignores SIGPIPE, so a write to a closed pipe is an error, and
/// `println!` panics on it. Restore the default action. Then dirtbag stops
/// quietly when the reader closes the pipe, as in `dirtbag status | head`.
fn restore_sigpipe() {
    use nix::sys::signal::{SigHandler, Signal, signal};
    // SAFETY: SIG_DFL installs no handler, so no code runs in signal context.
    // No other thread exists yet, so nothing races the change.
    unsafe {
        let _ = signal(Signal::SIGPIPE, SigHandler::SigDfl);
    }
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "dirtbag=info",
        1 => "dirtbag=debug",
        _ => "dirtbag=trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();
}
