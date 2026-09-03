# deskswitch Desk Service Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A per-machine Rust binary, `deskswitch`, that maps Easy-Switch slots, machines, and Samsung Odyssey Ark inputs, flips the Ark through pluggable backends (SmartThings first), delegates peripheral moves to OpenLogi, relays switches between machines over an authenticated LAN endpoint, and drives the Stream Deck Plus.

**Architecture:** One binary crate with focused modules. `config` + `resolve` turn TOML into a target input; `monitor` defines a `MonitorBackend` trait and a `Chain` that adds timeouts, ordering, retry, and read-before-write; `smartthings` implements the first backend with an OAuth token manager over the `secrets` store; `openlogi` shells out to `openlogi host list --json` / `host switch`; `switch` holds the two-mode orchestration from the spec (hook mode = monitor only, user mode = delegate to the agent, else monitor + LAN relay); `lan` is an axum server and reqwest client; `daemon` runs the LAN listener, the 30-second refresh, and the feature-gated `streamdeck` driver.

**Tech Stack:** Rust 2024, tokio, clap 4, serde/toml, reqwest (rustls), axum 0.8, keyring 3, tracing + tracing-appender, chrono, thiserror/anyhow, async-trait; dev: wiremock, assert_cmd, predicates, tempfile; feature `streamdeck`: elgato-streamdeck 0.13, image 0.25, imageproc + ab_glyph for button labels.

## Global Constraints

Copied from the spec (`docs/superpowers/specs/2026-09-02-ark-desk-switch-design.md`):

- Config path: `$DESKSWITCH_CONFIG_DIR` if set (tests), else `~/.config/deskswitch/config.toml` on macOS/Linux and `%APPDATA%\deskswitch\config.toml` on Windows. Secrets never live in `config.toml`.
- Canonical inputs: `HDMI1`, `HDMI2`, `HDMI3`, `DP`. Slots are 1-based, 1–3. Machine names, slots, and inputs are unique; `this_machine` must name an entry; every `[streamdeck].buttons` entry names a machine; every listed backend has its table.
- Hook mode is exactly "the `OPENLOGI_TRIGGER` environment variable is present": monitor only, never calls `openlogi host switch`.
- User mode: `openlogi host list --json` (3 s timeout); keyboard online here and current slot ≠ target → `openlogi host switch N` (10 s) and exit with its result; keyboard here and already on the target slot → monitor only; `openlogi` missing or agent down → warn and continue as if the keyboard were elsewhere; otherwise monitor directly, then POST `/switch` to every configured peer (2 s each, concurrently, failures logged).
- Backend chain: `current` = first backend's `Some`; `switch` tries backends in configured order with a 5 s timeout each, one more full pass after 3 s, then error; always read first and skip when the input already matches.
- `arrive`: stop if the last arrive was under 30 s ago; else record the time, sleep 2.5 s, read, switch only if the input is not already mine.
- LAN: HTTP/1.1 JSON on `[lan].listen`; every request needs `Authorization: Bearer <shared secret>` or gets 401; `POST /switch` body `{"slot":2}` or `{"machine":"…"}`; receiver answers `{"result":"delegated"}` when it holds the keyboard (after running `openlogi host switch N`), else `{"result":"ignored"}`; never relays onward; `GET /status` mirrors `deskswitch status --json`.
- Exit codes for `switch`: 0 done, 1 usage/config error, 2 every backend failed, 3 target not resolvable. State file `state.json` written atomically (temp + rename) with `last_target_slot`, `last_switch_at`, `last_known_input`, `last_arrive_at` (RFC 3339).
- Logging: `tracing` to stderr plus a daily-rotated `deskswitch.log` in the config dir, 7 files kept; `RUST_LOG` controls the level.
- SmartThings: OAuth authorization-code flow; authorize URL `https://api.smartthings.com/oauth/authorize`, token URL `https://auth-global.api.smartthings.com/oauth/token` (both overridable in config, confirmed against the developer docs at implementation), client credentials via HTTP basic auth, refresh on expiry or 401 with rotated tokens stored back; reads and writes use capability `samsungvd.mediaInputSource`; `config check` validates configured inputs against `supportedInputSourcesMap`.
- The Elgato Stream Deck app must not be running when the daemon drives the deck.
- Libraries return `thiserror` types; the binary uses `anyhow`. Every module stays well under 500 lines. Conventional commits (`feat(config): …`), no AI attribution.

---

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | crate, deps, `streamdeck` feature |
| `src/main.rs` | logging init, clap parse, dispatch, exit codes |
| `src/cli.rs` | clap `Cli`/`Command`/args types only |
| `src/input.rs` | `Input` enum, parse/display |
| `src/config.rs` | config types, load, `config_dir()`, validation |
| `src/resolve.rs` | `Target` → `Resolved { input, machine }` |
| `src/state.rs` | `State` + atomic `StateStore` |
| `src/secrets.rs` | `SecretStore` trait, `FileStore`, `KeyringStore` |
| `src/monitor.rs` | `MonitorBackend`, `BackendError`, `Chain`, `ChainError`, `Ensure` |
| `src/smartthings.rs` | `SmartThings` backend: status read, command write, `supported_inputs`, `list_devices` |
| `src/smartthings/auth.rs` | `TokenSet`, `TokenManager` (refresh), `login` flow |
| `src/openlogi.rs` | `OpenLogi` shim over the CLI: `keyboard_here()`, `switch(slot)` |
| `src/switch.rs` | `run_switch(mode, target)`, `run_arrive()` |
| `src/lan.rs` | axum server (`/switch`, `/status`) + `relay_to_peers` |
| `src/daemon.rs` | LAN listener + 30 s refresh + optional deck |
| `src/streamdeck.rs` | feature-gated deck thread and button rendering |
| `tests/cli.rs` | end-to-end with a fake `openlogi` script and a wiremock SmartThings |
| `tests/fixtures/openlogi.sh` | the fake `openlogi` |
| `assets/fonts/DejaVuSans.ttf` (+ `LICENSE`) | button label font (feature `streamdeck`) |
| `deploy/com.deskswitch.daemon.plist`, `deploy/windows-task.md` | install pieces |

---

### Task 1: Scaffold the crate, `Input`, and logging

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/cli.rs`, `src/input.rs`
- Modify: `.gitignore` (already has `target/`)

**Interfaces:**
- Produces: `pub enum Input { Hdmi1, Hdmi2, Hdmi3, Dp }` with `FromStr` (accepts `HDMI1`… case-insensitively), `Display` (`HDMI1`), `Input::as_str()`, `Input::ALL`; `pub fn init_logging(dir: Option<&Path>) -> Option<tracing_appender::non_blocking::WorkerGuard>` in `main.rs`.

- [ ] **Step 1: Create the crate**

```bash
cd "/Users/michaelcurtis/Samsun Arc 2 intergrations"
cargo init --name deskswitch --vcs none
```

Replace `Cargo.toml` with:

```toml
[package]
name = "deskswitch"
version = "0.1.0"
edition = "2024"
description = "Follow the keyboard: switch a Samsung Ark's input and Logitech Easy-Switch peripherals together"
license = "MIT"

[features]
default = []
streamdeck = ["dep:elgato-streamdeck", "dep:image", "dep:imageproc", "dep:ab_glyph"]

[dependencies]
anyhow = "1"
async-trait = "0.1"
axum = { version = "0.8", features = ["json"] }
chrono = { version = "0.4", features = ["serde"] }
clap = { version = "4", features = ["derive"] }
keyring = { version = "3", features = ["apple-native", "windows-native", "sync-secret-service"] }
open = "5"
reqwest = { version = "0.13", default-features = false, features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
toml = "1"
tracing = "0.1"
tracing-appender = "0.2"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
url = "2"
elgato-streamdeck = { version = "0.13", optional = true }
image = { version = "0.25", optional = true }
imageproc = { version = "0.25", optional = true }
ab_glyph = { version = "0.2", optional = true }

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"
wiremock = "0.6"

[lints.clippy]
pedantic = "warn"
unwrap_used = "warn"
expect_used = "warn"
```

If `cargo build` reports that reqwest 0.13 renamed the TLS feature, run `cargo add reqwest --no-default-features --features json,rustls` and keep whichever feature name it accepts; do the same for `imageproc` if 0.25 is not the current line (`cargo add imageproc --optional`).

- [ ] **Step 2: Write the failing `Input` tests**

`src/input.rs`:

```rust
//! The Ark's canonical input names.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// One physical input on the One Connect box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Input {
    Hdmi1,
    Hdmi2,
    Hdmi3,
    Dp,
}

#[cfg(test)]
mod tests {
    use super::Input;

    #[test]
    fn parses_case_insensitively_and_prints_canonically() {
        assert_eq!("hdmi2".parse::<Input>().unwrap(), Input::Hdmi2);
        assert_eq!("DP".parse::<Input>().unwrap(), Input::Dp);
        assert_eq!(Input::Hdmi3.to_string(), "HDMI3");
        assert!("HDMI4".parse::<Input>().is_err());
    }

    #[test]
    fn toml_round_trip_uses_the_canonical_string() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Row { input: Input }
        let row: Row = toml::from_str(r#"input = "hdmi1""#).unwrap();
        assert_eq!(row.input, Input::Hdmi1);
        assert_eq!(toml::to_string(&row).unwrap().trim(), r#"input = "HDMI1""#);
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test input`
Expected: compile errors (`FromStr`, `Display`, `TryFrom<String>`, `From<Input> for String` missing).

- [ ] **Step 4: Implement**

Insert between the enum and the tests:

```rust
impl Input {
    /// Every input, in port order.
    pub const ALL: [Input; 4] = [Input::Hdmi1, Input::Hdmi2, Input::Hdmi3, Input::Dp];

    /// The canonical name, which is also the SmartThings input id.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Input::Hdmi1 => "HDMI1",
            Input::Hdmi2 => "HDMI2",
            Input::Hdmi3 => "HDMI3",
            Input::Dp => "DP",
        }
    }
}

/// An input name outside the canonical set.
#[derive(Debug, thiserror::Error)]
#[error("unknown input {0:?}; expected one of HDMI1, HDMI2, HDMI3, DP")]
pub struct UnknownInput(pub String);

impl FromStr for Input {
    type Err = UnknownInput;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Input::ALL
            .into_iter()
            .find(|input| input.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| UnknownInput(s.to_string()))
    }
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for Input {
    type Error = UnknownInput;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Input> for String {
    fn from(input: Input) -> Self {
        input.as_str().to_string()
    }
}
```

`src/cli.rs` (grows in later tasks):

```rust
//! Command-line surface. Behaviour lives in the modules the commands call.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "deskswitch", version, about = "Follow the keyboard across machines")]
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
```

`src/main.rs`:

```rust
//! `deskswitch` — follow the keyboard: monitor input plus Logitech Easy-Switch.

mod cli;
mod input;

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
        tracing_subscriber::registry().with(filter).with(stderr).init();
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
    let _guard = init_logging(None);
    match cli.command {
        cli::Command::Config { action: cli::ConfigAction::Check } => {
            eprintln!("config check: not implemented yet");
            ExitCode::from(1)
        }
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test input && cargo clippy --all-targets -- -D warnings`
Expected: 2 passed, clippy clean (fix any pedantic nits it raises rather than suppressing them).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src
git commit -m "feat: scaffold deskswitch with Input type and logging"
```

---

### Task 2: Config types, loading, validation, and `config check`

**Files:**
- Create: `src/config.rs`, `src/resolve.rs`
- Modify: `src/main.rs`, `src/cli.rs`

**Interfaces:**
- Produces:
  - `pub fn config_dir() -> PathBuf` (env override → platform default).
  - `pub struct Config { pub schema_version: u32, pub this_machine: String, pub machines: Vec<Machine>, pub monitor: MonitorConfig, pub lan: Option<LanConfig>, pub streamdeck: StreamDeckConfig, pub openlogi: OpenLogiConfig, pub secrets: SecretsConfig }`
  - `pub struct Machine { pub name: String, pub slot: u8, pub input: Input, pub address: Option<String> }`
  - `pub struct MonitorConfig { pub backends: Vec<String>, pub smartthings: Option<SmartThingsConfig> }`
  - `pub struct SmartThingsConfig { pub device_id: String, pub client_id: String, pub base_url: String /* default https://api.smartthings.com/v1 */, pub token_url: String, pub authorize_url: String, pub redirect_uri: String /* default http://localhost:8765/callback */ }`
  - `pub struct LanConfig { pub listen: String }`, `pub struct StreamDeckConfig { pub enabled: bool, pub buttons: Vec<String> }`, `pub struct OpenLogiConfig { pub binary: String /* default "openlogi" */ }`, `pub struct SecretsConfig { pub store: SecretStoreKind }`, `pub enum SecretStoreKind { Keyring, File }` (default `Keyring`).
  - `Config::load(dir: &Path) -> Result<Config, ConfigError>` (parse + validate), `Config::validate(&self) -> Result<(), ConfigError>`, `Config::this(&self) -> &Machine`, `Config::machine_named(&self, name) -> Option<&Machine>`, `Config::machine_for_slot(&self, slot) -> Option<&Machine>`, `Config::peers(&self) -> Vec<&Machine>` (others with an address).
  - `pub enum Target { Slot(u8), Machine(String), Input(Input) }`, `pub struct Resolved { pub input: Input, pub machine: Option<Machine> }`, `pub fn resolve(config: &Config, target: &Target) -> Result<Resolved, ResolveError>`.

- [ ] **Step 1: Write the failing tests**

`src/config.rs` test module (put the `SAMPLE` constant at module level so `tests/cli.rs` can reuse the same text later by copying it):

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SAMPLE: &str = r#"
schema_version = 1
this_machine = "mac"

[[machines]]
name = "mac"
slot = 1
input = "HDMI1"
address = "mac.local:7391"

[[machines]]
name = "avapc"
slot = 3
input = "HDMI2"
address = "avapc-x825243p7.local:7391"

[[machines]]
name = "oldwaterboy"
slot = 2
input = "HDMI3"

[monitor]
backends = ["smartthings"]

[monitor.smartthings]
device_id = "dev-1"
client_id = "client-1"

[lan]
listen = "0.0.0.0:7391"

[streamdeck]
enabled = true
buttons = ["mac", "avapc", "oldwaterboy"]
"#;

    fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn sample_parses_with_defaults() {
        let config = parse(SAMPLE).unwrap();
        assert_eq!(config.this().input, Input::Hdmi1);
        assert_eq!(config.machine_for_slot(2).unwrap().name, "oldwaterboy");
        assert_eq!(config.openlogi.binary, "openlogi");
        assert_eq!(config.monitor.smartthings.as_ref().unwrap().base_url, "https://api.smartthings.com/v1");
        assert_eq!(config.peers().iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["avapc"]);
    }

    #[test]
    fn duplicate_slots_and_inputs_are_rejected() {
        let dup_slot = SAMPLE.replace("slot = 3", "slot = 1");
        assert!(matches!(parse(&dup_slot), Err(ConfigError::DuplicateSlot(1))));
        let dup_input = SAMPLE.replace(r#"input = "HDMI2""#, r#"input = "HDMI1""#);
        assert!(matches!(parse(&dup_input), Err(ConfigError::DuplicateInput(Input::Hdmi1))));
    }

    #[test]
    fn this_machine_and_buttons_must_name_machines() {
        let bad_this = SAMPLE.replace(r#"this_machine = "mac""#, r#"this_machine = "laptop""#);
        assert!(matches!(parse(&bad_this), Err(ConfigError::UnknownMachine(name)) if name == "laptop"));
        let bad_button = SAMPLE.replace(r#""oldwaterboy"]"#, r#""ghost"]"#);
        assert!(matches!(parse(&bad_button), Err(ConfigError::UnknownMachine(name)) if name == "ghost"));
    }

    #[test]
    fn listed_backends_need_their_table_and_a_known_name() {
        let missing = SAMPLE.replace("[monitor.smartthings]\ndevice_id = \"dev-1\"\nclient_id = \"client-1\"\n", "");
        assert!(matches!(parse(&missing), Err(ConfigError::BackendTableMissing(name)) if name == "smartthings"));
        let unknown = SAMPLE.replace(r#"backends = ["smartthings"]"#, r#"backends = ["hdmi-cec"]"#);
        assert!(matches!(parse(&unknown), Err(ConfigError::UnknownBackend(name)) if name == "hdmi-cec"));
        let none = SAMPLE.replace(r#"backends = ["smartthings"]"#, "backends = []");
        assert!(matches!(parse(&none), Err(ConfigError::NoBackends)));
    }

    #[test]
    fn slot_range_is_enforced() {
        let bad = SAMPLE.replace("slot = 3", "slot = 4");
        assert!(matches!(parse(&bad), Err(ConfigError::SlotOutOfRange(4))));
    }

    #[test]
    fn config_dir_honours_the_env_override() {
        temp_env::with_var("DESKSWITCH_CONFIG_DIR", Some("/tmp/dsw-test"), || {
            assert_eq!(config_dir(), std::path::PathBuf::from("/tmp/dsw-test"));
        });
    }
}
```

Add `temp-env = "0.3"` to `[dev-dependencies]` (it serialises env mutation across tests).

`src/resolve.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn config() -> Config {
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn resolves_by_slot_machine_and_input() {
        let config = config();
        let by_slot = resolve(&config, &Target::Slot(2)).unwrap();
        assert_eq!(by_slot.input, Input::Hdmi3);
        assert_eq!(by_slot.machine.as_ref().unwrap().name, "oldwaterboy");
        let by_name = resolve(&config, &Target::Machine("avapc".into())).unwrap();
        assert_eq!(by_name.input, Input::Hdmi2);
        let by_input = resolve(&config, &Target::Input(Input::Dp)).unwrap();
        assert_eq!(by_input.input, Input::Dp);
        assert!(by_input.machine.is_none(), "DP is not a configured machine");
    }

    #[test]
    fn unknown_targets_fail_with_the_target_in_the_error() {
        let config = config();
        assert!(matches!(resolve(&config, &Target::Slot(3)).map(|_| ()), Ok(())));
        assert!(matches!(resolve(&config, &Target::Machine("ghost".into())), Err(ResolveError::UnknownMachine(name)) if name == "ghost"));
        assert!(matches!(resolve(&config, &Target::Slot(9)), Err(ResolveError::UnknownSlot(9))));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test config resolve`
Expected: compile errors for every missing type.

- [ ] **Step 3: Implement `config.rs`**

```rust
//! `config.toml`: machines, backends, peers, and validation. No I/O beyond
//! reading the file; secrets live in [`crate::secrets`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::input::Input;

/// The config file name inside [`config_dir`].
pub const FILE_NAME: &str = "config.toml";

/// `$DESKSWITCH_CONFIG_DIR`, else `%APPDATA%\deskswitch` on Windows, else
/// `$XDG_CONFIG_HOME/deskswitch` or `~/.config/deskswitch`.
#[must_use]
pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DESKSWITCH_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("deskswitch");
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("deskswitch");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("deskswitch")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub this_machine: String,
    pub machines: Vec<Machine>,
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub lan: Option<LanConfig>,
    #[serde(default)]
    pub streamdeck: StreamDeckConfig,
    #[serde(default)]
    pub openlogi: OpenLogiConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub name: String,
    /// Easy-Switch slot, 1–3.
    pub slot: u8,
    pub input: Input,
    /// `host:port` of this machine's `deskswitch daemon`, for relayed switches.
    #[serde(default)]
    pub address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    /// Backend names in priority order. Known: `smartthings`.
    pub backends: Vec<String>,
    #[serde(default)]
    pub smartthings: Option<SmartThingsConfig>,
}

/// Backends this build implements, in the names config uses.
pub const KNOWN_BACKENDS: [&str; 1] = ["smartthings"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmartThingsConfig {
    pub device_id: String,
    pub client_id: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_token_url")]
    pub token_url: String,
    #[serde(default = "default_authorize_url")]
    pub authorize_url: String,
    #[serde(default = "default_redirect_uri")]
    pub redirect_uri: String,
}

fn default_base_url() -> String { "https://api.smartthings.com/v1".into() }
fn default_token_url() -> String { "https://auth-global.api.smartthings.com/oauth/token".into() }
fn default_authorize_url() -> String { "https://api.smartthings.com/oauth/authorize".into() }
fn default_redirect_uri() -> String { "http://localhost:8765/callback".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanConfig {
    pub listen: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamDeckConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub buttons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenLogiConfig {
    /// The `openlogi` executable; absolute in production, a fixture in tests.
    #[serde(default = "default_openlogi_binary")]
    pub binary: String,
}

fn default_openlogi_binary() -> String { "openlogi".into() }

impl Default for OpenLogiConfig {
    fn default() -> Self { Self { binary: default_openlogi_binary() } }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    #[serde(default)]
    pub store: SecretStoreKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretStoreKind {
    #[default]
    Keyring,
    /// `secrets.json` next to the config. Plain text: tests and throwaway
    /// machines only.
    File,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {0}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("config.toml is not valid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("schema_version {0} is not supported (expected 1)")]
    Schema(u32),
    #[error("no machines configured")]
    NoMachines,
    #[error("machine name {0:?} is used twice")]
    DuplicateMachine(String),
    #[error("slot {0} is used by two machines")]
    DuplicateSlot(u8),
    #[error("slot {0} is outside 1–3")]
    SlotOutOfRange(u8),
    #[error("input {0} is used by two machines")]
    DuplicateInput(Input),
    #[error("{0:?} is not a configured machine")]
    UnknownMachine(String),
    #[error("[monitor].backends is empty")]
    NoBackends,
    #[error("unknown backend {0:?}; known: smartthings")]
    UnknownBackend(String),
    #[error("backend {0:?} is listed but [monitor.{0}] is missing")]
    BackendTableMissing(String),
}

impl Config {
    /// Read and validate `config.toml` in `dir`.
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join(FILE_NAME);
        let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Read(path, e))?;
        let config: Config = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    /// Every invariant the rest of the program relies on.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != 1 {
            return Err(ConfigError::Schema(self.schema_version));
        }
        if self.machines.is_empty() {
            return Err(ConfigError::NoMachines);
        }
        for (index, machine) in self.machines.iter().enumerate() {
            if !(1..=3).contains(&machine.slot) {
                return Err(ConfigError::SlotOutOfRange(machine.slot));
            }
            for other in &self.machines[..index] {
                if other.name == machine.name {
                    return Err(ConfigError::DuplicateMachine(machine.name.clone()));
                }
                if other.slot == machine.slot {
                    return Err(ConfigError::DuplicateSlot(machine.slot));
                }
                if other.input == machine.input {
                    return Err(ConfigError::DuplicateInput(machine.input));
                }
            }
        }
        if self.machine_named(&self.this_machine).is_none() {
            return Err(ConfigError::UnknownMachine(self.this_machine.clone()));
        }
        for button in &self.streamdeck.buttons {
            if self.machine_named(button).is_none() {
                return Err(ConfigError::UnknownMachine(button.clone()));
            }
        }
        if self.monitor.backends.is_empty() {
            return Err(ConfigError::NoBackends);
        }
        for backend in &self.monitor.backends {
            if !KNOWN_BACKENDS.contains(&backend.as_str()) {
                return Err(ConfigError::UnknownBackend(backend.clone()));
            }
            if backend == "smartthings" && self.monitor.smartthings.is_none() {
                return Err(ConfigError::BackendTableMissing(backend.clone()));
            }
        }
        Ok(())
    }

    /// The machine this process runs on. Valid after [`Self::validate`].
    #[must_use]
    pub fn this(&self) -> &Machine {
        self.machine_named(&self.this_machine)
            .unwrap_or(&self.machines[0])
    }

    #[must_use]
    pub fn machine_named(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|machine| machine.name == name)
    }

    #[must_use]
    pub fn machine_for_slot(&self, slot: u8) -> Option<&Machine> {
        self.machines.iter().find(|machine| machine.slot == slot)
    }

    /// Other machines that run a daemon we can relay to.
    #[must_use]
    pub fn peers(&self) -> Vec<&Machine> {
        self.machines
            .iter()
            .filter(|machine| machine.name != self.this_machine && machine.address.is_some())
            .collect()
    }
}
```

`src/resolve.rs`:

```rust
//! Turn a slot, machine name, or input into the input to show.

use crate::config::{Config, Machine};
use crate::input::Input;

/// What the caller asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Slot(u8),
    Machine(String),
    Input(Input),
}

/// The input to show, and the machine it belongs to when one is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub input: Input,
    pub machine: Option<Machine>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no machine is configured on slot {0}")]
    UnknownSlot(u8),
    #[error("{0:?} is not a configured machine")]
    UnknownMachine(String),
}

pub fn resolve(config: &Config, target: &Target) -> Result<Resolved, ResolveError> {
    let machine = match target {
        Target::Slot(slot) => config
            .machine_for_slot(*slot)
            .ok_or(ResolveError::UnknownSlot(*slot))?,
        Target::Machine(name) => config
            .machine_named(name)
            .ok_or_else(|| ResolveError::UnknownMachine(name.clone()))?,
        Target::Input(input) => {
            return Ok(Resolved {
                input: *input,
                machine: config.machines.iter().find(|m| m.input == *input).cloned(),
            });
        }
    };
    Ok(Resolved {
        input: machine.input,
        machine: Some(machine.clone()),
    })
}
```

Wire `config check` in `main.rs`: declare `mod config; mod resolve;`, initialise logging with `Some(&config::config_dir())` when the dir exists, and implement the arm:

```rust
        cli::Command::Config { action: cli::ConfigAction::Check } => {
            let dir = config::config_dir();
            match config::Config::load(&dir) {
                Ok(config) => {
                    println!("config ok: {}", dir.join(config::FILE_NAME).display());
                    for machine in &config.machines {
                        let me = if machine.name == config.this_machine { " (this machine)" } else { "" };
                        println!("  slot {} → {} → {}{me}", machine.slot, machine.name, machine.input);
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("config error: {error}");
                    ExitCode::from(1)
                }
            }
        }
```

(The SmartThings `supportedInputSourcesMap` check joins this command in Task 7.)

- [ ] **Step 4: Run the tests**

Run: `cargo test config resolve && cargo clippy --all-targets -- -D warnings`
Expected: 8 passed, clean.

- [ ] **Step 5: Try it for real**

```bash
mkdir -p /tmp/dsw && sed -n '/^schema_version/,/^buttons/p' src/config.rs | grep -v '^"#' > /tmp/dsw/config.toml
DESKSWITCH_CONFIG_DIR=/tmp/dsw cargo run -- config check
```
Expected: `config ok` and three resolution lines.

- [ ] **Step 6: Commit**

```bash
git add src Cargo.toml Cargo.lock
git commit -m "feat(config): load, validate, and resolve the desk configuration"
```

---

### Task 3: Atomic state file

**Files:**
- Create: `src/state.rs`

**Interfaces:**
- Produces: `pub struct State { pub last_target_slot: Option<u8>, pub last_switch_at: Option<DateTime<Utc>>, pub last_known_input: Option<Input>, pub last_arrive_at: Option<DateTime<Utc>> }` (serde, all default); `pub struct StateStore { path: PathBuf }` with `StateStore::in_dir(dir) -> Self`, `load(&self) -> State` (missing or corrupt → default, corrupt logged), `save(&self, &State) -> std::io::Result<()>` (temp + rename), `update(&self, f: impl FnOnce(&mut State)) -> std::io::Result<State>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_corrupt_files_read_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::in_dir(dir.path());
        assert_eq!(store.load(), State::default());
        std::fs::write(dir.path().join("state.json"), "{not json").unwrap();
        assert_eq!(store.load(), State::default());
    }

    #[test]
    fn update_persists_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::in_dir(dir.path());
        let saved = store
            .update(|state| {
                state.last_target_slot = Some(2);
                state.last_known_input = Some(Input::Hdmi3);
            })
            .unwrap();
        assert_eq!(saved.last_target_slot, Some(2));
        assert_eq!(store.load().last_known_input, Some(Input::Hdmi3));
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["state.json"]);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test state`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! `state.json`: what this machine last did and last saw, written atomically.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::input::Input;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub last_target_slot: Option<u8>,
    #[serde(default)]
    pub last_switch_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_known_input: Option<Input>,
    #[serde(default)]
    pub last_arrive_at: Option<DateTime<Utc>>,
}

pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join("state.json") }
    }

    /// The saved state, or the default when the file is missing or unreadable.
    #[must_use]
    pub fn load(&self) -> State {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                warn!(path = %self.path.display(), %error, "state.json unreadable; starting fresh");
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    /// Write `state` via a sibling temp file and rename, so a crash mid-write
    /// never leaves a torn file.
    pub fn save(&self, state: &State) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(state)?)?;
        std::fs::rename(&temp, &self.path)
    }

    /// Load, mutate, save; returns the saved state.
    pub fn update(&self, f: impl FnOnce(&mut State)) -> std::io::Result<State> {
        let mut state = self.load();
        f(&mut state);
        self.save(&state)?;
        Ok(state)
    }
}
```

Add `mod state;` to `main.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test state && cargo clippy --all-targets -- -D warnings`
Expected: 2 passed, clean.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/main.rs
git commit -m "feat(state): persist last switch and known input atomically"
```

---

### Task 4: Secret store (file and keyring)

**Files:**
- Create: `src/secrets.rs`

**Interfaces:**
- Produces: `pub trait SecretStore: Send + Sync { fn get(&self, key: &str) -> Result<Option<String>, SecretError>; fn set(&self, key: &str, value: &str) -> Result<(), SecretError>; fn delete(&self, key: &str) -> Result<(), SecretError>; }`; `pub struct FileStore { path }` (`FileStore::in_dir`), `pub struct KeyringStore` (service `"deskswitch"`, user = key), `pub fn open_store(kind: SecretStoreKind, dir: &Path) -> Arc<dyn SecretStore>`; key constants `pub const SMARTTHINGS_CLIENT_SECRET: &str = "smartthings.client_secret"`, `SMARTTHINGS_TOKENS = "smartthings.tokens"`, `LAN_SECRET = "lan.secret"`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::in_dir(dir.path());
        assert_eq!(store.get("a").unwrap(), None);
        store.set("a", "1").unwrap();
        store.set("b", "2").unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        store.delete("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        assert_eq!(store.get("b").unwrap().as_deref(), Some("2"));
    }

    #[cfg(unix)]
    #[test]
    fn file_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::in_dir(dir.path());
        store.set("a", "1").unwrap();
        let mode = std::fs::metadata(dir.path().join("secrets.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn keyring_store_uses_the_mock_backend_in_tests() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let store = KeyringStore;
        store.set("t", "v").unwrap();
        assert_eq!(store.get("t").unwrap().as_deref(), Some("v"));
        store.delete("t").unwrap();
        assert_eq!(store.get("t").unwrap(), None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test secrets`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Where tokens and shared secrets live: the OS keychain by default, a plain
//! `secrets.json` when the config says so (tests, throwaway machines).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::SecretStoreKind;

pub const SMARTTHINGS_CLIENT_SECRET: &str = "smartthings.client_secret";
pub const SMARTTHINGS_TOKENS: &str = "smartthings.tokens";
pub const LAN_SECRET: &str = "lan.secret";

const SERVICE: &str = "deskswitch";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("secrets file error: {0}")]
    File(#[from] std::io::Error),
    #[error("secrets file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

/// Plain-text JSON map, owner-readable only. Not for real credentials.
pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join("secrets.json"), lock: Mutex::new(()) }
    }

    fn read(&self) -> Result<BTreeMap<String, String>, SecretError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn write(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(map)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let _guard = self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(self.read()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let _guard = self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.read()?;
        map.insert(key.to_string(), value.to_string());
        self.write(&map)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let _guard = self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut map = self.read()?;
        map.remove(key);
        self.write(&map)
    }
}

/// macOS Keychain / Windows Credential Manager / Secret Service, via `keyring`.
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match keyring::Entry::new(SERVICE, key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        Ok(keyring::Entry::new(SERVICE, key)?.set_password(value)?)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match keyring::Entry::new(SERVICE, key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[must_use]
pub fn open_store(kind: SecretStoreKind, dir: &Path) -> Arc<dyn SecretStore> {
    match kind {
        SecretStoreKind::Keyring => Arc::new(KeyringStore),
        SecretStoreKind::File => Arc::new(FileStore::in_dir(dir)),
    }
}
```

Add `mod secrets;` to `main.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test secrets && cargo clippy --all-targets -- -D warnings`
Expected: 3 passed (2 on Windows), clean.

- [ ] **Step 5: Commit**

```bash
git add src/secrets.rs src/main.rs
git commit -m "feat(secrets): keychain-backed secret store with a file store for tests"
```

---

### Task 5: `MonitorBackend` trait and the `Chain`

**Files:**
- Create: `src/monitor.rs`

**Interfaces:**
- Produces:
  - `#[async_trait] pub trait MonitorBackend: Send + Sync { fn name(&self) -> &'static str; async fn current(&self) -> Result<Option<Input>, BackendError>; async fn switch(&self, input: Input) -> Result<(), BackendError>; }`
  - `pub enum BackendError { Unauthorized, Http(String), Protocol(String), Unsupported(String) }`
  - `pub struct Chain { … }` with `Chain::new(backends: Vec<Arc<dyn MonitorBackend>>) -> Self`, `with_timing(self, op_timeout: Duration, retry_delay: Duration) -> Self`, `async fn current(&self) -> Option<Input>`, `async fn switch(&self, input: Input) -> Result<&'static str, ChainError>`, `async fn ensure(&self, input: Input) -> Result<Ensure, ChainError>`.
  - `pub enum Ensure { AlreadyThere, Switched { backend: &'static str } }`, `pub struct ChainError { pub attempts: Vec<(String, String)> }` (backend name, error text).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Fake {
        name: &'static str,
        current: Mutex<Option<Input>>,
        fail: bool,
        delay: Duration,
        switches: AtomicUsize,
    }

    impl Fake {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self { name, current: Mutex::new(None), fail: false, delay: Duration::ZERO, switches: AtomicUsize::new(0) })
        }
        fn failing(name: &'static str) -> Arc<Self> {
            Arc::new(Self { fail: true, ..Self::plain(name) })
        }
        fn slow(name: &'static str, delay: Duration) -> Arc<Self> {
            Arc::new(Self { delay, ..Self::plain(name) })
        }
        fn plain(name: &'static str) -> Self {
            Self { name, current: Mutex::new(None), fail: false, delay: Duration::ZERO, switches: AtomicUsize::new(0) }
        }
        fn seen(name: &'static str, input: Input) -> Arc<Self> {
            let fake = Self::plain(name);
            *fake.current.lock().unwrap() = Some(input);
            Arc::new(fake)
        }
    }

    #[async_trait]
    impl MonitorBackend for Fake {
        fn name(&self) -> &'static str { self.name }
        async fn current(&self) -> Result<Option<Input>, BackendError> {
            Ok(*self.current.lock().unwrap())
        }
        async fn switch(&self, input: Input) -> Result<(), BackendError> {
            tokio::time::sleep(self.delay).await;
            self.switches.fetch_add(1, Ordering::SeqCst);
            if self.fail { return Err(BackendError::Http("boom".into())); }
            *self.current.lock().unwrap() = Some(input);
            Ok(())
        }
    }

    fn chain(backends: Vec<Arc<dyn MonitorBackend>>) -> Chain {
        Chain::new(backends).with_timing(Duration::from_millis(50), Duration::from_millis(10))
    }

    #[tokio::test]
    async fn switch_uses_the_first_working_backend_in_order() {
        let a = Fake::failing("a");
        let b = Fake::new("b");
        let used = chain(vec![a.clone(), b.clone()]).switch(Input::Hdmi2).await.unwrap();
        assert_eq!(used, "b");
        assert_eq!(a.switches.load(Ordering::SeqCst), 1);
        assert_eq!(b.switches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_slow_backend_times_out_and_the_next_one_runs() {
        let slow = Fake::slow("slow", Duration::from_secs(5));
        let fast = Fake::new("fast");
        let used = chain(vec![slow, fast]).switch(Input::Hdmi1).await.unwrap();
        assert_eq!(used, "fast");
    }

    #[tokio::test]
    async fn all_failing_backends_get_a_second_pass_then_error() {
        let a = Fake::failing("a");
        let err = chain(vec![a.clone()]).switch(Input::Hdmi3).await.unwrap_err();
        assert_eq!(a.switches.load(Ordering::SeqCst), 2, "one retry pass");
        assert_eq!(err.attempts.len(), 2);
        assert_eq!(err.attempts[0].0, "a");
    }

    #[tokio::test]
    async fn ensure_reads_before_writing() {
        let a = Fake::seen("a", Input::Hdmi2);
        assert_eq!(chain(vec![a.clone()]).ensure(Input::Hdmi2).await.unwrap(), Ensure::AlreadyThere);
        assert_eq!(a.switches.load(Ordering::SeqCst), 0);
        assert_eq!(chain(vec![a.clone()]).ensure(Input::Dp).await.unwrap(), Ensure::Switched { backend: "a" });
        assert_eq!(a.switches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn current_is_the_first_backend_that_knows() {
        let blind = Fake::new("blind");
        let seen = Fake::seen("seen", Input::Hdmi1);
        assert_eq!(chain(vec![blind, seen]).current().await, Some(Input::Hdmi1));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test monitor`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Monitor backends and the chain that makes any of them dependable.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::input::Input;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("not authorized; run the backend's login again")]
    Unauthorized,
    #[error("http: {0}")]
    Http(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
    #[error("{0}")]
    Unsupported(String),
}

#[async_trait]
pub trait MonitorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// The input the monitor shows, or `None` when this backend cannot read.
    async fn current(&self) -> Result<Option<Input>, BackendError>;
    async fn switch(&self, input: Input) -> Result<(), BackendError>;
}

/// Every backend failed, twice.
#[derive(Debug, thiserror::Error)]
#[error("every monitor backend failed: {}", .attempts.iter().map(|(b, e)| format!("{b}: {e}")).collect::<Vec<_>>().join("; "))]
pub struct ChainError {
    pub attempts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensure {
    AlreadyThere,
    Switched { backend: &'static str },
}

pub struct Chain {
    backends: Vec<Arc<dyn MonitorBackend>>,
    op_timeout: Duration,
    retry_delay: Duration,
}

impl Chain {
    #[must_use]
    pub fn new(backends: Vec<Arc<dyn MonitorBackend>>) -> Self {
        Self { backends, op_timeout: Duration::from_secs(5), retry_delay: Duration::from_secs(3) }
    }

    #[must_use]
    pub fn with_timing(mut self, op_timeout: Duration, retry_delay: Duration) -> Self {
        self.op_timeout = op_timeout;
        self.retry_delay = retry_delay;
        self
    }

    /// The first backend that can read the input.
    pub async fn current(&self) -> Option<Input> {
        for backend in &self.backends {
            match tokio::time::timeout(self.op_timeout, backend.current()).await {
                Ok(Ok(Some(input))) => return Some(input),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => warn!(backend = backend.name(), %error, "read failed"),
                Err(_) => warn!(backend = backend.name(), "read timed out"),
            }
        }
        None
    }

    /// Switch through the first backend that succeeds; one retry pass.
    pub async fn switch(&self, input: Input) -> Result<&'static str, ChainError> {
        let mut attempts = Vec::new();
        for pass in 0..2 {
            if pass == 1 {
                tokio::time::sleep(self.retry_delay).await;
            }
            for backend in &self.backends {
                match tokio::time::timeout(self.op_timeout, backend.switch(input)).await {
                    Ok(Ok(())) => {
                        info!(backend = backend.name(), %input, "monitor switched");
                        return Ok(backend.name());
                    }
                    Ok(Err(error)) => {
                        warn!(backend = backend.name(), %error, "switch failed");
                        attempts.push((backend.name().to_string(), error.to_string()));
                    }
                    Err(_) => {
                        warn!(backend = backend.name(), "switch timed out");
                        attempts.push((backend.name().to_string(), "timed out".to_string()));
                    }
                }
            }
        }
        Err(ChainError { attempts })
    }

    /// Read first; write only when the monitor is not already on `input`.
    pub async fn ensure(&self, input: Input) -> Result<Ensure, ChainError> {
        if self.current().await == Some(input) {
            return Ok(Ensure::AlreadyThere);
        }
        let backend = self.switch(input).await?;
        Ok(Ensure::Switched { backend })
    }
}
```

Add `mod monitor;` to `main.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test monitor && cargo clippy --all-targets -- -D warnings`
Expected: 5 passed, clean.

- [ ] **Step 5: Commit**

```bash
git add src/monitor.rs src/main.rs
git commit -m "feat(monitor): backend trait and ordered chain with timeout, retry, and read-before-write"
```

---

### Task 6: OpenLogi shim over the CLI

**Files:**
- Create: `src/openlogi.rs`, `tests/fixtures/openlogi.sh`

**Interfaces:**
- Produces:
  - `#[async_trait] pub trait KeyboardAgent: Send + Sync { async fn probe(&self) -> Probe; async fn switch(&self, slot: u8) -> SwitchResult; }`
  - `pub enum Probe { Here { current_slot: u8, name: String }, Elsewhere, Unavailable(String) }`
  - `pub enum SwitchResult { Switched, AlreadyCurrent, Refused(String), NotFound, Unavailable(String) }`
  - `pub struct OpenLogi { binary: PathBuf }` (`OpenLogi::new(binary)`), implementing `KeyboardAgent` by running `<binary> host list --json` (3 s) and `<binary> host switch N` (10 s).
  - pure `pub fn interpret_list(json: &str) -> Result<Probe, serde_json::Error>` and `pub fn interpret_switch(code: Option<i32>, stdout: &str, stderr: &str) -> SwitchResult`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const HERE: &str = r#"{"keyboards":[{"name":"MX KEYS S MAC","route":"direct 046d:b37c","online":true,"current_slot":1,"slots":[{"slot":1,"paired":true,"name":"Mac"}]}]}"#;
    const AWAY: &str = r#"{"keyboards":[{"name":"MX KEYS S MAC","route":"direct 046d:b37c","online":false,"error":"offline"}]}"#;

    #[test]
    fn list_json_is_interpreted() {
        assert!(matches!(interpret_list(HERE).unwrap(), Probe::Here { current_slot: 1, .. }));
        assert!(matches!(interpret_list(AWAY).unwrap(), Probe::Elsewhere));
        assert!(matches!(interpret_list(r#"{"keyboards":[]}"#).unwrap(), Probe::Elsewhere));
        assert!(interpret_list("nope").is_err());
    }

    #[test]
    fn switch_exit_codes_are_mapped() {
        assert!(matches!(interpret_switch(Some(0), "MX → slot 2\n", ""), SwitchResult::Switched));
        assert!(matches!(interpret_switch(Some(0), "MX already on slot 1\n", ""), SwitchResult::AlreadyCurrent));
        assert!(matches!(interpret_switch(Some(2), "", "unpaired"), SwitchResult::Refused(m) if m == "unpaired"));
        assert!(matches!(interpret_switch(Some(3), "", ""), SwitchResult::NotFound));
        assert!(matches!(interpret_switch(Some(4), "", "no agent"), SwitchResult::Unavailable(_)));
        assert!(matches!(interpret_switch(None, "", ""), SwitchResult::Unavailable(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_fixture_script_drives_both_commands() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openlogi.sh");
        let agent = OpenLogi::new(fixture);
        temp_env::async_with_vars([("FAKE_OPENLOGI_HERE", Some("1")), ("FAKE_OPENLOGI_SLOT", Some("1"))], async {
            assert!(matches!(agent.probe().await, Probe::Here { current_slot: 1, .. }));
            assert!(matches!(agent.switch(2).await, SwitchResult::Switched));
            assert!(matches!(agent.switch(3).await, SwitchResult::Refused(_)));
        }).await;
        let missing = OpenLogi::new("/nonexistent/openlogi".into());
        assert!(matches!(missing.probe().await, Probe::Unavailable(_)));
    }
}
```

`tests/fixtures/openlogi.sh` (make it executable with `chmod +x`):

```sh
#!/bin/sh
# Fake `openlogi` for tests. Env: FAKE_OPENLOGI_HERE=1 → keyboard online here on slot $FAKE_OPENLOGI_SLOT.
# `host switch 3` is refused (unpaired); `host switch <current>` is already current; others switch.
# Every switch appends its slot to $FAKE_OPENLOGI_LOG when set.
case "$1 $2" in
  "host list")
    if [ "$FAKE_OPENLOGI_HERE" = "1" ]; then
      printf '{"keyboards":[{"name":"FAKE KEYS","route":"direct 046d:b37c","online":true,"current_slot":%s,"slots":[]}]}\n' "${FAKE_OPENLOGI_SLOT:-1}"
    else
      printf '{"keyboards":[{"name":"FAKE KEYS","route":"direct 046d:b37c","online":false,"error":"offline"}]}\n'
    fi
    exit 0 ;;
  "host switch")
    [ -n "$FAKE_OPENLOGI_LOG" ] && printf '%s\n' "$3" >> "$FAKE_OPENLOGI_LOG"
    if [ "$FAKE_OPENLOGI_HERE" != "1" ]; then echo "no online keyboard matched" >&2; exit 3; fi
    if [ "$3" = "3" ]; then echo "the requested host slot is not paired on the keyboard" >&2; exit 2; fi
    if [ "$3" = "${FAKE_OPENLOGI_SLOT:-1}" ]; then echo "FAKE KEYS already on slot $3"; exit 0; fi
    echo "FAKE KEYS → slot $3"; exit 0 ;;
  *) echo "unknown command" >&2; exit 1 ;;
esac
```

- [ ] **Step 2: Run to verify they fail**

Run: `chmod +x tests/fixtures/openlogi.sh && cargo test openlogi`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! The keyboard side, delegated to OpenLogi's CLI: is the keyboard here, and
//! move it. Exit codes follow `openlogi host` (0 done, 2 refused, 3 no
//! keyboard, 4 agent unavailable).

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tracing::warn;

#[derive(Debug)]
pub enum Probe {
    Here { current_slot: u8, name: String },
    Elsewhere,
    Unavailable(String),
}

#[derive(Debug)]
pub enum SwitchResult {
    Switched,
    AlreadyCurrent,
    Refused(String),
    NotFound,
    Unavailable(String),
}

#[async_trait]
pub trait KeyboardAgent: Send + Sync {
    async fn probe(&self) -> Probe;
    async fn switch(&self, slot: u8) -> SwitchResult;
}

pub struct OpenLogi {
    binary: PathBuf,
}

impl OpenLogi {
    #[must_use]
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }

    async fn run(&self, args: &[&str], timeout: Duration) -> Result<std::process::Output, String> {
        let child = tokio::process::Command::new(&self.binary)
            .args(args)
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(timeout, child).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(format!("cannot run {}: {error}", self.binary.display())),
            Err(_) => Err(format!("{} {} timed out after {timeout:?}", self.binary.display(), args.join(" "))),
        }
    }
}

#[async_trait]
impl KeyboardAgent for OpenLogi {
    async fn probe(&self) -> Probe {
        match self.run(&["host", "list", "--json"], Duration::from_secs(3)).await {
            Ok(output) if output.status.code() == Some(4) => {
                Probe::Unavailable(String::from_utf8_lossy(&output.stderr).trim().to_string())
            }
            Ok(output) => match interpret_list(&String::from_utf8_lossy(&output.stdout)) {
                Ok(probe) => probe,
                Err(error) => Probe::Unavailable(format!("bad host list output: {error}")),
            },
            Err(message) => Probe::Unavailable(message),
        }
    }

    async fn switch(&self, slot: u8) -> SwitchResult {
        let slot = slot.to_string();
        match self.run(&["host", "switch", &slot], Duration::from_secs(10)).await {
            Ok(output) => interpret_switch(
                output.status.code(),
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
            ),
            Err(message) => {
                warn!(%message, "openlogi host switch could not run");
                SwitchResult::Unavailable(message)
            }
        }
    }
}

#[derive(Deserialize)]
struct ListReport {
    keyboards: Vec<KeyboardReport>,
}

#[derive(Deserialize)]
struct KeyboardReport {
    name: String,
    online: bool,
    #[serde(default)]
    current_slot: Option<u8>,
}

/// `Here` for the first online keyboard reporting a current slot.
pub fn interpret_list(json: &str) -> Result<Probe, serde_json::Error> {
    let report: ListReport = serde_json::from_str(json)?;
    Ok(report
        .keyboards
        .into_iter()
        .find_map(|keyboard| match (keyboard.online, keyboard.current_slot) {
            (true, Some(current_slot)) => Some(Probe::Here { current_slot, name: keyboard.name }),
            _ => None,
        })
        .unwrap_or(Probe::Elsewhere))
}

pub fn interpret_switch(code: Option<i32>, stdout: &str, stderr: &str) -> SwitchResult {
    match code {
        Some(0) if stdout.contains("already") => SwitchResult::AlreadyCurrent,
        Some(0) => SwitchResult::Switched,
        Some(2) => SwitchResult::Refused(stderr.trim().to_string()),
        Some(3) => SwitchResult::NotFound,
        Some(4) => SwitchResult::Unavailable(stderr.trim().to_string()),
        other => SwitchResult::Unavailable(format!("openlogi exited with {other:?}: {}", stderr.trim())),
    }
}
```

Add `mod openlogi;` to `main.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test openlogi && cargo clippy --all-targets -- -D warnings`
Expected: 3 passed, clean.

- [ ] **Step 5: Commit**

```bash
git add src/openlogi.rs src/main.rs tests/fixtures/openlogi.sh
git commit -m "feat(openlogi): probe and switch the keyboard through the OpenLogi CLI"
```

---

### Task 7: SmartThings backend (status, commands, device list)

**Files:**
- Create: `src/smartthings.rs`
- Modify: `src/main.rs` (`mod smartthings;`), `src/cli.rs` (no new commands yet)

**Interfaces:**
- Produces:
  - `#[async_trait] pub trait TokenProvider: Send + Sync { async fn bearer(&self) -> Result<String, BackendError>; async fn invalidate(&self); }` and `pub struct StaticToken(pub String)` implementing it.
  - `pub struct SmartThings { http: reqwest::Client, base_url: Url, device_id: String, tokens: Arc<dyn TokenProvider> }` with `SmartThings::new(config: &SmartThingsConfig, tokens: Arc<dyn TokenProvider>) -> Result<Self, BackendError>`, `async fn supported_inputs(&self) -> Result<Vec<String>, BackendError>`, `async fn list_devices(&self) -> Result<Vec<(String, String)>, BackendError>` (id, label), and `impl MonitorBackend` (`name() == "smartthings"`).
  - One 401 triggers `invalidate()` and a single retry.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config(server: &MockServer) -> SmartThingsConfig {
        SmartThingsConfig {
            device_id: "dev-1".into(),
            client_id: "client".into(),
            base_url: server.uri(),
            token_url: format!("{}/oauth/token", server.uri()),
            authorize_url: format!("{}/oauth/authorize", server.uri()),
            redirect_uri: "http://localhost:8765/callback".into(),
        }
    }

    fn status_body(input: &str) -> serde_json::Value {
        serde_json::json!({
            "inputSource": {"value": input},
            "supportedInputSourcesMap": {"value": [
                {"id": "HDMI1", "name": "PC"}, {"id": "HDMI2", "name": "HDMI2"}, {"id": "HDMI3", "name": "HDMI3"}
            ]}
        })
    }

    #[tokio::test]
    async fn reads_the_current_input_and_supported_map() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/devices/dev-1/components/main/capabilities/samsungvd.mediaInputSource/status"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(status_body("HDMI2")))
            .mount(&server).await;
        let st = SmartThings::new(&config(&server), Arc::new(StaticToken("tok".into()))).unwrap();
        assert_eq!(st.current().await.unwrap(), Some(Input::Hdmi2));
        assert_eq!(st.supported_inputs().await.unwrap(), ["HDMI1", "HDMI2", "HDMI3"]);
    }

    #[tokio::test]
    async fn switch_posts_the_documented_command() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/devices/dev-1/commands"))
            .and(body_json(serde_json::json!({"commands":[{"component":"main","capability":"samsungvd.mediaInputSource","command":"setInputSource","arguments":["HDMI3"]}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"results":[{"status":"ACCEPTED"}]})))
            .expect(1)
            .mount(&server).await;
        let st = SmartThings::new(&config(&server), Arc::new(StaticToken("tok".into()))).unwrap();
        st.switch(Input::Hdmi3).await.unwrap();
    }

    #[tokio::test]
    async fn a_401_invalidates_and_retries_once() {
        struct Counting(std::sync::atomic::AtomicUsize);
        #[async_trait]
        impl TokenProvider for Counting {
            async fn bearer(&self) -> Result<String, BackendError> {
                Ok(format!("tok{}", self.0.load(std::sync::atomic::Ordering::SeqCst)))
            }
            async fn invalidate(&self) { self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/devices/dev-1/commands")).and(header("authorization", "Bearer tok0"))
            .respond_with(ResponseTemplate::new(401)).expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/devices/dev-1/commands")).and(header("authorization", "Bearer tok1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({}))).expect(1).mount(&server).await;
        let st = SmartThings::new(&config(&server), Arc::new(Counting(0.into()))).unwrap();
        st.switch(Input::Hdmi1).await.unwrap();
    }

    #[tokio::test]
    async fn device_list_keeps_only_input_capable_devices() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items":[
                {"deviceId":"ark","label":"Odyssey Ark","components":[{"id":"main","capabilities":[{"id":"samsungvd.mediaInputSource"}]}]},
                {"deviceId":"bulb","label":"Lamp","components":[{"id":"main","capabilities":[{"id":"switch"}]}]}
            ]})))
            .mount(&server).await;
        let st = SmartThings::new(&config(&server), Arc::new(StaticToken("tok".into()))).unwrap();
        assert_eq!(st.list_devices().await.unwrap(), vec![("ark".to_string(), "Odyssey Ark".to_string())]);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test smartthings`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Samsung SmartThings cloud backend: `samsungvd.mediaInputSource` reads and
//! writes on the Ark's device id.

pub mod auth;

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use url::Url;

use crate::config::SmartThingsConfig;
use crate::input::Input;
use crate::monitor::{BackendError, MonitorBackend};

const CAPABILITY: &str = "samsungvd.mediaInputSource";

#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// A currently valid access token.
    async fn bearer(&self) -> Result<String, BackendError>;
    /// The last token was rejected; fetch a fresh one next time.
    async fn invalidate(&self);
}

/// A fixed token, for tests and one-off scripts.
pub struct StaticToken(pub String);

#[async_trait]
impl TokenProvider for StaticToken {
    async fn bearer(&self) -> Result<String, BackendError> { Ok(self.0.clone()) }
    async fn invalidate(&self) {}
}

pub struct SmartThings {
    http: reqwest::Client,
    base_url: Url,
    device_id: String,
    tokens: Arc<dyn TokenProvider>,
}

#[derive(Deserialize)]
struct CapabilityStatus {
    #[serde(rename = "inputSource")]
    input_source: Option<Attr<String>>,
    #[serde(rename = "supportedInputSourcesMap")]
    supported: Option<Attr<Vec<SupportedInput>>>,
}

#[derive(Deserialize)]
struct Attr<T> {
    value: Option<T>,
}

#[derive(Deserialize)]
struct SupportedInput {
    id: String,
}

#[derive(Deserialize)]
struct DeviceList {
    items: Vec<Device>,
}

#[derive(Deserialize)]
struct Device {
    #[serde(rename = "deviceId")]
    device_id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    components: Vec<Component>,
}

#[derive(Deserialize)]
struct Component {
    #[serde(default)]
    capabilities: Vec<Capability>,
}

#[derive(Deserialize)]
struct Capability {
    id: String,
}

impl SmartThings {
    pub fn new(config: &SmartThingsConfig, tokens: Arc<dyn TokenProvider>) -> Result<Self, BackendError> {
        let base_url = Url::parse(config.base_url.trim_end_matches('/'))
            .map_err(|e| BackendError::Protocol(format!("bad base_url: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .map_err(|e| BackendError::Http(e.to_string()))?;
        Ok(Self { http, base_url, device_id: config.device_id.clone(), tokens })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.as_str().trim_end_matches('/'), path.trim_start_matches('/'))
    }

    /// Send with the current token; on 401 invalidate and retry exactly once.
    async fn send(&self, build: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response, BackendError> {
        for attempt in 0..2 {
            let token = self.tokens.bearer().await?;
            let response = build()
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| BackendError::Http(e.to_string()))?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.tokens.invalidate().await;
                continue;
            }
            if response.status() == StatusCode::UNAUTHORIZED {
                return Err(BackendError::Unauthorized);
            }
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(BackendError::Http(format!("{status}: {body}")));
            }
            return Ok(response);
        }
        Err(BackendError::Unauthorized)
    }

    async fn status(&self) -> Result<CapabilityStatus, BackendError> {
        let url = self.url(&format!("devices/{}/components/main/capabilities/{CAPABILITY}/status", self.device_id));
        self.send(|| self.http.get(&url))
            .await?
            .json()
            .await
            .map_err(|e| BackendError::Protocol(e.to_string()))
    }

    /// Input ids the Ark reports, e.g. `["HDMI1", "HDMI2", "HDMI3"]`.
    pub async fn supported_inputs(&self) -> Result<Vec<String>, BackendError> {
        Ok(self
            .status()
            .await?
            .supported
            .and_then(|attr| attr.value)
            .unwrap_or_default()
            .into_iter()
            .map(|input| input.id)
            .collect())
    }

    /// `(deviceId, label)` for every device exposing the input-source capability.
    pub async fn list_devices(&self) -> Result<Vec<(String, String)>, BackendError> {
        let url = self.url("devices");
        let list: DeviceList = self
            .send(|| self.http.get(&url))
            .await?
            .json()
            .await
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        Ok(list
            .items
            .into_iter()
            .filter(|device| {
                device.components.iter().any(|component| {
                    component.capabilities.iter().any(|capability| capability.id == CAPABILITY)
                })
            })
            .map(|device| {
                let label = device.label.clone().unwrap_or_else(|| device.device_id.clone());
                (device.device_id, label)
            })
            .collect())
    }
}

#[async_trait]
impl MonitorBackend for SmartThings {
    fn name(&self) -> &'static str { "smartthings" }

    async fn current(&self) -> Result<Option<Input>, BackendError> {
        let value = self.status().await?.input_source.and_then(|attr| attr.value);
        match value {
            Some(id) => id
                .parse::<Input>()
                .map(Some)
                .map_err(|e| BackendError::Protocol(e.to_string())),
            None => Ok(None),
        }
    }

    async fn switch(&self, input: Input) -> Result<(), BackendError> {
        let url = self.url(&format!("devices/{}/commands", self.device_id));
        let body = serde_json::json!({"commands": [{
            "component": "main",
            "capability": CAPABILITY,
            "command": "setInputSource",
            "arguments": [input.as_str()],
        }]});
        self.send(|| self.http.post(&url).json(&body)).await?;
        Ok(())
    }
}
```

Create an empty `src/smartthings/auth.rs` containing only `//! OAuth token management for SmartThings.` so `pub mod auth;` compiles; Task 8 fills it. Add `mod smartthings;` to `main.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test smartthings && cargo clippy --all-targets -- -D warnings`
Expected: 4 passed, clean.

- [ ] **Step 5: Commit**

```bash
git add src/smartthings.rs src/smartthings/auth.rs src/main.rs
git commit -m "feat(smartthings): input-source reads and writes with one-shot 401 retry"
```

---

### Task 8: SmartThings OAuth: token manager and login

**Files:**
- Modify: `src/smartthings/auth.rs`

**Interfaces:**
- Produces:
  - `pub struct TokenSet { pub access_token: String, pub refresh_token: String, pub expires_at: DateTime<Utc> }` (serde JSON in the secret store under `SMARTTHINGS_TOKENS`).
  - `pub struct TokenManager { … }` with `TokenManager::new(config: &SmartThingsConfig, secrets: Arc<dyn SecretStore>) -> Self`, implementing `TokenProvider`: `bearer()` returns the stored access token while `expires_at > now + 60s`, otherwise refreshes (`grant_type=refresh_token`, basic auth client id/secret) and stores the rotated pair; `invalidate()` forces a refresh on the next call; no stored tokens or a failed refresh → `BackendError::Unauthorized` with a message to run `smartthings login`.
  - `pub async fn login(config: &SmartThingsConfig, secrets: &dyn SecretStore, client_secret: &str, open_browser: bool) -> anyhow::Result<TokenSet>`: stores the client secret, listens on the redirect URI's port for `GET /callback?code=…`, opens the authorize URL, exchanges the code (`grant_type=authorization_code`), stores the tokens.
  - pure `pub fn authorize_url(config: &SmartThingsConfig) -> String` and `pub fn code_from_request_line(line: &str) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{FileStore, SMARTTHINGS_CLIENT_SECRET, SMARTTHINGS_TOKENS};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config(server: &MockServer, redirect: &str) -> SmartThingsConfig {
        SmartThingsConfig {
            device_id: "dev-1".into(),
            client_id: "client".into(),
            base_url: server.uri(),
            token_url: format!("{}/oauth/token", server.uri()),
            authorize_url: format!("{}/oauth/authorize", server.uri()),
            redirect_uri: redirect.into(),
        }
    }

    fn secrets() -> (tempfile::TempDir, Arc<FileStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::in_dir(dir.path()));
        store.set(SMARTTHINGS_CLIENT_SECRET, "shh").unwrap();
        (dir, store)
    }

    fn tokens(expires_in_secs: i64) -> TokenSet {
        TokenSet { access_token: "old".into(), refresh_token: "r0".into(), expires_at: Utc::now() + chrono::Duration::seconds(expires_in_secs) }
    }

    #[tokio::test]
    async fn a_fresh_token_is_returned_without_a_refresh() {
        let server = MockServer::start().await;
        let (_dir, store) = secrets();
        store.set(SMARTTHINGS_TOKENS, &serde_json::to_string(&tokens(3600)).unwrap()).unwrap();
        let manager = TokenManager::new(&config(&server, "http://localhost:0/callback"), store);
        assert_eq!(manager.bearer().await.unwrap(), "old");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_expiring_token_is_refreshed_with_basic_auth_and_rotated_tokens_are_stored() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/oauth/token"))
            .and(header("authorization", "Basic Y2xpZW50OnNoaA=="))
            .and(body_string_contains("grant_type=refresh_token")).and(body_string_contains("refresh_token=r0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"new","refresh_token":"r1","expires_in":86400})))
            .expect(1).mount(&server).await;
        let (_dir, store) = secrets();
        store.set(SMARTTHINGS_TOKENS, &serde_json::to_string(&tokens(30)).unwrap()).unwrap();
        let manager = TokenManager::new(&config(&server, "http://localhost:0/callback"), store.clone());
        assert_eq!(manager.bearer().await.unwrap(), "new");
        let stored: TokenSet = serde_json::from_str(&store.get(SMARTTHINGS_TOKENS).unwrap().unwrap()).unwrap();
        assert_eq!(stored.refresh_token, "r1");
        assert_eq!(manager.bearer().await.unwrap(), "new", "second call served from memory");
    }

    #[tokio::test]
    async fn missing_tokens_or_a_failed_refresh_ask_for_login() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/oauth/token")).respond_with(ResponseTemplate::new(400)).mount(&server).await;
        let (_dir, store) = secrets();
        let manager = TokenManager::new(&config(&server, "http://localhost:0/callback"), store.clone());
        assert!(matches!(manager.bearer().await, Err(BackendError::Unauthorized)));
        store.set(SMARTTHINGS_TOKENS, &serde_json::to_string(&tokens(0)).unwrap()).unwrap();
        assert!(matches!(manager.bearer().await, Err(BackendError::Unauthorized)));
    }

    #[test]
    fn authorize_url_and_callback_parsing() {
        let cfg = SmartThingsConfig { device_id: "d".into(), client_id: "client".into(), base_url: String::new(), token_url: String::new(), authorize_url: "https://api.smartthings.com/oauth/authorize".into(), redirect_uri: "http://localhost:8765/callback".into() };
        let url = authorize_url(&cfg);
        assert!(url.starts_with("https://api.smartthings.com/oauth/authorize?"));
        assert!(url.contains("client_id=client") && url.contains("response_type=code") && url.contains("scope=r%3Adevices%3A%2A+x%3Adevices%3A%2A"));
        assert_eq!(code_from_request_line("GET /callback?code=abc123&state=x HTTP/1.1").as_deref(), Some("abc123"));
        assert_eq!(code_from_request_line("GET /favicon.ico HTTP/1.1"), None);
    }

    #[tokio::test]
    async fn login_exchanges_the_code_it_receives_on_the_redirect_port() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/oauth/token"))
            .and(body_string_contains("grant_type=authorization_code")).and(body_string_contains("code=abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"a","refresh_token":"r","expires_in":86400})))
            .expect(1).mount(&server).await;
        let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
        let redirect = format!("http://localhost:{port}/callback");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let cfg = config(&server, &redirect);
        let login = tokio::spawn(async move {
            // `open_browser = false`: the test plays the browser.
            let store = FileStore::in_dir(&path);
            login(&cfg, &store, "shh", false).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let body = reqwest::get(format!("{redirect}?code=abc")).await.unwrap().text().await.unwrap();
        assert!(body.contains("You can close this tab"));
        let tokens = login.await.unwrap().unwrap();
        assert_eq!(tokens.access_token, "a");
        let stored = FileStore::in_dir(dir.path()).get(SMARTTHINGS_TOKENS).unwrap();
        assert!(stored.is_some(), "tokens were persisted");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test smartthings::auth`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! OAuth token management for SmartThings: authorization-code login once, then
//! refresh with the stored refresh token (rotated on every refresh).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::SmartThingsConfig;
use crate::monitor::BackendError;
use crate::secrets::{SMARTTHINGS_CLIENT_SECRET, SMARTTHINGS_TOKENS, SecretStore};
use crate::smartthings::TokenProvider;

const SCOPES: &str = "r:devices:* x:devices:*";
/// Refresh this long before the access token actually expires.
const REFRESH_MARGIN_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

impl TokenSet {
    fn fresh(&self) -> bool {
        self.expires_at > Utc::now() + chrono::Duration::seconds(REFRESH_MARGIN_SECS)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

fn default_expires_in() -> i64 { 86_400 }

impl From<TokenResponse> for TokenSet {
    fn from(response: TokenResponse) -> Self {
        Self {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: Utc::now() + chrono::Duration::seconds(response.expires_in),
        }
    }
}

pub struct TokenManager {
    client_id: String,
    token_url: String,
    secrets: Arc<dyn SecretStore>,
    http: reqwest::Client,
    cached: Mutex<Option<TokenSet>>,
}

impl TokenManager {
    #[must_use]
    pub fn new(config: &SmartThingsConfig, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            client_id: config.client_id.clone(),
            token_url: config.token_url.clone(),
            secrets,
            http: reqwest::Client::new(),
            cached: Mutex::new(None),
        }
    }

    fn load(&self) -> Result<Option<TokenSet>, BackendError> {
        let Some(json) = self.secrets.get(SMARTTHINGS_TOKENS).map_err(|e| BackendError::Protocol(e.to_string()))? else {
            return Ok(None);
        };
        serde_json::from_str(&json).map(Some).map_err(|e| BackendError::Protocol(format!("stored tokens unreadable: {e}")))
    }

    fn store(&self, tokens: &TokenSet) -> Result<(), BackendError> {
        let json = serde_json::to_string(tokens).map_err(|e| BackendError::Protocol(e.to_string()))?;
        self.secrets.set(SMARTTHINGS_TOKENS, &json).map_err(|e| BackendError::Protocol(e.to_string()))
    }

    async fn refresh(&self, refresh_token: &str) -> Result<TokenSet, BackendError> {
        let client_secret = self
            .secrets
            .get(SMARTTHINGS_CLIENT_SECRET)
            .map_err(|e| BackendError::Protocol(e.to_string()))?
            .ok_or(BackendError::Unauthorized)?;
        let response = self
            .http
            .post(&self.token_url)
            .basic_auth(&self.client_id, Some(&client_secret))
            .form(&[("grant_type", "refresh_token"), ("client_id", self.client_id.as_str()), ("refresh_token", refresh_token)])
            .send()
            .await
            .map_err(|e| BackendError::Http(e.to_string()))?;
        if !response.status().is_success() {
            warn!(status = %response.status(), "SmartThings token refresh refused; run `deskswitch smartthings login`");
            return Err(BackendError::Unauthorized);
        }
        let tokens: TokenSet = response.json::<TokenResponse>().await.map_err(|e| BackendError::Protocol(e.to_string()))?.into();
        self.store(&tokens)?;
        info!("SmartThings access token refreshed");
        Ok(tokens)
    }
}

#[async_trait]
impl TokenProvider for TokenManager {
    async fn bearer(&self) -> Result<String, BackendError> {
        let mut cached = self.cached.lock().await;
        if cached.is_none() {
            *cached = self.load()?;
        }
        let Some(tokens) = cached.as_ref() else {
            return Err(BackendError::Unauthorized);
        };
        if tokens.fresh() {
            return Ok(tokens.access_token.clone());
        }
        let refreshed = self.refresh(&tokens.refresh_token.clone()).await?;
        let access = refreshed.access_token.clone();
        *cached = Some(refreshed);
        Ok(access)
    }

    async fn invalidate(&self) {
        let mut cached = self.cached.lock().await;
        if let Some(tokens) = cached.as_mut() {
            tokens.expires_at = Utc::now();
        }
    }
}

/// The browser URL that starts the authorization-code flow.
#[must_use]
pub fn authorize_url(config: &SmartThingsConfig) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &config.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", SCOPES)
        .finish();
    format!("{}?{query}", config.authorize_url)
}

/// `code` from the first line of the redirect request, e.g.
/// `GET /callback?code=abc&state=x HTTP/1.1`.
#[must_use]
pub fn code_from_request_line(line: &str) -> Option<String> {
    let target = line.split_whitespace().nth(1)?;
    let query = target.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
}

/// Run the one-time login: store the client secret, wait for the browser to
/// hit the redirect URI, exchange the code, store the tokens.
pub async fn login(
    config: &SmartThingsConfig,
    secrets: &dyn SecretStore,
    client_secret: &str,
    open_browser: bool,
) -> anyhow::Result<TokenSet> {
    secrets.set(SMARTTHINGS_CLIENT_SECRET, client_secret)?;
    let redirect = url::Url::parse(&config.redirect_uri)?;
    let port = redirect.port().ok_or_else(|| anyhow::anyhow!("redirect_uri needs an explicit port"))?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let url = authorize_url(config);
    if open_browser {
        if let Err(error) = open::that(&url) {
            warn!(%error, "could not open the browser; open this URL yourself:\n{url}");
        }
    }
    println!("Waiting for SmartThings to redirect to {} …", config.redirect_uri);
    println!("If the browser did not open, visit:\n{url}");
    let code = loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = vec![0u8; 4096];
        let read = stream.read(&mut buffer).await?;
        let request = String::from_utf8_lossy(&buffer[..read]);
        let line = request.lines().next().unwrap_or_default();
        match code_from_request_line(line) {
            Some(code) => {
                let body = "deskswitch: authorized. You can close this tab.";
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).await?;
                break code;
            }
            None => {
                stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
            }
        }
    };
    let response = reqwest::Client::new()
        .post(&config.token_url)
        .basic_auth(&config.client_id, Some(client_secret))
        .form(&[("grant_type", "authorization_code"), ("client_id", config.client_id.as_str()), ("code", code.as_str()), ("redirect_uri", config.redirect_uri.as_str())])
        .send()
        .await?;
    anyhow::ensure!(response.status().is_success(), "token exchange failed: {} {}", response.status(), response.text().await.unwrap_or_default());
    let tokens: TokenSet = response.json::<TokenResponse>().await?.into();
    secrets.set(SMARTTHINGS_TOKENS, &serde_json::to_string(&tokens)?)?;
    Ok(tokens)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test smartthings && cargo clippy --all-targets -- -D warnings`
Expected: 9 passed, clean.

- [ ] **Step 5: Commit**

```bash
git add src/smartthings/auth.rs
git commit -m "feat(smartthings): OAuth login and refreshing token manager"
```

---

### Task 9: Switch orchestration and `arrive`

**Files:**
- Create: `src/switch.rs`
- Modify: `src/monitor.rs` (move the `Fake` backend into `#[cfg(test)] pub(crate) mod test_support` so other modules' tests can use it), `src/main.rs`

**Interfaces:**
- Produces:
  - `pub struct Deps { pub config: Config, pub chain: Chain, pub agent: Arc<dyn KeyboardAgent>, pub state: StateStore, pub relay: Arc<dyn Relay> }`
  - `#[async_trait] pub trait Relay: Send + Sync { async fn relay(&self, config: &Config, slot: u8) -> Vec<RelayOutcome>; }`, `pub struct RelayOutcome { pub peer: String, pub result: Result<String, String> }`, `pub struct NoRelay;`
  - `#[derive(PartialEq, Eq, Clone, Copy)] pub enum Mode { Hook, User }`, `pub fn mode_from_env() -> Mode` (`Hook` iff `OPENLOGI_TRIGGER` is set).
  - `pub enum Outcome { Delegated, Monitor { ensure: Ensure, relayed: Vec<RelayOutcome> } }`
  - `pub enum SwitchError { Resolve(ResolveError), Monitor(ChainError), AgentRefused(String) }` with `fn exit_code(&self) -> u8` (3, 2, 1).
  - `pub async fn run_switch(deps: &Deps, target: &Target, mode: Mode) -> Result<Outcome, SwitchError>`
  - `pub struct ArriveTiming { pub rate_limit: Duration, pub grace: Duration }` (`Default` = 30 s / 2.5 s), `pub enum ArriveOutcome { RateLimited, AlreadyMine, Switched, Failed(ChainError) }`, `pub async fn run_arrive(deps: &Deps, timing: ArriveTiming) -> ArriveOutcome`.
  - `pub struct StatusReport { pub machine: String, pub input: Option<Input>, pub keyboard_here: bool, pub current_slot: Option<u8>, pub last_target_slot: Option<u8>, pub last_switch_at: Option<DateTime<Utc>> }` (Serialize), `pub async fn status(deps: &Deps) -> StatusReport`.

- [ ] **Step 1: Move the fake backend into shared test support**

In `src/monitor.rs`, delete the `Fake` struct, its `impl Fake`, and its `MonitorBackend` impl from `mod tests`, and add this module above `mod tests`; then make `mod tests` start with `use super::test_support::Fake;` in place of the deleted code (the five Task 5 test functions stay exactly as they are):

```rust
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::{BackendError, MonitorBackend};
    use crate::input::Input;

    /// A scripted backend: optional failure, optional delay, a remembered
    /// input, and a switch counter.
    pub(crate) struct Fake {
        pub(crate) name: &'static str,
        pub(crate) current: Mutex<Option<Input>>,
        pub(crate) fail: bool,
        pub(crate) delay: Duration,
        pub(crate) switches: AtomicUsize,
    }

    impl Fake {
        fn plain(name: &'static str) -> Self {
            Self { name, current: Mutex::new(None), fail: false, delay: Duration::ZERO, switches: AtomicUsize::new(0) }
        }
        pub(crate) fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self::plain(name))
        }
        pub(crate) fn failing(name: &'static str) -> Arc<Self> {
            Arc::new(Self { fail: true, ..Self::plain(name) })
        }
        pub(crate) fn slow(name: &'static str, delay: Duration) -> Arc<Self> {
            Arc::new(Self { delay, ..Self::plain(name) })
        }
        pub(crate) fn seen(name: &'static str, input: Input) -> Arc<Self> {
            let fake = Self::plain(name);
            *fake.current.lock().unwrap() = Some(input);
            Arc::new(fake)
        }
    }

    #[async_trait]
    impl MonitorBackend for Fake {
        fn name(&self) -> &'static str { self.name }
        async fn current(&self) -> Result<Option<Input>, BackendError> {
            Ok(*self.current.lock().unwrap())
        }
        async fn switch(&self, input: Input) -> Result<(), BackendError> {
            tokio::time::sleep(self.delay).await;
            self.switches.fetch_add(1, Ordering::SeqCst);
            if self.fail { return Err(BackendError::Http("boom".into())); }
            *self.current.lock().unwrap() = Some(input);
            Ok(())
        }
    }
}
```

Run: `cargo test monitor` — Expected: still 5 passed.

- [ ] **Step 2: Write the failing tests**

`src/switch.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::monitor::test_support::Fake;
    use crate::openlogi::{Probe, SwitchResult};

    struct FakeAgent {
        probe: Mutex<Option<Probe>>,      // None = Unavailable
        switches: Mutex<Vec<u8>>,
        refuse: Option<u8>,
    }

    impl FakeAgent {
        fn here(slot: u8) -> Arc<Self> {
            Arc::new(Self { probe: Mutex::new(Some(Probe::Here { current_slot: slot, name: "K".into() })), switches: Mutex::new(vec![]), refuse: None })
        }
        fn away() -> Arc<Self> {
            Arc::new(Self { probe: Mutex::new(Some(Probe::Elsewhere)), switches: Mutex::new(vec![]), refuse: None })
        }
        fn down() -> Arc<Self> {
            Arc::new(Self { probe: Mutex::new(None), switches: Mutex::new(vec![]), refuse: None })
        }
        fn here_refusing(slot: u8, refuse: u8) -> Arc<Self> {
            Arc::new(Self { probe: Mutex::new(Some(Probe::Here { current_slot: slot, name: "K".into() })), switches: Mutex::new(vec![]), refuse: Some(refuse) })
        }
    }

    #[async_trait]
    impl KeyboardAgent for FakeAgent {
        async fn probe(&self) -> Probe {
            match self.probe.lock().unwrap().as_ref() {
                Some(Probe::Here { current_slot, name }) => Probe::Here { current_slot: *current_slot, name: name.clone() },
                Some(Probe::Elsewhere) => Probe::Elsewhere,
                Some(Probe::Unavailable(m)) => Probe::Unavailable(m.clone()),
                None => Probe::Unavailable("down".into()),
            }
        }
        async fn switch(&self, slot: u8) -> SwitchResult {
            self.switches.lock().unwrap().push(slot);
            if self.refuse == Some(slot) { return SwitchResult::Refused("unpaired".into()); }
            SwitchResult::Switched
        }
    }

    struct RecordingRelay(Mutex<Vec<u8>>);

    #[async_trait]
    impl Relay for RecordingRelay {
        async fn relay(&self, _config: &Config, slot: u8) -> Vec<RelayOutcome> {
            self.0.lock().unwrap().push(slot);
            vec![RelayOutcome { peer: "avapc".into(), result: Ok("delegated".into()) }]
        }
    }

    fn deps(agent: Arc<dyn KeyboardAgent>, backend: Arc<Fake>, relay: Arc<RecordingRelay>) -> (tempfile::TempDir, Deps) {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        let chain = Chain::new(vec![backend]).with_timing(Duration::from_millis(50), Duration::from_millis(1));
        let deps = Deps { config, chain, agent, state: StateStore::in_dir(dir.path()), relay };
        (dir, deps)
    }

    fn relay() -> Arc<RecordingRelay> { Arc::new(RecordingRelay(Mutex::new(vec![]))) }

    #[tokio::test]
    async fn user_mode_delegates_when_the_keyboard_is_here_on_another_slot() {
        let agent = FakeAgent::here(1);
        let backend = Fake::new("st");
        let (_d, deps) = deps(agent.clone(), backend.clone(), relay());
        let outcome = run_switch(&deps, &Target::Slot(2), Mode::User).await.unwrap();
        assert!(matches!(outcome, Outcome::Delegated));
        assert_eq!(*agent.switches.lock().unwrap(), vec![2]);
        assert_eq!(backend.switches.load(std::sync::atomic::Ordering::SeqCst), 0, "the hook does the monitor");
    }

    #[tokio::test]
    async fn hook_mode_never_calls_the_agent_and_only_moves_the_monitor() {
        let agent = FakeAgent::here(1);
        let backend = Fake::new("st");
        let r = relay();
        let (_d, deps) = deps(agent.clone(), backend.clone(), r.clone());
        let outcome = run_switch(&deps, &Target::Slot(2), Mode::Hook).await.unwrap();
        assert!(matches!(outcome, Outcome::Monitor { ensure: Ensure::Switched { .. }, ref relayed } if relayed.is_empty()));
        assert!(agent.switches.lock().unwrap().is_empty());
        assert!(r.0.lock().unwrap().is_empty());
        assert_eq!(deps.state.load().last_known_input, Some(Input::Hdmi3));
    }

    #[tokio::test]
    async fn keyboard_here_on_the_target_slot_means_monitor_only() {
        let agent = FakeAgent::here(2);
        let backend = Fake::new("st");
        let (_d, deps) = deps(agent.clone(), backend.clone(), relay());
        let outcome = run_switch(&deps, &Target::Slot(2), Mode::User).await.unwrap();
        assert!(matches!(outcome, Outcome::Monitor { .. }));
        assert!(agent.switches.lock().unwrap().is_empty());
        assert_eq!(backend.switches.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn keyboard_elsewhere_or_agent_down_moves_the_monitor_and_relays() {
        for agent in [FakeAgent::away(), FakeAgent::down()] {
            let backend = Fake::new("st");
            let r = relay();
            let (_d, deps) = deps(agent, backend.clone(), r.clone());
            let outcome = run_switch(&deps, &Target::Machine("avapc".into()), Mode::User).await.unwrap();
            assert!(matches!(outcome, Outcome::Monitor { ref relayed, .. } if relayed.len() == 1));
            assert_eq!(*r.0.lock().unwrap(), vec![3]);
        }
    }

    #[tokio::test]
    async fn a_refused_agent_switch_is_an_error_with_exit_code_one() {
        let (_d, deps) = deps(FakeAgent::here_refusing(1, 3), Fake::new("st"), relay());
        let error = run_switch(&deps, &Target::Slot(3), Mode::User).await.unwrap_err();
        assert!(matches!(error, SwitchError::AgentRefused(_)));
        assert_eq!(error.exit_code(), 1);
    }

    #[tokio::test]
    async fn unresolvable_targets_and_dead_backends_have_their_exit_codes() {
        let (_d, deps) = deps(FakeAgent::away(), Fake::new("st"), relay());
        assert_eq!(run_switch(&deps, &Target::Slot(9), Mode::Hook).await.unwrap_err().exit_code(), 3);
        let (_d2, dead) = deps(FakeAgent::away(), Fake::failing("st"), relay());
        assert_eq!(run_switch(&dead, &Target::Input(Input::Dp), Mode::Hook).await.unwrap_err().exit_code(), 2);
    }

    #[tokio::test]
    async fn arrive_reads_first_rate_limits_and_switches_to_my_input() {
        let backend = Fake::seen("st", Input::Hdmi1);
        let (_d, deps) = deps(FakeAgent::here(1), backend.clone(), relay());
        let fast = ArriveTiming { rate_limit: Duration::from_secs(30), grace: Duration::ZERO };
        assert!(matches!(run_arrive(&deps, fast).await, ArriveOutcome::AlreadyMine));
        assert!(matches!(run_arrive(&deps, fast).await, ArriveOutcome::RateLimited));
        let elsewhere = Fake::seen("st", Input::Hdmi2);
        let (_d2, deps2) = deps(FakeAgent::here(1), elsewhere.clone(), relay());
        assert!(matches!(run_arrive(&deps2, fast).await, ArriveOutcome::Switched));
        assert_eq!(*elsewhere.current.lock().unwrap(), Some(Input::Hdmi1));
    }

    #[test]
    fn hook_mode_is_the_presence_of_the_trigger_variable() {
        temp_env::with_var("OPENLOGI_TRIGGER", Some("key"), || assert_eq!(mode_from_env(), Mode::Hook));
        temp_env::with_var("OPENLOGI_TRIGGER", None::<&str>, || assert_eq!(mode_from_env(), Mode::User));
    }
}
```

(`Fake::new`, `Fake::failing`, `Fake::seen` come from `test_support`; make `Fake.current` `pub(crate)`.)

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test switch`
Expected: compile errors.

- [ ] **Step 4: Implement**

```rust
//! The two switch modes from the spec, `arrive`, and the status report.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::{info, warn};

use crate::config::Config;
use crate::input::Input;
use crate::monitor::{Chain, ChainError, Ensure};
use crate::openlogi::{KeyboardAgent, Probe, SwitchResult};
use crate::resolve::{ResolveError, Target, resolve};
use crate::state::StateStore;

pub struct Deps {
    pub config: Config,
    pub chain: Chain,
    pub agent: Arc<dyn KeyboardAgent>,
    pub state: StateStore,
    pub relay: Arc<dyn Relay>,
}

#[derive(Debug)]
pub struct RelayOutcome {
    pub peer: String,
    pub result: Result<String, String>,
}

#[async_trait]
pub trait Relay: Send + Sync {
    async fn relay(&self, config: &Config, slot: u8) -> Vec<RelayOutcome>;
}

/// Relay nothing (no `[lan]` section, or tests).
pub struct NoRelay;

#[async_trait]
impl Relay for NoRelay {
    async fn relay(&self, _config: &Config, _slot: u8) -> Vec<RelayOutcome> { Vec::new() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Invoked by an OpenLogi hook: monitor only.
    Hook,
    /// Invoked by a person, the Stream Deck, or a peer.
    User,
}

/// Hook mode is exactly "OpenLogi set `OPENLOGI_TRIGGER`".
#[must_use]
pub fn mode_from_env() -> Mode {
    if std::env::var_os("OPENLOGI_TRIGGER").is_some() { Mode::Hook } else { Mode::User }
}

#[derive(Debug)]
pub enum Outcome {
    /// The local agent moved the peripherals; its hook moves the monitor.
    Delegated,
    Monitor { ensure: Ensure, relayed: Vec<RelayOutcome> },
}

#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Monitor(#[from] ChainError),
    #[error("the OpenLogi agent refused the switch: {0}")]
    AgentRefused(String),
}

impl SwitchError {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Resolve(_) => 3,
            Self::Monitor(_) => 2,
            Self::AgentRefused(_) => 1,
        }
    }
}

pub async fn run_switch(deps: &Deps, target: &Target, mode: Mode) -> Result<Outcome, SwitchError> {
    let resolved = resolve(&deps.config, target)?;
    let target_slot = resolved.machine.as_ref().map(|machine| machine.slot);
    let mut keyboard_here = false;
    if mode == Mode::User {
        match deps.agent.probe().await {
            Probe::Here { current_slot, name } => {
                keyboard_here = true;
                if let Some(slot) = target_slot.filter(|slot| *slot != current_slot) {
                    match deps.agent.switch(slot).await {
                        SwitchResult::Switched => {
                            info!(%name, slot, "delegated to the OpenLogi agent; its hook moves the monitor");
                            return Ok(Outcome::Delegated);
                        }
                        SwitchResult::AlreadyCurrent => {}
                        SwitchResult::Refused(message) => return Err(SwitchError::AgentRefused(message)),
                        SwitchResult::NotFound => {
                            warn!("keyboard vanished between probe and switch; moving the monitor only");
                            keyboard_here = false;
                        }
                        SwitchResult::Unavailable(message) => {
                            warn!(%message, "agent unavailable for the switch; moving the monitor only");
                            keyboard_here = false;
                        }
                    }
                }
            }
            Probe::Elsewhere => {}
            Probe::Unavailable(message) => warn!(%message, "OpenLogi unavailable; treating the keyboard as elsewhere"),
        }
    }
    let ensure = deps.chain.ensure(resolved.input).await?;
    let now = Utc::now();
    if let Err(error) = deps.state.update(|state| {
        state.last_target_slot = target_slot;
        state.last_switch_at = Some(now);
        state.last_known_input = Some(resolved.input);
    }) {
        warn!(%error, "could not write state.json");
    }
    let relayed = match (mode, keyboard_here, target_slot) {
        (Mode::User, false, Some(slot)) => deps.relay.relay(&deps.config, slot).await,
        _ => Vec::new(),
    };
    for outcome in &relayed {
        match &outcome.result {
            Ok(result) => info!(peer = %outcome.peer, %result, "relayed"),
            Err(error) => warn!(peer = %outcome.peer, %error, "relay failed"),
        }
    }
    Ok(Outcome::Monitor { ensure, relayed })
}

#[derive(Debug, Clone, Copy)]
pub struct ArriveTiming {
    pub rate_limit: Duration,
    pub grace: Duration,
}

impl Default for ArriveTiming {
    fn default() -> Self {
        Self { rate_limit: Duration::from_secs(30), grace: Duration::from_millis(2500) }
    }
}

#[derive(Debug)]
pub enum ArriveOutcome {
    RateLimited,
    AlreadyMine,
    Switched,
    Failed(ChainError),
}

/// The keyboard just connected here: after a grace period, make sure the
/// monitor shows this machine. Never a second writer on the happy path.
pub async fn run_arrive(deps: &Deps, timing: ArriveTiming) -> ArriveOutcome {
    let now = Utc::now();
    let state = deps.state.load();
    if let Some(last) = state.last_arrive_at {
        if now.signed_duration_since(last) < chrono::Duration::from_std(timing.rate_limit).unwrap_or_default() {
            return ArriveOutcome::RateLimited;
        }
    }
    if let Err(error) = deps.state.update(|state| state.last_arrive_at = Some(now)) {
        warn!(%error, "could not write state.json");
    }
    tokio::time::sleep(timing.grace).await;
    let mine = deps.config.this().input;
    if deps.chain.current().await == Some(mine) {
        return ArriveOutcome::AlreadyMine;
    }
    match deps.chain.switch(mine).await {
        Ok(_) => {
            let _ = deps.state.update(|state| {
                state.last_known_input = Some(mine);
                state.last_switch_at = Some(Utc::now());
            });
            ArriveOutcome::Switched
        }
        Err(error) => ArriveOutcome::Failed(error),
    }
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub machine: String,
    pub input: Option<Input>,
    pub keyboard_here: bool,
    pub current_slot: Option<u8>,
    pub last_target_slot: Option<u8>,
    pub last_switch_at: Option<DateTime<Utc>>,
}

pub async fn status(deps: &Deps) -> StatusReport {
    let input = deps.chain.current().await;
    if input.is_some() {
        let _ = deps.state.update(|state| state.last_known_input = input);
    }
    let (keyboard_here, current_slot) = match deps.agent.probe().await {
        Probe::Here { current_slot, .. } => (true, Some(current_slot)),
        _ => (false, None),
    };
    let state = deps.state.load();
    StatusReport {
        machine: deps.config.this_machine.clone(),
        input,
        keyboard_here,
        current_slot,
        last_target_slot: state.last_target_slot,
        last_switch_at: state.last_switch_at,
    }
}
```

Add `mod switch;` to `main.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo test switch && cargo clippy --all-targets -- -D warnings`
Expected: 8 passed, clean.

- [ ] **Step 6: Commit**

```bash
git add src/switch.rs src/monitor.rs src/main.rs
git commit -m "feat(switch): hook and user mode orchestration, arrive, and status"
```

---

### Task 10: LAN server and relay client

**Files:**
- Create: `src/lan.rs`
- Modify: `Cargo.toml` (add `futures = "0.3"`), `src/main.rs`

**Interfaces:**
- Produces:
  - `pub fn router(deps: Arc<Deps>, secret: String) -> axum::Router` serving `POST /switch` and `GET /status` with bearer auth.
  - `pub async fn serve(listen: &str, router: axum::Router) -> anyhow::Result<()>`.
  - `pub struct HttpRelay { secret: String, client: reqwest::Client }` (`HttpRelay::new(secret)`) implementing `Relay`: POST `http://{address}/switch` `{"slot":N}` to every peer concurrently, 2 s timeout each.
  - Response bodies: `{"result":"delegated","agent":"switched"|"already"}`, `{"result":"ignored"}`, `{"result":"error","detail":"…"}` (agent refused/unavailable), status 401 `{"error":"unauthorized"}`, 400 for an unresolvable slot/machine.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use std::future::IntoFuture as _;

    use super::*;
    use crate::monitor::test_support::Fake;
    use crate::openlogi::{Probe, SwitchResult};
    use crate::switch::NoRelay;

    struct Agent(bool);

    #[async_trait]
    impl KeyboardAgent for Agent {
        async fn probe(&self) -> Probe {
            if self.0 { Probe::Here { current_slot: 1, name: "K".into() } } else { Probe::Elsewhere }
        }
        async fn switch(&self, _slot: u8) -> SwitchResult { SwitchResult::Switched }
    }

    async fn serve_test(here: bool) -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        let deps = Arc::new(Deps {
            config,
            chain: Chain::new(vec![Fake::seen("st", Input::Hdmi1)]),
            agent: Arc::new(Agent(here)),
            state: StateStore::in_dir(dir.path()),
            relay: Arc::new(NoRelay),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router(deps, "s3cret".into())).into_future());
        (format!("http://{addr}"), dir)
    }

    #[tokio::test]
    async fn requests_without_the_bearer_are_rejected() {
        let (base, _d) = serve_test(true).await;
        let response = reqwest::Client::new().post(format!("{base}/switch")).json(&serde_json::json!({"slot": 2})).send().await.unwrap();
        assert_eq!(response.status(), 401);
        let response = reqwest::Client::new().get(format!("{base}/status")).bearer_auth("wrong").send().await.unwrap();
        assert_eq!(response.status(), 401);
    }

    #[tokio::test]
    async fn the_holder_delegates_and_others_ignore() {
        let (holder, _d1) = serve_test(true).await;
        let (other, _d2) = serve_test(false).await;
        let client = reqwest::Client::new();
        let body: serde_json::Value = client.post(format!("{holder}/switch")).bearer_auth("s3cret").json(&serde_json::json!({"slot": 2})).send().await.unwrap().json().await.unwrap();
        assert_eq!(body["result"], "delegated");
        let body: serde_json::Value = client.post(format!("{other}/switch")).bearer_auth("s3cret").json(&serde_json::json!({"machine": "avapc"})).send().await.unwrap().json().await.unwrap();
        assert_eq!(body["result"], "ignored");
        let response = client.post(format!("{other}/switch")).bearer_auth("s3cret").json(&serde_json::json!({"slot": 9})).send().await.unwrap();
        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn status_is_served_as_json() {
        let (base, _d) = serve_test(true).await;
        let body: serde_json::Value = reqwest::Client::new().get(format!("{base}/status")).bearer_auth("s3cret").send().await.unwrap().json().await.unwrap();
        assert_eq!(body["machine"], "mac");
        assert_eq!(body["input"], "HDMI1");
        assert_eq!(body["keyboard_here"], true);
    }

    #[tokio::test]
    async fn the_relay_reports_every_peer_including_dead_ones() {
        let (holder, _d) = serve_test(true).await;
        let mut config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        config.machines[1].address = Some(holder.trim_start_matches("http://").to_string());
        config.machines[2].address = Some("127.0.0.1:1".into());
        let outcomes = HttpRelay::new("s3cret".into()).relay(&config, 2).await;
        assert_eq!(outcomes.len(), 2);
        let by_peer: std::collections::BTreeMap<_, _> = outcomes.into_iter().map(|o| (o.peer, o.result)).collect();
        assert_eq!(by_peer["avapc"].as_deref(), Ok("delegated"));
        assert!(by_peer["oldwaterboy"].is_err());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo add futures@0.3 && cargo test lan`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Peer-to-peer relay: an authenticated HTTP endpoint every daemon serves, and
//! the client that asks peers to move the keyboard they hold. Never relays a
//! relayed request onward.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tracing::info;

use crate::config::Config;
use crate::openlogi::{KeyboardAgent, Probe, SwitchResult};
use crate::resolve::{Target, resolve};
use crate::switch::{Deps, Relay, RelayOutcome, status};

struct Shared {
    deps: Arc<Deps>,
    secret: String,
}

#[derive(Deserialize)]
struct SwitchRequest {
    #[serde(default)]
    slot: Option<u8>,
    #[serde(default)]
    machine: Option<String>,
}

pub fn router(deps: Arc<Deps>, secret: String) -> Router {
    let shared = Arc::new(Shared { deps, secret });
    Router::new()
        .route("/switch", post(switch_handler))
        .route("/status", get(status_handler))
        .with_state(shared)
}

pub async fn serve(listen: &str, router: Router) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    info!(%listen, "LAN listener up");
    axum::serve(listener, router).await?;
    Ok(())
}

fn authorized(shared: &Shared, headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| token == shared.secret)
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"}))).into_response()
}

async fn switch_handler(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(request): Json<SwitchRequest>,
) -> Response {
    if !authorized(&shared, &headers) {
        return unauthorized();
    }
    let target = match (request.slot, request.machine) {
        (Some(slot), _) => Target::Slot(slot),
        (None, Some(machine)) => Target::Machine(machine),
        (None, None) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "slot or machine required"}))).into_response(),
    };
    let slot = match resolve(&shared.deps.config, &target) {
        Ok(resolved) => match resolved.machine {
            Some(machine) => machine.slot,
            None => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "target is not a machine"}))).into_response(),
        },
        Err(error) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": error.to_string()}))).into_response(),
    };
    let agent: &dyn KeyboardAgent = shared.deps.agent.as_ref();
    let body = match agent.probe().await {
        Probe::Here { .. } => match agent.switch(slot).await {
            SwitchResult::Switched => serde_json::json!({"result": "delegated", "agent": "switched"}),
            SwitchResult::AlreadyCurrent => serde_json::json!({"result": "delegated", "agent": "already"}),
            SwitchResult::Refused(detail) | SwitchResult::Unavailable(detail) => serde_json::json!({"result": "error", "detail": detail}),
            SwitchResult::NotFound => serde_json::json!({"result": "ignored"}),
        },
        Probe::Elsewhere | Probe::Unavailable(_) => serde_json::json!({"result": "ignored"}),
    };
    info!(slot, result = %body["result"], "relayed switch handled");
    Json(body).into_response()
}

async fn status_handler(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !authorized(&shared, &headers) {
        return unauthorized();
    }
    Json(status(&shared.deps).await).into_response()
}

pub struct HttpRelay {
    secret: String,
    client: reqwest::Client,
}

impl HttpRelay {
    #[must_use]
    pub fn new(secret: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_default();
        Self { secret, client }
    }
}

#[async_trait]
impl Relay for HttpRelay {
    async fn relay(&self, config: &Config, slot: u8) -> Vec<RelayOutcome> {
        let requests = config.peers().into_iter().filter_map(|peer| {
            let address = peer.address.clone()?;
            let name = peer.name.clone();
            let client = self.client.clone();
            let secret = self.secret.clone();
            Some(async move {
                let result = async {
                    let response = client
                        .post(format!("http://{address}/switch"))
                        .bearer_auth(secret)
                        .json(&serde_json::json!({"slot": slot}))
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    let status = response.status();
                    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
                    if !status.is_success() {
                        return Err(format!("{status}: {body}"));
                    }
                    Ok(body["result"].as_str().unwrap_or("unknown").to_string())
                }
                .await;
                RelayOutcome { peer: name, result }
            })
        });
        futures::future::join_all(requests).await
    }
}
```

Add `mod lan;` to `main.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test lan && cargo clippy --all-targets -- -D warnings`
Expected: 4 passed, clean.

- [ ] **Step 5: Commit**

```bash
git add src/lan.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat(lan): authenticated switch/status endpoint and peer relay"
```

---

### Task 11: CLI commands and the end-to-end test

**Files:**
- Modify: `src/cli.rs`, `src/main.rs`
- Create: `tests/cli.rs`

**Interfaces:**
- Produces the user-facing commands: `switch (--slot N | --to NAME | --input X)`, `arrive`, `status [--json]`, `config check`, `smartthings login --client-id ID [--client-secret SECRET] [--no-browser]` (secret prompted on stdin when omitted), `smartthings devices`, `lan set-secret [SECRET]` (prompted when omitted), `daemon` (Task 12 fills in; parses now).
- `fn build_deps(config: Config, dir: &Path) -> anyhow::Result<Deps>` in `main.rs`: secrets from `config.secrets.store`; chain from `config.monitor.backends` (`smartthings` → `SmartThings::new(cfg, Arc::new(TokenManager::new(cfg, secrets)))`); agent `OpenLogi::new(config.openlogi.binary)`; relay `HttpRelay` when `[lan]` and a stored `LAN_SECRET` exist, else `NoRelay`.
- `config check` additionally calls `SmartThings::supported_inputs()` when SmartThings is configured and fails with exit 1 naming any configured input the Ark does not report.

- [ ] **Step 1: Write the failing end-to-end test**

`tests/cli.rs`:

```rust
#![cfg(unix)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openlogi.sh")
}

fn write_config(dir: &Path, server: &MockServer, inputs: &[&str]) {
    let machines = inputs
        .iter()
        .zip([("mac", 1u8), ("avapc", 3), ("oldwaterboy", 2)])
        .map(|(input, (name, slot))| format!("[[machines]]\nname = \"{name}\"\nslot = {slot}\ninput = \"{input}\"\n"))
        .collect::<String>();
    let config = format!(
        "schema_version = 1\nthis_machine = \"mac\"\n{machines}\n[monitor]\nbackends = [\"smartthings\"]\n\n[monitor.smartthings]\ndevice_id = \"dev-1\"\nclient_id = \"client\"\nbase_url = \"{base}\"\ntoken_url = \"{base}/oauth/token\"\n\n[openlogi]\nbinary = \"{bin}\"\n\n[secrets]\nstore = \"file\"\n",
        base = server.uri(),
        bin = fixture().display(),
    );
    std::fs::write(dir.join("config.toml"), config).unwrap();
    let tokens = serde_json::json!({"access_token": "tok", "refresh_token": "r", "expires_at": "2099-01-01T00:00:00Z"});
    std::fs::write(dir.join("secrets.json"), serde_json::json!({"smartthings.tokens": tokens.to_string()}).to_string()).unwrap();
}

async fn ark(server: &MockServer, current: &str) {
    Mock::given(method("GET"))
        .and(path("/devices/dev-1/components/main/capabilities/samsungvd.mediaInputSource/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "inputSource": {"value": current},
            "supportedInputSourcesMap": {"value": [{"id":"HDMI1","name":"a"},{"id":"HDMI2","name":"b"},{"id":"HDMI3","name":"c"}]}
        })))
        .mount(server).await;
    Mock::given(method("POST")).and(path("/devices/dev-1/commands"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(server).await;
}

fn deskswitch(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("deskswitch").unwrap();
    cmd.env("DESKSWITCH_CONFIG_DIR", dir).env_remove("OPENLOGI_TRIGGER");
    cmd
}

async fn commands_sent(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().iter().filter(|r| r.method == "POST").count()
}

#[tokio::test]
async fn user_mode_with_the_keyboard_here_delegates_to_openlogi() {
    let server = MockServer::start().await;
    ark(&server, "HDMI1").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "HDMI3"]);
    let log = dir.path().join("switches.log");
    deskswitch(dir.path())
        .env("FAKE_OPENLOGI_HERE", "1").env("FAKE_OPENLOGI_SLOT", "1").env("FAKE_OPENLOGI_LOG", &log)
        .args(["switch", "--to", "avapc"])
        .assert().success().stdout(predicate::str::contains("delegated"));
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "3");
    assert_eq!(commands_sent(&server).await, 0, "the hook, not this call, moves the monitor");
}

#[tokio::test]
async fn hook_mode_moves_only_the_monitor() {
    let server = MockServer::start().await;
    ark(&server, "HDMI1").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "HDMI3"]);
    let log = dir.path().join("switches.log");
    deskswitch(dir.path())
        .env("OPENLOGI_TRIGGER", "key").env("FAKE_OPENLOGI_HERE", "1").env("FAKE_OPENLOGI_LOG", &log)
        .args(["switch", "--slot", "2"])
        .assert().success().stdout(predicate::str::contains("HDMI3"));
    assert!(!log.exists(), "hook mode never calls openlogi");
    assert_eq!(commands_sent(&server).await, 1);
    let state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(state["last_known_input"], "HDMI3");
}

#[tokio::test]
async fn keyboard_elsewhere_moves_the_monitor_and_already_there_is_a_noop() {
    let server = MockServer::start().await;
    ark(&server, "HDMI2").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "HDMI3"]);
    deskswitch(dir.path()).env("FAKE_OPENLOGI_HERE", "0").args(["switch", "--input", "HDMI2"])
        .assert().success().stdout(predicate::str::contains("already"));
    assert_eq!(commands_sent(&server).await, 0);
    deskswitch(dir.path()).env("FAKE_OPENLOGI_HERE", "0").args(["switch", "--slot", "1"])
        .assert().success();
    assert_eq!(commands_sent(&server).await, 1);
}

#[tokio::test]
async fn exit_codes_and_config_check() {
    let server = MockServer::start().await;
    ark(&server, "HDMI1").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "DP"]);
    deskswitch(dir.path()).args(["switch", "--slot", "9"]).assert().code(3);
    deskswitch(dir.path()).args(["config", "check"]).assert().code(1)
        .stderr(predicate::str::contains("DP"));
    deskswitch(dir.path()).args(["status", "--json"]).assert().success()
        .stdout(predicate::str::contains("\"input\":\"HDMI1\""));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test cli`
Expected: failures — the `switch` subcommand does not exist yet.

- [ ] **Step 3: Implement the CLI types**

Replace `src/cli.rs`:

```rust
//! Command-line surface. Behaviour lives in the modules the commands call.

use clap::{Args, Parser, Subcommand};

use crate::input::Input;

#[derive(Debug, Parser)]
#[command(name = "deskswitch", version, about = "Follow the keyboard across machines")]
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
    /// SmartThings account setup.
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
    /// Easy-Switch slot 1–3.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=3))]
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
```

`Input` needs `clap::ValueEnum`-free parsing: clap uses `FromStr` for `Option<Input>` automatically because `Input: FromStr + Clone + Send + Sync + 'static` — ensure `UnknownInput: std::error::Error + Send + Sync` (it is, via thiserror).

- [ ] **Step 4: Implement `main.rs`**

```rust
//! `deskswitch` — follow the keyboard: monitor input plus Logitech Easy-Switch.

mod cli;
mod config;
mod input;
mod lan;
mod monitor;
mod openlogi;
mod resolve;
mod secrets;
mod smartthings;
mod state;
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
use crate::switch::{ArriveOutcome, ArriveTiming, Deps, Mode, Outcome, mode_from_env};

/// stderr always; a daily-rotated `deskswitch.log` in `dir` when given (7 kept).
fn init_logging(dir: Option<&Path>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = fmt::layer().with_writer(std::io::stderr).with_target(false);
    let Some(dir) = dir else {
        tracing_subscriber::registry().with(filter).with(stderr).init();
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
                let cfg = config.monitor.smartthings.as_ref().context("[monitor.smartthings] missing")?;
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
        agent: Arc::new(openlogi::OpenLogi::new(config.openlogi.binary.clone().into())),
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

async fn run(cli: cli::Cli, dir: &Path) -> anyhow::Result<u8> {
    match cli.command {
        cli::Command::Config { action: cli::ConfigAction::Check } => {
            let config = Config::load(dir)?;
            println!("config ok: {}", dir.join(config::FILE_NAME).display());
            for machine in &config.machines {
                let me = if machine.name == config.this_machine { " (this machine)" } else { "" };
                println!("  slot {} → {} → {}{me}", machine.slot, machine.name, machine.input);
            }
            if let Some(st) = &config.monitor.smartthings {
                let secrets = secrets::open_store(config.secrets.store, dir);
                let tokens = Arc::new(smartthings::auth::TokenManager::new(st, secrets));
                let supported = smartthings::SmartThings::new(st, tokens)?.supported_inputs().await?;
                let unsupported: Vec<String> = config.machines.iter()
                    .filter(|m| !supported.iter().any(|s| s == m.input.as_str()))
                    .map(|m| format!("{} ({})", m.input, m.name))
                    .collect();
                if !unsupported.is_empty() {
                    eprintln!("the Ark does not report these configured inputs: {}; it reports {}", unsupported.join(", "), supported.join(", "));
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
                Ok(Outcome::Delegated) => { println!("delegated to the OpenLogi agent"); Ok(0) }
                Ok(Outcome::Monitor { ensure, relayed }) => {
                    let input = resolve::resolve(&deps.config, &target).map(|r| r.input.to_string()).unwrap_or_default();
                    match ensure {
                        monitor::Ensure::AlreadyThere => println!("monitor already on {input}"),
                        monitor::Ensure::Switched { backend } => println!("monitor → {input} via {backend}"),
                    }
                    for outcome in relayed {
                        match outcome.result {
                            Ok(result) => println!("peer {}: {result}", outcome.peer),
                            Err(error) => println!("peer {}: failed ({error})", outcome.peer),
                        }
                    }
                    Ok(0)
                }
                Err(error) => { eprintln!("{error}"); Ok(error.exit_code()) }
            }
        }
        cli::Command::Arrive => {
            let deps = build_deps(Config::load(dir)?, dir)?;
            match switch::run_arrive(&deps, ArriveTiming::default()).await {
                ArriveOutcome::RateLimited => { println!("arrive: rate limited"); Ok(0) }
                ArriveOutcome::AlreadyMine => { println!("arrive: monitor already here"); Ok(0) }
                ArriveOutcome::Switched => { println!("arrive: monitor switched here"); Ok(0) }
                ArriveOutcome::Failed(error) => { eprintln!("arrive: {error}"); Ok(2) }
            }
        }
        cli::Command::Status { json } => {
            let deps = build_deps(Config::load(dir)?, dir)?;
            let report = switch::status(&deps).await;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("machine: {}", report.machine);
                println!("input: {}", report.input.map_or("unknown".to_string(), |i| i.to_string()));
                println!("keyboard here: {}{}", report.keyboard_here, report.current_slot.map_or(String::new(), |s| format!(" (slot {s})")));
            }
            Ok(0)
        }
        cli::Command::Daemon => {
            eprintln!("daemon: implemented in the next task");
            Ok(1)
        }
        cli::Command::Smartthings { action } => {
            let config = Config::load(dir)?;
            let st = config.monitor.smartthings.as_ref().context("[monitor.smartthings] missing")?;
            let secrets = secrets::open_store(config.secrets.store, dir);
            match action {
                cli::SmartThingsAction::Login { client_id, client_secret, no_browser } => {
                    anyhow::ensure!(client_id == st.client_id, "--client-id does not match [monitor.smartthings].client_id");
                    let secret = match client_secret { Some(s) => s, None => prompt("client secret")? };
                    smartthings::auth::login(st, secrets.as_ref(), &secret, !no_browser).await?;
                    println!("SmartThings login stored");
                    Ok(0)
                }
                cli::SmartThingsAction::Devices => {
                    let tokens = Arc::new(smartthings::auth::TokenManager::new(st, secrets));
                    for (id, label) in smartthings::SmartThings::new(st, tokens)?.list_devices().await? {
                        println!("{id}  {label}");
                    }
                    Ok(0)
                }
            }
        }
        cli::Command::Lan { action: cli::LanAction::SetSecret { secret } } => {
            let config = Config::load(dir)?;
            let secrets = secrets::open_store(config.secrets.store, dir);
            let secret = match secret { Some(s) => s, None => prompt("shared secret")? };
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
```

- [ ] **Step 5: Run everything**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all unit tests and the 4 end-to-end tests pass, clean.

- [ ] **Step 6: Commit**

```bash
git add src tests
git commit -m "feat(cli): switch, arrive, status, config check, smartthings and lan setup"
```

---

### Task 12: `daemon`: LAN listener and 30-second refresh

**Files:**
- Create: `src/daemon.rs`
- Modify: `src/main.rs` (`mod daemon;` and the `Daemon` arm)

**Interfaces:**
- Produces:
  - `pub struct DeckView { pub buttons: Vec<DeckButton>, pub error_until: Option<std::time::Instant> }`, `pub struct DeckButton { pub machine: String, pub lit: bool }`; `pub fn view_for(config: &Config, input: Option<Input>, error_until: Option<Instant>) -> DeckView` (pure: a button is lit when the machine's input equals `input`).
  - `pub async fn refresh_once(deps: &Deps, view: &tokio::sync::watch::Sender<DeckView>, error_until: Option<Instant>)` — reads the input through `switch::status` and publishes a view.
  - `pub async fn run(deps: Arc<Deps>, lan_secret: Option<String>) -> anyhow::Result<()>` — serves the LAN router when `[lan]` and a secret exist (logs a warning otherwise), refreshes every 30 s and after every deck-triggered switch, and, with the `streamdeck` feature, drives the deck (Task 13). Runs until Ctrl-C.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::test_support::Fake;
    use crate::openlogi::{KeyboardAgent, Probe, SwitchResult};
    use crate::switch::NoRelay;

    struct Away;
    #[async_trait::async_trait]
    impl KeyboardAgent for Away {
        async fn probe(&self) -> Probe { Probe::Elsewhere }
        async fn switch(&self, _slot: u8) -> SwitchResult { SwitchResult::NotFound }
    }

    #[test]
    fn the_lit_button_is_the_machine_on_the_current_input() {
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        let view = view_for(&config, Some(Input::Hdmi2), None);
        let lit: Vec<&str> = view.buttons.iter().filter(|b| b.lit).map(|b| b.machine.as_str()).collect();
        assert_eq!(lit, ["avapc"]);
        assert_eq!(view.buttons.iter().map(|b| b.machine.as_str()).collect::<Vec<_>>(), ["mac", "avapc", "oldwaterboy"], "config order");
        assert!(view_for(&config, None, None).buttons.iter().all(|b| !b.lit));
    }

    #[tokio::test]
    async fn refresh_publishes_the_input_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        let deps = Deps {
            config,
            chain: Chain::new(vec![Fake::seen("st", Input::Hdmi3)]),
            agent: Arc::new(Away),
            state: crate::state::StateStore::in_dir(dir.path()),
            relay: Arc::new(NoRelay),
        };
        let (tx, rx) = tokio::sync::watch::channel(DeckView::default());
        refresh_once(&deps, &tx, None).await;
        let lit: Vec<String> = rx.borrow().buttons.iter().filter(|b| b.lit).map(|b| b.machine.clone()).collect();
        assert_eq!(lit, ["oldwaterboy"]);
        assert_eq!(deps.state.load().last_known_input, Some(Input::Hdmi3));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test daemon`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! Long-running mode: LAN listener, periodic input refresh, and the deck.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::Config;
use crate::input::Input;
use crate::switch::{Deps, status};

const REFRESH_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeckButton {
    pub machine: String,
    pub lit: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DeckView {
    pub buttons: Vec<DeckButton>,
    pub error_until: Option<Instant>,
}

/// One button per `[streamdeck].buttons` entry (or every machine when that
/// list is empty), lit when the machine owns the current input.
#[must_use]
pub fn view_for(config: &Config, input: Option<Input>, error_until: Option<Instant>) -> DeckView {
    let names: Vec<&str> = if config.streamdeck.buttons.is_empty() {
        config.machines.iter().map(|m| m.name.as_str()).collect()
    } else {
        config.streamdeck.buttons.iter().map(String::as_str).collect()
    };
    let buttons = names
        .into_iter()
        .filter_map(|name| config.machine_named(name))
        .map(|machine| DeckButton { machine: machine.name.clone(), lit: input == Some(machine.input) })
        .collect();
    DeckView { buttons, error_until }
}

pub async fn refresh_once(deps: &Deps, view: &watch::Sender<DeckView>, error_until: Option<Instant>) {
    let report = status(deps).await;
    let _ = view.send(view_for(&deps.config, report.input, error_until));
}

pub async fn run(deps: Arc<Deps>, lan_secret: Option<String>) -> anyhow::Result<()> {
    let (view_tx, view_rx) = watch::channel(DeckView::default());
    match (&deps.config.lan, lan_secret) {
        (Some(lan), Some(secret)) => {
            let router = crate::lan::router(Arc::clone(&deps), secret);
            let listen = lan.listen.clone();
            tokio::spawn(async move {
                if let Err(error) = crate::lan::serve(&listen, router).await {
                    warn!(%error, "LAN listener stopped");
                }
            });
        }
        (Some(_), None) => warn!("[lan] configured but no secret stored; run `deskswitch lan set-secret`"),
        (None, _) => info!("no [lan] section; relayed switches disabled"),
    }
    #[cfg(feature = "streamdeck")]
    let mut deck_events = if deps.config.streamdeck.enabled {
        crate::streamdeck::spawn(view_rx.clone())
    } else {
        None
    };
    refresh_once(&deps, &view_tx, None).await;
    let mut ticker = tokio::time::interval(REFRESH_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        #[cfg(feature = "streamdeck")]
        let deck_event = async {
            match deck_events.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        #[cfg(not(feature = "streamdeck"))]
        let deck_event = std::future::pending::<Option<usize>>();
        tokio::select! {
            _ = ticker.tick() => refresh_once(&deps, &view_tx, None).await,
            Some(index) = deck_event => {
                let machine = view_rx.borrow().buttons.get(index).map(|b| b.machine.clone());
                let Some(machine) = machine else { continue };
                let target = crate::resolve::Target::Machine(machine.clone());
                let error_until = match crate::switch::run_switch(&deps, &target, crate::switch::Mode::User).await {
                    Ok(_) => None,
                    Err(error) => {
                        warn!(%machine, %error, "deck switch failed");
                        Some(Instant::now() + Duration::from_secs(1))
                    }
                };
                refresh_once(&deps, &view_tx, error_until).await;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("daemon stopping");
                return Ok(());
            }
        }
    }
}
```

`crate::streamdeck::spawn(view: watch::Receiver<DeckView>) -> Option<tokio::sync::mpsc::Receiver<usize>>` is defined in Task 13; until then, build without the feature. Replace the `Daemon` arm in `main.rs`:

```rust
        cli::Command::Daemon => {
            let config = Config::load(dir)?;
            let secrets = secrets::open_store(config.secrets.store, dir);
            let lan_secret = secrets.get(secrets::LAN_SECRET)?;
            let deps = Arc::new(build_deps(config, dir)?);
            daemon::run(deps, lan_secret).await?;
            Ok(0)
        }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test daemon && cargo clippy --all-targets -- -D warnings`
Expected: 2 passed, clean.

- [ ] **Step 5: Smoke the daemon against the fixture**

```bash
DESKSWITCH_CONFIG_DIR=/tmp/dsw FAKE_OPENLOGI_HERE=1 cargo run -- daemon
```
(with `/tmp/dsw/config.toml` from Task 2 plus `[secrets] store = "file"`, `[lan] listen = "127.0.0.1:7391"`, `[openlogi] binary = "<repo>/tests/fixtures/openlogi.sh"`, and `deskswitch lan set-secret test` run first). Expected: "LAN listener up"; `curl -H 'Authorization: Bearer test' localhost:7391/status` returns JSON; Ctrl-C stops it.

- [ ] **Step 6: Commit**

```bash
git add src/daemon.rs src/main.rs
git commit -m "feat(daemon): LAN listener and periodic refresh"
```

---

### Task 13: Stream Deck Plus driver (feature `streamdeck`)

**Files:**
- Create: `src/streamdeck.rs`, `assets/fonts/DejaVuSans.ttf`, `assets/fonts/LICENSE-DejaVu.txt`
- Modify: `src/main.rs` (`#[cfg(feature = "streamdeck")] mod streamdeck;`)

**Interfaces:**
- Produces: `pub fn spawn(view: watch::Receiver<DeckView>) -> Option<tokio::sync::mpsc::Receiver<usize>>` — connects to the first Stream Deck (preferring `Kind::Plus`), starts a blocking thread that repaints on every view change and emits the index of each pressed key; `None` when no deck is found (logged). Pure `pub fn render(label: &str, lit: bool, error: bool, size: (u32, u32)) -> image::DynamicImage`.

- [ ] **Step 1: Fetch the font**

```bash
mkdir -p assets/fonts
curl -L -o assets/fonts/DejaVuSans.ttf https://github.com/dejavu-fonts/dejavu-fonts/raw/master/ttf/DejaVuSans.ttf
curl -L -o assets/fonts/LICENSE-DejaVu.txt https://github.com/dejavu-fonts/dejavu-fonts/raw/master/LICENSE
ls -la assets/fonts
```
Expected: a ~750 KB TTF and the license text.

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders_the_requested_size_and_distinct_states() {
        let lit = render("avapc", true, false, (120, 120));
        let dark = render("avapc", false, false, (120, 120));
        let error = render("avapc", false, true, (120, 120));
        assert_eq!((lit.width(), lit.height()), (120, 120));
        let px = |img: &image::DynamicImage| img.to_rgb8().get_pixel(4, 4).0;
        assert_ne!(px(&lit), px(&dark));
        assert_ne!(px(&error), px(&dark));
    }

    #[test]
    fn long_labels_are_truncated_not_overflowed() {
        let img = render("a-very-long-machine-name-indeed", false, false, (120, 120));
        assert_eq!(img.width(), 120);
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --features streamdeck streamdeck`
Expected: compile errors.

- [ ] **Step 4: Implement**

```rust
//! Stream Deck Plus: one key per machine, lit when that machine is on screen.
//! Blocking HID on its own thread; the daemon talks to it through channels.

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use ab_glyph::{FontRef, PxScale};
use elgato_streamdeck::{StreamDeck, StreamDeckInput, info::Kind, new_hidapi};
use image::{DynamicImage, Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::daemon::DeckView;

const FONT: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");
const LIT: Rgb<u8> = Rgb([40, 200, 90]);
const DARK: Rgb<u8> = Rgb([28, 28, 32]);
const ERROR: Rgb<u8> = Rgb([200, 40, 40]);
const TEXT: Rgb<u8> = Rgb([245, 245, 245]);

/// A key image: background by state, the label centred, truncated to fit.
#[must_use]
pub fn render(label: &str, lit: bool, error: bool, size: (u32, u32)) -> DynamicImage {
    let background = if error { ERROR } else if lit { LIT } else { DARK };
    let mut img = RgbImage::from_pixel(size.0, size.1, background);
    let font = FontRef::try_from_slice(FONT).ok();
    if let Some(font) = font {
        let max_chars = usize::try_from(size.0 / 11).unwrap_or(10).max(3);
        let text: String = if label.chars().count() > max_chars {
            label.chars().take(max_chars - 1).chain(std::iter::once('…')).collect()
        } else {
            label.to_string()
        };
        let scale = PxScale::from(size.1 as f32 / 5.0);
        let x = i32::try_from(size.0 / 12).unwrap_or(4);
        let y = i32::try_from(size.1 / 2 - size.1 / 10).unwrap_or(40);
        draw_text_mut(&mut img, TEXT, x, y, scale, &font, &text);
    }
    DynamicImage::ImageRgb8(img)
}

/// Connect and start the deck thread. Emits pressed key indexes.
pub fn spawn(mut view: watch::Receiver<DeckView>) -> Option<mpsc::Receiver<usize>> {
    let hid = new_hidapi().ok()?;
    let mut devices = StreamDeck::list_devices(&hid);
    devices.sort_by_key(|(kind, _)| if *kind == Kind::Plus { 0 } else { 1 });
    let Some((kind, serial)) = devices.into_iter().next() else {
        warn!("no Stream Deck found; is the Elgato app holding it?");
        return None;
    };
    let deck = match StreamDeck::connect(&hid, kind, &serial) {
        Ok(deck) => deck,
        Err(error) => {
            warn!(%error, "could not open the Stream Deck");
            return None;
        }
    };
    info!(?kind, %serial, "Stream Deck connected");
    let (tx, rx) = mpsc::channel(8);
    let (repaint_tx, repaint_rx) = std_mpsc::channel::<DeckView>();
    // Bridge the async watch into a std channel the blocking thread can poll.
    tokio::spawn(async move {
        loop {
            let current = view.borrow_and_update().clone();
            if repaint_tx.send(current).is_err() {
                return;
            }
            if view.changed().await.is_err() {
                return;
            }
        }
    });
    std::thread::Builder::new()
        .name("deskswitch-streamdeck".into())
        .spawn(move || deck_loop(&deck, &repaint_rx, &tx))
        .ok()?;
    Some(rx)
}

fn deck_loop(deck: &StreamDeck, repaint: &std_mpsc::Receiver<DeckView>, presses: &mpsc::Sender<usize>) {
    let kind = deck.kind();
    let format = kind.key_image_format();
    let size = (u32::try_from(format.size.0).unwrap_or(120), u32::try_from(format.size.1).unwrap_or(120));
    let key_count = usize::from(kind.key_count());
    let mut last_view = DeckView::default();
    let mut was_down = vec![false; key_count];
    let _ = deck.set_brightness(60);
    loop {
        while let Ok(view) = repaint.try_recv() {
            last_view = view;
            let error = last_view.error_until.is_some_and(|until| until > std::time::Instant::now());
            for key in 0..key_count {
                let image = match last_view.buttons.get(key) {
                    Some(button) => render(&button.machine, button.lit, error && button.lit, size),
                    None => render("", false, false, size),
                };
                let _ = deck.set_button_image(u8::try_from(key).unwrap_or(0), image);
            }
            let _ = deck.flush();
        }
        match deck.read_input(Some(Duration::from_millis(100))) {
            Ok(StreamDeckInput::ButtonStateChange(states)) => {
                for (key, down) in states.iter().enumerate().take(key_count) {
                    if *down && !was_down[key] && presses.blocking_send(key).is_err() {
                        return;
                    }
                    was_down[key] = *down;
                }
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "Stream Deck read failed; stopping deck thread");
                return;
            }
        }
    }
}
```

If `key_image_format().size` is not the field name in 0.13, open `~/.cargo/registry/src/*/elgato-streamdeck-0.13.*/src/info.rs` and use the `ImageFormat` field that carries `(width, height)`; the rest is unchanged. Add to `main.rs`: `#[cfg(feature = "streamdeck")] mod streamdeck;`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --features streamdeck && cargo clippy --features streamdeck --all-targets -- -D warnings`
Expected: pass, clean (a few `cast_precision_loss` pedantic warnings in `render` are acceptable to fix with explicit `f32::from` conversions rather than suppressions).

- [ ] **Step 6: Try it on the real deck**

Quit the Elgato Stream Deck app (menu bar icon → Quit), then:

```bash
DESKSWITCH_CONFIG_DIR=/tmp/dsw FAKE_OPENLOGI_HERE=0 cargo run --features streamdeck -- daemon
```
Expected: three labelled keys, the one matching the mock/current input lit; pressing a key logs a switch attempt.

- [ ] **Step 7: Commit**

```bash
git add src/streamdeck.rs src/main.rs assets
git commit -m "feat(streamdeck): drive the Stream Deck Plus with one key per machine"
```

---

### Task 14: Install pieces and README

**Files:**
- Create: `deploy/com.deskswitch.daemon.plist`, `deploy/windows-task.md`, `README.md`

- [ ] **Step 1: LaunchAgent**

`deploy/com.deskswitch.daemon.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.deskswitch.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/deskswitch</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/deskswitch.out.log</string>
  <key>StandardErrorPath</key><string>/tmp/deskswitch.err.log</string>
</dict>
</plist>
```

Install commands (documented in the README):

```bash
cargo build --release --features streamdeck
sudo cp target/release/deskswitch /usr/local/bin/deskswitch
cp deploy/com.deskswitch.daemon.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.deskswitch.daemon.plist
```

- [ ] **Step 2: Windows task**

`deploy/windows-task.md`:

```markdown
# deskswitch on Windows

1. Build on the PC (or cross-compile): `cargo build --release` (no `streamdeck` feature).
2. Copy `target\release\deskswitch.exe` to `C:\Program Files\deskswitch\deskswitch.exe`.
3. Config lives at `%APPDATA%\deskswitch\config.toml`; run `deskswitch config check`.
4. Store the LAN secret: `deskswitch lan set-secret`.
5. Register the daemon at logon:

   schtasks /Create /TN deskswitch /SC ONLOGON /RL LIMITED /TR "\"C:\Program Files\deskswitch\deskswitch.exe\" daemon"

6. OpenLogi hooks on this machine use the absolute path:
   `C:\Program Files\deskswitch\deskswitch.exe switch --slot N`.
```

- [ ] **Step 3: README**

`README.md` with: what it does (two sentences), the config example from the spec §4.2 verbatim, the OpenLogi `host_switch_actions` snippet from the spec §4.1 with absolute paths, the SmartThings one-time setup (`smartthings apps:create` → OAuth-In, scopes `r:devices:*` `x:devices:*`, redirect `http://localhost:8765/callback`; then `deskswitch smartthings login --client-id … ; deskswitch smartthings devices`; put the id in `device_id`; `deskswitch config check`), the install commands above, and the exit codes table from the spec §7.2.

- [ ] **Step 4: Commit**

```bash
git add deploy README.md
git commit -m "docs: install pieces for macOS and Windows and a README"
```

---

### Task 15: Rollout verification (Michael, with the plan's stages 3, 5, 6)

- [ ] **Stage 3 — SmartThings from the Mac.** Install per Task 14. Create the OAuth app with the SmartThings CLI, run `deskswitch smartthings login`, `deskswitch smartthings devices`, fill `device_id`, run `deskswitch config check` (expect "SmartThings reports inputs: HDMI1, HDMI2, HDMI3"). Then `deskswitch switch --input HDMI2` from a terminal: the Ark flips to AVAPC within ~3 s; `deskswitch status` shows `input: HDMI2`. Switch back with `--input HDMI1`.

- [ ] **Stage 4 — with OpenLogi PR 1 installed.** Put the `host_switch_actions` table in the Mac's OpenLogi config with `/usr/local/bin/deskswitch switch --slot N` and `arrive`. Press 2: mouse and keyboard hop, Ark → HDMI3 within ~3 s. Press 1 from OLDWATERBOY's keyboard session: everything comes back (arrive is the fallback while OLDWATERBOY has no agent yet).

- [ ] **Stage 5 — both Windows machines.** Install OpenLogi (Windows build with PR 1) and `deskswitch` per `deploy/windows-task.md`; same config with `this_machine` set per box; `deskswitch lan set-secret` with the same secret everywhere; `deskswitch config check` on each. Test all six directions from the keyboard. Then sleep OLDWATERBOY, press 3 on its keyboard session: AVAPC's `arrive` moves the Ark after ~3 s; the mouse stays until its own button is pressed (documented).

- [ ] **Stage 6 — Stream Deck and relay (needs OpenLogi PR 2 on every machine).** Quit the Elgato app, start the daemon on the Mac. While AVAPC holds the keyboard, press the OLDWATERBOY key on the deck: the Ark flips immediately and, within ~2 s, AVAPC's daemon reports `delegated` in the Mac's log and the peripherals hop. Unplug AVAPC's network and press again: the Ark still flips; the log shows the relay failure; the key does not flash red (monitor succeeded). Break the SmartThings token (delete it from the keychain) and press a key: red flash for a second, error in the log.

- [ ] **Record** what happened, including anything that did not work, in `docs/rollout-log.md`.
