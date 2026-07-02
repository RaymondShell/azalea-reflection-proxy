//! azalea-reflection-proxy — phase 2: replicator.
//!
//! Port of the architecture from aesthetic0001/mineflayer-reflection-proxy:
//! the proxy owns the single real (Microsoft-authed) connection to the
//! target server; local clients connect as offline clients. The first
//! client to connect becomes the controller and triggers the upstream
//! connection; every later client attaches to the SAME session as a
//! viewer (broadcast clientbound, swallowed serverbound). When the
//! controller leaves, the session dies and the next connection starts a
//! fresh one.
//!
//! Bot-side change required: connect with
//!     Account::offline("reflected")  ->  "127.0.0.1:25566"
//! instead of Account::microsoft(...) -> the real server. The proxy holds
//! the Microsoft session now. Viewers (vanilla client works) just add a
//! multiplayer server entry for 127.0.0.1:25566 while a session is live.

mod ids;
mod local_server;
mod plugin;
mod reflect;
mod relay;
mod session;
mod snapshot;
mod upstream;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use eyre::Result;
use plugin::Pipeline;
use tokio::sync::{Mutex, mpsc};

struct Config {
    local_bind: String,
    target_host: String,
    target_port: u16,
    email: String,
}

impl Config {
    fn from_env() -> Self {
        Self {
            local_bind: std::env::var("PROXY_BIND").unwrap_or_else(|_| "127.0.0.1:25566".into()),
            target_host: std::env::var("PROXY_TARGET").unwrap_or_else(|_| "mc.hypixel.net".into()),
            target_port: 25565,
            email: std::env::var("PROXY_EMAIL").unwrap_or_else(|_| "restsidcrotibig@mail.com".into()),
        }
    }
}

/// At most one live session; new connections attach to it as viewers.
/// When its sender reports closed the session task has exited, and the
/// next connection becomes a fresh controller.
type SessionRegistry = Arc<Mutex<Option<mpsc::Sender<session::SessionMsg>>>>;

static NEXT_CLIENT_ID: AtomicU32 = AtomicU32::new(1);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();
    let cfg = Arc::new(Config::from_env());

    let listener = local_server::listen(&local_server::LocalServerConfig {
        bind: cfg.local_bind.clone(),
    })
    .await?;

    // Phase 3+: push snapshot/anonymize/etc. here, in order — first
    // Drop/Replace verdict wins, like the original.
    let pipeline = Arc::new(Pipeline { plugins: Vec::new() });
    let registry: SessionRegistry = Arc::new(Mutex::new(None));

    loop {
        let (stream, addr) = listener.accept().await?;
        tracing::info!("connection from {addr}");

        let (cfg, pipeline, registry) = (cfg.clone(), pipeline.clone(), registry.clone());
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, cfg, pipeline, registry).await {
                // status pings land here too, so this is not an error
                tracing::info!("connection ended: {e:#}");
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    cfg: Arc<Config>,
    pipeline: Arc<Pipeline>,
    registry: SessionRegistry,
) -> Result<()> {
    let local = local_server::accept_login(stream).await?;
    let username = local.username.clone();
    let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    // Held across the upstream connect on purpose: a second client that
    // races in while the controller is still authenticating waits here,
    // then attaches as a viewer instead of spawning a second session.
    let mut guard = registry.lock().await;

    if let Some(tx) = guard.as_ref().filter(|tx| !tx.is_closed()).cloned() {
        drop(guard);
        session::attach_viewer(&tx, id, local).await?;
        tracing::info!("'{username}' attached as viewer (client {id})");
        return Ok(());
    }

    tracing::info!("'{username}' is the controller (client {id}); connecting upstream");
    let up = upstream::connect(&upstream::UpstreamConfig {
        host: cfg.target_host.clone(),
        port: cfg.target_port,
        email: cfg.email.clone(),
    })
    .await?;
    tracing::info!("upstream established as {}", up.profile.name);

    *guard = Some(session::spawn(up, local, id, pipeline));
    Ok(())
}
