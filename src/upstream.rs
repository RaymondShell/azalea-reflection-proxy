//! Upstream leg: the proxy's own authenticated connection to the target
//! server. The proxy — not the bot — owns the real Microsoft session.

use azalea_protocol::{
    connect::Connection,
    packets::{
        ClientIntention,
        PROTOCOL_VERSION,
        handshake::s_intention::ServerboundIntention,
        login::{
            ClientboundLoginPacket,
            s_hello::ServerboundHello,
            s_key::ServerboundKey,
            s_login_acknowledged::ServerboundLoginAcknowledged,
        },
        config::{
            ClientboundConfigPacket, ServerboundConfigPacket,
        },
    },
};
use azalea_auth::{auth, AuthOpts, ProfileResponse};
use azalea_auth::sessionserver::{self, SessionServerJoinOpts};
use azalea_crypto::encrypt;
use eyre::Result;
use tokio::net::lookup_host;

/// Everything the relay needs about the established upstream leg.
pub struct Upstream {
    pub connection: Connection<ClientboundConfigPacket, ServerboundConfigPacket>,
    pub compression_threshold: Option<u32>,
    pub profile: ProfileResponse,
}

pub struct UpstreamConfig {
    pub host: String,
    pub port: u16,
    /// Microsoft account email; token cache handled by azalea-auth.
    pub email: String,
    /// Auth cache override; None = PROXY_AUTH_CACHE env or azalea's
    /// standard `~/.minecraft/azalea-auth.json`.
    pub auth_cache: Option<std::path::PathBuf>,
}

/// Where refreshed Microsoft tokens live between runs. Defaults to the
/// exact path azalea's Account::microsoft uses
/// (`~/.minecraft/azalea-auth.json`), so the proxy and any azalea bot on
/// this machine share one cache and one device-code login covers both.
/// Override with PROXY_AUTH_CACHE.
fn auth_cache_file() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("PROXY_AUTH_CACHE") {
        return Some(p.into());
    }
    let dir = minecraft_folder_path::minecraft_dir()?;
    Some(dir.join("azalea-auth.json"))
}

pub async fn connect(cfg: &UpstreamConfig) -> Result<Upstream> {
    tracing::info!("authenticating {} and connecting to {}:{}", cfg.email, cfg.host, cfg.port);

    // 1. Auth with Microsoft. Without cache_file azalea-auth keeps NO
    // cache and forces the device-code flow on every launch.
    let cache_file = cfg.auth_cache.clone().or_else(auth_cache_file);
    if cache_file.is_none() {
        tracing::warn!("could not locate .minecraft dir; auth will not be cached");
    }
    let auth_result = auth(
        &cfg.email,
        AuthOpts {
            cache_file,
            ..AuthOpts::default()
        },
    )
    .await
    .map_err(|e| eyre::eyre!("Authentication failed: {:?}", e))?;

    let profile = auth_result.profile.clone();
    let access_token = auth_result.access_token;

    // 2. Resolve and connect
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let resolved_addr = lookup_host(&addr).await?
        .next()
        .ok_or_else(|| eyre::eyre!("Failed to resolve {}", addr))?;

    let mut conn = Connection::new(&resolved_addr).await
        .map_err(|e| eyre::eyre!("Connection failed: {:?}", e))?;

    // 3. Handshake packet
    conn.write(ServerboundIntention {
        protocol_version: PROTOCOL_VERSION as i32,
        hostname: cfg.host.clone(),
        port: cfg.port,
        intention: ClientIntention::Login,
    }).await?;

    // 4. Switch to login state
    let mut conn = conn.login();

    // Send Hello with username
    conn.write(ServerboundHello {
        name: profile.name.clone(),
        profile_id: profile.id,
    }).await?;

    // 5. Handle encryption
    let packet = conn.read().await?;
    let encryption_request = match packet {
        ClientboundLoginPacket::Hello(p) => p,
        _ => return Err(eyre::eyre!("Expected encryption request, got {:?}", packet)),
    };

    // Generate secret and encrypt
    let encrypt_result = encrypt(&encryption_request.public_key, &encryption_request.challenge)
        .map_err(|e| eyre::eyre!("Encryption failed: {}", e))?;

    // Join session server
    sessionserver::join(SessionServerJoinOpts {
        access_token: &access_token,
        public_key: &encryption_request.public_key,
        private_key: &encrypt_result.secret_key,
        uuid: &profile.id,
        server_id: &encryption_request.server_id,
        proxy: None,
    }).await.map_err(|e| eyre::eyre!("Session join failed: {:?}", e))?;

    // Send key response
    conn.write(ServerboundKey {
        key_bytes: encrypt_result.encrypted_public_key,
        encrypted_challenge: encrypt_result.encrypted_challenge,
    }).await?;

    // Enable encryption
    conn.set_encryption_key(encrypt_result.secret_key);

    // 6. Handle compression
    let mut compression_threshold = None;
    loop {
        let packet = conn.read().await?;
        match packet {
            ClientboundLoginPacket::LoginCompression(p) => {
                compression_threshold = Some(p.compression_threshold as u32);
                conn.set_compression_threshold(p.compression_threshold);
            }
            ClientboundLoginPacket::LoginFinished(_) => {
                // Send acknowledgement
                conn.write(ServerboundLoginAcknowledged {}).await?;
                break;
            }
            _ => {}
        }
    }

    // 7. Enter configuration state
    let config_conn = conn.config();

    Ok(Upstream {
        connection: config_conn,
        compression_threshold,
        profile,
    })
}