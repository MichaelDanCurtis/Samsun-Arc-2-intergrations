//! `deskswitch` — follow the keyboard: monitor input plus Logitech Easy-Switch.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cli;
mod config;
mod input;
#[allow(dead_code)] // Later tasks use this module
mod monitor;
mod resolve;
#[allow(dead_code)] // Later tasks use this module
mod secrets;
#[allow(dead_code)] // Later tasks use this module
mod state;

use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// stderr always; a daily-rotated `deskswitch.log` in `dir` when given (7 kept).
/// The returned guard flushes the file writer on drop — keep it alive in `main`.
fn init_logging(dir: Option<&Path>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = fmt::layer().with_writer(std::io::stderr).with_target(false);
    let Some(dir) = dir else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr)
            .init();
        return None;
    };
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .max_log_files(7)
        .filename_prefix("deskswitch")
        .filename_suffix("log")
        .build(dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .init();
    Some(guard)
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    let config_dir = config::config_dir();
    let log_dir = config_dir.exists().then_some(config_dir.as_path());
    let _guard = init_logging(log_dir);
    match cli.command {
        cli::Command::Config {
            action: cli::ConfigAction::Check,
        } => {
            let dir = config::config_dir();
            match config::Config::load(&dir) {
                Ok(config) => {
                    println!("config ok: {}", dir.join(config::FILE_NAME).display());
                    for machine in &config.machines {
                        let me = if machine.name == config.this_machine {
                            " (this machine)"
                        } else {
                            ""
                        };
                        println!(
                            "  slot {} → {} → {}{me}",
                            machine.slot, machine.name, machine.input
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("config error: {error}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
