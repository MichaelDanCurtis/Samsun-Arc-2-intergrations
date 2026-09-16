//! `deskswitch` — follow the keyboard: monitor input plus Logitech Easy-Switch.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cli;
mod config;
mod daemon;
mod input;
mod lan;
mod monitor;
mod openlogi;
mod resolve;
mod secrets;
mod smartthings;
mod state;
#[cfg(feature = "streamdeck")]
mod streamdeck;
mod switch;

use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser as _;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::config::Config;
use crate::monitor::{Chain, MonitorBackend};
use crate::resolve::Target;
use crate::switch::{ArriveOutcome, ArriveTiming, Deps, Outcome, mode_from_env};

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

/// Everything a command needs, built from the config on disk.
fn build_deps(config: Config, dir: &Path) -> anyhow::Result<Deps> {
    let secrets = secrets::open_store(config.secrets.store, dir);
    let mut backends: Vec<Arc<dyn MonitorBackend>> = Vec::new();
    for name in &config.monitor.backends {
        match name.as_str() {
            "smartthings" => {
                let cfg = config
                    .monitor
                    .smartthings
                    .as_ref()
                    .context("[monitor.smartthings] missing")?;
                let tokens = Arc::new(smartthings::auth::TokenManager::new(cfg, secrets.clone()));
                backends.push(Arc::new(smartthings::SmartThings::new(cfg, tokens)?));
            }
            other => anyhow::bail!("backend {other:?} is not implemented"),
        }
    }
    let relay: Arc<dyn switch::Relay> = match (&config.lan, secrets.get(secrets::LAN_SECRET)?) {
        (Some(_), Some(secret)) => Arc::new(lan::HttpRelay::new(secret)),
        _ => Arc::new(switch::NoRelay),
    };
    Ok(Deps {
        agent: Arc::new(openlogi::OpenLogi::new(
            config.openlogi.binary.clone().into(),
        )),
        state: state::StateStore::in_dir(dir),
        chain: Chain::new(backends),
        relay,
        config,
    })
}

fn prompt(label: &str) -> anyhow::Result<String> {
    print!("{label}: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

// One arm per subcommand; splitting the dispatch table would scatter the
// command surface across helper functions for no real gain in clarity.
#[allow(clippy::too_many_lines)]
async fn run(cli: cli::Cli, dir: &Path) -> anyhow::Result<u8> {
    match cli.command {
        cli::Command::Config {
            action: cli::ConfigAction::Check,
        } => {
            let config = Config::load(dir)?;
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
            if let Some(st) = &config.monitor.smartthings {
                let secrets = secrets::open_store(config.secrets.store, dir);
                let tokens = Arc::new(smartthings::auth::TokenManager::new(st, secrets));
                let supported = smartthings::SmartThings::new(st, tokens)?
                    .supported_inputs()
                    .await?;
                let unsupported: Vec<String> = config
                    .machines
                    .iter()
                    .filter(|m| !supported.iter().any(|s| s == m.input.as_str()))
                    .map(|m| format!("{} ({})", m.input, m.name))
                    .collect();
                if !unsupported.is_empty() {
                    eprintln!(
                        "the Ark does not report these configured inputs: {}; it reports {}",
                        unsupported.join(", "),
                        supported.join(", ")
                    );
                    return Ok(1);
                }
                println!("  SmartThings reports inputs: {}", supported.join(", "));
            }
            Ok(0)
        }
        cli::Command::Switch(args) => {
            let deps = build_deps(Config::load(dir)?, dir)?;
            let target = match (args.slot, args.to, args.input) {
                (Some(slot), _, _) => Target::Slot(slot),
                (_, Some(name), _) => Target::Machine(name),
                (_, _, Some(input)) => Target::Input(input),
                _ => unreachable!("clap requires exactly one"),
            };
            let mode = mode_from_env();
            match switch::run_switch(&deps, &target, mode).await {
                Ok(Outcome::Delegated) => {
                    println!("delegated to the OpenLogi agent");
                    Ok(0)
                }
                Ok(Outcome::Monitor { ensure, relayed }) => {
                    let input = resolve::resolve(&deps.config, &target)
                        .map(|r| r.input.to_string())
                        .unwrap_or_default();
                    match ensure {
                        monitor::Ensure::AlreadyThere => println!("monitor already on {input}"),
                        monitor::Ensure::Switched { backend } => {
                            println!("monitor → {input} via {backend}");
                        }
                    }
                    for outcome in relayed {
                        match outcome.result {
                            Ok(result) => println!("peer {}: {result}", outcome.peer),
                            Err(error) => println!("peer {}: failed ({error})", outcome.peer),
                        }
                    }
                    Ok(0)
                }
                Err(error) => {
                    eprintln!("{error}");
                    Ok(error.exit_code())
                }
            }
        }
        cli::Command::Arrive => {
            let deps = build_deps(Config::load(dir)?, dir)?;
            match switch::run_arrive(&deps, ArriveTiming::default()).await {
                ArriveOutcome::RateLimited => {
                    println!("arrive: rate limited");
                    Ok(0)
                }
                ArriveOutcome::AlreadyMine => {
                    println!("arrive: monitor already here");
                    Ok(0)
                }
                ArriveOutcome::Switched => {
                    println!("arrive: monitor switched here");
                    Ok(0)
                }
                ArriveOutcome::Failed(error) => {
                    eprintln!("arrive: {error}");
                    Ok(2)
                }
            }
        }
        cli::Command::Status { json } => {
            let deps = build_deps(Config::load(dir)?, dir)?;
            let report = switch::status(&deps).await;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("machine: {}", report.machine);
                println!(
                    "input: {}",
                    report
                        .input
                        .map_or("unknown".to_string(), |i| i.to_string())
                );
                println!(
                    "keyboard here: {}{}",
                    report.keyboard_here,
                    report
                        .current_slot
                        .map_or(String::new(), |s| format!(" (slot {s})"))
                );
            }
            Ok(0)
        }
        cli::Command::Daemon => {
            let config = Config::load(dir)?;
            let secrets = secrets::open_store(config.secrets.store, dir);
            let lan_secret = secrets.get(secrets::LAN_SECRET)?;
            let deps = Arc::new(build_deps(config, dir)?);
            daemon::run(deps, lan_secret).await?;
            Ok(0)
        }
        cli::Command::Smartthings { action } => {
            let config = Config::load(dir)?;
            let st = config
                .monitor
                .smartthings
                .as_ref()
                .context("[monitor.smartthings] missing")?;
            let secrets = secrets::open_store(config.secrets.store, dir);
            match action {
                cli::SmartThingsAction::Login {
                    client_id,
                    client_secret,
                    no_browser,
                } => {
                    anyhow::ensure!(
                        client_id == st.client_id,
                        "--client-id does not match [monitor.smartthings].client_id"
                    );
                    let secret = match client_secret {
                        Some(s) => s,
                        None => prompt("client secret")?,
                    };
                    smartthings::auth::login(st, secrets.as_ref(), &secret, !no_browser).await?;
                    println!("SmartThings login stored");
                    Ok(0)
                }
                cli::SmartThingsAction::Devices => {
                    let tokens = Arc::new(smartthings::auth::TokenManager::new(st, secrets));
                    for (id, label) in smartthings::SmartThings::new(st, tokens)?
                        .list_devices()
                        .await?
                    {
                        println!("{id}  {label}");
                    }
                    Ok(0)
                }
            }
        }
        cli::Command::Lan {
            action: cli::LanAction::SetSecret { secret },
        } => {
            let config = Config::load(dir)?;
            let secrets = secrets::open_store(config.secrets.store, dir);
            let secret = match secret {
                Some(s) => s,
                None => prompt("shared secret")?,
            };
            secrets.set(secrets::LAN_SECRET, &secret)?;
            println!("LAN secret stored");
            Ok(0)
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    let dir = config::config_dir();
    let _guard = init_logging(dir.exists().then_some(dir.as_path()));
    match run(cli, &dir).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(1)
        }
    }
}
