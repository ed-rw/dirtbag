mod cli;
mod commands;
mod config;
mod error;
mod guest;
mod process;
mod provision;
mod ssh;
mod state;
mod tart;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    init_tracing(cli.verbose);

    match commands::dispatch(cli.command) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
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
