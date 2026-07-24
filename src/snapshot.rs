//! World snapshot — the azalea port of the original's snapshot.js.
//!
//! Caches everything (beyond chunks, which live in session.rs's
//! JoinCache) that a mid-session viewer needs to see the CURRENT world:
//! entities with accumulated positions/metadata/equipment/attributes/
//! effects, the tab list, scoreboards and teams, inventory/HUD state,
//! maps, biome/light deltas, world-border state, time, and weather.
//! State that receives partial updates is normalized so late viewers
//! get one bounded, internally consistent reconstruction; opaque state
//! is retained as raw frames and replayed verbatim.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::time::Instant;

use azalea_buf::AzBufVar;
use azalea_core::entity_id::MinecraftEntityId;
use azalea_core::position::{BlockPos, Vec3};
use azalea_entity::LookDirection;
use azalea_inventory::{components::EquipmentSlot, ItemStack};
use azalea_protocol::packets::game::c_player_info_update::PlayerInfoEntry;
use azalea_registry::builtin::ItemKind;
use uuid::Uuid;

use crate::ids::{self, frame_of};
use crate::plugin::Frame;

#[derive(Default)]
pub struct WorldSnapshot {
    /// The bot's own uuid. The proxy shows the bot to viewers as the
    /// synthesized reflected entity, so any REAL entity the server sends
    /// carrying this uuid (some servers echo the player's own body) must
    /// be dropped, or it collides with the reflected entity as a
    /// "Duplicate entity UUID".
    bot_uuid: Option<Uuid>,
    /// Entity id assigned to the session player by Login. Viewers inherit
    /// this id as their own client entity, so player-only metadata,
    /// attributes and effects must be replayed against it separately from
    /// ordinary world entities.
    player_entity_id: Option<i32>,
    entities: HashMap<i32, EntityRecord>,
    self_metadata: HashMap<u8, azalea_entity::EntityDataValue>,
    self_attributes: HashMap<
        azalea_registry::builtin::Attribute,
        azalea_protocol::packets::game::c_update_attributes::AttributeSnapshot,
    >,
    self_effects: HashMap<u32, Frame>,
    /// Merged tab-list entries by uuid.
    players: HashMap<Uuid, PlayerInfoEntry>,
    objectives: HashMap<String, Frame>,
    displays: HashMap<u8, (String, Frame)>,
    scores: HashMap<(String, String), Frame>,
    teams: HashMap<String, TeamRecord>,
    /// Block changes since each cached full chunk packet. These are
    /// normalized to one latest BlockUpdate per position, so long-running
    /// redstone activity stays bounded by the number of changed blocks.
    block_updates: HashMap<BlockPos, (u64, Frame)>,
    block_entities: HashMap<BlockPos, (u64, Frame)>,
    block_sequence: u64,
    time: Option<Frame>,
    experience: Option<Frame>,
    health: Option<Frame>,
    tab_list: Option<Frame>,
    /// Full player-inventory content + slot deltas since.
    inventory_content: Option<Frame>,
    inventory_slots: HashMap<u16, (u64, Frame)>,
    inventory_sequence: u64,
    /// Normalized player-inventory slots. Indices follow PlayerInventory:
    /// 0..=8 hotbar, 9..=35 main, 36..=39 armor feet..head, 40 offhand.
    inventory_items: HashMap<u32, ItemStack>,
    selected_hotbar_slot: u32,
    cursor_item: Option<Frame>,
    rain: Option<Frame>,
    rain_level: Option<Frame>,
    thunder_level: Option<Frame>,
    /// Active boss bars by uuid, accumulated from their Add/Update
    /// operations. A viewer joining mid-fight never saw the original
    /// Add, so a later Update* (e.g. UpdateName) for an unknown bar
    /// makes the vanilla client dereference a null map entry and
    /// disconnect — we replay a synthesized Add for each on join.
    boss_bars: HashMap<Uuid, BossBar>,
    passengers: HashMap<i32, Frame>,
    entity_links: HashMap<i32, Frame>,
    chunk_biomes:
        HashMap<(i32, i32), azalea_protocol::packets::game::c_chunks_biomes::ChunkBiomeData>,
    light_updates: HashMap<(i32, i32), Frame>,
    maps: HashMap<u32, MapState>,
    cooldowns: HashMap<ItemKind, CooldownState>,
    border: Option<BorderState>,
    difficulty: Option<Frame>,
    game_rules: Option<Frame>,
    simulation_distance: Option<Frame>,
}

/// Accumulated state of one boss bar, enough to rebuild its Add.
struct BossBar {
    name: azalea_chat::FormattedText,
    progress: f32,
    style: azalea_protocol::packets::game::c_boss_event::Style,
    properties: azalea_protocol::packets::game::c_boss_event::Properties,
}

struct CooldownState {
    duration_ticks: u32,
    started: Instant,
}

struct MapState {
    scale: u8,
    locked: bool,
    decorations: Option<Vec<azalea_protocol::packets::game::c_map_item_data::MapDecoration>>,
    colors: Box<[u8; 128 * 128]>,
    has_colors: bool,
}

struct BorderState {
    center_x: f64,
    center_z: f64,
    old_size: f64,
    new_size: f64,
    lerp_started: Instant,
    lerp_millis: u64,
    absolute_max_size: u32,
    warning_blocks: u32,
    warning_time: u32,
}

impl Default for BorderState {
    fn default() -> Self {
        Self {
            center_x: 0.0,
            center_z: 0.0,
            old_size: 59_999_968.0,
            new_size: 59_999_968.0,
            lerp_started: Instant::now(),
            lerp_millis: 0,
            absolute_max_size: 29_999_984,
            warning_blocks: 5,
            warning_time: 15,
        }
    }
}

impl BorderState {
    fn current_size(&self) -> f64 {
        if self.lerp_millis == 0 {
            return self.new_size;
        }
        let elapsed = self.lerp_started.elapsed().as_millis() as f64;
        let progress = (elapsed / self.lerp_millis as f64).clamp(0.0, 1.0);
        self.old_size + (self.new_size - self.old_size) * progress
    }
}

struct TeamRecord {
    parameters: azalea_protocol::packets::game::c_set_player_team::Parameters,
    players: HashSet<String>,
}

struct EntityRecord {
    add: Frame,
    uuid: Uuid,
    pos: Vec3,
    /// (y_rot, x_rot) in compact protocol angles.
    look: (i8, i8),
    head_rot: i8,
    on_ground: bool,
    motion: azalea_core::delta::LpVec3,
    /// Latest value for every metadata index. Keeping raw delta frames with
    /// a length ceiling either leaked memory or eventually discarded fields
    /// that had not changed recently.
    metadata: HashMap<u8, azalea_entity::EntityDataValue>,
    equipment: HashMap<azalea_inventory::components::EquipmentSlot, azalea_inventory::ItemStack>,
    attributes: HashMap<
        azalea_registry::builtin::Attribute,
        azalea_protocol::packets::game::c_update_attributes::AttributeSnapshot,
    >,
    /// Keyed by the effect's registry id (second leading varint).
    effects: HashMap<u32, Frame>,
}

/// Read the first two leading varints (entity id + registry id).
fn leading_varint_pair(body: &[u8]) -> Option<(u32, u32)> {
    let mut cur = Cursor::new(body);
    let a = u32::azalea_read_var(&mut cur).ok()?;
    let b = u32::azalea_read_var(&mut cur).ok()?;
    Some((a, b))
}

/// Convert a container-0 menu slot to the stable PlayerInventory index
/// used by SetPlayerInventory and by reflected-equipment synthesis.
fn player_inventory_slot(menu_slot: u16) -> Option<u32> {
    match menu_slot {
        5 => Some(39), // head
        6 => Some(38), // chest
        7 => Some(37), // legs
        8 => Some(36), // feet
        9..=35 => Some(u32::from(menu_slot)),
        36..=44 => Some(u32::from(menu_slot - 36)),
        45 => Some(40), // offhand
        _ => None,      // crafting result/grid
    }
}

fn remaining_ticks(started: Instant, duration_ticks: u64) -> u64 {
    let elapsed = started.elapsed().as_millis().saturating_mul(20) / 1000;
    duration_ticks.saturating_sub(elapsed.try_into().unwrap_or(u64::MAX))
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

    pub fn set_player_entity_id(&mut self, id: i32) {
        self.player_entity_id = Some(id);
    }

    pub fn selected_hotbar_slot(&self) -> u32 {
        self.selected_hotbar_slot
    }

    pub fn selected_hotbar_frame(&self) -> Frame {
        use azalea_protocol::packets::game::c_set_held_slot::ClientboundSetHeldSlot;
        frame_of(ClientboundSetHeldSlot {
            slot: self.selected_hotbar_slot,
        })
    }

    /// Update the authoritative hotbar selection from a controller's
    /// serverbound SetCarriedItem packet. Servers do not normally echo
    /// that packet, so the proxy must synthesize the clientbound form for
    /// viewers and retain it for late joins.
    pub fn set_selected_hotbar_slot(&mut self, slot: u32) -> Option<Frame> {
        if slot > 8 {
            return None;
        }
        self.selected_hotbar_slot = slot;
        let frame = self.selected_hotbar_frame();
        Some(frame)
    }

    /// Equipment derived from the normalized player inventory for the
    /// synthesized reflected bot.
    pub fn reflected_equipment_frame(&self, entity_id: i32) -> Frame {
        use azalea_protocol::packets::game::c_set_equipment::{
            ClientboundSetEquipment, EquipmentSlots,
        };

        let item = |slot| {
            self.inventory_items
                .get(&slot)
                .cloned()
                .unwrap_or(ItemStack::Empty)
        };
        frame_of(ClientboundSetEquipment {
            entity_id: MinecraftEntityId(entity_id),
            slots: EquipmentSlots {
                slots: vec![
                    (EquipmentSlot::Mainhand, item(self.selected_hotbar_slot)),
                    (EquipmentSlot::Offhand, item(40)),
                    (EquipmentSlot::Feet, item(36)),
                    (EquipmentSlot::Legs, item(37)),
                    (EquipmentSlot::Chest, item(38)),
                    (EquipmentSlot::Head, item(39)),
                ],
            },
        })
    }

    /// Dimension change: world entities and weather are gone, but the
    /// player list, scoreboards, inventory and vitals persist.
    pub fn on_respawn(&mut self, data_to_keep: u8) {
        self.entities.clear();
        self.block_updates.clear();
        self.block_entities.clear();
        self.block_sequence = 0;
        self.passengers.clear();
        self.entity_links.clear();
        self.chunk_biomes.clear();
        self.light_updates.clear();
        self.border = None;
        self.rain = None;
        self.rain_level = None;
        self.thunder_level = None;
        if data_to_keep & 0x01 == 0 {
            self.self_attributes.clear();
        }
        if data_to_keep & 0x02 == 0 {
            self.self_metadata.clear();
            self.self_effects.clear();
        }
    }

    /// Feed every game-state clientbound frame through here.
    pub fn observe(&mut self, f: &Frame) {
        use azalea_protocol::packets::game::ClientboundGamePacket;
        use azalea_protocol::packets::ProtocolPacket;

        let typed = |f: &Frame| {
            ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])).ok()
        };

        match f.packet_id {
            ids::CB_GAME_LEVEL_CHUNK_WITH_LIGHT | ids::CB_GAME_FORGET_LEVEL_CHUNK => {
                let chunk = if f.packet_id == ids::CB_GAME_LEVEL_CHUNK_WITH_LIGHT {
                    ids::chunk_key(&f.body)
                } else {
                    ids::forget_chunk_key(&f.body)
                };
                if let Some((x, z)) = chunk {
                    self.block_updates
                        .retain(|pos, _| (pos.x >> 4, pos.z >> 4) != (x, z));
                    self.block_entities
                        .retain(|pos, _| (pos.x >> 4, pos.z >> 4) != (x, z));
                    self.chunk_biomes.remove(&(x, z));
                    self.light_updates.remove(&(x, z));
                }
            }
            ids::CB_GAME_CHUNKS_BIOMES => {
                if let Some(ClientboundGamePacket::ChunksBiomes(p)) = typed(f) {
                    for data in p.chunk_biome_data {
                        self.chunk_biomes.insert((data.pos.x, data.pos.z), data);
                    }
                }
            }
            ids::CB_GAME_LIGHT_UPDATE => {
                if let Some(ClientboundGamePacket::LightUpdate(p)) = typed(f) {
                    self.light_updates.insert((p.x, p.z), f.clone());
                }
            }
            ids::CB_GAME_BLOCK_UPDATE => {
                if let Some(ClientboundGamePacket::BlockUpdate(p)) = typed(f) {
                    self.block_sequence = self.block_sequence.wrapping_add(1);
                    self.block_updates
                        .insert(p.pos, (self.block_sequence, f.clone()));
                }
            }
            ids::CB_GAME_SECTION_BLOCKS_UPDATE => {
                use azalea_protocol::packets::game::c_block_update::ClientboundBlockUpdate;

                if let Some(ClientboundGamePacket::SectionBlocksUpdate(p)) = typed(f) {
                    for update in p.states {
                        let pos = BlockPos {
                            x: p.section_pos.x * 16 + i32::from(update.pos.x),
                            y: p.section_pos.y * 16 + i32::from(update.pos.y),
                            z: p.section_pos.z * 16 + i32::from(update.pos.z),
                        };
                        self.block_sequence = self.block_sequence.wrapping_add(1);
                        self.block_updates.insert(
                            pos,
                            (
                                self.block_sequence,
                                frame_of(ClientboundBlockUpdate {
                                    pos,
                                    block_state: update.state,
                                }),
                            ),
                        );
                    }
                }
            }
            ids::CB_GAME_BLOCK_ENTITY_DATA => {
                if let Some(ClientboundGamePacket::BlockEntityData(p)) = typed(f) {
                    self.block_sequence = self.block_sequence.wrapping_add(1);
                    self.block_entities
                        .insert(p.pos, (self.block_sequence, f.clone()));
                }
            }
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
                            motion: p.movement,
                            metadata: HashMap::new(),
                            equipment: HashMap::new(),
                            attributes: HashMap::new(),
                            effects: HashMap::new(),
                        },
                    );
                }
            }
            ids::CB_GAME_REMOVE_ENTITIES => {
                if let Some(ClientboundGamePacket::RemoveEntities(p)) = typed(f) {
                    for id in p.entity_ids {
                        self.entities.remove(&id.0);
                        self.passengers.remove(&id.0);
                        self.entity_links.remove(&id.0);
                        self.entity_links.retain(|_, frame| {
                            if let Some(ClientboundGamePacket::SetEntityLink(link)) = typed(frame) {
                                link.dest_id != id
                            } else {
                                false
                            }
                        });
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
                        e.pos = Vec3::new(
                            if p.relative.x {
                                e.pos.x + p.change.pos.x
                            } else {
                                p.change.pos.x
                            },
                            if p.relative.y {
                                e.pos.y + p.change.pos.y
                            } else {
                                p.change.pos.y
                            },
                            if p.relative.z {
                                e.pos.z + p.change.pos.z
                            } else {
                                p.change.pos.z
                            },
                        );
                        let y_rot = if p.relative.y_rot {
                            degrees(e.look.0) + p.change.look_direction.y_rot()
                        } else {
                            p.change.look_direction.y_rot()
                        };
                        let x_rot = if p.relative.x_rot {
                            degrees(e.look.1) + p.change.look_direction.x_rot()
                        } else {
                            p.change.look_direction.x_rot()
                        };
                        e.look = (compact_angle(y_rot), compact_angle(x_rot));
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
                if let Some(ClientboundGamePacket::SetEntityData(p)) = typed(f) {
                    if self.player_entity_id == Some(p.id.0) {
                        for item in p.packed_items.0 {
                            self.self_metadata.insert(item.index, item.value);
                        }
                    } else if let Some(e) = self.entities.get_mut(&p.id.0) {
                        for item in p.packed_items.0 {
                            e.metadata.insert(item.index, item.value);
                        }
                    }
                }
            }
            ids::CB_GAME_SET_EQUIPMENT => {
                if let Some(ClientboundGamePacket::SetEquipment(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.entity_id.0) {
                        for (slot, item) in p.slots.slots {
                            e.equipment.insert(slot, item);
                        }
                    }
                }
            }
            ids::CB_GAME_SET_ENTITY_MOTION => {
                if let Some(ClientboundGamePacket::SetEntityMotion(p)) = typed(f) {
                    if let Some(e) = self.entities.get_mut(&p.id.0) {
                        e.motion = p.delta;
                    }
                }
            }
            ids::CB_GAME_SET_ENTITY_LINK => {
                if let Some(ClientboundGamePacket::SetEntityLink(p)) = typed(f) {
                    if p.dest_id.0 == 0 {
                        self.entity_links.remove(&p.source_id.0);
                    } else {
                        self.entity_links.insert(p.source_id.0, f.clone());
                    }
                }
            }
            ids::CB_GAME_UPDATE_ATTRIBUTES => {
                if let Some(ClientboundGamePacket::UpdateAttributes(p)) = typed(f) {
                    if self.player_entity_id == Some(p.entity_id.0) {
                        for value in p.values {
                            self.self_attributes.insert(value.attribute, value);
                        }
                    } else if let Some(e) = self.entities.get_mut(&p.entity_id.0) {
                        for value in p.values {
                            e.attributes.insert(value.attribute, value);
                        }
                    }
                }
            }
            ids::CB_GAME_UPDATE_MOB_EFFECT => {
                if let Some((id, effect)) = leading_varint_pair(&f.body) {
                    if self.player_entity_id == Some(id as i32) {
                        self.self_effects.insert(effect, f.clone());
                    } else if let Some(e) = self.entities.get_mut(&(id as i32)) {
                        e.effects.insert(effect, f.clone());
                    }
                }
            }
            ids::CB_GAME_REMOVE_MOB_EFFECT => {
                if let Some((id, effect)) = leading_varint_pair(&f.body) {
                    if self.player_entity_id == Some(id as i32) {
                        self.self_effects.remove(&effect);
                    } else if let Some(e) = self.entities.get_mut(&(id as i32)) {
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
                        if p.actions.update_hat {
                            merged.update_hat = entry.update_hat;
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
                            self.displays.retain(|_, (obj, _)| obj != &p.objective_name);
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
                    if p.objective_name.is_empty() {
                        self.displays.remove(&(p.slot as u8));
                    } else {
                        self.displays
                            .insert(p.slot as u8, (p.objective_name, f.clone()));
                    }
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
                        Method::Add((parameters, players)) => {
                            self.teams.insert(
                                p.name,
                                TeamRecord {
                                    parameters,
                                    players: players.into_iter().collect(),
                                },
                            );
                        }
                        Method::Change(parameters) => {
                            if let Some(team) = self.teams.get_mut(&p.name) {
                                team.parameters = parameters;
                            }
                        }
                        Method::Join(players) => {
                            if let Some(team) = self.teams.get_mut(&p.name) {
                                team.players.extend(players);
                            }
                        }
                        Method::Leave(players) => {
                            if let Some(team) = self.teams.get_mut(&p.name) {
                                for player in players {
                                    team.players.remove(&player);
                                }
                            }
                        }
                    }
                }
            }
            ids::CB_GAME_SET_PASSENGERS => {
                if let Some(ClientboundGamePacket::SetPassengers(p)) = typed(f) {
                    self.passengers.insert(p.vehicle.0, f.clone());
                }
            }
            ids::CB_GAME_SET_TIME => self.time = Some(f.clone()),
            ids::CB_GAME_SET_EXPERIENCE => self.experience = Some(f.clone()),
            ids::CB_GAME_SET_HEALTH => self.health = Some(f.clone()),
            ids::CB_GAME_TAB_LIST => self.tab_list = Some(f.clone()),
            ids::CB_GAME_SET_HELD_SLOT => {
                if let Some(ClientboundGamePacket::SetHeldSlot(p)) = typed(f) {
                    if p.slot <= 8 {
                        self.selected_hotbar_slot = p.slot;
                    }
                }
            }
            ids::CB_GAME_CONTAINER_SET_CONTENT => {
                // container 0 = the player inventory
                if let Some(ClientboundGamePacket::ContainerSetContent(p)) = typed(f) {
                    if p.container_id == 0 {
                        self.inventory_content = Some(f.clone());
                        self.inventory_slots.clear();
                        self.inventory_sequence = 0;
                        self.inventory_items.clear();
                        for (slot, item) in p.items.into_iter().enumerate() {
                            if let Ok(slot) = u16::try_from(slot) {
                                if let Some(player_slot) = player_inventory_slot(slot) {
                                    self.inventory_items.insert(player_slot, item);
                                }
                            }
                        }
                    }
                }
            }
            ids::CB_GAME_CONTAINER_SET_SLOT => {
                if let Some(ClientboundGamePacket::ContainerSetSlot(p)) = typed(f) {
                    if p.container_id == 0 || p.container_id == -2 {
                        self.inventory_sequence = self.inventory_sequence.wrapping_add(1);
                        self.inventory_slots
                            .insert(p.slot, (self.inventory_sequence, f.clone()));
                        if let Some(player_slot) = player_inventory_slot(p.slot) {
                            self.inventory_items.insert(player_slot, p.item_stack);
                        }
                    } else if p.container_id == -1 {
                        self.cursor_item = Some(f.clone());
                    }
                }
            }
            ids::CB_GAME_SET_PLAYER_INVENTORY => {
                if let Some(ClientboundGamePacket::SetPlayerInventory(p)) = typed(f) {
                    self.inventory_items.insert(p.slot, p.contents);
                }
            }
            ids::CB_GAME_SET_CURSOR_ITEM => self.cursor_item = Some(f.clone()),
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
            ids::CB_GAME_MAP_ITEM_DATA => {
                if let Some(ClientboundGamePacket::MapItemData(p)) = typed(f) {
                    let state = self.maps.entry(p.map_id).or_insert_with(|| MapState {
                        scale: p.scale,
                        locked: p.locked,
                        decorations: None,
                        colors: Box::new([0; 128 * 128]),
                        has_colors: false,
                    });
                    state.scale = p.scale;
                    state.locked = p.locked;
                    if let Some(decorations) = p.decorations {
                        state.decorations = Some(decorations);
                    }
                    if let Some(patch) = p.color_patch.0 {
                        let width = usize::from(patch.width);
                        let height = usize::from(patch.height);
                        let start_x = usize::from(patch.start_x);
                        let start_y = usize::from(patch.start_y);
                        for row in 0..height {
                            for column in 0..width {
                                let Some(&color) = patch.map_colors.get(row * width + column)
                                else {
                                    continue;
                                };
                                let x = start_x + column;
                                let y = start_y + row;
                                if x < 128 && y < 128 {
                                    state.colors[y * 128 + x] = color;
                                    state.has_colors = true;
                                }
                            }
                        }
                    }
                }
            }
            ids::CB_GAME_COOLDOWN => {
                if let Some(ClientboundGamePacket::Cooldown(p)) = typed(f) {
                    if p.duration == 0 {
                        self.cooldowns.remove(&p.item);
                    } else {
                        self.cooldowns.insert(
                            p.item,
                            CooldownState {
                                duration_ticks: p.duration,
                                started: Instant::now(),
                            },
                        );
                    }
                }
            }
            ids::CB_GAME_INITIALIZE_BORDER => {
                if let Some(ClientboundGamePacket::InitializeBorder(p)) = typed(f) {
                    self.border = Some(BorderState {
                        center_x: p.new_center_x,
                        center_z: p.new_center_z,
                        old_size: p.old_size,
                        new_size: p.new_size,
                        lerp_started: Instant::now(),
                        lerp_millis: p.lerp_time,
                        absolute_max_size: p.new_absolute_max_size,
                        warning_blocks: p.warning_blocks,
                        warning_time: p.warning_time,
                    });
                }
            }
            ids::CB_GAME_SET_BORDER_CENTER => {
                if let Some(ClientboundGamePacket::SetBorderCenter(p)) = typed(f) {
                    let border = self.border.get_or_insert_with(BorderState::default);
                    border.center_x = p.new_center_x;
                    border.center_z = p.new_center_z;
                }
            }
            ids::CB_GAME_SET_BORDER_SIZE => {
                if let Some(ClientboundGamePacket::SetBorderSize(p)) = typed(f) {
                    let border = self.border.get_or_insert_with(BorderState::default);
                    border.old_size = p.size;
                    border.new_size = p.size;
                    border.lerp_millis = 0;
                    border.lerp_started = Instant::now();
                }
            }
            ids::CB_GAME_SET_BORDER_LERP_SIZE => {
                if let Some(ClientboundGamePacket::SetBorderLerpSize(p)) = typed(f) {
                    let border = self.border.get_or_insert_with(BorderState::default);
                    border.old_size = p.old_size;
                    border.new_size = p.new_size;
                    border.lerp_millis = p.lerp_time;
                    border.lerp_started = Instant::now();
                }
            }
            ids::CB_GAME_SET_BORDER_WARNING_DELAY => {
                if let Some(ClientboundGamePacket::SetBorderWarningDelay(p)) = typed(f) {
                    self.border
                        .get_or_insert_with(BorderState::default)
                        .warning_time = p.warning_delay;
                }
            }
            ids::CB_GAME_SET_BORDER_WARNING_DISTANCE => {
                if let Some(ClientboundGamePacket::SetBorderWarningDistance(p)) = typed(f) {
                    self.border
                        .get_or_insert_with(BorderState::default)
                        .warning_blocks = p.warning_blocks;
                }
            }
            ids::CB_GAME_CHANGE_DIFFICULTY => self.difficulty = Some(f.clone()),
            ids::CB_GAME_GAME_RULE_VALUES => self.game_rules = Some(f.clone()),
            ids::CB_GAME_SET_SIMULATION_DISTANCE => {
                self.simulation_distance = Some(f.clone());
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
    pub fn player_target(&self, name: &str) -> Option<(Uuid, i32)> {
        let uuid = self
            .players
            .values()
            .find(|e| e.profile.name.eq_ignore_ascii_case(name))
            .map(|e| e.profile.uuid)?;
        let entity_id = self
            .entities
            .iter()
            .find(|(_, e)| e.uuid == uuid)
            .map(|(&id, _)| id)?;
        Some((uuid, entity_id))
    }

    pub fn player_name(&self, uuid: Uuid) -> Option<&str> {
        self.players
            .get(&uuid)
            .map(|entry| entry.profile.name.as_str())
    }

    /// Everything a fresh viewer needs after login/position/chunks, in
    /// roughly the original snapshot.js replay order: player list first
    /// (player entities won't render without it), then entities at
    /// their accumulated positions, then vitals, inventory, scoreboards
    /// and ambience.
    pub fn replay(&self) -> Vec<Frame> {
        use azalea_entity::{EntityDataItem, EntityMetadataItems};
        use azalea_protocol::common::movements::PositionMoveRotation;
        use azalea_protocol::packets::game::c_entity_position_sync::ClientboundEntityPositionSync;
        use azalea_protocol::packets::game::c_player_info_update::{
            ActionEnumSet, ClientboundPlayerInfoUpdate,
        };
        use azalea_protocol::packets::game::c_rotate_head::ClientboundRotateHead;
        use azalea_protocol::packets::game::c_set_entity_data::ClientboundSetEntityData;
        use azalea_protocol::packets::game::c_set_equipment::{
            ClientboundSetEquipment, EquipmentSlots,
        };
        use azalea_protocol::packets::game::c_set_player_team::{ClientboundSetPlayerTeam, Method};
        use azalea_protocol::packets::game::c_update_attributes::ClientboundUpdateAttributes;

        let mut q = Vec::new();
        q.extend(self.chunk_biome_frames());
        q.extend(self.sorted_chunk_frames(&self.light_updates));
        q.extend(self.block_change_frames());
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
                    delta: e.motion.to_vec3(),
                    look_direction: LookDirection::new(degrees(e.look.0), degrees(e.look.1)),
                },
                on_ground: e.on_ground,
            }));
            q.push(frame_of(ClientboundRotateHead {
                entity_id: MinecraftEntityId(*id),
                y_head_rot: e.head_rot,
            }));
            if !e.metadata.is_empty() {
                q.push(frame_of(ClientboundSetEntityData {
                    id: MinecraftEntityId(*id),
                    packed_items: EntityMetadataItems(
                        e.metadata
                            .iter()
                            .map(|(&index, value)| EntityDataItem {
                                index,
                                value: value.clone(),
                            })
                            .collect(),
                    ),
                }));
            }
            if !e.equipment.is_empty() {
                q.push(frame_of(ClientboundSetEquipment {
                    entity_id: MinecraftEntityId(*id),
                    slots: EquipmentSlots {
                        slots: e
                            .equipment
                            .iter()
                            .map(|(&slot, item)| (slot, item.clone()))
                            .collect(),
                    },
                }));
            }
            if !e.attributes.is_empty() {
                q.push(frame_of(ClientboundUpdateAttributes {
                    entity_id: MinecraftEntityId(*id),
                    values: e.attributes.values().cloned().collect(),
                }));
            }
            q.extend(e.effects.values().cloned());
        }
        q.extend(self.passengers.values().cloned());
        q.extend(self.entity_links.values().cloned());
        q.extend(self.difficulty.iter().cloned());
        q.extend(self.game_rules.iter().cloned());
        q.extend(self.simulation_distance.iter().cloned());
        q.extend(self.border_frame());
        q.extend(self.time.iter().cloned());
        q.extend(self.self_state_frames());
        q.extend(self.experience.iter().cloned());
        q.extend(self.health.iter().cloned());
        q.push(self.selected_hotbar_frame());
        q.extend(self.inventory_content.iter().cloned());
        q.extend(self.inventory_slot_frames());
        q.extend(self.player_inventory_frames());
        q.extend(self.cursor_item.iter().cloned());
        q.extend(self.map_frames());
        q.extend(self.cooldown_frames());

        let mut objective_names: Vec<_> = self.objectives.keys().collect();
        objective_names.sort_unstable();
        q.extend(
            objective_names
                .into_iter()
                .filter_map(|name| self.objectives.get(name).cloned()),
        );

        let mut team_names: Vec<_> = self.teams.keys().collect();
        team_names.sort_unstable();
        for name in team_names {
            let team = &self.teams[name];
            let mut players: Vec<_> = team.players.iter().cloned().collect();
            players.sort_unstable();
            q.push(frame_of(ClientboundSetPlayerTeam {
                name: name.clone(),
                method: Method::Add((team.parameters.clone(), players)),
            }));
        }

        let mut displays: Vec<_> = self.displays.iter().collect();
        displays.sort_unstable_by_key(|(&slot, _)| slot);
        q.extend(displays.into_iter().map(|(_, (_, frame))| frame.clone()));

        let mut scores: Vec<_> = self.scores.iter().collect();
        scores.sort_unstable_by(|((obj_a, owner_a), _), ((obj_b, owner_b), _)| {
            obj_a.cmp(obj_b).then_with(|| owner_a.cmp(owner_b))
        });
        q.extend(scores.into_iter().map(|(_, frame)| frame.clone()));
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
        q.extend(self.self_state_frames());
        q.push(self.selected_hotbar_frame());
        q.extend(self.inventory_content.iter().cloned());
        q.extend(self.inventory_slot_frames());
        q.extend(self.player_inventory_frames());
        q.extend(self.cursor_item.iter().cloned());
        q.extend(self.health.iter().cloned());
        q.extend(self.experience.iter().cloned());
        q.extend(self.cooldown_frames());
        q
    }

    /// Player metadata, attributes, and effect particles retargeted to
    /// the synthesized reflected bot for viewers joining mid-session.
    pub fn reflected_self_state_frames(&self, entity_id: i32) -> Vec<Frame> {
        let mut q = self.self_metadata_attribute_frames(entity_id);
        let Some(source_entity_id) = self.player_entity_id else {
            return q;
        };
        let mut effect_ids: Vec<_> = self.self_effects.keys().copied().collect();
        effect_ids.sort_unstable();
        q.extend(effect_ids.into_iter().filter_map(|id| {
            crate::reflect::retarget_self_visual(self.self_effects.get(&id)?, source_entity_id)
        }));
        q
    }

    fn self_state_frames(&self) -> Vec<Frame> {
        let Some(entity_id) = self.player_entity_id else {
            return Vec::new();
        };
        let mut q = self.self_metadata_attribute_frames(entity_id);
        let mut effect_ids: Vec<_> = self.self_effects.keys().copied().collect();
        effect_ids.sort_unstable();
        q.extend(
            effect_ids
                .into_iter()
                .filter_map(|id| self.self_effects.get(&id).cloned()),
        );
        q
    }

    fn self_metadata_attribute_frames(&self, entity_id: i32) -> Vec<Frame> {
        use azalea_entity::{EntityDataItem, EntityMetadataItems};
        use azalea_protocol::packets::game::c_set_entity_data::ClientboundSetEntityData;
        use azalea_protocol::packets::game::c_update_attributes::ClientboundUpdateAttributes;

        let mut q = Vec::new();
        if !self.self_metadata.is_empty() {
            let mut metadata: Vec<_> = self.self_metadata.iter().collect();
            metadata.sort_unstable_by_key(|(&index, _)| index);
            q.push(frame_of(ClientboundSetEntityData {
                id: MinecraftEntityId(entity_id),
                packed_items: EntityMetadataItems(
                    metadata
                        .into_iter()
                        .map(|(&index, value)| EntityDataItem {
                            index,
                            value: value.clone(),
                        })
                        .collect(),
                ),
            }));
        }
        if !self.self_attributes.is_empty() {
            let mut values: Vec<_> = self.self_attributes.values().cloned().collect();
            values.sort_unstable_by_key(|value| value.attribute);
            q.push(frame_of(ClientboundUpdateAttributes {
                entity_id: MinecraftEntityId(entity_id),
                values,
            }));
        }
        q
    }

    fn chunk_biome_frames(&self) -> Vec<Frame> {
        use azalea_protocol::packets::game::c_chunks_biomes::ClientboundChunksBiomes;

        if self.chunk_biomes.is_empty() {
            return Vec::new();
        }
        let mut chunks: Vec<_> = self.chunk_biomes.iter().collect();
        chunks.sort_unstable_by_key(|(&(x, z), _)| (x, z));
        chunks
            .chunks(256)
            .map(|batch| {
                frame_of(ClientboundChunksBiomes {
                    chunk_biome_data: batch.iter().map(|(_, data)| (*data).clone()).collect(),
                })
            })
            .collect()
    }

    fn sorted_chunk_frames(&self, frames: &HashMap<(i32, i32), Frame>) -> Vec<Frame> {
        let mut frames: Vec<_> = frames.iter().collect();
        frames.sort_unstable_by_key(|(&(x, z), _)| (x, z));
        frames.into_iter().map(|(_, frame)| frame.clone()).collect()
    }

    fn map_frames(&self) -> Vec<Frame> {
        use azalea_protocol::packets::game::c_map_item_data::{
            ClientboundMapItemData, MapPatch, OptionalMapPatch,
        };

        let mut ids: Vec<_> = self.maps.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|map_id| {
                let map = self.maps.get(&map_id)?;
                Some(frame_of(ClientboundMapItemData {
                    map_id,
                    scale: map.scale,
                    locked: map.locked,
                    decorations: map.decorations.clone(),
                    color_patch: OptionalMapPatch(map.has_colors.then(|| MapPatch {
                        width: 128,
                        height: 128,
                        start_x: 0,
                        start_y: 0,
                        map_colors: map.colors.to_vec(),
                    })),
                }))
            })
            .collect()
    }

    fn cooldown_frames(&self) -> Vec<Frame> {
        use azalea_protocol::packets::game::c_cooldown::ClientboundCooldown;

        self.cooldowns
            .iter()
            .filter_map(|(&item, cooldown)| {
                let remaining =
                    remaining_ticks(cooldown.started, u64::from(cooldown.duration_ticks));
                (remaining > 0).then(|| {
                    frame_of(ClientboundCooldown {
                        item,
                        duration: remaining.min(u64::from(u32::MAX)) as u32,
                    })
                })
            })
            .collect()
    }

    fn border_frame(&self) -> Option<Frame> {
        use azalea_protocol::packets::game::c_initialize_border::ClientboundInitializeBorder;

        let border = self.border.as_ref()?;
        let elapsed = border.lerp_started.elapsed().as_millis();
        let remaining = border
            .lerp_millis
            .saturating_sub(elapsed.try_into().unwrap_or(u64::MAX));
        let current_size = border.current_size();
        Some(frame_of(ClientboundInitializeBorder {
            new_center_x: border.center_x,
            new_center_z: border.center_z,
            old_size: current_size,
            new_size: if remaining == 0 {
                current_size
            } else {
                border.new_size
            },
            lerp_time: remaining,
            new_absolute_max_size: border.absolute_max_size,
            warning_blocks: border.warning_blocks,
            warning_time: border.warning_time,
        }))
    }

    fn inventory_slot_frames(&self) -> Vec<Frame> {
        let mut frames: Vec<_> = self.inventory_slots.values().collect();
        frames.sort_unstable_by_key(|(sequence, _)| *sequence);
        frames.into_iter().map(|(_, frame)| frame.clone()).collect()
    }

    fn player_inventory_frames(&self) -> Vec<Frame> {
        use azalea_protocol::packets::game::c_set_player_inventory::ClientboundSetPlayerInventory;

        let mut items: Vec<_> = self.inventory_items.iter().collect();
        items.sort_unstable_by_key(|(&slot, _)| slot);
        items
            .into_iter()
            .map(|(&slot, contents)| {
                frame_of(ClientboundSetPlayerInventory {
                    slot,
                    contents: contents.clone(),
                })
            })
            .collect()
    }

    fn block_change_frames(&self) -> Vec<Frame> {
        let mut frames: Vec<_> = self
            .block_updates
            .values()
            .chain(self.block_entities.values())
            .collect();
        frames.sort_unstable_by_key(|(sequence, _)| *sequence);
        frames.into_iter().map(|(_, frame)| frame.clone()).collect()
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

    fn entity_frame(id: i32, uuid: Uuid) -> Frame {
        use azalea_core::delta::LpVec3;
        use azalea_core::entity_id::MinecraftEntityId;
        use azalea_protocol::packets::game::c_add_entity::ClientboundAddEntity;
        use azalea_registry::builtin::EntityKind;

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
    }

    /// A viewer that missed the Add must still get one on join, carrying
    /// the latest name from subsequent Update operations — the exact gap
    /// that NPE-crashed the client on a bare UpdateName.
    #[test]
    fn boss_bar_add_then_update_replays_as_add() {
        let id = Uuid::from_u128(7);
        let mut snap = WorldSnapshot::default();
        snap.observe(&boss_frame(id, add_op("Wither")));
        snap.observe(&boss_frame(
            id,
            Operation::UpdateName(FormattedText::from("Ender Dragon")),
        ));
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
        let bot = Uuid::from_u128(0xB07);
        let other = Uuid::from_u128(0x07);

        let mut snap = WorldSnapshot::default();
        snap.set_bot_uuid(bot);
        snap.observe(&entity_frame(5, bot)); // skipped
        snap.observe(&entity_frame(6, other)); // kept

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
    fn metadata_updates_merge_by_index() {
        use azalea_core::entity_id::MinecraftEntityId;
        use azalea_entity::{EntityDataItem, EntityDataValue, EntityMetadataItems};
        use azalea_protocol::packets::game::c_set_entity_data::ClientboundSetEntityData;

        let mut snap = WorldSnapshot::default();
        snap.observe(&entity_frame(7, Uuid::from_u128(7)));
        let metadata = |items| {
            frame_of(ClientboundSetEntityData {
                id: MinecraftEntityId(7),
                packed_items: EntityMetadataItems(items),
            })
        };
        snap.observe(&metadata(vec![EntityDataItem {
            index: 0,
            value: EntityDataValue::Byte(1),
        }]));
        snap.observe(&metadata(vec![
            EntityDataItem {
                index: 0,
                value: EntityDataValue::Byte(2),
            },
            EntityDataItem {
                index: 1,
                value: EntityDataValue::Int(3),
            },
        ]));

        let packet = snap.replay().into_iter().find_map(|f| {
            match ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])) {
                Ok(ClientboundGamePacket::SetEntityData(p)) => Some(p),
                _ => None,
            }
        });
        let packet = packet.expect("metadata should be replayed");
        assert_eq!(packet.packed_items.0.len(), 2);
        assert!(packet.packed_items.0.contains(&EntityDataItem {
            index: 0,
            value: EntityDataValue::Byte(2),
        }));
    }

    #[test]
    fn inventory_slot_cache_keeps_the_latest_update() {
        use azalea_inventory::ItemStack;
        use azalea_protocol::packets::game::c_container_set_slot::ClientboundContainerSetSlot;

        let mut snap = WorldSnapshot::default();
        for state_id in 0..100 {
            snap.observe(&frame_of(ClientboundContainerSetSlot {
                container_id: 0,
                state_id,
                slot: 5,
                item_stack: ItemStack::Empty,
            }));
        }
        let slots: Vec<_> = snap
            .replay()
            .into_iter()
            .filter_map(|f| {
                match ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])) {
                    Ok(ClientboundGamePacket::ContainerSetSlot(p)) => Some(p),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].state_id, 99);
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
        let entry = PlayerInfoEntry {
            profile: GameProfile {
                uuid: Uuid::from_u128(1),
                name: "Player".to_string(),
                properties: Arc::new(props),
            },
            ..PlayerInfoEntry::default()
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
        use azalea_protocol::packets::game::c_set_objective::{ClientboundSetObjective, Method};

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

    #[test]
    fn reflected_equipment_tracks_hotbar_and_armor() {
        use azalea_inventory::components::EquipmentSlot;
        use azalea_protocol::packets::game::{
            c_container_set_slot::ClientboundContainerSetSlot,
            c_set_player_inventory::ClientboundSetPlayerInventory,
        };
        use azalea_registry::builtin::ItemKind;

        let sword = ItemStack::from(ItemKind::DiamondSword);
        let helmet = ItemStack::from(ItemKind::DiamondHelmet);
        let mut snap = WorldSnapshot::default();
        snap.observe(&frame_of(ClientboundSetPlayerInventory {
            slot: 4,
            contents: sword.clone(),
        }));
        snap.observe(&frame_of(ClientboundContainerSetSlot {
            container_id: -2,
            state_id: 1,
            slot: 5,
            item_stack: helmet.clone(),
        }));
        snap.set_selected_hotbar_slot(4).unwrap();

        let frame = snap.reflected_equipment_frame(99);
        let packet =
            ClientboundGamePacket::read(frame.packet_id, &mut Cursor::new(frame.body.as_slice()))
                .unwrap();
        let ClientboundGamePacket::SetEquipment(packet) = packet else {
            panic!("expected SetEquipment");
        };
        assert!(packet
            .slots
            .slots
            .contains(&(EquipmentSlot::Mainhand, sword)));
        assert!(packet.slots.slots.contains(&(EquipmentSlot::Head, helmet)));
    }

    #[test]
    fn normalized_inventory_replay_wins_across_packet_kinds() {
        use azalea_protocol::packets::game::{
            c_container_set_slot::ClientboundContainerSetSlot,
            c_set_player_inventory::ClientboundSetPlayerInventory,
        };
        use azalea_registry::builtin::ItemKind;

        let mut snap = WorldSnapshot::default();
        snap.observe(&frame_of(ClientboundSetPlayerInventory {
            slot: 0,
            contents: ItemStack::from(ItemKind::DiamondSword),
        }));
        snap.observe(&frame_of(ClientboundContainerSetSlot {
            container_id: 0,
            state_id: 2,
            slot: 36,
            item_stack: ItemStack::Empty,
        }));

        let final_slot = snap
            .self_hud_frames()
            .into_iter()
            .filter_map(|frame| {
                match ClientboundGamePacket::read(
                    frame.packet_id,
                    &mut Cursor::new(frame.body.as_slice()),
                ) {
                    Ok(ClientboundGamePacket::SetPlayerInventory(packet)) if packet.slot == 0 => {
                        Some(packet.contents)
                    }
                    _ => None,
                }
            })
            .next_back();
        assert_eq!(final_slot, Some(ItemStack::Empty));
    }

    #[test]
    fn self_metadata_is_replayed_for_late_viewers() {
        use azalea_core::entity_id::MinecraftEntityId;
        use azalea_entity::{EntityDataItem, EntityDataValue, EntityMetadataItems};
        use azalea_protocol::packets::game::c_set_entity_data::ClientboundSetEntityData;

        let mut snap = WorldSnapshot::default();
        snap.set_player_entity_id(42);
        snap.observe(&frame_of(ClientboundSetEntityData {
            id: MinecraftEntityId(42),
            packed_items: EntityMetadataItems(vec![EntityDataItem {
                index: 8,
                value: EntityDataValue::Byte(3),
            }]),
        }));

        let replayed =
            snap.self_hud_frames()
                .into_iter()
                .find_map(|frame| {
                    match ClientboundGamePacket::read(
                        frame.packet_id,
                        &mut Cursor::new(frame.body.as_slice()),
                    ) {
                        Ok(ClientboundGamePacket::SetEntityData(packet)) => Some(packet),
                        _ => None,
                    }
                });
        let replayed = replayed.expect("self metadata should be replayed");
        assert_eq!(replayed.id, MinecraftEntityId(42));
        assert_eq!(
            replayed.packed_items.0,
            vec![EntityDataItem {
                index: 8,
                value: EntityDataValue::Byte(3),
            }]
        );

        let reflected = snap
            .reflected_self_state_frames(99)
            .into_iter()
            .find_map(|frame| {
                match ClientboundGamePacket::read(
                    frame.packet_id,
                    &mut Cursor::new(frame.body.as_slice()),
                ) {
                    Ok(ClientboundGamePacket::SetEntityData(packet)) => Some(packet),
                    _ => None,
                }
            })
            .expect("reflected metadata should be replayed");
        assert_eq!(reflected.id, MinecraftEntityId(99));
    }

    #[test]
    fn map_patches_merge_into_a_full_replay() {
        use azalea_protocol::packets::game::c_map_item_data::{
            ClientboundMapItemData, MapPatch, OptionalMapPatch,
        };

        let mut snap = WorldSnapshot::default();
        let map = |start_x: u8, colors: Vec<u8>| {
            frame_of(ClientboundMapItemData {
                map_id: 7,
                scale: 2,
                locked: false,
                decorations: None,
                color_patch: OptionalMapPatch(Some(MapPatch {
                    width: colors.len() as u8,
                    height: 1,
                    start_x,
                    start_y: 3,
                    map_colors: colors,
                })),
            })
        };
        snap.observe(&map(4, vec![11, 12]));
        snap.observe(&map(6, vec![13]));

        let replayed = snap.map_frames();
        assert_eq!(replayed.len(), 1);
        let packet = ClientboundGamePacket::read(
            replayed[0].packet_id,
            &mut Cursor::new(replayed[0].body.as_slice()),
        )
        .unwrap();
        let ClientboundGamePacket::MapItemData(packet) = packet else {
            panic!("expected MapItemData");
        };
        let patch = packet.color_patch.0.expect("full map patch");
        assert_eq!((patch.width, patch.height), (128, 128));
        assert_eq!(patch.map_colors[3 * 128 + 4], 11);
        assert_eq!(patch.map_colors[3 * 128 + 5], 12);
        assert_eq!(patch.map_colors[3 * 128 + 6], 13);
    }

    #[test]
    fn scoreboard_replay_orders_dependencies_first() {
        use azalea_chat::{numbers::NumberFormat, style::ChatFormatting};
        use azalea_core::objectives::ObjectiveCriteria;
        use azalea_protocol::packets::game::{
            c_set_display_objective::{ClientboundSetDisplayObjective, DisplaySlot},
            c_set_objective::{ClientboundSetObjective, Method as ObjectiveMethod},
            c_set_player_team::{
                ClientboundSetPlayerTeam, CollisionRule, Method as TeamMethod, NameTagVisibility,
                Parameters,
            },
            c_set_score::ClientboundSetScore,
        };

        let mut snap = WorldSnapshot::default();
        snap.observe(&frame_of(ClientboundSetScore {
            owner: "line".to_string(),
            objective_name: "sidebar".to_string(),
            score: 1,
            display: None,
            number_format: None,
        }));
        snap.observe(&frame_of(ClientboundSetDisplayObjective {
            slot: DisplaySlot::Sidebar,
            objective_name: "sidebar".to_string(),
        }));
        snap.observe(&frame_of(ClientboundSetPlayerTeam {
            name: "format".to_string(),
            method: TeamMethod::Add((
                Parameters {
                    display_name: FormattedText::from("format"),
                    player_prefix: FormattedText::from("["),
                    player_suffix: FormattedText::from("]"),
                    nametag_visibility: NameTagVisibility::Always,
                    collision_rule: CollisionRule::Always,
                    color: ChatFormatting::White,
                    options: 0,
                },
                vec!["line".to_string()],
            )),
        }));
        snap.observe(&frame_of(ClientboundSetObjective {
            objective_name: "sidebar".to_string(),
            method: ObjectiveMethod::Add {
                display_name: FormattedText::from("Title"),
                render_type: ObjectiveCriteria::Integer,
                number_format: NumberFormat::Blank,
            },
        }));

        let kinds: Vec<_> = snap
            .replay()
            .into_iter()
            .filter_map(|frame| {
                match ClientboundGamePacket::read(
                    frame.packet_id,
                    &mut Cursor::new(frame.body.as_slice()),
                ) {
                    Ok(ClientboundGamePacket::SetObjective(_)) => Some("objective"),
                    Ok(ClientboundGamePacket::SetPlayerTeam(_)) => Some("team"),
                    Ok(ClientboundGamePacket::SetDisplayObjective(_)) => Some("display"),
                    Ok(ClientboundGamePacket::SetScore(_)) => Some("score"),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(kinds, ["objective", "team", "display", "score"]);
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
