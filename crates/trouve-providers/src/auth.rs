//! Pluggable credentials: static API keys, and OAuth tokens with refresh
//! (device flow for providers that support RFC 8628, PKCE authorization-code
//! for OpenAI subscription auth).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::secrets::SecretStore;
use crate::ProviderError;

/// Something that can produce a bearer token/API key on demand. Providers
/// take a `TokenSource` instead of a raw key so auth is swappable: static
/// key, env var, or refreshing OAuth tokens.
#[async_trait::async_trait]
pub trait TokenSource: Send + Sync {
    async fn bearer(&self) -> Result<String, ProviderError>;
}

pub struct StaticToken(pub String);

#[async_trait::async_trait]
impl TokenSource for StaticToken {
    async fn bearer(&self) -> Result<String, ProviderError> {
        Ok(self.0.clone())
    }
}

/// Persisted OAuth token set (stored as JSON in the secret store).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds; refresh a minute early.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
}

impl OAuthTokens {
    pub fn from_token_response(v: &Value) -> Result<Self, ProviderError> {
        let access_token = v["access_token"]
            .as_str()
            .ok_or_else(|| ProviderError::Auth("token response missing access_token".into()))?
            .to_string();
        let expires_at = v["expires_in"].as_u64().map(|secs| now_unix() + secs);
        Ok(Self {
            access_token,
            refresh_token: v["refresh_token"].as_str().map(String::from),
            expires_at,
            id_token: v["id_token"].as_str().map(String::from),
        })
    }

    pub fn expired(&self) -> bool {
        match self.expires_at {
            Some(at) => now_unix() + 60 >= at,
            None => false,
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// OAuth endpoints/client info for a provider (config-driven).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    pub client_id: String,
    /// RFC 8628 device authorization endpoint (device flow).
    #[serde(default)]
    pub device_authorization_url: Option<String>,
    /// Authorization endpoint (PKCE browser flow).
    #[serde(default)]
    pub authorization_url: Option<String>,
    pub token_url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Fixed localhost port for the PKCE redirect (some providers pin it).
    #[serde(default)]
    pub redirect_port: Option<u16>,
    /// Redirect path for the PKCE callback (defaults to "/callback"; some
    /// providers register an exact path like "/auth/callback").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_path: Option<String>,
}

/// Token source backed by the secret store, refreshing via the provider's
/// token endpoint when expired.
pub struct StoredOAuthToken {
    pub store: Arc<dyn SecretStore>,
    pub secret_key: String,
    pub oauth: OAuthConfig,
    pub http: reqwest::Client,
}

impl StoredOAuthToken {
    pub fn new(store: Arc<dyn SecretStore>, secret_key: String, oauth: OAuthConfig) -> Self {
        Self {
            store,
            secret_key,
            oauth,
            http: reqwest::Client::new(),
        }
    }

    fn load(&self) -> Result<OAuthTokens, ProviderError> {
        let raw = self
            .store
            .get(&self.secret_key)
            .map_err(|e| ProviderError::Auth(format!("secret store: {e}")))?
            .ok_or_else(|| {
                ProviderError::Auth(format!(
                    "no stored credentials ({}); run `trouve auth login`",
                    self.secret_key
                ))
            })?;
        serde_json::from_str(&raw)
            .map_err(|e| ProviderError::Auth(format!("corrupt stored credentials: {e}")))
    }

    async fn refresh(&self, tokens: &OAuthTokens) -> Result<OAuthTokens, ProviderError> {
        let refresh_token = tokens
            .refresh_token
            .as_deref()
            .ok_or_else(|| ProviderError::Auth("token expired and no refresh token".into()))?;
        let resp = self
            .http
            .post(&self.oauth.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", &self.oauth.client_id),
            ])
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        if !status.is_success() {
            return Err(ProviderError::Auth(format!("token refresh failed: {body}")));
        }
        let mut new_tokens = OAuthTokens::from_token_response(&body)?;
        if new_tokens.refresh_token.is_none() {
            new_tokens.refresh_token = tokens.refresh_token.clone();
        }
        let raw =
            serde_json::to_string(&new_tokens).map_err(|e| ProviderError::Auth(e.to_string()))?;
        self.store
            .set(&self.secret_key, &raw)
            .map_err(|e| ProviderError::Auth(format!("secret store: {e}")))?;
        Ok(new_tokens)
    }
}

#[async_trait::async_trait]
impl TokenSource for StoredOAuthToken {
    async fn bearer(&self) -> Result<String, ProviderError> {
        let tokens = self.load()?;
        if tokens.expired() {
            return Ok(self.refresh(&tokens).await?.access_token);
        }
        Ok(tokens.access_token)
    }
}

// --- RFC 8628 device flow ----------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    pub expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

/// Start the device flow: returns the code the user must enter.
pub async fn device_authorize(oauth: &OAuthConfig) -> Result<DeviceAuthorization, ProviderError> {
    let url = oauth
        .device_authorization_url
        .as_deref()
        .ok_or_else(|| ProviderError::Auth("provider has no device_authorization_url".into()))?;
    let resp = reqwest::Client::new()
        .post(url)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", oauth.client_id.as_str()),
            ("scope", &oauth.scopes.join(" ")),
        ])
        .send()
        .await
        .map_err(|e| ProviderError::Request(e.to_string()))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Request(e.to_string()))?;
    if !status.is_success() {
        return Err(ProviderError::Auth(format!(
            "device authorization failed: {body}"
        )));
    }
    serde_json::from_value(body).map_err(|e| ProviderError::Auth(e.to_string()))
}

/// Poll the token endpoint until the user approves (or the code expires).
pub async fn device_poll(
    oauth: &OAuthConfig,
    device: &DeviceAuthorization,
) -> Result<OAuthTokens, ProviderError> {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(device.expires_in);
    let mut interval = device.interval.max(1);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let resp = client
            .post(&oauth.token_url)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &device.device_code),
                ("client_id", &oauth.client_id),
            ])
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        match body["error"].as_str() {
            None => return OAuthTokens::from_token_response(&body),
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some(other) => {
                return Err(ProviderError::Auth(format!(
                    "device flow failed: {other}: {}",
                    body["error_description"].as_str().unwrap_or("")
                )))
            }
        }
    }
    Err(ProviderError::Auth("device flow timed out".into()))
}

// --- PKCE authorization-code flow (OpenAI subscription auth) -------------------

pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
}

/// S256 PKCE pair from OS randomness.
pub fn pkce_challenge() -> PkceChallenge {
    use sha2::Digest;
    let verifier: String = {
        let mut buf = [0u8; 64];
        getrandom::fill(&mut buf).expect("os rng");
        base64_url(&buf)
    };
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    PkceChallenge {
        challenge: base64_url(&digest),
        verifier,
    }
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the browser URL for the PKCE flow.
pub fn pkce_authorize_url(
    oauth: &OAuthConfig,
    challenge: &PkceChallenge,
    redirect_uri: &str,
    state: &str,
) -> Result<String, ProviderError> {
    let auth_url = oauth
        .authorization_url
        .as_deref()
        .ok_or_else(|| ProviderError::Auth("provider has no authorization_url".into()))?;
    let mut url = reqwest::Url::parse(auth_url)
        .map_err(|e| ProviderError::Auth(format!("bad authorization_url: {e}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oauth.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &oauth.scopes.join(" "))
        .append_pair("code_challenge", &challenge.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    Ok(url.to_string())
}

/// Wait for the OAuth redirect on a localhost listener and extract `code`.
/// Serves a tiny "you can close this tab" page.
pub async fn pkce_wait_for_code(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let mut buf = vec![0u8; 8192];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let request = String::from_utf8_lossy(&buf[..n]);
        let Some(path) = request.split_whitespace().nth(1) else {
            continue;
        };
        let Ok(url) = reqwest::Url::parse(&format!("http://localhost{path}")) else {
            continue;
        };
        let mut code = None;
        let mut state_ok = false;
        for (k, v) in url.query_pairs() {
            match &*k {
                "code" => code = Some(v.to_string()),
                "state" => state_ok = v == expected_state,
                _ => {}
            }
        }
        let body = "<html><body>trouve: login complete, you can close this tab.</body></html>";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        if let (Some(code), true) = (code, state_ok) {
            return Ok(code);
        }
    }
}

/// Exchange the authorization code for tokens.
pub async fn pkce_exchange(
    oauth: &OAuthConfig,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthTokens, ProviderError> {
    let resp = reqwest::Client::new()
        .post(&oauth.token_url)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &oauth.client_id),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|e| ProviderError::Request(e.to_string()))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Request(e.to_string()))?;
    if !status.is_success() {
        return Err(ProviderError::Auth(format!("code exchange failed: {body}")));
    }
    OAuthTokens::from_token_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        use base64::Engine;
        use sha2::Digest;
        let pair = pkce_challenge();
        let digest = sha2::Sha256::digest(pair.verifier.as_bytes());
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        assert_eq!(pair.challenge, expect);
        assert!(pair.verifier.len() >= 43);
    }

    #[test]
    fn tokens_expiry() {
        let mut t = OAuthTokens {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(now_unix() + 3600),
            id_token: None,
        };
        assert!(!t.expired());
        t.expires_at = Some(now_unix() + 30);
        assert!(t.expired(), "refresh a minute early");
        t.expires_at = None;
        assert!(!t.expired(), "no expiry info means assume valid");
    }
}
