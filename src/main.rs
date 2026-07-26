//! Standalone launcher for the reflection proxy. The same functionality
//! is available as a library — see lib.rs / README — which is the
//! intended way to embed the proxy directly in an azalea bot.

use azalea_reflection_proxy::ReflectionProxy;
use eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let email = std::env::var("PROXY_EMAIL")
        .map_err(|_| eyre::eyre!("set PROXY_EMAIL to the Microsoft account email"))?;
    let mut b = ReflectionProxy::builder()
        .bind(std::env::var("PROXY_BIND").unwrap_or_else(|_| "0.0.0.0:25566".into()))
        .target(std::env::var("PROXY_TARGET").unwrap_or_else(|_| "localhost".into()))
        .email(email);
    if let Ok(cache) = std::env::var("PROXY_AUTH_CACHE") {
        b = b.auth_cache(cache);
    }
    if let Ok(max) = std::env::var("PROXY_MAX_CLIENTS") {
        b = b.max_clients(
            max.parse()
                .map_err(|_| eyre::eyre!("PROXY_MAX_CLIENTS must be a positive integer"))?,
        );
    }
    if let Ok(max) = std::env::var("PROXY_MAX_PENDING_HANDSHAKES") {
        b =
            b.max_pending_handshakes(max.parse().map_err(|_| {
                eyre::eyre!("PROXY_MAX_PENDING_HANDSHAKES must be a positive integer")
            })?);
    }

    let proxy = b.spawn().await?;
    tracing::info!(
        "reflection proxy on {} — connect the bot with Account::offline, spectate with a vanilla client",
        proxy.local_addr()
    );
    proxy.wait().await;
    Ok(())
}
