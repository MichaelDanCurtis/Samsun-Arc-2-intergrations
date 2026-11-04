//! Command-line surface. Behaviour lives in the modules the commands call.

use clap::{Args, Parser, Subcommand};

use crate::input::Input;

#[derive(Debug, Parser)]
#[command(
    name = "deskswitch",
    version,
    about = "Follow the keyboard across machines"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Switch the desk to a slot, a machine, or an input.
    Switch(SwitchArgs),
    /// The keyboard just connected here: make sure the monitor follows (rate limited).
    Arrive,
    /// Current input, keyboard holder, and last switch.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// LAN listener, periodic refresh, and the Stream Deck.
    Daemon,
    /// Validate the config and dry-run every resolution.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// `SmartThings` account setup.
    Smartthings {
        #[command(subcommand)]
        action: SmartThingsAction,
    },
    /// LAN relay setup.
    Lan {
        #[command(subcommand)]
        action: LanAction,
    },
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct SwitchArgs {
    /// Easy-Switch slot 1–3. Out-of-range values are rejected by `resolve`
    /// (exit code 3), not here, so an unknown slot gets the same domain
    /// error and exit code as an unknown machine name.
    #[arg(long)]
    pub slot: Option<u8>,
    /// A machine name from the config.
    #[arg(long, value_name = "MACHINE")]
    pub to: Option<String>,
    /// An input name: HDMI1, HDMI2, HDMI3, DP.
    #[arg(long)]
    pub input: Option<Input>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Parse, validate, print resolutions, and check inputs against the Ark.
    Check,
}

#[derive(Debug, Subcommand)]
pub enum SmartThingsAction {
    /// One-time OAuth login; stores tokens in the secret store.
    Login {
        #[arg(long)]
        client_id: String,
        /// Prompted on stdin when omitted.
        #[arg(long)]
        client_secret: Option<String>,
        /// Print the URL instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// List devices that expose an input source, to pick `device_id`.
    Devices,
}

#[derive(Debug, Subcommand)]
pub enum LanAction {
    /// Store the shared secret peers must present. Prompted when omitted.
    SetSecret { secret: Option<String> },
}
