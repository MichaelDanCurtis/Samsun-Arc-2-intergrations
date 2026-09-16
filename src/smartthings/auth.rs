//! OAuth token management for `SmartThings`: authorization-code login once, then
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
            warn!("no SmartThings tokens stored; run `deskswitch smartthings login`");
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
    // `form_urlencoded` leaves `*` unescaped (it is unreserved in
    // application/x-www-form-urlencoded); SmartThings' own examples percent-encode it.
    let query = query.replace('*', "%2A");
    format!("{}?{query}", config.authorize_url)
}

/// `code` from the first line of the redirect request, e.g.
/// `GET /callback?code=abc&state=x HTTP/1.1`.
#[must_use]
pub fn code_from_request_line(line: &str) -> Option<String> {
    let target = line.split_whitespace().nth(1)?;
    let (request_path, query) = target.split_once('?')?;
    if request_path != "/callback" {
        return None;
    }
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
    let port = redirect
        .port()
        .ok_or_else(|| anyhow::anyhow!("redirect_uri needs an explicit port"))?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let url = authorize_url(config);
    if open_browser && let Err(error) = open::that(&url) {
        warn!(%error, "could not open the browser; open this URL yourself:\n{url}");
    }
    println!(
        "Waiting for SmartThings to redirect to {} …",
        config.redirect_uri
    );
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
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await?;
                break code;
            }
            None => {
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await?;
            }
        }
    };
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
        let url = authorize_url(&cfg);
        assert!(url.starts_with("https://api.smartthings.com/oauth/authorize?"));
        assert!(
            url.contains("client_id=client")
                && url.contains("response_type=code")
                && url.contains("scope=r%3Adevices%3A%2A+x%3Adevices%3A%2A")
        );
        assert_eq!(
            code_from_request_line("GET /callback?code=abc123&state=x HTTP/1.1").as_deref(),
            Some("abc123")
        );
        assert_eq!(code_from_request_line("GET /favicon.ico HTTP/1.1"), None);
        assert_eq!(code_from_request_line("GET /other?code=abc HTTP/1.1"), None);
    }

    #[tokio::test]
    async fn login_exchanges_the_code_it_receives_on_the_redirect_port() {
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
            login(&cfg, &store, "shh", false).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let body = reqwest::get(format!("{redirect}?code=abc"))
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
}
