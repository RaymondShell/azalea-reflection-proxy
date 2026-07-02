//! Standalone launcher for the reflection proxy. The same functionality
//! is available as a library — see lib.rs / README — which is the
//! intended way to embed the proxy directly in an azalea bot.

use azalea_reflection_proxy::ReflectionProxy;
use eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let mut b = ReflectionProxy::builder()
        .bind(std::env::var("PROXY_BIND").unwrap_or_else(|_| "127.0.0.1:25566".into()))
        .target(std::env::var("PROXY_TARGET").unwrap_or_else(|_| "mc.hypixel.net".into()))
        .email(std::env::var("PROXY_EMAIL").unwrap_or_else(|_| "restsidcrotibig@mail.com".into()));
    if let Ok(cache) = std::env::var("PROXY_AUTH_CACHE") {
        b = b.auth_cache(cache);
    }

    let proxy = b.spawn().await?;
    tracing::info!(
        "reflection proxy on {} — connect the bot with Account::offline, spectate with a vanilla client",
        proxy.local_addr()
    );
    proxy.wait().await;
    Ok(())
}
