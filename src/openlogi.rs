//! The keyboard side, delegated to `OpenLogi`'s CLI: is the keyboard here, and
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

#[derive(Debug, Clone)]
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
            Err(_) => Err(format!(
                "{} {} timed out after {timeout:?}",
                self.binary.display(),
                args.join(" ")
            )),
        }
    }
}

#[async_trait]
impl KeyboardAgent for OpenLogi {
    async fn probe(&self) -> Probe {
        match self
            .run(&["host", "list", "--json"], Duration::from_secs(3))
            .await
        {
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
        match self
            .run(&["host", "switch", &slot], Duration::from_secs(10))
            .await
        {
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
            (true, Some(current_slot)) => Some(Probe::Here {
                current_slot,
                name: keyboard.name,
            }),
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
        other => {
            SwitchResult::Unavailable(format!("openlogi exited with {other:?}: {}", stderr.trim()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: &str = r#"{"keyboards":[{"name":"MX KEYS S MAC","route":"direct 046d:b37c","online":true,"current_slot":1,"slots":[{"slot":1,"paired":true,"name":"Mac"}]}]}"#;
    const AWAY: &str = r#"{"keyboards":[{"name":"MX KEYS S MAC","route":"direct 046d:b37c","online":false,"error":"offline"}]}"#;

    #[test]
    fn list_json_is_interpreted() {
        assert!(matches!(
            interpret_list(HERE).unwrap(),
            Probe::Here {
                current_slot: 1,
                ..
            }
        ));
        assert!(matches!(interpret_list(AWAY).unwrap(), Probe::Elsewhere));
        assert!(matches!(
            interpret_list(r#"{"keyboards":[]}"#).unwrap(),
            Probe::Elsewhere
        ));
        assert!(interpret_list("nope").is_err());
    }

    #[test]
    fn switch_exit_codes_are_mapped() {
        assert!(matches!(
            interpret_switch(Some(0), "MX → slot 2\n", ""),
            SwitchResult::Switched
        ));
        assert!(matches!(
            interpret_switch(Some(0), "MX already on slot 1\n", ""),
            SwitchResult::AlreadyCurrent
        ));
        assert!(
            matches!(interpret_switch(Some(2), "", "unpaired"), SwitchResult::Refused(m) if m == "unpaired")
        );
        assert!(matches!(
            interpret_switch(Some(3), "", ""),
            SwitchResult::NotFound
        ));
        assert!(matches!(
            interpret_switch(Some(4), "", "no agent"),
            SwitchResult::Unavailable(_)
        ));
        assert!(matches!(
            interpret_switch(None, "", ""),
            SwitchResult::Unavailable(_)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_fixture_script_drives_both_commands() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openlogi.sh");
        let agent = OpenLogi::new(fixture);
        temp_env::async_with_vars(
            [
                ("FAKE_OPENLOGI_HERE", Some("1")),
                ("FAKE_OPENLOGI_SLOT", Some("1")),
            ],
            async {
                assert!(matches!(
                    agent.probe().await,
                    Probe::Here {
                        current_slot: 1,
                        ..
                    }
                ));
                assert!(matches!(agent.switch(2).await, SwitchResult::Switched));
                assert!(matches!(agent.switch(3).await, SwitchResult::Refused(_)));
            },
        )
        .await;
        let missing = OpenLogi::new("/nonexistent/openlogi".into());
        assert!(matches!(missing.probe().await, Probe::Unavailable(_)));
    }
}
