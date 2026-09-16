//! Command-line surface. Behaviour lives in the modules the commands call.

use clap::{Parser, Subcommand};

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
    /// Validate the config and dry-run every resolution.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Parse, validate, and print what each slot/machine/input resolves to.
    Check,
}
