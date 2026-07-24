//! azalea-reflection-proxy — spectate and control an azalea bot session
//! through a local reflection proxy. Rust port of
//! aesthetic0001/mineflayer-reflection-proxy.
//!
//! The proxy owns the single real (Microsoft-authed) connection to the
//! target server. Your bot connects to the proxy locally as an offline
//! client and becomes the controller; vanilla clients that join the
//! same local address become spectators, see the bot as a live player
//! entity, and can take over with `,acquire`.
//!
//! ```no_run
//! # async fn example() -> eyre::Result<()> {
//! use azalea_reflection_proxy::ReflectionProxy;
//!
//! let proxy = ReflectionProxy::builder()
//!     .target("mc.hypixel.net")
//!     .email("you@example.com")
//!     .spawn()
//!     .await?;
//!
//! // then point your azalea bot at it instead of the real server:
//! //   ClientBuilder::new()
//! //       .set_handler(handle)
//! //       .start(Account::offline("reflected"), proxy.local_addr())
//! // and add a vanilla-client server entry for the same address to
//! // spectate. proxy.local_addr() is a real SocketAddr, so a bound
//! // port of 0 picks a free one.
//! # Ok(()) }
//! ```

const _: () = assert!(
    azalea_protocol::packets::PROTOCOL_VERSION == 776,
    "azalea-reflection-proxy requires Minecraft 26.2 (protocol 776)"
);

mod ids;
mod local_server;
pub mod plugin;
mod reflect;
mod relay;
mod session;
mod snapshot;
mod upstream;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use eyre::Result;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::task::JoinHandle;

pub use plugin::{Frame, Pipeline, ProxyPlugin, Verdict};
pub use session::ClientId;

/// Things happening inside the proxy that the host program may care
/// about — the port of the original's `clientJoin`/`changeControl`
/// server events. Subscribe with [`ReflectionProxy::subscribe`].
#[derive(Clone, Debug)]
pub enum ProxyEvent {
    /// A session (upstream connection) was established.
    SessionStarted,
    /// The session ended; the next client starts a fresh one.
    SessionEnded,
    ClientJoined {
        id: ClientId,
        username: String,
    },
    ClientLeft {
        id: ClientId,
        username: String,
    },
    /// Control moved (None = controllerless; the proxy stands in).
    ControlChanged {
        controller: Option<(ClientId, String)>,
    },
}

/// Configuration for a reflection proxy. Build with
/// [`ReflectionProxy::builder`].
pub struct ProxyBuilder {
    bind: String,
    target_host: String,
    target_port: u16,
    email: String,
    auth_cache: Option<PathBuf>,
    plugins: Vec<Box<dyn ProxyPlugin>>,
    whitelist: Vec<String>,
    max_clients: Option<usize>,
    always_first_control: bool,
}

impl Default for ProxyBuilder {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:25566".into(),
            target_host: "localhost".into(),
            target_port: 25565,
            email: String::new(),
            auth_cache: None,
            plugins: Vec::new(),
            whitelist: Vec::new(),
            max_clients: None,
            always_first_control: false,
        }
    }
}

impl ProxyBuilder {
    /// Local address the proxy listens on (default `0.0.0.0:25566`;
    /// use port 0 for an OS-assigned free port).
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.bind = addr.into();
        self
    }

    /// The real server, e.g. `"mc.hypixel.net"` or `"host:port"`.
    pub fn target(mut self, host: impl Into<String>) -> Self {
        let host = host.into();
        if let Some((target_host, target_port)) = split_target(&host) {
            self.target_host = target_host.to_string();
            self.target_port = target_port;
        } else {
            // An unbracketed IPv6 address contains several colons but no
            // unambiguous port. Keep it intact and use the default port.
            self.target_host = host.trim_matches(['[', ']']).to_string();
        }
        self
    }

    /// Microsoft account email. Tokens are cached (and refreshed) in
    /// azalea's standard cache file unless [`Self::auth_cache`] is set,
    /// so interactive login happens at most once per account.
    pub fn email(mut self, email: impl Into<String>) -> Self {
        self.email = email.into();
        self
    }

    /// Override the auth token cache path (default:
    /// `~/.minecraft/azalea-auth.json`, shared with azalea itself).
    pub fn auth_cache(mut self, path: impl Into<PathBuf>) -> Self {
        self.auth_cache = Some(path.into());
        self
    }

    /// Add a frame-level plugin (Forward/Drop/Replace verdicts on raw
    /// packets, in registration order — the port of the original's
    /// plugin pipeline).
    pub fn plugin(mut self, p: Box<dyn ProxyPlugin>) -> Self {
        self.plugins.push(p);
        self
    }

    /// Only allow these usernames to connect (case-insensitive). Empty
    /// (the default) = anyone who can reach the bind address.
    pub fn whitelist<I: IntoIterator<Item = S>, S: Into<String>>(mut self, names: I) -> Self {
        self.whitelist = names.into_iter().map(Into::into).collect();
        self
    }

    /// Cap simultaneous clients (controller + viewers). Default: no cap.
    pub fn max_clients(mut self, max: usize) -> Self {
        self.max_clients = Some(max);
        self
    }

    /// When the controller disconnects, hand control to the oldest
    /// connected viewer instead of going controllerless (the original's
    /// `alwaysFirstControl`). Default: off — the proxy stands in and the
    /// session idles until someone runs `,acquire`.
    pub fn always_first_control(mut self, on: bool) -> Self {
        self.always_first_control = on;
        self
    }

    /// Bind the listener and start accepting clients in the background.
    pub async fn spawn(self) -> Result<ReflectionProxy> {
        if self.email.is_empty() {
            eyre::bail!("ProxyBuilder::email is required");
        }
        if self.max_clients == Some(0) {
            eyre::bail!("ProxyBuilder::max_clients must be at least 1");
        }
        let listener = local_server::listen(&local_server::LocalServerConfig {
            bind: self.bind.clone(),
        })
        .await?;
        let local_addr = listener.local_addr()?;

        let cfg = Arc::new(upstream::UpstreamConfig {
            host: self.target_host,
            port: self.target_port,
            email: self.email,
            auth_cache: self.auth_cache,
        });
        let pipeline = Arc::new(Pipeline {
            plugins: self.plugins,
        });
        let registry: SessionRegistry = Arc::new(Mutex::new(None));
        let (events_tx, _) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            cfg,
            pipeline,
            registry,
            events: events_tx.clone(),
            whitelist: self.whitelist,
            opts: session::SessionOpts {
                max_clients: self.max_clients,
                always_first_control: self.always_first_control,
            },
        });

        let accept_task = tokio::spawn(accept_loop(listener, shared));

        Ok(ReflectionProxy {
            local_addr,
            accept_task,
            events: events_tx,
        })
    }
}

/// A running reflection proxy. Dropping the handle does NOT stop it;
/// call [`Self::shutdown`] for that.
pub struct ReflectionProxy {
    local_addr: SocketAddr,
    accept_task: JoinHandle<()>,
    events: broadcast::Sender<ProxyEvent>,
}

impl ReflectionProxy {
    pub fn builder() -> ProxyBuilder {
        ProxyBuilder::default()
    }

    /// Subscribe to proxy events (client joins/leaves, control changes,
    /// session lifecycle). Each subscriber gets every event from the
    /// moment it subscribes; slow subscribers may observe
    /// [`broadcast::error::RecvError::Lagged`].
    pub fn subscribe(&self) -> broadcast::Receiver<ProxyEvent> {
        self.events.subscribe()
    }

    /// The address your bot (`Account::offline(...)`) and any vanilla
    /// spectator clients should connect to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting new clients. Live sessions keep running until
    /// their connections close.
    pub fn shutdown(&self) {
        self.accept_task.abort();
    }

    /// Run until the accept loop ends (i.e. forever, unless shutdown()
    /// is called or the listener fails). Handy for binary main().
    pub async fn wait(self) {
        let _ = self.accept_task.await;
    }
}

/// At most one live session; new connections attach to it as viewers.
/// When its sender reports closed the session task has exited, and the
/// next connection becomes a fresh controller.
type SessionRegistry = Arc<Mutex<Option<mpsc::Sender<session::SessionMsg>>>>;

/// Everything the accept path needs, bundled once at spawn.
struct Shared {
    cfg: Arc<upstream::UpstreamConfig>,
    pipeline: Arc<Pipeline>,
    registry: SessionRegistry,
    events: broadcast::Sender<ProxyEvent>,
    whitelist: Vec<String>,
    opts: session::SessionOpts,
}

static NEXT_CLIENT_ID: AtomicU32 = AtomicU32::new(1);

async fn accept_loop(listener: tokio::net::TcpListener, shared: Arc<Shared>) {
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::error!("accept failed: {e}");
                break;
            }
        };
        tracing::info!("connection from {addr}");
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, shared).await {
                // status pings land here too, so this is not an error
                tracing::info!("connection ended: {e:#}");
            }
        });
    }
}

async fn handle_connection(stream: tokio::net::TcpStream, shared: Arc<Shared>) -> Result<()> {
    // A peer that connects and never completes the handshake should not
    // retain a task and socket forever (especially if the caller chose a
    // non-loopback bind address).
    let mut local = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        local_server::accept_login(stream),
    )
    .await
    .map_err(|_| eyre::eyre!("local login timed out"))??;
    let username = local.username.clone();

    if !shared.whitelist.is_empty()
        && !shared
            .whitelist
            .iter()
            .any(|w| w.eq_ignore_ascii_case(&username))
    {
        use azalea_chat::FormattedText;
        use azalea_protocol::packets::config::c_disconnect::ClientboundDisconnect;
        tracing::info!("'{username}' rejected: not whitelisted");
        let _ = local
            .connection
            .write(ClientboundDisconnect {
                reason: FormattedText::from("not on this proxy's whitelist"),
            })
            .await;
        return Ok(());
    }

    let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    // Held across the upstream connect on purpose: a second client that
    // races in while the controller is still authenticating waits here,
    // then attaches as a viewer instead of spawning a second session.
    let mut guard = shared.registry.lock().await;

    if let Some(tx) = guard.as_ref().filter(|tx| !tx.is_closed()).cloned() {
        drop(guard);
        session::attach_viewer(&tx, id, local).await?;
        tracing::info!("'{username}' attached as viewer (client {id})");
        return Ok(());
    }

    tracing::info!("'{username}' is the controller (client {id}); connecting upstream");
    let up = upstream::connect(&shared.cfg).await?;
    tracing::info!("upstream established as {}", up.profile.name);

    *guard = Some(session::spawn(
        up,
        local,
        id,
        shared.pipeline.clone(),
        shared.opts.clone(),
        shared.events.clone(),
    ));
    Ok(())
}

/// Split a DNS/IPv4 `host:port` or bracketed IPv6 `[host]:port` target.
/// Raw IPv6 addresses deliberately return `None` because their final
/// component is not a port.
fn split_target(target: &str) -> Option<(&str, u16)> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        return port.parse().ok().map(|port| (host, port));
    }
    if target.bytes().filter(|&b| b == b':').count() != 1 {
        return None;
    }
    let (host, port) = target.rsplit_once(':')?;
    (!host.is_empty()).then_some(())?;
    port.parse().ok().map(|port| (host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parsing_preserves_ipv6() {
        assert_eq!(
            split_target("example.com:25570"),
            Some(("example.com", 25570))
        );
        assert_eq!(
            split_target("[2001:db8::1]:25570"),
            Some(("2001:db8::1", 25570))
        );
        assert_eq!(split_target("2001:db8::1"), None);
        assert_eq!(split_target("example.com"), None);
    }
}
