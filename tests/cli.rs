#![cfg(unix)]
// Integration tests are their own crate root, so the crate-wide test
// exemption `main.rs` applies via `#![cfg_attr(test, ...)]` to unit tests
// doesn't reach here; restate it for this file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openlogi.sh")
}

fn write_config(dir: &Path, server: &MockServer, inputs: &[&str]) {
    #[allow(clippy::format_collect)]
    let machines = inputs
        .iter()
        .zip([("mac", 1u8), ("avapc", 3), ("oldwaterboy", 2)])
        .map(|(input, (name, slot))| {
            format!("[[machines]]\nname = \"{name}\"\nslot = {slot}\ninput = \"{input}\"\n")
        })
        .collect::<String>();
    let config = format!(
        "schema_version = 1\nthis_machine = \"mac\"\n{machines}\n[monitor]\nbackends = [\"smartthings\"]\n\n[monitor.smartthings]\ndevice_id = \"dev-1\"\nclient_id = \"client\"\nbase_url = \"{base}\"\ntoken_url = \"{base}/oauth/token\"\n\n[openlogi]\nbinary = \"{bin}\"\n\n[secrets]\nstore = \"file\"\n",
        base = server.uri(),
        bin = fixture().display(),
    );
    std::fs::write(dir.join("config.toml"), config).unwrap();
    let tokens = serde_json::json!({"access_token": "tok", "refresh_token": "r", "expires_at": "2099-01-01T00:00:00Z"});
    std::fs::write(
        dir.join("secrets.json"),
        serde_json::json!({"smartthings.tokens": tokens.to_string()}).to_string(),
    )
    .unwrap();
}

async fn ark(server: &MockServer, current: &str) {
    Mock::given(method("GET"))
        .and(path("/devices/dev-1/components/main/capabilities/samsungvd.mediaInputSource/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "inputSource": {"value": current},
            "supportedInputSourcesMap": {"value": [{"id":"HDMI1","name":"a"},{"id":"HDMI2","name":"b"},{"id":"HDMI3","name":"c"}]}
        })))
        .mount(server).await;
    Mock::given(method("POST"))
        .and(path("/devices/dev-1/commands"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(server)
        .await;
}

fn deskswitch(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("deskswitch").unwrap();
    cmd.env("DESKSWITCH_CONFIG_DIR", dir)
        .env_remove("OPENLOGI_TRIGGER");
    cmd
}

async fn commands_sent(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method == "POST")
        .count()
}

#[tokio::test]
async fn user_mode_with_the_keyboard_here_delegates_to_openlogi() {
    let server = MockServer::start().await;
    ark(&server, "HDMI1").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "HDMI3"]);
    let log = dir.path().join("switches.log");
    deskswitch(dir.path())
        .env("FAKE_OPENLOGI_HERE", "1")
        .env("FAKE_OPENLOGI_SLOT", "1")
        .env("FAKE_OPENLOGI_LOG", &log)
        // avapc is slot 3, which tests/fixtures/openlogi.sh always refuses
        // (see its header comment); target oldwaterboy (slot 2) instead so
        // this exercises a genuine different-slot, successful delegation.
        .args(["switch", "--to", "oldwaterboy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("delegated"));
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "2");
    assert_eq!(
        commands_sent(&server).await,
        0,
        "the hook, not this call, moves the monitor"
    );
}

#[tokio::test]
async fn hook_mode_moves_only_the_monitor() {
    let server = MockServer::start().await;
    ark(&server, "HDMI1").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "HDMI3"]);
    let log = dir.path().join("switches.log");
    deskswitch(dir.path())
        .env("OPENLOGI_TRIGGER", "key")
        .env("FAKE_OPENLOGI_HERE", "1")
        .env("FAKE_OPENLOGI_LOG", &log)
        .args(["switch", "--slot", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("HDMI3"));
    assert!(!log.exists(), "hook mode never calls openlogi");
    assert_eq!(commands_sent(&server).await, 1);
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("state.json")).unwrap())
            .unwrap();
    assert_eq!(state["last_known_input"], "HDMI3");
}

#[tokio::test]
async fn keyboard_elsewhere_moves_the_monitor_and_already_there_is_a_noop() {
    let server = MockServer::start().await;
    ark(&server, "HDMI2").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "HDMI3"]);
    deskswitch(dir.path())
        .env("FAKE_OPENLOGI_HERE", "0")
        .args(["switch", "--input", "HDMI2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already"));
    assert_eq!(commands_sent(&server).await, 0);
    deskswitch(dir.path())
        .env("FAKE_OPENLOGI_HERE", "0")
        .args(["switch", "--slot", "1"])
        .assert()
        .success();
    assert_eq!(commands_sent(&server).await, 1);
}

#[tokio::test]
async fn exit_codes_and_config_check() {
    let server = MockServer::start().await;
    ark(&server, "HDMI1").await;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &server, &["HDMI1", "HDMI2", "DP"]);
    deskswitch(dir.path())
        .args(["switch", "--slot", "9"])
        .assert()
        .code(3);
    deskswitch(dir.path())
        .args(["config", "check"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("DP"));
    deskswitch(dir.path())
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"input\":\"HDMI1\""));
}
