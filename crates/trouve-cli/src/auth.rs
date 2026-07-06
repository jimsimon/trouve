//! Credential management: API keys and OAuth logins, stored in the OS
//! keychain (file fallback on headless machines).

use std::io::Write as _;

use anyhow::{bail, Context, Result};
use trouve_core::config::{data_dir, Config};
use trouve_providers::auth::{self, OAuthConfig};
use trouve_providers::secrets::{api_key_secret, default_store, oauth_secret};

pub async fn set_key(provider: &str) -> Result<()> {
    print!("API key for {provider}: ");
    std::io::stdout().flush()?;
    let mut key = String::new();
    std::io::stdin().read_line(&mut key)?;
    let key = key.trim();
    if key.is_empty() {
        bail!("no key entered");
    }
    let store = default_store(&data_dir());
    store.set(&api_key_secret(provider), key)?;
    println!("Stored. Restart the server to pick it up.");
    Ok(())
}

pub fn logout(provider: &str) -> Result<()> {
    let store = default_store(&data_dir());
    store.delete(&api_key_secret(provider))?;
    store.delete(&oauth_secret(provider))?;
    println!("Removed stored credentials for {provider}.");
    Ok(())
}

fn oauth_config(provider: &str) -> Result<OAuthConfig> {
    let config = Config::load();
    config
        .providers
        .get(provider)
        .and_then(|p| p.oauth.clone())
        .with_context(|| format!("no [providers.{provider}.oauth] section in the trouve config"))
}

pub async fn login(provider: &str) -> Result<()> {
    let oauth = oauth_config(provider)?;
    let tokens = if oauth.device_authorization_url.is_some() {
        login_device(&oauth).await?
    } else if oauth.authorization_url.is_some() {
        login_pkce(&oauth).await?
    } else {
        bail!("provider {provider} has neither device_authorization_url nor authorization_url");
    };
    let store = default_store(&data_dir());
    store.set(&oauth_secret(provider), &serde_json::to_string(&tokens)?)?;
    println!("Logged in to {provider}. Restart the server to pick it up.");
    Ok(())
}

async fn login_device(oauth: &OAuthConfig) -> Result<auth::OAuthTokens> {
    let device = auth::device_authorize(oauth).await?;
    println!(
        "Open {} and enter code: {}",
        device.verification_uri, device.user_code
    );
    if let Some(complete) = &device.verification_uri_complete {
        println!("(or open {complete})");
    }
    println!("Waiting for approval…");
    Ok(auth::device_poll(oauth, &device).await?)
}

async fn login_pkce(oauth: &OAuthConfig) -> Result<auth::OAuthTokens> {
    let listener =
        tokio::net::TcpListener::bind(("127.0.0.1", oauth.redirect_port.unwrap_or(0))).await?;
    let redirect_uri = format!(
        "http://localhost:{}{}",
        listener.local_addr()?.port(),
        oauth.redirect_path.as_deref().unwrap_or("/callback")
    );

    let challenge = auth::pkce_challenge();
    let state = uuid_like();
    let url = auth::pkce_authorize_url(oauth, &challenge, &redirect_uri, &state)?;
    println!("Open this URL in your browser to log in:\n\n  {url}\n");
    println!("Waiting for the redirect…");
    let code = auth::pkce_wait_for_code(listener, &state).await?;
    Ok(auth::pkce_exchange(oauth, &code, &challenge.verifier, &redirect_uri).await?)
}

fn uuid_like() -> String {
    // Random state parameter; no uuid dependency needed here.
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("os rng");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
