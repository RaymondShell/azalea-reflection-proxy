//! Local leg: the proxy acting as an offline-mode server on localhost.
//! The bot connects here with Account::offline("anything") — no Microsoft
//! auth on this side, because the proxy owns the real session upstream.

use azalea_auth::game_profile::GameProfile;
use azalea_chat::FormattedText;
use azalea_protocol::{
    connect::Connection,
    packets::{
        config::{ClientboundConfigPacket, ServerboundConfigPacket},
        handshake::{ClientboundHandshakePacket, ServerboundHandshakePacket},
        login::{c_login_finished::ClientboundLoginFinished, ServerboundLoginPacket},
        status::{
            c_pong_response::ClientboundPongResponse,
            c_status_response::{ClientboundStatusResponse, Players, Version},
            ServerboundStatusPacket,
        },
        ClientIntention, PROTOCOL_VERSION,
    },
};
use eyre::Result;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

pub struct LocalServerConfig {
    pub bind: String, // e.g. "0.0.0.0:25566"
}

pub struct LocalClient {
    pub username: String,
    /// The uuid the client declared at login (offline/launcher uuid —
    /// distinct from the real account's uuid upstream).
    pub uuid: uuid::Uuid,
    pub connection: Connection<ServerboundConfigPacket, ClientboundConfigPacket>,
}

pub async fn listen(cfg: &LocalServerConfig) -> Result<TcpListener> {
    let listener = TcpListener::bind(&cfg.bind).await?;
    tracing::info!("listening for the bot on {}", cfg.bind);
    Ok(listener)
}

pub async fn accept_login(stream: TcpStream) -> Result<LocalClient> {
    // Wrap incoming TcpStream
    let mut conn: Connection<ServerboundHandshakePacket, ClientboundHandshakePacket> =
        Connection::wrap(stream);

    // 1. Read handshake
    let packet = conn.read().await?;

    let ServerboundHandshakePacket::Intention(intention) = packet;

    match intention.intention {
        ClientIntention::Status => {
            // Handle server list ping
            let mut conn = conn.status();

            // Wait for status request
            let packet = conn.read().await?;
            if let ServerboundStatusPacket::StatusRequest(_) = packet {
                // Send status response with basic info
                conn.write(ClientboundStatusResponse {
                    description: FormattedText::from("Azalea Reflection Proxy"),
                    favicon: None,
                    players: Players {
                        max: 1,
                        online: 0,
                        sample: vec![],
                    },
                    version: Version {
                        name: "Azalea Reflection Proxy".to_string(),
                        protocol: PROTOCOL_VERSION,
                    },
                    enforces_secure_chat: None,
                })
                .await?;
            }

            // Handle ping
            let packet = conn.read().await?;
            if let ServerboundStatusPacket::PingRequest(p) = packet {
                conn.write(ClientboundPongResponse { time: p.time }).await?;
            }

            Err(eyre::eyre!("Client performed status ping and disconnected"))
        }
        ClientIntention::Login => {
            // Status requests from other versions still need a response so
            // the server list can display the incompatibility. Only reject
            // an actual login attempt.
            if intention.protocol_version != PROTOCOL_VERSION {
                return Err(eyre::eyre!(
                    "Protocol version mismatch: client has {}, proxy expects {}",
                    intention.protocol_version,
                    PROTOCOL_VERSION
                ));
            }
            // Continue with login
            let mut conn = conn.login();

            // 2. Read Hello from client
            let packet = conn.read().await?;
            let (username, profile_id) = match packet {
                ServerboundLoginPacket::Hello(p) => (p.name, p.profile_id),
                _ => return Err(eyre::eyre!("Expected Hello packet, got {:?}", packet)),
            };

            // 3. Optional: Set compression (we'll match upstream's threshold if available)
            // For now, skip compression in phase 1 for simplicity

            // 4. Send LoginFinished with the UUID (offline clients send a UUID based on their name)
            let uuid = profile_id;
            conn.write(ClientboundLoginFinished {
                game_profile: GameProfile {
                    uuid,
                    name: username.clone(),
                    properties: Arc::new(Default::default()),
                },
                // mc26.2 added a chat-session UUID here. The proxy owns the
                // real upstream session and doesn't sign chat on this local
                // leg, so a nil UUID is the correct neutral value.
                session_id: uuid::Uuid::nil(),
            })
            .await?;

            // 5. Wait for LoginAcknowledged
            let packet = conn.read().await?;
            match packet {
                ServerboundLoginPacket::LoginAcknowledged(_) => {
                    tracing::debug!("Client acknowledged login");
                }
                _ => return Err(eyre::eyre!("Expected LoginAcknowledged, got {:?}", packet)),
            }

            // 6. Switch to configuration state
            let config_conn = conn.config();

            Ok(LocalClient {
                username,
                uuid,
                connection: config_conn,
            })
        }
        ClientIntention::Transfer => Err(eyre::eyre!("Transfer intention not supported")),
    }
}
