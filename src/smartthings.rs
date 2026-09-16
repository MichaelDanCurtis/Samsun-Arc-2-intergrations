//! Samsung `SmartThings` cloud backend: `samsungvd.mediaInputSource` reads and
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
    async fn bearer(&self) -> Result<String, BackendError> {
        Ok(self.0.clone())
    }
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
    id: String,
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
    pub fn new(
        config: &SmartThingsConfig,
        tokens: Arc<dyn TokenProvider>,
    ) -> Result<Self, BackendError> {
        let base_url = Url::parse(config.base_url.trim_end_matches('/'))
            .map_err(|e| BackendError::Protocol(format!("bad base_url: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .map_err(|e| BackendError::Http(e.to_string()))?;
        Ok(Self {
            http,
            base_url,
            device_id: config.device_id.clone(),
            tokens,
        })
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Send with the current token; on 401 invalidate and retry exactly once.
    async fn send(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, BackendError> {
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
        let url = self.url(&format!(
            "devices/{}/components/main/capabilities/{CAPABILITY}/status",
            self.device_id
        ));
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
                    component
                        .capabilities
                        .iter()
                        .any(|capability| capability.id == CAPABILITY)
                })
            })
            .map(|device| {
                let label = device.label.clone().unwrap_or_else(|| device.id.clone());
                (device.id, label)
            })
            .collect())
    }
}

#[async_trait]
impl MonitorBackend for SmartThings {
    fn name(&self) -> &'static str {
        "smartthings"
    }

    async fn current(&self) -> Result<Option<Input>, BackendError> {
        let value = self
            .status()
            .await?
            .input_source
            .and_then(|attr| attr.value);
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
            .and(path(
                "/devices/dev-1/components/main/capabilities/samsungvd.mediaInputSource/status",
            ))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(status_body("HDMI2")))
            .mount(&server)
            .await;
        let st = SmartThings::new(&config(&server), Arc::new(StaticToken("tok".into()))).unwrap();
        assert_eq!(st.current().await.unwrap(), Some(Input::Hdmi2));
        assert_eq!(
            st.supported_inputs().await.unwrap(),
            ["HDMI1", "HDMI2", "HDMI3"]
        );
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
                Ok(format!(
                    "tok{}",
                    self.0.load(std::sync::atomic::Ordering::SeqCst)
                ))
            }
            async fn invalidate(&self) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/devices/dev-1/commands"))
            .and(header("authorization", "Bearer tok0"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/devices/dev-1/commands"))
            .and(header("authorization", "Bearer tok1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
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
        assert_eq!(
            st.list_devices().await.unwrap(),
            vec![("ark".to_string(), "Odyssey Ark".to_string())]
        );
    }
}
