//! OAuth token management for `SmartThings`: authorization-code login once, then
//! refresh with the stored refresh token (rotated on every refresh).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::config::SmartThingsConfig;
use crate::monitor::BackendError;
use crate::secrets::{SMARTTHINGS_CLIENT_SECRET, SMARTTHINGS_TOKENS, SecretStore};
use crate::smartthings::TokenProvider;

const SCOPES: &str = "r:devices:* x:devices:*";
/// Refresh this long before the access token actually expires.
const REFRESH_MARGIN_SECS: i64 = 60;
/// How long `login` waits for the browser to hit the redirect URI.
const LOGIN_TIMEOUT: Duration = Duration::from_mins(5);

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

fn default_expires_in() -> i64 {
    86_400
}

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
    /// Set once the "no tokens stored" warning has fired, so a daemon
    /// polling every 30s logs the login hint once per process instead of on
    /// every tick; the store is still re-read every time so a later login is
    /// noticed.
    missing_reported: AtomicBool,
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
            missing_reported: AtomicBool::new(false),
        }
    }

    fn load(&self) -> Result<Option<TokenSet>, BackendError> {
        let Some(json) = self
            .secrets
            .get(SMARTTHINGS_TOKENS)
            .map_err(|e| BackendError::Protocol(e.to_string()))?
        else {
            return Ok(None);
        };
        serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| BackendError::Protocol(format!("stored tokens unreadable: {e}")))
    }

    fn store(&self, tokens: &TokenSet) -> Result<(), BackendError> {
        let json =
            serde_json::to_string(tokens).map_err(|e| BackendError::Protocol(e.to_string()))?;
        self.secrets
            .set(SMARTTHINGS_TOKENS, &json)
            .map_err(|e| BackendError::Protocol(e.to_string()))
    }

    async fn refresh(&self, refresh_token: &str) -> Result<TokenSet, BackendError> {
        let Some(client_secret) = self
            .secrets
            .get(SMARTTHINGS_CLIENT_SECRET)
            .map_err(|e| BackendError::Protocol(e.to_string()))?
        else {
            warn!("no SmartThings client secret stored; run `deskswitch smartthings login`");
            return Err(BackendError::Unauthorized);
        };
        let response = self
            .http
            .post(&self.token_url)
            .basic_auth(&self.client_id, Some(&client_secret))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.client_id.as_str()),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|e| BackendError::Http(e.to_string()))?;
        if !response.status().is_success() {
            warn!(status = %response.status(), "SmartThings token refresh refused; run `deskswitch smartthings login`");
            return Err(BackendError::Unauthorized);
        }
        let tokens: TokenSet = response
            .json::<TokenResponse>()
            .await
            .map_err(|e| BackendError::Protocol(e.to_string()))?
            .into();
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
            if self.missing_reported.swap(true, Ordering::Relaxed) {
                debug!("no SmartThings tokens stored; run `deskswitch smartthings login`");
            } else {
                warn!("no SmartThings tokens stored; run `deskswitch smartthings login`");
            }
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

/// The browser URL that starts the authorization-code flow, bound to `state`
/// so the callback that completes it can be verified as the one we started.
#[must_use]
pub fn authorize_url(config: &SmartThingsConfig, state: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &config.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("state", state)
        .finish();
    // `form_urlencoded` leaves `*` unescaped (it is unreserved in
    // application/x-www-form-urlencoded); SmartThings' own examples percent-encode it.
    let query = query.replace('*', "%2A");
    format!("{}?{query}", config.authorize_url)
}

/// `(code, state)` from the first line of the redirect request, e.g.
/// `GET /callback?code=abc&state=x HTTP/1.1`. `None` when the path isn't
/// `/callback` or `code` is missing; `state` is `None` when absent.
#[must_use]
pub fn callback_params(line: &str) -> Option<(String, Option<String>)> {
    let target = line.split_whitespace().nth(1)?;
    let (request_path, query) = target.split_once('?')?;
    if request_path != "/callback" {
        return None;
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    Some((code?, state))
}

/// A random 32-hex-char nonce for the OAuth `state` parameter.
fn random_state() -> anyhow::Result<String> {
    use std::fmt::Write as _;

    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("no OS RNG available: {error}"))?;
    let mut state = String::with_capacity(32);
    for byte in bytes {
        // `fmt::Write` for `String` never actually fails.
        let _ = write!(state, "{byte:02x}");
    }
    Ok(state)
}

/// Run the one-time login: store the client secret, wait for the browser to
/// hit the redirect URI, exchange the code, store the tokens.
pub async fn login(
    config: &SmartThingsConfig,
    secrets: &dyn SecretStore,
    client_secret: &str,
    open_browser: bool,
) -> anyhow::Result<TokenSet> {
    let state = random_state()?;
    login_with_state(
        config,
        secrets,
        client_secret,
        open_browser,
        &state,
        LOGIN_TIMEOUT,
    )
    .await
}

/// Wait for a callback whose `state` matches `expected`, answering every
/// other attempt (wrong state, or a code with no state at all) with 400 and
/// continuing to wait — so a stray page or an attacker's own callback can
/// never bind this login to a code they chose.
async fn accept_code(listener: &TcpListener, expected: &str) -> anyhow::Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = vec![0u8; 4096];
        let read = stream.read(&mut buffer).await?;
        let request = String::from_utf8_lossy(&buffer[..read]);
        let line = request.lines().next().unwrap_or_default();
        match callback_params(line) {
            Some((code, Some(state))) if state == expected => {
                let body = "deskswitch: authorized. You can close this tab.";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await?;
                return Ok(code);
            }
            Some(_) => {
                warn!("rejected an OAuth callback with a missing or mismatched state");
                stream
                    .write_all(
                        b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await?;
            }
            None => {
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await?;
            }
        }
    }
}

/// [`login`], but with the `state` nonce and accept timeout given explicitly
/// (so tests can pin the state and shrink the timeout).
pub async fn login_with_state(
    config: &SmartThingsConfig,
    secrets: &dyn SecretStore,
    client_secret: &str,
    open_browser: bool,
    state: &str,
    timeout: Duration,
) -> anyhow::Result<TokenSet> {
    secrets.set(SMARTTHINGS_CLIENT_SECRET, client_secret)?;
    let redirect = url::Url::parse(&config.redirect_uri)?;
    let port = redirect
        .port()
        .ok_or_else(|| anyhow::anyhow!("redirect_uri needs an explicit port"))?;
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let url = authorize_url(config, state);
    if open_browser && let Err(error) = open::that(&url) {
        warn!(%error, "could not open the browser; open this URL yourself:\n{url}");
    }
    println!(
        "Waiting for SmartThings to redirect to {} …",
        config.redirect_uri
    );
    println!("If the browser did not open, visit:\n{url}");
    let code = tokio::time::timeout(timeout, accept_code(&listener, state))
        .await
        .map_err(|_| {
            anyhow::anyhow!("timed out after {timeout:?} waiting for the SmartThings redirect")
        })??;
    let response = reqwest::Client::new()
        .post(&config.token_url)
        .basic_auth(&config.client_id, Some(client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", config.client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", config.redirect_uri.as_str()),
        ])
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "token exchange failed: {} {}",
        response.status(),
        response.text().await.unwrap_or_default()
    );
    let tokens: TokenSet = response.json::<TokenResponse>().await?.into();
    secrets.set(SMARTTHINGS_TOKENS, &serde_json::to_string(&tokens)?)?;
    Ok(tokens)
}

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
        TokenSet {
            access_token: "old".into(),
            refresh_token: "r0".into(),
            expires_at: Utc::now() + chrono::Duration::seconds(expires_in_secs),
        }
    }

    #[tokio::test]
    async fn a_fresh_token_is_returned_without_a_refresh() {
        let server = MockServer::start().await;
        let (_dir, store) = secrets();
        store
            .set(
                SMARTTHINGS_TOKENS,
                &serde_json::to_string(&tokens(3600)).unwrap(),
            )
            .unwrap();
        let manager = TokenManager::new(&config(&server, "http://localhost:0/callback"), store);
        assert_eq!(manager.bearer().await.unwrap(), "old");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_expiring_token_is_refreshed_with_basic_auth_and_rotated_tokens_are_stored() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header("authorization", "Basic Y2xpZW50OnNoaA=="))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=r0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"access_token":"new","refresh_token":"r1","expires_in":86400}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, store) = secrets();
        store
            .set(
                SMARTTHINGS_TOKENS,
                &serde_json::to_string(&tokens(30)).unwrap(),
            )
            .unwrap();
        let manager = TokenManager::new(
            &config(&server, "http://localhost:0/callback"),
            store.clone(),
        );
        assert_eq!(manager.bearer().await.unwrap(), "new");
        let stored: TokenSet =
            serde_json::from_str(&store.get(SMARTTHINGS_TOKENS).unwrap().unwrap()).unwrap();
        assert_eq!(stored.refresh_token, "r1");
        assert_eq!(
            manager.bearer().await.unwrap(),
            "new",
            "second call served from memory"
        );
    }

    #[tokio::test]
    async fn missing_tokens_or_a_failed_refresh_ask_for_login() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let (_dir, store) = secrets();
        let manager = TokenManager::new(
            &config(&server, "http://localhost:0/callback"),
            store.clone(),
        );
        assert!(matches!(
            manager.bearer().await,
            Err(BackendError::Unauthorized)
        ));
        store
            .set(
                SMARTTHINGS_TOKENS,
                &serde_json::to_string(&tokens(0)).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            manager.bearer().await,
            Err(BackendError::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn the_missing_tokens_warning_fires_once_but_the_store_keeps_being_read() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::in_dir(dir.path()));
        let manager = TokenManager::new(&config(&server, "http://localhost:0/callback"), store);
        assert!(!manager.missing_reported.load(Ordering::Relaxed));
        assert!(matches!(
            manager.bearer().await,
            Err(BackendError::Unauthorized)
        ));
        assert!(manager.missing_reported.load(Ordering::Relaxed));
        // A second call still re-reads the store (not just the cached "missing"
        // state), so a login recorded between calls would be noticed.
        assert!(matches!(
            manager.bearer().await,
            Err(BackendError::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn a_missing_client_secret_asks_for_login() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::in_dir(dir.path()));
        // Tokens are stored (and expired, forcing a refresh) but the client
        // secret never was: `secrets()` is deliberately not used here.
        store
            .set(
                SMARTTHINGS_TOKENS,
                &serde_json::to_string(&tokens(0)).unwrap(),
            )
            .unwrap();
        let manager = TokenManager::new(&config(&server, "http://localhost:0/callback"), store);
        assert!(matches!(
            manager.bearer().await,
            Err(BackendError::Unauthorized)
        ));
    }

    #[test]
    fn authorize_url_and_callback_parsing() {
        let cfg = SmartThingsConfig {
            device_id: "d".into(),
            client_id: "client".into(),
            base_url: String::new(),
            token_url: String::new(),
            authorize_url: "https://api.smartthings.com/oauth/authorize".into(),
            redirect_uri: "http://localhost:8765/callback".into(),
        };
        let url = authorize_url(&cfg, "somestate123");
        assert!(url.starts_with("https://api.smartthings.com/oauth/authorize?"));
        assert!(
            url.contains("client_id=client")
                && url.contains("response_type=code")
                && url.contains("scope=r%3Adevices%3A%2A+x%3Adevices%3A%2A")
                && url.contains("state=somestate123")
        );
        assert_eq!(
            callback_params("GET /callback?code=abc123&state=x HTTP/1.1"),
            Some(("abc123".to_string(), Some("x".to_string())))
        );
        assert_eq!(
            callback_params("GET /callback?code=abc123 HTTP/1.1"),
            Some(("abc123".to_string(), None)),
            "a code with no state at all is still parsed; accept_code is what rejects it"
        );
        assert_eq!(callback_params("GET /favicon.ico HTTP/1.1"), None);
        assert_eq!(callback_params("GET /other?code=abc HTTP/1.1"), None);
        assert_eq!(
            callback_params("GET /callback?state=x HTTP/1.1"),
            None,
            "code is required"
        );
    }

    #[tokio::test]
    async fn login_with_state_rejects_a_state_mismatch_then_exchanges_the_matching_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"access_token":"a","refresh_token":"r","expires_in":86400}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let redirect = format!("http://localhost:{port}/callback");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let cfg = config(&server, &redirect);
        let login = tokio::spawn(async move {
            // `open_browser = false`: the test plays the browser.
            let store = FileStore::in_dir(&path);
            login_with_state(
                &cfg,
                &store,
                "shh",
                false,
                "teststate",
                Duration::from_secs(10),
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // An attacker's (or a stray tab's) callback with the wrong state is
        // rejected, and the login keeps waiting for the real one.
        let status = reqwest::get(format!("{redirect}?code=evil&state=wrong"))
            .await
            .unwrap()
            .status();
        assert_eq!(status.as_u16(), 400);
        assert!(
            !login.is_finished(),
            "a state mismatch must not end the login"
        );

        let body = reqwest::get(format!("{redirect}?code=abc&state=teststate"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(body.contains("You can close this tab"));
        let tokens = login.await.unwrap().unwrap();
        assert_eq!(tokens.access_token, "a");
        let stored = FileStore::in_dir(dir.path())
            .get(SMARTTHINGS_TOKENS)
            .unwrap();
        assert!(stored.is_some(), "tokens were persisted");
    }

    #[tokio::test]
    async fn login_with_state_times_out_when_nothing_ever_calls_back() {
        let server = MockServer::start().await;
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let redirect = format!("http://localhost:{port}/callback");
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::in_dir(dir.path());
        let cfg = config(&server, &redirect);
        let result = login_with_state(
            &cfg,
            &store,
            "shh",
            false,
            "teststate",
            Duration::from_millis(50),
        )
        .await;
        assert!(result.is_err());
    }
}
