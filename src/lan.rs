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
#[cfg(test)]
use crate::input::Input;
#[cfg(test)]
use crate::monitor::Chain;
use crate::openlogi::{KeyboardAgent, Probe, SwitchResult};
use crate::resolve::{Target, resolve};
#[cfg(test)]
use crate::state::StateStore;
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
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "unauthorized"})),
    )
        .into_response()
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
        (None, None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "slot or machine required"})),
            )
                .into_response();
        }
    };
    let slot = match resolve(&shared.deps.config, &target) {
        Ok(resolved) => match resolved.machine {
            Some(machine) => machine.slot,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "target is not a machine"})),
                )
                    .into_response();
            }
        },
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    let agent: &dyn KeyboardAgent = shared.deps.agent.as_ref();
    let body = match agent.probe().await {
        Probe::Here { .. } => match agent.switch(slot).await {
            SwitchResult::Switched => {
                serde_json::json!({"result": "delegated", "agent": "switched"})
            }
            SwitchResult::AlreadyCurrent => {
                serde_json::json!({"result": "delegated", "agent": "already"})
            }
            SwitchResult::Refused(detail) | SwitchResult::Unavailable(detail) => {
                serde_json::json!({"result": "error", "detail": detail})
            }
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
                    let body: serde_json::Value =
                        response.json().await.map_err(|e| e.to_string())?;
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
            if self.0 {
                Probe::Here {
                    current_slot: 1,
                    name: "K".into(),
                }
            } else {
                Probe::Elsewhere
            }
        }
        async fn switch(&self, _slot: u8) -> SwitchResult {
            SwitchResult::Switched
        }
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
        let response = reqwest::Client::new()
            .post(format!("{base}/switch"))
            .json(&serde_json::json!({"slot": 2}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        let response = reqwest::Client::new()
            .get(format!("{base}/status"))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }

    #[tokio::test]
    async fn the_holder_delegates_and_others_ignore() {
        let (holder, _d1) = serve_test(true).await;
        let (other, _d2) = serve_test(false).await;
        let client = reqwest::Client::new();
        let body: serde_json::Value = client
            .post(format!("{holder}/switch"))
            .bearer_auth("s3cret")
            .json(&serde_json::json!({"slot": 2}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["result"], "delegated");
        let body: serde_json::Value = client
            .post(format!("{other}/switch"))
            .bearer_auth("s3cret")
            .json(&serde_json::json!({"machine": "avapc"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["result"], "ignored");
        let response = client
            .post(format!("{other}/switch"))
            .bearer_auth("s3cret")
            .json(&serde_json::json!({"slot": 9}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn status_is_served_as_json() {
        let (base, _d) = serve_test(true).await;
        let body: serde_json::Value = reqwest::Client::new()
            .get(format!("{base}/status"))
            .bearer_auth("s3cret")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
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
        let by_peer: std::collections::BTreeMap<_, _> =
            outcomes.into_iter().map(|o| (o.peer, o.result)).collect();
        assert_eq!(by_peer["avapc"].as_deref(), Ok("delegated"));
        assert!(by_peer["oldwaterboy"].is_err());
    }
}
