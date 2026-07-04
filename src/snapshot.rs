//! World snapshot — the azalea port of the original's snapshot.js.
//!
//! Caches everything (beyond chunks, which live in session.rs's
//! JoinCache) that a mid-session viewer needs to see the CURRENT world:
//! entities with accumulated positions/metadata/equipment/attributes/
//! effects, the tab list, scoreboards and teams, inventory, health/
//! food/xp, time, held slot, and weather. Frames are stored raw and
//! replayed verbatim; only the fields needed for keying and position
//! accumulation are parsed (typed reads for small packets, leading
//! varints for entity-indexed ones whose bodies we don't care about).

use std::collections::HashMap;
use std::io::Cursor;

use azalea_buf::AzBufVar;
use azalea_core::entity_id::MinecraftEntityId;
use azalea_core::position::Vec3;
use azalea_entity::LookDirection;
use azalea_protocol::packets::game::c_player_info_update::PlayerInfoEntry;
use uuid::Uuid;

use crate::ids::{self, frame_of};
use crate::plugin::Frame;

/// Per-entity ceiling on stored metadata delta frames; oldest dropped.
const MAX_METADATA_FRAMES: usize = 16;
/// Ceiling on inventory slot-delta frames since the last full content.
const MAX_SLOT_FRAMES: usize = 64;

#[derive(Default)]
pub struct WorldSnapshot {
    /// The bot's own uuid. The proxy shows the bot to viewers as the
    /// synthesized reflected entity, so any REAL entity the server sends
    /// carrying this uuid (some servers echo the player's own body) must
    /// be dropped, or it collides with the reflected entity as a
    /// "Duplicate entity UUID".
    bot_uuid: Option<Uuid>,
    entities: HashMap<i32, EntityRecord>,
    /// Merged tab-list entries by uuid.
    players: HashMap<Uuid, PlayerInfoEntry>,
    objectives: HashMap<String, Frame>,
    displays: HashMap<u8, Frame>,
    scores: HashMap<(String, String), Frame>,
    /// Team base frame (Add/Change) plus subsequent membership deltas.
    teams: HashMap<String, Vec<Frame>>,
    time: Option<Frame>,
    experience: Option<Frame>,
    health: Option<Frame>,
    tab_list: Option<Frame>,
    held_slot: Option<Frame>,
    /// Full player-inventory content + slot deltas since.
    inventory_content: Option<Frame>,
    inventory_slots: Vec<Frame>,
    /// Per-slot `set_player_inventory` (1.21.2+): the modern packet the
    /// server uses to populate the player's own inventory and hotbar
    /// outside a container screen. Keyed by slot so the latest wins.
    player_inventory: HashMap<u32, Frame>,
    rain: Option<Frame>,
    rain_level: Option<Frame>,
    thunder_level: Option<Frame>,
    /// Active boss bars by uuid, accumulated from their Add/Update
    /// operations. A viewer joining mid-fight never saw the original
    /// Add, so a later Update* (e.g. UpdateName) for an unknown bar
    /// makes the vanilla client dereference a null map entry and
    /// disconnect — we replay a synthesized Add for each on join.
    boss_bars: HashMap<Uuid, BossBar>,
}

/// Accumulated state of one boss bar, enough to rebuild its Add.
struct BossBar {
    name: azalea_chat::FormattedText,
    progress: f32,
    style: azalea_protocol::packets::game::c_boss_event::Style,
    properties: azalea_protocol::packets::game::c_boss_event::Properties,
}

struct EntityRecord {
    add: Frame,
    uuid: Uuid,
    pos: Vec3,
    /// (y_rot, x_rot) in compact protocol angles.
    look: (i8, i8),
    head_rot: i8,
    on_ground: bool,
    metadata: Vec<Frame>,
    equipment: Option<Frame>,
    attributes: Option<Frame>,
    /// Keyed by the effect's registry id (second leading varint).
    effects: HashMap<u32, Frame>,
}

/// Read the leading varint of a frame body (entity-indexed packets all
/// start with the entity id; container packets with the container id).
fn leading_varint(body: &[u8]) -> Option<u32> {
    u32::azalea_read_var(&mut Cursor::new(body)).ok()
}

/// Read the first two leading varints (entity id + registry id).
fn leading_varint_pair(body: &[u8]) -> Option<(u32, u32)> {
    let mut cur = Cursor::new(body);
    let a = u32::azalea_read_var(&mut cur).ok()?;
    let b = u32::azalea_read_var(&mut cur).ok()?;
    Some((a, b))
}

/// Return a copy of a player-info entry with all property signatures
/// removed. The bot re-serializes forwarded profiles through azalea,
/// and a texture property whose signature is present-but-empty makes
/// the vanilla viewer throw "Bad signature length: got 0 but was
/// expecting 512" and drop the skin. Offline-mode viewers can't verify
/// Mojang signatures anyway, so stripping them (signature = None) makes
/// the client use the skin unsigned instead of erroring.
fn strip_signatures(entry: &PlayerInfoEntry) -> PlayerInfoEntry {
    let mut e = entry.clone();
    let mut props = (*e.profile.properties).clone();
    for v in props.map.values_mut() {
        v.signature = None;
    }
    e.profile.properties = std::sync::Arc::new(props);
    e
}

fn compact_angle(deg: f32) -> i8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0) as i32 as i8
}

fn degrees(compact: i8) -> f32 {
    compact as f32 * 360.0 / 256.0
}

impl WorldSnapshot {
    /// Record the bot's own uuid so its real entity (if the server ever
    /// echoes it) is never stored, keeping it from colliding with the
    /// reflected entity on a viewer's client.
    pub fn set_bot_uuid(&mut self, uuid: Uuid) {
        self.bot_uuid = Some(uuid);
    }

    /// Dimension change: world entities and weather are gone, but the
    /// player list, scoreboards, inventory and vitals persist.
    pub fn on_respawn(&mut self) {
        self.entities.clear();
        self.rain = None;
        self.rain_level = None;
        self.thunder_level = None;
    }

    /// Feed every game-state clientbound frame through here.
    pub fn observe(&mut self, f: &Frame) {
        use azalea_protocol::packets::ProtocolPacket;
        use azalea_protocol::packets::game::ClientboundGamePacket;

        let typed =
            |f: &Frame| ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])).ok();

        match f.packet_id {
            ids::CB_GAME_ADD_ENTITY => {
                if let Some(ClientboundGamePacket::AddEntity(p)) = typed(f) {
                    // never store the bot's own body — the reflected entity
                    // represents it, and a second entity with the same uuid
                    // is a "Duplicate entity UUID" on the viewer
                    if self.bot_uuid == Some(p.uuid) {
                        return;
                    }
                    self.entities.insert(
                        p.id.0,
                        EntityRecord {
                            add: f.clone(),
                            uuid: p.uuid,
                            pos: p.position,
                            look: (p.y_rot, p.x_rot),
                            head_rot: p.y_head_rot,
                            on_ground: false,
                            metadata: Vec::new(),
                            equipment: None,
                            attributes: None,
                            effects: HashMap::new(),
                        },
                    );
                }
            }
            ids::CB_GAME_REMOVE_ENTITIES => {
                if let Some(ClientboundGamePacket::RemoveEntities(p)) = typed(f) {
                    for id in p.entity_ids {
                        self.entities.remove(&id.0);
                    }
                }
            }
            ids::CB_GAME_MOVE_ENTITY_POS => {
                if let Some(ClientboundGamePacket::MoveEntityPos(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.entity_id.0) {
                        e.pos.x += p.delta.xa as f64 / 4096.0;
                        e.pos.y += p.delta.ya as f64 / 4096.0;
                        e.pos.z += p.delta.za as f64 / 4096.0;
                        e.on_ground = p.on_ground;
                    }
                }
            }
            ids::CB_GAME_MOVE_ENTITY_POS_ROT => {
                if let Some(ClientboundGamePacket::MoveEntityPosRot(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.entity_id.0) {
                        e.pos.x += p.delta.xa as f64 / 4096.0;
                        e.pos.y += p.delta.ya as f64 / 4096.0;
                        e.pos.z += p.delta.za as f64 / 4096.0;
                        e.look = (p.look_direction.y_rot, p.look_direction.x_rot);
                        e.on_ground = p.on_ground;
                    }
                }
            }
            ids::CB_GAME_MOVE_ENTITY_ROT => {
                if let Some(ClientboundGamePacket::MoveEntityRot(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.entity_id.0) {
                        e.look = (p.look_direction.y_rot, p.look_direction.x_rot);
                        e.on_ground = p.on_ground;
                    }
                }
            }
            ids::CB_GAME_ENTITY_POSITION_SYNC => {
                if let Some(ClientboundGamePacket::EntityPositionSync(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.id.0) {
                        e.pos = p.values.pos;
                        e.look = (
                            compact_angle(p.values.look_direction.y_rot()),
                            compact_angle(p.values.look_direction.x_rot()),
                        );
                        e.on_ground = p.on_ground;
                    }
                }
            }
            ids::CB_GAME_TELEPORT_ENTITY => {
                if let Some(ClientboundGamePacket::TeleportEntity(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.id.0) {
                        if !p.relative.x && !p.relative.y && !p.relative.z {
                            e.pos = p.change.pos;
                        }
                        e.on_ground = p.on_ground;
                    }
                }
            }
            ids::CB_GAME_ROTATE_HEAD => {
                if let Some(ClientboundGamePacket::RotateHead(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.entity_id.0) {
                        e.head_rot = p.y_head_rot;
                    }
                }
            }
            ids::CB_GAME_SET_ENTITY_DATA => {
                if let Some(id) = leading_varint(&f.body) {
                    if let Some(e) = self.entities.get_mut(&(id as i32)) {
                        if e.metadata.len() >= MAX_METADATA_FRAMES {
                            e.metadata.remove(0);
                        }
                        e.metadata.push(f.clone());
                    }
                }
            }
            ids::CB_GAME_SET_EQUIPMENT => {
                if let Some(id) = leading_varint(&f.body) {
                    if let Some(e) = self.entities.get_mut(&(id as i32)) {
                        e.equipment = Some(f.clone());
                    }
                }
            }
            ids::CB_GAME_UPDATE_ATTRIBUTES => {
                if let Some(id) = leading_varint(&f.body) {
                    if let Some(e) = self.entities.get_mut(&(id as i32)) {
                        e.attributes = Some(f.clone());
                    }
                }
            }
            ids::CB_GAME_UPDATE_MOB_EFFECT => {
                if let Some((id, effect)) = leading_varint_pair(&f.body) {
                    if let Some(e) = self.entities.get_mut(&(id as i32)) {
                        e.effects.insert(effect, f.clone());
                    }
                }
            }
            ids::CB_GAME_REMOVE_MOB_EFFECT => {
                if let Some((id, effect)) = leading_varint_pair(&f.body) {
                    if let Some(e) = self.entities.get_mut(&(id as i32)) {
                        e.effects.remove(&effect);
                    }
                }
            }
            ids::CB_GAME_PLAYER_INFO_UPDATE => {
                if let Some(ClientboundGamePacket::PlayerInfoUpdate(p)) = typed(f) {
                    for entry in p.entries {
                        let uuid = entry.profile.uuid;
                        let merged = self.players.entry(uuid).or_insert_with(|| entry.clone());
                        if p.actions.add_player {
                            merged.profile = entry.profile.clone();
                        }
                        if p.actions.update_game_mode {
                            merged.game_mode = entry.game_mode;
                        }
                        if p.actions.update_listed {
                            merged.listed = entry.listed;
                        }
                        if p.actions.update_latency {
                            merged.latency = entry.latency;
                        }
                        if p.actions.update_display_name {
                            merged.display_name = entry.display_name.clone();
                        }
                        if p.actions.update_list_order {
                            merged.list_order = entry.list_order;
                        }
                        // chat sessions are deliberately not replayed
                        merged.chat_session = None;
                    }
                }
            }
            ids::CB_GAME_PLAYER_INFO_REMOVE => {
                if let Some(ClientboundGamePacket::PlayerInfoRemove(p)) = typed(f) {
                    for uuid in p.profile_ids {
                        self.players.remove(&uuid);
                    }
                }
            }
            ids::CB_GAME_SET_OBJECTIVE => {
                use azalea_protocol::packets::game::c_set_objective::{
                    ClientboundSetObjective, Method,
                };
                if let Some(ClientboundGamePacket::SetObjective(p)) = typed(f) {
                    match p.method {
                        Method::Remove => {
                            self.objectives.remove(&p.objective_name);
                            self.scores.retain(|(obj, _), _| obj != &p.objective_name);
                        }
                        // Store every objective as an Add. A mid-session
                        // viewer needs the Add to CREATE the objective; a
                        // Change on an objective a fresh client doesn't have
                        // is silently dropped, after which every score and
                        // display for it warns "unknown scoreboard
                        // objective". Servers with animated sidebar titles
                        // (Hypixel) send a stream of Change ops, so the last
                        // stored frame is almost always a Change — hence the
                        // normalization. Add and Change carry identical
                        // payloads, so this is lossless.
                        Method::Add {
                            display_name,
                            render_type,
                            number_format,
                        }
                        | Method::Change {
                            display_name,
                            render_type,
                            number_format,
                        } => {
                            let add = frame_of(ClientboundSetObjective {
                                objective_name: p.objective_name.clone(),
                                method: Method::Add {
                                    display_name,
                                    render_type,
                                    number_format,
                                },
                            });
                            self.objectives.insert(p.objective_name, add);
                        }
                    }
                }
            }
            ids::CB_GAME_SET_DISPLAY_OBJECTIVE => {
                if let Some(ClientboundGamePacket::SetDisplayObjective(p)) = typed(f) {
                    self.displays.insert(p.slot as u8, f.clone());
                }
            }
            ids::CB_GAME_SET_SCORE => {
                if let Some(ClientboundGamePacket::SetScore(p)) = typed(f) {
                    self.scores.insert((p.objective_name, p.owner), f.clone());
                }
            }
            ids::CB_GAME_RESET_SCORE => {
                if let Some(ClientboundGamePacket::ResetScore(p)) = typed(f) {
                    match p.objective_name {
                        Some(obj) => {
                            self.scores.remove(&(obj, p.owner));
                        }
                        None => self.scores.retain(|(_, owner), _| owner != &p.owner),
                    }
                }
            }
            ids::CB_GAME_SET_PLAYER_TEAM => {
                use azalea_protocol::packets::game::c_set_player_team::Method;
                if let Some(ClientboundGamePacket::SetPlayerTeam(p)) = typed(f) {
                    match p.method {
                        Method::Remove => {
                            self.teams.remove(&p.name);
                        }
                        Method::Add(_) => {
                            self.teams.insert(p.name, vec![f.clone()]);
                        }
                        _ => {
                            if let Some(frames) = self.teams.get_mut(&p.name) {
                                if frames.len() < 32 {
                                    frames.push(f.clone());
                                }
                            }
                        }
                    }
                }
            }
            ids::CB_GAME_SET_TIME => self.time = Some(f.clone()),
            ids::CB_GAME_SET_EXPERIENCE => self.experience = Some(f.clone()),
            ids::CB_GAME_SET_HEALTH => self.health = Some(f.clone()),
            ids::CB_GAME_TAB_LIST => self.tab_list = Some(f.clone()),
            ids::CB_GAME_SET_HELD_SLOT => self.held_slot = Some(f.clone()),
            ids::CB_GAME_CONTAINER_SET_CONTENT => {
                // container 0 = the player inventory
                if leading_varint(&f.body) == Some(0) {
                    self.inventory_content = Some(f.clone());
                    self.inventory_slots.clear();
                }
            }
            ids::CB_GAME_CONTAINER_SET_SLOT => {
                if leading_varint(&f.body) == Some(0)
                    && self.inventory_slots.len() < MAX_SLOT_FRAMES
                {
                    self.inventory_slots.push(f.clone());
                }
            }
            ids::CB_GAME_SET_PLAYER_INVENTORY => {
                // body starts with the slot index (var u32)
                if let Some(slot) = leading_varint(&f.body) {
                    self.player_inventory.insert(slot, f.clone());
                }
            }
            ids::CB_GAME_BOSS_EVENT => {
                use azalea_protocol::packets::game::c_boss_event::Operation;
                if let Some(ClientboundGamePacket::BossEvent(p)) = typed(f) {
                    match p.operation {
                        Operation::Add(a) => {
                            self.boss_bars.insert(
                                p.id,
                                BossBar {
                                    name: a.name,
                                    progress: a.progress,
                                    style: a.style,
                                    properties: a.properties,
                                },
                            );
                        }
                        Operation::Remove => {
                            self.boss_bars.remove(&p.id);
                        }
                        // Updates only mutate a bar we already Added; a bar
                        // the proxy never saw Added can't be reconstructed
                        // (Add carries required style/properties), so ignore.
                        Operation::UpdateProgress(v) => {
                            if let Some(b) = self.boss_bars.get_mut(&p.id) {
                                b.progress = v;
                            }
                        }
                        Operation::UpdateName(n) => {
                            if let Some(b) = self.boss_bars.get_mut(&p.id) {
                                b.name = n;
                            }
                        }
                        Operation::UpdateStyle(s) => {
                            if let Some(b) = self.boss_bars.get_mut(&p.id) {
                                b.style = s;
                            }
                        }
                        Operation::UpdateProperties(pr) => {
                            if let Some(b) = self.boss_bars.get_mut(&p.id) {
                                b.properties = pr;
                            }
                        }
                    }
                }
            }
            ids::CB_GAME_GAME_EVENT => match f.body.first() {
                // 1 = start rain, 2 = stop rain: latest wins either way
                Some(&1) | Some(&2) => self.rain = Some(f.clone()),
                Some(&7) => self.rain_level = Some(f.clone()),
                Some(&8) => self.thunder_level = Some(f.clone()),
                _ => {}
            },
            _ => {}
        }
    }

    /// Entity id of a visible player by (case-insensitive) name, via
    /// the tab list — for `,spectate <username>`.
    pub fn entity_id_for_player(&self, name: &str) -> Option<i32> {
        let uuid = self
            .players
            .values()
            .find(|e| e.profile.name.eq_ignore_ascii_case(name))
            .map(|e| e.profile.uuid)?;
        self.entities
            .iter()
            .find(|(_, e)| e.uuid == uuid)
            .map(|(&id, _)| id)
    }

    /// Everything a fresh viewer needs after login/position/chunks, in
    /// roughly the original snapshot.js replay order: player list first
    /// (player entities won't render without it), then entities at
    /// their accumulated positions, then vitals, inventory, scoreboards
    /// and ambience.
    pub fn replay(&self) -> Vec<Frame> {
        use azalea_protocol::common::movements::PositionMoveRotation;
        use azalea_protocol::packets::game::c_entity_position_sync::ClientboundEntityPositionSync;
        use azalea_protocol::packets::game::c_player_info_update::{
            ActionEnumSet, ClientboundPlayerInfoUpdate,
        };
        use azalea_protocol::packets::game::c_rotate_head::ClientboundRotateHead;

        let mut q = Vec::new();
        if !self.players.is_empty() {
            q.push(frame_of(ClientboundPlayerInfoUpdate {
                // update_list_order and update_hat MUST stay false:
                // azalea 0.16's hand-written azalea_write sets their
                // bits in the action set but never writes their entry
                // data, producing a packet vanilla can't decode
                // ("Failed to decode player_info_update"). Tab-list
                // ordering is the only casualty. Canary test in ids.rs
                // flags when azalea fixes this.
                actions: ActionEnumSet {
                    add_player: true,
                    initialize_chat: false,
                    update_game_mode: true,
                    update_listed: true,
                    update_latency: true,
                    update_display_name: true,
                    update_list_order: false,
                    update_hat: false,
                },
                entries: self.players.values().map(strip_signatures).collect(),
            }));
        }
        for (id, e) in &self.entities {
            q.push(e.add.clone());
            q.push(frame_of(ClientboundEntityPositionSync {
                id: MinecraftEntityId(*id),
                values: PositionMoveRotation {
                    pos: e.pos,
                    delta: Vec3::default(),
                    look_direction: LookDirection::new(degrees(e.look.0), degrees(e.look.1)),
                },
                on_ground: e.on_ground,
            }));
            q.push(frame_of(ClientboundRotateHead {
                entity_id: MinecraftEntityId(*id),
                y_head_rot: e.head_rot,
            }));
            q.extend(e.metadata.iter().cloned());
            q.extend(e.equipment.iter().cloned());
            q.extend(e.attributes.iter().cloned());
            q.extend(e.effects.values().cloned());
        }
        q.extend(self.time.iter().cloned());
        q.extend(self.experience.iter().cloned());
        q.extend(self.health.iter().cloned());
        q.extend(self.held_slot.iter().cloned());
        q.extend(self.inventory_content.iter().cloned());
        q.extend(self.inventory_slots.iter().cloned());
        q.extend(self.player_inventory.values().cloned());
        q.extend(self.objectives.values().cloned());
        q.extend(self.displays.values().cloned());
        q.extend(self.scores.values().cloned());
        for frames in self.teams.values() {
            q.extend(frames.iter().cloned());
        }
        q.extend(self.tab_list.iter().cloned());
        for (id, b) in &self.boss_bars {
            use azalea_protocol::packets::game::c_boss_event::{
                AddOperation, ClientboundBossEvent, Operation,
            };
            q.push(frame_of(ClientboundBossEvent {
                id: *id,
                operation: Operation::Add(AddOperation {
                    name: b.name.clone(),
                    progress: b.progress,
                    style: b.style.clone(),
                    properties: b.properties.clone(),
                }),
            }));
        }
        q.extend(self.rain.iter().cloned());
        q.extend(self.rain_level.iter().cloned());
        q.extend(self.thunder_level.iter().cloned());
        q
    }

    /// The session player's own HUD state: held slot, inventory, health/
    /// food, and xp. These already reach viewers live and are part of
    /// `replay`, but a spectator's game mode hides the HUD; re-sending
    /// them the moment a viewer switches to a HUD-showing game mode
    /// (`,spectate`) guarantees the hotbar/inventory/bars are populated
    /// immediately rather than waiting for the next server update.
    pub fn self_hud_frames(&self) -> Vec<Frame> {
        let mut q = Vec::new();
        q.extend(self.held_slot.iter().cloned());
        q.extend(self.inventory_content.iter().cloned());
        q.extend(self.inventory_slots.iter().cloned());
        q.extend(self.player_inventory.values().cloned());
        q.extend(self.health.iter().cloned());
        q.extend(self.experience.iter().cloned());
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea_chat::FormattedText;
    use azalea_protocol::packets::game::c_boss_event::{
        AddOperation, BossBarColor, BossBarOverlay, ClientboundBossEvent, Operation, Properties,
        Style,
    };
    use azalea_protocol::packets::game::ClientboundGamePacket;
    use azalea_protocol::packets::ProtocolPacket;
    use std::io::Cursor;

    fn boss_frame(id: Uuid, operation: Operation) -> Frame {
        frame_of(ClientboundBossEvent { id, operation })
    }

    fn add_op(name: &str) -> Operation {
        Operation::Add(AddOperation {
            name: FormattedText::from(name),
            progress: 1.0,
            style: Style {
                color: BossBarColor::Purple,
                overlay: BossBarOverlay::Progress,
            },
            properties: Properties {
                darken_screen: false,
                play_music: false,
                create_world_fog: false,
            },
        })
    }

    /// A viewer that missed the Add must still get one on join, carrying
    /// the latest name from subsequent Update operations — the exact gap
    /// that NPE-crashed the client on a bare UpdateName.
    #[test]
    fn boss_bar_add_then_update_replays_as_add() {
        let id = Uuid::from_u128(7);
        let mut snap = WorldSnapshot::default();
        snap.observe(&boss_frame(id, add_op("Wither")));
        snap.observe(&boss_frame(id, Operation::UpdateName(FormattedText::from("Ender Dragon"))));
        snap.observe(&boss_frame(id, Operation::UpdateProgress(0.5)));

        let bars: Vec<_> = snap
            .replay()
            .into_iter()
            .filter_map(|f| {
                match ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])) {
                    Ok(ClientboundGamePacket::BossEvent(b)) => Some(b),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].id, id);
        match &bars[0].operation {
            Operation::Add(a) => {
                assert_eq!(a.name, FormattedText::from("Ender Dragon"));
                assert_eq!(a.progress, 0.5);
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    /// The bot's own body must never enter the snapshot: the reflected
    /// entity represents it, so a real one with the same uuid would be a
    /// "Duplicate entity UUID" on the viewer.
    #[test]
    fn snapshot_skips_bot_own_entity() {
        use azalea_core::delta::LpVec3;
        use azalea_core::entity_id::MinecraftEntityId;
        use azalea_core::position::Vec3;
        use azalea_protocol::packets::game::c_add_entity::ClientboundAddEntity;
        use azalea_registry::builtin::EntityKind;

        let bot = Uuid::from_u128(0xB07);
        let other = Uuid::from_u128(0x07);
        let add = |id: i32, uuid: Uuid| {
            frame_of(ClientboundAddEntity {
                id: MinecraftEntityId(id),
                uuid,
                entity_type: EntityKind::Player,
                position: Vec3::default(),
                movement: LpVec3::Zero,
                x_rot: 0,
                y_rot: 0,
                y_head_rot: 0,
                data: 0,
            })
        };

        let mut snap = WorldSnapshot::default();
        snap.set_bot_uuid(bot);
        snap.observe(&add(5, bot)); // skipped
        snap.observe(&add(6, other)); // kept

        let uuids: Vec<Uuid> = snap
            .replay()
            .into_iter()
            .filter_map(|f| {
                match ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])) {
                    Ok(ClientboundGamePacket::AddEntity(a)) => Some(a.uuid),
                    _ => None,
                }
            })
            .collect();
        assert!(!uuids.contains(&bot));
        assert!(uuids.contains(&other));
    }

    #[test]
    fn boss_bar_remove_drops_it() {
        let id = Uuid::from_u128(9);
        let mut snap = WorldSnapshot::default();
        snap.observe(&boss_frame(id, add_op("Boss")));
        snap.observe(&boss_frame(id, Operation::Remove));
        let has_boss = snap.replay().into_iter().any(|f| {
            matches!(
                ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])),
                Ok(ClientboundGamePacket::BossEvent(_))
            )
        });
        assert!(!has_boss);
    }

    /// Replayed player-info must carry no property signatures, or an
    /// offline viewer throws "Bad signature length: got 0" and drops the
    /// skin. Value is kept; only the signature is cleared.
    #[test]
    fn strip_signatures_clears_only_the_signature() {
        use azalea_auth::game_profile::{GameProfile, GameProfileProperties, ProfilePropertyValue};
        use std::sync::Arc;

        let mut props = GameProfileProperties::default();
        props.map.insert(
            "textures".to_string(),
            ProfilePropertyValue {
                value: "base64value".to_string(),
                signature: Some("some-signature".to_string()),
            },
        );
        let mut entry = PlayerInfoEntry::default();
        entry.profile = GameProfile {
            uuid: Uuid::from_u128(1),
            name: "Player".to_string(),
            properties: Arc::new(props),
        };

        let stripped = strip_signatures(&entry);
        let v = stripped.profile.properties.map.get("textures").unwrap();
        assert_eq!(v.value, "base64value");
        assert_eq!(v.signature, None);
    }

    /// A viewer that missed the objective's Add and only sees a stream
    /// of Change ops (animated sidebar titles) must still get an Add on
    /// join, or every score/display for it warns "unknown objective".
    #[test]
    fn objective_change_replays_as_add() {
        use azalea_chat::numbers::NumberFormat;
        use azalea_core::objectives::ObjectiveCriteria;
        use azalea_protocol::packets::game::c_set_objective::{
            ClientboundSetObjective, Method,
        };

        let obj = |name: &str, method| {
            frame_of(ClientboundSetObjective {
                objective_name: name.to_string(),
                method,
            })
        };
        let params = || Method::Add {
            display_name: FormattedText::from("Title"),
            render_type: ObjectiveCriteria::Integer,
            number_format: NumberFormat::Blank,
        };
        let change = || Method::Change {
            display_name: FormattedText::from("Title v2"),
            render_type: ObjectiveCriteria::Integer,
            number_format: NumberFormat::Blank,
        };

        let mut snap = WorldSnapshot::default();
        snap.observe(&obj("SBScoreboard", params()));
        snap.observe(&obj("SBScoreboard", change())); // title tick

        let objectives: Vec<_> = snap
            .replay()
            .into_iter()
            .filter_map(|f| {
                match ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])) {
                    Ok(ClientboundGamePacket::SetObjective(o)) => Some(o),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(objectives.len(), 1);
        assert_eq!(objectives[0].objective_name, "SBScoreboard");
        // the replayed frame must be an Add (creates the objective),
        // carrying the latest title
        match &objectives[0].method {
            Method::Add { display_name, .. } => {
                assert_eq!(display_name, &FormattedText::from("Title v2"));
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    /// An Update for a bar the proxy never saw Added can't be rebuilt
    /// (Add carries required style/properties), so it is ignored rather
    /// than replayed as a broken bar.
    #[test]
    fn boss_bar_orphan_update_is_ignored() {
        let mut snap = WorldSnapshot::default();
        snap.observe(&boss_frame(
            Uuid::from_u128(1),
            Operation::UpdateName(FormattedText::from("ghost")),
        ));
        let has_boss = snap.replay().into_iter().any(|f| {
            matches!(
                ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])),
                Ok(ClientboundGamePacket::BossEvent(_))
            )
        });
        assert!(!has_boss);
    }
}
