//! Phase 2: the replicator. One upstream session shared by many local
//! clients — the azalea equivalent of the original's replicator plugin.
//!
//! Clientbound traffic is broadcast to every attached client; only the
//! controller's serverbound traffic reaches the target server. Viewers'
//! serverbound frames (keepalive replies, teleport confirms, everything)
//! are swallowed here — the server only ever hears the controller.
//!
//! Everything runs through one actor task that owns all mutable state;
//! upstream and client sockets talk to it over channels, so there are no
//! locks on the packet path.
//!
//! Mid-session joins need the config-state registry data the server only
//! sent once, so the session keeps a minimal JoinCache: config frames,
//! the game Login packet, and the last teleport. That's just enough for
//! a viewer to reach the game state — chunks/entities/inventory replay
//! is Phase 3, so until then late viewers spawn over the void and see
//! only live traffic from their join onward.

use std::collections::HashMap;
use std::sync::Arc;

use azalea_protocol::connect::Connection;
use azalea_protocol::packets::config::{ClientboundConfigPacket, ServerboundConfigPacket};
use eyre::Result;
use tokio::sync::{broadcast, mpsc};

use uuid::Uuid;

use crate::ProxyEvent;

/// Behavior knobs forwarded from the builder.
#[derive(Clone)]
pub struct SessionOpts {
    /// Refuse attaches beyond this many simultaneous clients.
    pub max_clients: Option<usize>,
    /// When the controller disconnects, promote the oldest live client
    /// instead of going controllerless (the original's
    /// `alwaysFirstControl`).
    pub always_first_control: bool,
}

use crate::ids;
use crate::local_server::LocalClient;
use crate::plugin::{Frame, Pipeline};
use crate::reflect::{self, BotPose};
use crate::relay::{AzaleaFrameSink, AzaleaFrameSource, FrameSink, FrameSource};
use crate::upstream::Upstream;

pub type ClientId = u32;

pub enum SessionMsg {
    FromUpstream(Frame),
    UpstreamClosed(String),
    FromClient(ClientId, Frame),
    Attach {
        id: ClientId,
        tx: mpsc::Sender<Frame>,
        username: String,
        uuid: Uuid,
    },
    Detach(ClientId),
    /// Once-a-second timer: while controllerless, the stand-in must
    /// report the player's position like an idle client would.
    StandInTick,
}

enum ClientState {
    /// Attached before the session reached game state; replay starts once
    /// the Login packet is cached.
    Parked,
    /// Config replay sent; waiting for the client's serverbound
    /// FinishConfiguration ack.
    Joining,
    /// Receiving live broadcast.
    Live,
}

struct ClientHandle {
    tx: mpsc::Sender<Frame>,
    state: ClientState,
    username: String,
    uuid: Uuid,
    /// Swallow the accept for a proxy-synthesized handoff teleport so it
    /// never reaches the server.
    swallow_next_accept: bool,
    /// `,spectate` camera lock currently active for this viewer.
    camera_on: bool,
}

/// Join cache: config replay + world state a late viewer needs. Chunks
/// are cached raw, keyed by coordinates parsed from the frame body —
/// the vanilla client refuses to leave "Loading terrain..." until the
/// chunk under its feet loads, so chunk replay is a join requirement,
/// not a nicety. Everything else world-shaped (entities, players,
/// scoreboards, inventory, vitals) lives in the WorldSnapshot.
#[derive(Default)]
struct JoinCache {
    config_frames: Vec<Frame>,
    login: Option<Frame>,
    last_position: Option<Frame>,
    respawn: Option<Frame>,
    spawn_pos: Option<Frame>,
    chunk_center: Option<Frame>,
    chunk_radius: Option<Frame>,
    chunks: HashMap<(i32, i32), Frame>,
    world: crate::snapshot::WorldSnapshot,
}

impl JoinCache {
    /// The dimension changed: everything tied to the old world is stale.
    fn on_respawn(&mut self, respawn: Frame) {
        self.respawn = Some(respawn);
        self.last_position = None;
        self.spawn_pos = None;
        self.chunk_center = None;
        self.chunks.clear();
        self.world.on_respawn();
    }

    /// The game-state frames to replay at a viewer that just entered the
    /// game state, in vanilla join order: identity, position, the
    /// chunk-loading handshake, then the world snapshot.
    fn join_frames(&self) -> Vec<Frame> {
        let mut q = Vec::with_capacity(self.chunks.len() + 32);
        q.extend(self.login.iter().cloned());
        q.extend(self.respawn.iter().cloned());
        q.extend(self.spawn_pos.iter().cloned());
        q.extend(self.last_position.iter().cloned());
        q.push(ids::wait_for_chunks_frame());
        q.extend(self.chunk_radius.iter().cloned());
        q.extend(self.chunk_center.iter().cloned());
        q.extend(self.chunks.values().cloned());
        q.extend(self.world.replay());
        q
    }
}

enum UpstreamState {
    Config,
    Game,
}

struct Session {
    pipeline: Arc<Pipeline>,
    upstream_tx: mpsc::Sender<Frame>,
    clients: HashMap<ClientId, ClientHandle>,
    /// Whoever's serverbound traffic reaches the server. None = nobody:
    /// the proxy answers keepalives/teleports itself and the session
    /// player stands AFK.
    controller: Option<ClientId>,
    cache: JoinCache,
    upstream_state: UpstreamState,
    seen_first_game_frame: bool,
    /// The real account's identity, for the reflected entity viewers see.
    bot_uuid: Uuid,
    bot_name: String,
    pose: BotPose,
    /// The session player's actual game mode (from Login / game events),
    /// restored to a client when it acquires control.
    real_game_mode: u8,
    /// Last clientbound abilities frame, replayed to a new controller.
    abilities: Option<Frame>,
    /// Entities were wiped client-side (login/respawn); the reflected
    /// entity must be re-spawned at the next known pose.
    respawn_entity_pending: bool,
    /// Viewers normally don't get the session's PlayerPosition frames
    /// (their camera is free), but after a dimension change they need
    /// exactly one to land in the new world.
    forward_next_position: bool,
    /// The session player's entity id from the Login packet — a
    /// viewer's own client entity, used to detach `,spectate` cameras.
    real_player_id: Option<i32>,
    opts: SessionOpts,
    events: broadcast::Sender<ProxyEvent>,
}

/// Start a session: the controller is already logged in locally, the
/// upstream leg is established. Returns the handle new viewers attach
/// through; when it reports closed, the session is dead and the next
/// connection becomes a fresh controller.
pub fn spawn(
    upstream: Upstream,
    controller: LocalClient,
    controller_id: ClientId,
    pipeline: Arc<Pipeline>,
    opts: SessionOpts,
    events: broadcast::Sender<ProxyEvent>,
) -> mpsc::Sender<SessionMsg> {
    tracing::info!(
        "session start: controller '{}', upstream compression threshold {:?}",
        controller.username,
        upstream.compression_threshold
    );
    let bot_uuid = upstream.profile.id;
    let bot_name = upstream.profile.name.clone();

    for p in &pipeline.plugins {
        p.on_session_start();
    }

    let (msg_tx, msg_rx) = mpsc::channel::<SessionMsg>(1024);
    let upstream_tx = start_upstream_io(upstream, msg_tx.clone());

    let (ctl_tx, ctl_rx) = mpsc::channel::<Frame>(4096);
    let mut clients = HashMap::new();
    clients.insert(
        controller_id,
        ClientHandle {
            tx: ctl_tx,
            state: ClientState::Live,
            username: controller.username.clone(),
            uuid: controller.uuid,
            swallow_next_accept: false,
            camera_on: false,
        },
    );
    start_client_io(controller_id, controller.connection, msg_tx.clone(), ctl_rx);

    let _ = events.send(ProxyEvent::SessionStarted);
    let _ = events.send(ProxyEvent::ClientJoined {
        id: controller_id,
        username: controller.username.clone(),
    });
    let _ = events.send(ProxyEvent::ControlChanged {
        controller: Some((controller_id, controller.username.clone())),
    });

    let session = Session {
        pipeline,
        upstream_tx,
        clients,
        controller: Some(controller_id),
        cache: JoinCache::default(),
        upstream_state: UpstreamState::Config,
        seen_first_game_frame: false,
        bot_uuid,
        bot_name,
        pose: BotPose::default(),
        real_game_mode: 0,
        abilities: None,
        respawn_entity_pending: false,
        forward_next_position: false,
        real_player_id: None,
        opts,
        events,
    };
    // drive the stand-in heartbeat; ends when the session drops msg_rx
    let tick_tx = msg_tx.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            ticker.tick().await;
            if tick_tx.send(SessionMsg::StandInTick).await.is_err() {
                break;
            }
        }
    });

    tokio::spawn(session.run(msg_rx));
    msg_tx
}

/// Attach an already-logged-in local client to a running session as a
/// viewer. The Attach message is sent before the reader task spawns so
/// the session never sees a FromClient for an unknown id.
pub async fn attach_viewer(
    session_tx: &mpsc::Sender<SessionMsg>,
    id: ClientId,
    client: LocalClient,
) -> Result<()> {
    // Sized for the worst-case join replay: a render-distance-32 world
    // is ~4200 cached chunk frames queued in one burst.
    let (tx, rx) = mpsc::channel::<Frame>(8192);
    session_tx
        .send(SessionMsg::Attach {
            id,
            tx,
            username: client.username.clone(),
            uuid: client.uuid,
        })
        .await
        .map_err(|_| eyre::eyre!("session closed while attaching"))?;
    start_client_io(id, client.connection, session_tx.clone(), rx);
    Ok(())
}

/// Game mode and player entity id of the session player, from the
/// Login packet.
fn login_info(f: &Frame) -> (Option<u8>, Option<i32>) {
    use azalea_protocol::packets::ProtocolPacket;
    use azalea_protocol::packets::game::ClientboundGamePacket;
    use std::io::Cursor;
    match ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])) {
        Ok(ClientboundGamePacket::Login(l)) => {
            (Some(l.common.game_type.to_id()), Some(l.player_id.0))
        }
        _ => (None, None),
    }
}

fn start_upstream_io(upstream: Upstream, msg_tx: mpsc::Sender<SessionMsg>) -> mpsc::Sender<Frame> {
    let (read, write) = upstream.connection.into_split_raw();
    let (tx, mut rx) = mpsc::channel::<Frame>(1024);

    tokio::spawn(async move {
        let mut sink = AzaleaFrameSink { writer: write };
        while let Some(f) = rx.recv().await {
            if let Err(e) = sink.write_frame(f).await {
                tracing::warn!("upstream write failed: {e:#}");
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut src = AzaleaFrameSource { reader: read };
        loop {
            match src.read_frame().await {
                Ok(f) => {
                    if msg_tx.send(SessionMsg::FromUpstream(f)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = msg_tx
                        .send(SessionMsg::UpstreamClosed(format!("{e:#}")))
                        .await;
                    break;
                }
            }
        }
    });

    tx
}

fn start_client_io(
    id: ClientId,
    conn: Connection<ServerboundConfigPacket, ClientboundConfigPacket>,
    msg_tx: mpsc::Sender<SessionMsg>,
    mut frame_rx: mpsc::Receiver<Frame>,
) {
    let (read, write) = conn.into_split_raw();

    tokio::spawn(async move {
        let mut sink = AzaleaFrameSink { writer: write };
        while let Some(f) = frame_rx.recv().await {
            if let Err(e) = sink.write_frame(f).await {
                tracing::debug!("client {id} write failed: {e:#}");
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut src = AzaleaFrameSource { reader: read };
        loop {
            match src.read_frame().await {
                Ok(f) => {
                    if msg_tx.send(SessionMsg::FromClient(id, f)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("client {id} read ended: {e:#}");
                    let _ = msg_tx.send(SessionMsg::Detach(id)).await;
                    break;
                }
            }
        }
    });
}

impl Session {
    async fn run(mut self, mut rx: mpsc::Receiver<SessionMsg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                SessionMsg::FromUpstream(frame) => self.on_upstream_frame(frame).await,
                SessionMsg::UpstreamClosed(reason) => {
                    tracing::info!("upstream closed: {reason}");
                    break;
                }
                SessionMsg::FromClient(id, frame) => {
                    if let Err(e) = self.on_client_frame(id, frame).await {
                        tracing::info!("session ending: {e:#}");
                        break;
                    }
                }
                SessionMsg::Attach {
                    id,
                    tx,
                    username,
                    uuid,
                } => self.on_attach(id, tx, username, uuid),
                SessionMsg::Detach(id) => self.drop_client(id, "disconnected"),
                SessionMsg::StandInTick => self.stand_in_tick().await,
            }
            if self.clients.is_empty() {
                tracing::info!("last client left; tearing session down");
                break;
            }
        }
        tracing::info!(
            "session ended ({} client(s) still attached will be dropped)",
            self.clients.len()
        );
        let _ = self.events.send(ProxyEvent::SessionEnded);
    }

    async fn on_upstream_frame(&mut self, frame: Frame) {
        for f in self.pipeline.clientbound(frame) {
            let id = f.packet_id;
            self.observe_clientbound(&f);
            self.stand_in(&f).await;
            self.broadcast(f).await;
            // Login/Respawn reset client game modes — re-spectator every
            // viewer AFTER they processed the reset
            if matches!(self.upstream_state, UpstreamState::Game)
                && (id == ids::CB_GAME_LOGIN || id == ids::CB_GAME_RESPAWN)
            {
                self.reassert_spectators();
            }
        }
    }

    /// With no controller attached, the proxy must keep the session
    /// alive itself: answer keepalives and confirm teleports. (Duplicate
    /// replies are dangerous, so this runs ONLY when controllerless.)
    async fn stand_in(&mut self, f: &Frame) {
        if self.controller.is_some() {
            return;
        }
        let reply = match self.upstream_state {
            UpstreamState::Game => match f.packet_id {
                ids::CB_GAME_KEEP_ALIVE => reflect::keepalive_id(f).map(reflect::keepalive_reply),
                ids::CB_GAME_PLAYER_POSITION => {
                    reflect::teleport_id(f).map(reflect::accept_teleport_frame)
                }
                _ => None,
            },
            UpstreamState::Config => match f.packet_id {
                ids::CB_CONFIG_KEEP_ALIVE => {
                    reflect::keepalive_id(f).map(reflect::config_keepalive_reply)
                }
                _ => None,
            },
        };
        if let Some(r) = reply {
            if self.upstream_tx.send(r).await.is_err() {
                tracing::warn!("stand-in reply failed: upstream writer closed");
            }
        }
    }

    /// Controllerless position heartbeat. An idle vanilla client still
    /// reports its position every second; without this, Hypixel treats
    /// the silent movement stream as a broken connection ("Out of sync,
    /// check your internet connection!") and dumps the player to Limbo
    /// ~15s after the controller releases or disconnects.
    async fn stand_in_tick(&mut self) {
        if self.controller.is_some() || !matches!(self.upstream_state, UpstreamState::Game) {
            return;
        }
        let Some(f) = reflect::idle_move_frame(&self.pose) else {
            return; // pose unknown until the first teleport lands
        };
        if self.upstream_tx.send(f).await.is_err() {
            tracing::warn!("stand-in heartbeat failed: upstream writer closed");
        }
    }

    /// Track upstream protocol state and maintain the join cache. Runs on
    /// post-pipeline frames, so the cache holds what clients actually saw.
    fn observe_clientbound(&mut self, f: &Frame) {
        match self.upstream_state {
            UpstreamState::Config => match f.packet_id {
                ids::CB_CONFIG_FINISH => {
                    self.upstream_state = UpstreamState::Game;
                    self.seen_first_game_frame = false;
                }
                // never replay stale keepalives/pings to a joining viewer
                ids::CB_CONFIG_KEEP_ALIVE | ids::CB_CONFIG_PING => {}
                _ => self.cache.config_frames.push(f.clone()),
            },
            UpstreamState::Game => {
                if !self.seen_first_game_frame {
                    self.seen_first_game_frame = true;
                    if f.packet_id != ids::CB_GAME_LOGIN {
                        // runtime guard for the one id we can't pin in tests
                        tracing::warn!(
                            "first game-state frame has id {} but Login should be {} — \
                             ids.rs may be stale for this azalea version",
                            f.packet_id,
                            ids::CB_GAME_LOGIN
                        );
                    }
                }
                self.cache.world.observe(f);
                match f.packet_id {
                    ids::CB_GAME_LOGIN => {
                        self.cache.login = Some(f.clone());
                        let (mode, pid) = login_info(f);
                        self.real_game_mode = mode.unwrap_or(0);
                        self.real_player_id = pid;
                        // reconfiguration path: Live viewers' entities were
                        // wiped and they need the upcoming position
                        self.respawn_entity_pending = true;
                        self.forward_next_position = true;
                        self.flush_parked();
                    }
                    ids::CB_GAME_PLAYER_POSITION => {
                        self.cache.last_position = Some(f.clone());
                        reflect::apply_server_teleport(&mut self.pose, f);
                    }
                    ids::CB_GAME_RESPAWN => {
                        self.cache.on_respawn(f.clone());
                        // dimension change wipes entities and positions
                        self.pose.pos = None;
                        self.respawn_entity_pending = true;
                        self.forward_next_position = true;
                    }
                    ids::CB_GAME_PLAYER_ABILITIES => {
                        self.abilities = Some(f.clone());
                    }
                    ids::CB_GAME_GAME_EVENT => {
                        // event 3 = the session player's mode changed
                        if f.body.first() == Some(&3) {
                            if let Some(mode) = f.body.get(1..5).and_then(|b| {
                                b.try_into().ok().map(|a| f32::from_be_bytes(a) as u8)
                            }) {
                                self.real_game_mode = mode;
                            }
                        }
                    }
                    ids::CB_GAME_SET_DEFAULT_SPAWN_POSITION => {
                        self.cache.spawn_pos = Some(f.clone());
                    }
                    ids::CB_GAME_SET_CHUNK_CACHE_CENTER => {
                        self.cache.chunk_center = Some(f.clone());
                    }
                    ids::CB_GAME_SET_CHUNK_CACHE_RADIUS => {
                        self.cache.chunk_radius = Some(f.clone());
                    }
                    ids::CB_GAME_LEVEL_CHUNK_WITH_LIGHT => {
                        if let Some(key) = ids::chunk_key(&f.body) {
                            self.cache.chunks.insert(key, f.clone());
                        }
                    }
                    ids::CB_GAME_FORGET_LEVEL_CHUNK => {
                        if let Some(key) = ids::forget_chunk_key(&f.body) {
                            self.cache.chunks.remove(&key);
                        }
                    }
                    ids::CB_GAME_START_CONFIGURATION => {
                        // server is reconfiguring: every cached frame is
                        // stale. Live viewers follow the transition like
                        // the controller does (their acks are swallowed).
                        self.upstream_state = UpstreamState::Config;
                        self.cache = JoinCache::default();
                    }
                    _ => {}
                }
            }
        }
    }

    /// Controller frames go upstream (through the pipeline); viewer
    /// frames are swallowed except join acks. `,commands` work from
    /// anyone and never reach the server.
    async fn on_client_frame(&mut self, id: ClientId, frame: Frame) -> Result<()> {
        // chat commands, from controller and viewers alike
        if matches!(self.upstream_state, UpstreamState::Game) {
            if let Some(text) = reflect::chat_text(&frame) {
                if text.starts_with(',') {
                    self.handle_command(id, text.trim()).await?;
                    return Ok(());
                }
            }
        }

        if Some(id) == self.controller {
            // swallow the accept for a proxy-issued handoff teleport
            if frame.packet_id == ids::SB_GAME_ACCEPT_TELEPORTATION
                && matches!(self.upstream_state, UpstreamState::Game)
            {
                if let Some(c) = self.clients.get_mut(&id) {
                    // only the accept whose id matches the synthesized
                    // handoff teleport is swallowed. A real server teleport
                    // can race the handoff; blindly eating the NEXT accept
                    // would forward the handoff id (garbage to the server)
                    // and drop the real confirm — instant desync.
                    if c.swallow_next_accept
                        && reflect::teleport_id(&frame) == Some(reflect::HANDOFF_TELEPORT_ID)
                    {
                        c.swallow_next_accept = false;
                        return Ok(());
                    }
                }
            }
            // mirror the bot's movement onto the reflected entity before
            // the frame moves on
            if reflect::apply_controller_move(&mut self.pose, &frame) {
                let update = if self.respawn_entity_pending && self.pose.pos.is_some() {
                    self.respawn_entity_pending = false;
                    reflect::spawn_frames(self.bot_uuid, &self.pose)
                } else {
                    reflect::move_frames(&self.pose)
                };
                self.send_to_viewers(&update);
            }
            for f in self.pipeline.serverbound(frame) {
                if self.upstream_tx.send(f).await.is_err() {
                    eyre::bail!("upstream writer closed");
                }
            }
            return Ok(());
        }

        let is_join_ack = matches!(
            self.clients.get(&id),
            Some(c) if matches!(c.state, ClientState::Joining)
        ) && frame.packet_id == ids::SB_CONFIG_FINISH;

        if is_join_ack {
            let mut queue = self.cache.join_frames();
            // spectator kit + the reflected bot (tab entry, then entity)
            let (uuid, name) = {
                let c = self.clients.get(&id).expect("checked above");
                (c.uuid, c.username.clone())
            };
            queue.extend(reflect::spectator_kit(uuid, &name));
            queue.push(reflect::bot_info_frame(self.bot_uuid, &self.bot_name));
            queue.extend(reflect::spawn_frames(self.bot_uuid, &self.pose));
            let c = self.clients.get_mut(&id).expect("checked above");
            let mut ok = true;
            for f in queue {
                if c.tx.try_send(f).is_err() {
                    ok = false;
                    break;
                }
            }
            if ok {
                c.state = ClientState::Live;
                tracing::info!("viewer {id} ('{}') is live", c.username);
            } else {
                self.drop_client(id, "queue overflow during join");
            }
        }
        Ok(())
    }

    /// The `,command` set — port of the original's synchronization
    /// plugin plus its command modules (acquire/release/spectate/
    /// gamemode).
    async fn handle_command(&mut self, id: ClientId, cmd: &str) -> Result<()> {
        tracing::info!("client {id} issued command: {cmd}");
        let (verb, arg) = match cmd.split_once(' ') {
            Some((v, a)) => (v, a.trim()),
            None => (cmd, ""),
        };
        match verb {
            ",acquire" => {
                if Some(id) == self.controller {
                    self.feedback(id, "you already have control");
                    return Ok(());
                }
                // demote whoever had it
                if let Some(old) = self.controller.take() {
                    self.demote_to_spectator(old);
                    self.feedback(old, "your control was taken by another client");
                }
                self.promote_to_controller(id);
                self.feedback(id, "you have control now");
            }
            ",release" => {
                if Some(id) == self.controller {
                    self.controller = None;
                    self.demote_to_spectator(id);
                    self.feedback(id, "control released; proxy is keeping the session alive");
                    let _ = self.events.send(ProxyEvent::ControlChanged { controller: None });
                } else {
                    self.feedback(id, "you are not the controller");
                }
            }
            ",spectate" => self.cmd_spectate(id, arg),
            ",gamemode" | ",gm" => self.cmd_gamemode(id, arg),
            _ => self.feedback(
                id,
                "commands: ,acquire ,release ,spectate [player] ,gamemode <0-3|name>",
            ),
        }
        Ok(())
    }

    /// `,spectate [username]` — lock the camera to a player entity (no
    /// arg = the reflected bot; repeat with no arg to detach).
    fn cmd_spectate(&mut self, id: ClientId, arg: &str) {
        if Some(id) == self.controller {
            self.feedback(id, ",release first — the controller cannot spectate");
            return;
        }
        let camera_on = match self.clients.get(&id) {
            Some(c) => c.camera_on,
            None => return,
        };
        let (target, turning_on) = if arg.is_empty() {
            if camera_on {
                // toggle off: back to the viewer's own client entity
                let Some(own) = self.real_player_id else {
                    self.feedback(id, "cannot detach camera yet (no login seen)");
                    return;
                };
                (own, false)
            } else {
                (reflect::REFLECTED_ENTITY_ID, true)
            }
        } else if arg.eq_ignore_ascii_case(&self.bot_name) {
            (reflect::REFLECTED_ENTITY_ID, true)
        } else {
            match self.cache.world.entity_id_for_player(arg) {
                Some(eid) => (eid, true),
                None => {
                    self.feedback(id, "player not found (not in render distance?)");
                    return;
                }
            }
        };
        if let Some(c) = self.clients.get_mut(&id) {
            let _ = c.tx.try_send(reflect::camera_frame(target));
            c.camera_on = turning_on;
        }
        self.feedback(
            id,
            if turning_on {
                "camera attached — ,spectate again to detach"
            } else {
                "camera detached"
            },
        );
    }

    /// `,gamemode <0-3|name>` — client-side game mode for the issuing
    /// viewer only (nothing reaches the server).
    fn cmd_gamemode(&mut self, id: ClientId, arg: &str) {
        if Some(id) == self.controller {
            self.feedback(id, "not while controlling — it would desync your client");
            return;
        }
        let mode = match arg.to_ascii_lowercase().as_str() {
            "0" | "survival" => 0u8,
            "1" | "creative" => 1,
            "2" | "adventure" => 2,
            "3" | "spectator" => 3,
            _ => {
                self.feedback(id, "usage: ,gamemode <survival|creative|adventure|spectator|0-3>");
                return;
            }
        };
        if let Some(c) = self.clients.get(&id) {
            for f in reflect::gamemode_kit(c.uuid, &c.username, mode) {
                let _ = c.tx.try_send(f);
            }
        }
        self.feedback(id, "client-side game mode updated");
    }

    fn feedback(&mut self, id: ClientId, msg: &str) {
        if let Some(c) = self.clients.get(&id) {
            let _ = c.tx.try_send(reflect::system_chat_frame(msg));
        }
    }

    /// Turn a client into a spectator: kit + the reflected bot entity.
    fn demote_to_spectator(&mut self, id: ClientId) {
        let Some(c) = self.clients.get(&id) else {
            return;
        };
        let mut frames = reflect::spectator_kit(c.uuid, &c.username);
        frames.push(reflect::bot_info_frame(self.bot_uuid, &self.bot_name));
        frames.extend(reflect::spawn_frames(self.bot_uuid, &self.pose));
        let c = self.clients.get(&id).expect("checked above");
        for f in frames {
            let _ = c.tx.try_send(f);
        }
    }

    /// Turn a viewer into the controller: real game mode + abilities
    /// back, ghost entity gone, client teleported onto the bot so its
    /// movement continues from the right place (GrimAC-style alignment).
    fn promote_to_controller(&mut self, id: ClientId) {
        let Some(c) = self.clients.get(&id) else {
            return;
        };
        let mut frames = reflect::controller_kit(c.uuid, &c.username, self.real_game_mode);
        frames.extend(self.abilities.iter().cloned());
        let teleport = reflect::handoff_teleport_frame(&self.pose);
        let has_teleport = teleport.is_some();
        frames.extend(teleport);
        for f in frames {
            let _ = c.tx.try_send(f);
        }
        if let Some(c) = self.clients.get_mut(&id) {
            c.swallow_next_accept = has_teleport;
            c.camera_on = false; // controller drives its own camera
        }
        self.controller = Some(id);
        let username = self
            .clients
            .get(&id)
            .map(|c| c.username.clone())
            .unwrap_or_default();
        let _ = self.events.send(ProxyEvent::ControlChanged {
            controller: Some((id, username)),
        });
    }

    /// Re-send the spectator kit to every Live viewer (after Login /
    /// Respawn broadcasts, which reset client game modes).
    fn reassert_spectators(&mut self) {
        let viewers: Vec<(ClientId, Uuid, String)> = self
            .clients
            .iter()
            .filter(|(&cid, c)| {
                Some(cid) != self.controller && matches!(c.state, ClientState::Live)
            })
            .map(|(&cid, c)| (cid, c.uuid, c.username.clone()))
            .collect();
        for (cid, uuid, name) in viewers {
            let kit = reflect::spectator_kit(uuid, &name);
            if let Some(c) = self.clients.get(&cid) {
                for f in kit {
                    let _ = c.tx.try_send(f);
                }
            }
        }
    }

    fn on_attach(&mut self, id: ClientId, tx: mpsc::Sender<Frame>, username: String, uuid: Uuid) {
        if let Some(max) = self.opts.max_clients {
            if self.clients.len() >= max {
                tracing::info!("refusing viewer {id} ('{username}'): max_clients={max} reached");
                // dropping tx closes the writer and the socket
                return;
            }
        }
        tracing::info!("viewer {id} ('{username}') attaching");
        let _ = self.events.send(ProxyEvent::ClientJoined {
            id,
            username: username.clone(),
        });
        self.clients.insert(
            id,
            ClientHandle {
                tx,
                state: ClientState::Parked,
                username,
                uuid,
                swallow_next_accept: false,
                camera_on: false,
            },
        );
        if self.cache.login.is_some() {
            self.start_replay(id);
        } else {
            tracing::info!("viewer {id} parked until session reaches game state");
        }
    }

    /// Queue the cached config frames + a synthesized FinishConfiguration
    /// at a Parked viewer; it answers with the ack handled in
    /// on_client_frame, which promotes it to Live.
    fn start_replay(&mut self, id: ClientId) {
        let mut frames = self.cache.config_frames.clone();
        frames.push(ids::finish_config_frame());

        let ok = {
            let Some(c) = self.clients.get_mut(&id) else {
                return;
            };
            let mut ok = true;
            for f in frames {
                if c.tx.try_send(f).is_err() {
                    ok = false;
                    break;
                }
            }
            if ok {
                c.state = ClientState::Joining;
            }
            ok
        };
        if !ok {
            self.drop_client(id, "queue overflow during replay");
        }
    }

    fn flush_parked(&mut self) {
        let parked: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| matches!(c.state, ClientState::Parked))
            .map(|(&id, _)| id)
            .collect();
        for id in parked {
            self.start_replay(id);
        }
    }

    /// Should a Live viewer receive this session-player frame? Viewers
    /// are spectators with their own camera and game mode: the session's
    /// teleports would yank their view to the bot, and its abilities /
    /// game-mode changes would undo their spectator state. Only relevant
    /// in the game state — config ids never reach these numbers.
    fn viewers_receive(&mut self, f: &Frame) -> bool {
        if !matches!(self.upstream_state, UpstreamState::Game) {
            return true;
        }
        match f.packet_id {
            ids::CB_GAME_PLAYER_POSITION => {
                if self.forward_next_position {
                    self.forward_next_position = false;
                    true
                } else {
                    false
                }
            }
            ids::CB_GAME_PLAYER_ABILITIES => false,
            // GameEvent body starts with the event byte; 3 = ChangeGameMode
            ids::CB_GAME_GAME_EVENT => f.body.first() != Some(&3),
            _ => true,
        }
    }

    /// Send a clientbound frame to every Live client. The controller gets
    /// backpressure (awaited send — it must not lose frames); viewers get
    /// try_send and are dropped if they can't keep up, so one laggy
    /// spectator can never stall the real session.
    async fn broadcast(&mut self, frame: Frame) {
        let viewers_receive = self.viewers_receive(&frame);
        let mut dead = Vec::new();
        for (&id, c) in self.clients.iter() {
            if !matches!(c.state, ClientState::Live) {
                continue;
            }
            if Some(id) == self.controller {
                if c.tx.send(frame.clone()).await.is_err() {
                    dead.push(id);
                }
            } else if viewers_receive && c.tx.try_send(frame.clone()).is_err() {
                dead.push(id);
            }
        }
        for id in dead {
            self.drop_client(id, "send failed or fell behind");
        }
    }

    /// Push synthesized frames at every Live viewer (never the controller).
    fn send_to_viewers(&mut self, frames: &[Frame]) {
        let mut dead = Vec::new();
        for (&id, c) in self.clients.iter() {
            if Some(id) == self.controller || !matches!(c.state, ClientState::Live) {
                continue;
            }
            for f in frames {
                if c.tx.try_send(f.clone()).is_err() {
                    dead.push(id);
                    break;
                }
            }
        }
        for id in dead {
            self.drop_client(id, "send failed or fell behind");
        }
    }

    fn drop_client(&mut self, id: ClientId, reason: &str) {
        if let Some(c) = self.clients.remove(&id) {
            tracing::info!("client {id} ('{}') dropped: {reason}", c.username);
            let _ = self.events.send(ProxyEvent::ClientLeft {
                id,
                username: c.username,
            });
            // dropping c.tx ends the writer task, which closes the socket
        }
        if self.controller == Some(id) {
            self.controller = None;
            if self.opts.always_first_control {
                // the original's alwaysFirstControl: oldest live client
                // inherits control immediately
                let oldest = self
                    .clients
                    .iter()
                    .filter(|(_, c)| matches!(c.state, ClientState::Live))
                    .map(|(&cid, _)| cid)
                    .min();
                if let Some(next) = oldest {
                    tracing::info!("controller left; promoting client {next} (always_first_control)");
                    self.promote_to_controller(next);
                    self.feedback(next, "previous controller left — you have control now");
                    return;
                }
            }
            // session survives controllerless: stand_in() takes over
            // keepalives/teleports until someone runs ,acquire
            tracing::info!("controller left; session is now controllerless (use ,acquire)");
            let _ = self.events.send(ProxyEvent::ControlChanged { controller: None });
        }
    }
}
