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
    if cfg!(windows)
        && let Ok(appdata) = std::env::var("APPDATA")
    {
        return PathBuf::from(appdata).join("deskswitch");
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

fn default_base_url() -> String {
    "https://api.smartthings.com/v1".into()
}
fn default_token_url() -> String {
    "https://auth-global.api.smartthings.com/oauth/token".into()
}
fn default_authorize_url() -> String {
    "https://api.smartthings.com/oauth/authorize".into()
}
fn default_redirect_uri() -> String {
    "http://localhost:8765/callback".into()
}

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
    /// Path to a `.ttf`/`.otf` used to draw key labels. When unset, the
    /// driver falls back to a per-OS system font and, failing that, draws
    /// keys without text.
    #[serde(default)]
    pub font: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenLogiConfig {
    /// The `openlogi` executable; absolute in production, a fixture in tests.
    #[serde(default = "default_openlogi_binary")]
    pub binary: String,
}

fn default_openlogi_binary() -> String {
    "openlogi".into()
}

impl Default for OpenLogiConfig {
    fn default() -> Self {
        Self {
            binary: default_openlogi_binary(),
        }
    }
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
    #[error("could not read {0}: {1}")]
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
    #[allow(dead_code)]
    pub fn this(&self) -> &Machine {
        self.machine_named(&self.this_machine)
            .unwrap_or(&self.machines[0])
    }

    #[must_use]
    pub fn machine_named(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|machine| machine.name == name)
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn machine_for_slot(&self, slot: u8) -> Option<&Machine> {
        self.machines.iter().find(|machine| machine.slot == slot)
    }

    /// Other machines that run a daemon we can relay to.
    #[must_use]
    #[allow(dead_code)]
    pub fn peers(&self) -> Vec<&Machine> {
        self.machines
            .iter()
            .filter(|machine| machine.name != self.this_machine && machine.address.is_some())
            .collect()
    }
}

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
        assert_eq!(
            config.monitor.smartthings.as_ref().unwrap().base_url,
            "https://api.smartthings.com/v1"
        );
        assert_eq!(
            config
                .peers()
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["avapc"]
        );
    }

    #[test]
    fn duplicate_slots_and_inputs_are_rejected() {
        let dup_slot = SAMPLE.replace("slot = 3", "slot = 1");
        assert!(matches!(
            parse(&dup_slot),
            Err(ConfigError::DuplicateSlot(1))
        ));
        let dup_input = SAMPLE.replace(r#"input = "HDMI2""#, r#"input = "HDMI1""#);
        assert!(matches!(
            parse(&dup_input),
            Err(ConfigError::DuplicateInput(Input::Hdmi1))
        ));
    }

    #[test]
    fn this_machine_and_buttons_must_name_machines() {
        let bad_this = SAMPLE.replace(r#"this_machine = "mac""#, r#"this_machine = "laptop""#);
        assert!(
            matches!(parse(&bad_this), Err(ConfigError::UnknownMachine(name)) if name == "laptop")
        );
        let bad_button = SAMPLE.replace(r#""oldwaterboy"]"#, r#""ghost"]"#);
        assert!(
            matches!(parse(&bad_button), Err(ConfigError::UnknownMachine(name)) if name == "ghost")
        );
    }

    #[test]
    fn listed_backends_need_their_table_and_a_known_name() {
        let missing = SAMPLE.replace(
            "[monitor.smartthings]\ndevice_id = \"dev-1\"\nclient_id = \"client-1\"\n",
            "",
        );
        assert!(matches!(
            parse(&missing),
            Err(ConfigError::BackendTableMissing(name)) if name == "smartthings"
        ));
        let unknown = SAMPLE.replace(
            r#"backends = ["smartthings"]"#,
            r#"backends = ["hdmi-cec"]"#,
        );
        assert!(matches!(
            parse(&unknown),
            Err(ConfigError::UnknownBackend(name)) if name == "hdmi-cec"
        ));
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

    #[test]
    fn schema_version_other_than_one_is_rejected() {
        let bad = SAMPLE.replace("schema_version = 1", "schema_version = 2");
        assert!(matches!(parse(&bad), Err(ConfigError::Schema(2))));
    }

    #[test]
    fn empty_machines_are_rejected() {
        let empty = r#"
schema_version = 1
this_machine = "mac"
machines = []

[monitor]
backends = ["smartthings"]

[monitor.smartthings]
device_id = "dev-1"
client_id = "client-1"
"#;
        assert!(matches!(parse(empty), Err(ConfigError::NoMachines)));
    }

    #[test]
    fn duplicate_machine_names_are_rejected() {
        let dup = SAMPLE.replace(r#"name = "avapc""#, r#"name = "mac""#);
        assert!(matches!(
            parse(&dup),
            Err(ConfigError::DuplicateMachine(name)) if name == "mac"
        ));
    }

    #[test]
    fn load_reports_a_missing_file_and_a_parse_error() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path();
        match Config::load(path) {
            Err(ConfigError::Read(read_path, _)) => {
                assert!(read_path.to_string_lossy().ends_with("config.toml"));
            }
            other => panic!("expected Read error, got {other:?}"),
        }

        let config_file = path.join(FILE_NAME);
        std::fs::write(&config_file, "not = [toml").unwrap();
        match Config::load(path) {
            Err(ConfigError::Parse(_)) => {
                // expected
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }
}
