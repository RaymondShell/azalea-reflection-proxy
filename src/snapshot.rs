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

fn compact_angle(deg: f32) -> i8 {
    (deg.rem_euclid(360.0) / 360.0 * 256.0) as i32 as i8
}

fn degrees(compact: i8) -> f32 {
    compact as f32 * 360.0 / 256.0
}

impl WorldSnapshot {
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
                use azalea_protocol::packets::game::c_set_objective::Method;
                if let Some(ClientboundGamePacket::SetObjective(p)) = typed(f) {
                    if matches!(p.method, Method::Remove) {
                        self.objectives.remove(&p.objective_name);
                        self.scores.retain(|(obj, _), _| obj != &p.objective_name);
                    } else {
                        self.objectives.insert(p.objective_name, f.clone());
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
                entries: self.players.values().cloned().collect(),
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
