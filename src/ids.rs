//! The handful of packet ids the proxy must recognize without parsing.
//!
//! Ids come from the positional order of azalea-protocol 0.16's
//! `declare_state_packets!` declarations. There are no per-packet id
//! constants in the crate, so we hardcode them here and pin every
//! cheap-to-construct one with the test below — a crate bump that
//! renumbers packets fails `cargo test` instead of silently mis-routing.
//! Login and PlayerPosition can't be constructed cheaply; session.rs has
//! a runtime guard for Login (it must be the first game-state frame).

use crate::plugin::Frame;

// config, clientbound
pub const CB_CONFIG_FINISH: u32 = 3;
pub const CB_CONFIG_KEEP_ALIVE: u32 = 4;
pub const CB_CONFIG_PING: u32 = 5;

// config, serverbound
pub const SB_CONFIG_KEEP_ALIVE: u32 = 4;
pub const SB_CONFIG_FINISH: u32 = 3;

// game, clientbound
pub const CB_GAME_ADD_ENTITY: u32 = 1;
pub const CB_GAME_BLOCK_ENTITY_DATA: u32 = 6;
pub const CB_GAME_BLOCK_UPDATE: u32 = 8;
pub const CB_GAME_BOSS_EVENT: u32 = 9;
pub const CB_GAME_CHANGE_DIFFICULTY: u32 = 10;
pub const CB_GAME_CHUNKS_BIOMES: u32 = 13;
pub const CB_GAME_CONTAINER_SET_CONTENT: u32 = 18;
pub const CB_GAME_CONTAINER_SET_SLOT: u32 = 20;
pub const CB_GAME_COOLDOWN: u32 = 22;
pub const CB_GAME_ENTITY_POSITION_SYNC: u32 = 35;
pub const CB_GAME_FORGET_LEVEL_CHUNK: u32 = 37;
pub const CB_GAME_GAME_EVENT: u32 = 38;
pub const CB_GAME_GAME_RULE_VALUES: u32 = 39;
pub const CB_GAME_INITIALIZE_BORDER: u32 = 43;
pub const CB_GAME_KEEP_ALIVE: u32 = 44;
pub const CB_GAME_LEVEL_CHUNK_WITH_LIGHT: u32 = 45;
pub const CB_GAME_LIGHT_UPDATE: u32 = 48;
pub const CB_GAME_LOGIN: u32 = 49;
pub const CB_GAME_MAP_ITEM_DATA: u32 = 51;
pub const CB_GAME_MOVE_ENTITY_POS: u32 = 53;
pub const CB_GAME_MOVE_ENTITY_POS_ROT: u32 = 54;
pub const CB_GAME_MOVE_ENTITY_ROT: u32 = 56;
pub const CB_GAME_PLAYER_ABILITIES: u32 = 64;
pub const CB_GAME_PLAYER_INFO_REMOVE: u32 = 69;
pub const CB_GAME_PLAYER_INFO_UPDATE: u32 = 70;
pub const CB_GAME_PLAYER_LOOK_AT: u32 = 71;
pub const CB_GAME_PLAYER_POSITION: u32 = 72;
pub const CB_GAME_PLAYER_ROTATION: u32 = 73;
pub const CB_GAME_REMOVE_ENTITIES: u32 = 77;
pub const CB_GAME_REMOVE_MOB_EFFECT: u32 = 78;
pub const CB_GAME_RESET_SCORE: u32 = 79;
pub const CB_GAME_RESPAWN: u32 = 82;
pub const CB_GAME_ROTATE_HEAD: u32 = 83;
pub const CB_GAME_SECTION_BLOCKS_UPDATE: u32 = 84;
pub const CB_GAME_SET_BORDER_CENTER: u32 = 88;
pub const CB_GAME_SET_BORDER_LERP_SIZE: u32 = 89;
pub const CB_GAME_SET_BORDER_SIZE: u32 = 90;
pub const CB_GAME_SET_BORDER_WARNING_DELAY: u32 = 91;
pub const CB_GAME_SET_BORDER_WARNING_DISTANCE: u32 = 92;
pub const CB_GAME_SET_CAMERA: u32 = 93;
pub const CB_GAME_SET_CHUNK_CACHE_CENTER: u32 = 94;
pub const CB_GAME_SET_CHUNK_CACHE_RADIUS: u32 = 95;
pub const CB_GAME_SET_CURSOR_ITEM: u32 = 96;
pub const CB_GAME_SET_DEFAULT_SPAWN_POSITION: u32 = 97;
pub const CB_GAME_SET_DISPLAY_OBJECTIVE: u32 = 98;
pub const CB_GAME_SET_ENTITY_DATA: u32 = 99;
pub const CB_GAME_SET_ENTITY_LINK: u32 = 100;
pub const CB_GAME_SET_ENTITY_MOTION: u32 = 101;
pub const CB_GAME_SET_EQUIPMENT: u32 = 102;
pub const CB_GAME_SET_EXPERIENCE: u32 = 103;
pub const CB_GAME_SET_HEALTH: u32 = 104;
pub const CB_GAME_SET_HELD_SLOT: u32 = 105;
pub const CB_GAME_SET_OBJECTIVE: u32 = 106;
pub const CB_GAME_SET_PASSENGERS: u32 = 107;
pub const CB_GAME_SET_PLAYER_INVENTORY: u32 = 108;
pub const CB_GAME_SET_PLAYER_TEAM: u32 = 109;
pub const CB_GAME_SET_SCORE: u32 = 110;
pub const CB_GAME_SET_SIMULATION_DISTANCE: u32 = 111;
pub const CB_GAME_SET_TIME: u32 = 113;
pub const CB_GAME_START_CONFIGURATION: u32 = 118;
pub const CB_GAME_TAB_LIST: u32 = 122;
pub const CB_GAME_TELEPORT_ENTITY: u32 = 125;
pub const CB_GAME_UPDATE_ATTRIBUTES: u32 = 131;
pub const CB_GAME_UPDATE_MOB_EFFECT: u32 = 132;

// game, serverbound
pub const SB_GAME_ACCEPT_TELEPORTATION: u32 = 0;
pub const SB_GAME_CHAT: u32 = 9;
pub const SB_GAME_KEEP_ALIVE: u32 = 28;
pub const SB_GAME_MOVE_PLAYER_POS: u32 = 30;
pub const SB_GAME_MOVE_PLAYER_POS_ROT: u32 = 31;
pub const SB_GAME_MOVE_PLAYER_ROT: u32 = 32;
pub const SB_GAME_PLAYER_ACTION: u32 = 41;
pub const SB_GAME_PLAYER_COMMAND: u32 = 42;
pub const SB_GAME_PLAYER_INPUT: u32 = 43;
pub const SB_GAME_SET_CARRIED_ITEM: u32 = 53;
pub const SB_GAME_SWING: u32 = 63;
pub const SB_GAME_USE_ITEM: u32 = 67;

/// Build a Frame from any typed packet — keeps synthesized frames honest
/// against the real encoders.
pub(crate) fn frame_of<P, T>(pkt: T) -> Frame
where
    P: azalea_protocol::packets::ProtocolPacket,
    T: azalea_protocol::packets::Packet<P>,
{
    let pkt = pkt.into_variant();
    let mut body = Vec::new();
    pkt.write(&mut body).expect("writing to a Vec cannot fail");
    Frame {
        packet_id: pkt.id(),
        body,
    }
}

/// The ClientboundFinishConfiguration frame that ends a viewer's config
/// replay.
pub fn finish_config_frame() -> Frame {
    use azalea_protocol::packets::config::c_finish_configuration::ClientboundFinishConfiguration;
    frame_of(ClientboundFinishConfiguration)
}

/// Game Event 13 ("start waiting for level chunks"). Without this the
/// vanilla client never leaves the "Loading terrain..." screen — since
/// 1.20.2 it dismisses only after this event AND the chunk it stands in
/// has loaded.
pub fn wait_for_chunks_frame() -> Frame {
    use azalea_protocol::packets::game::c_game_event::{ClientboundGameEvent, EventType};
    frame_of(ClientboundGameEvent {
        event: EventType::WaitForLevelChunks,
        param: 0.0,
    })
}

/// Chunk coordinates of a LevelChunkWithLight frame: the body starts
/// `x: i32, z: i32` (big-endian, NOT a packed ChunkPos — azalea's own
/// struct comment warns about the difference).
pub fn chunk_key(body: &[u8]) -> Option<(i32, i32)> {
    let x = i32::from_be_bytes(body.get(0..4)?.try_into().ok()?);
    let z = i32::from_be_bytes(body.get(4..8)?.try_into().ok()?);
    Some((x, z))
}

/// Chunk coordinates of a ForgetLevelChunk frame: the body is a packed
/// ChunkPos long.
pub fn forget_chunk_key(body: &[u8]) -> Option<(i32, i32)> {
    use azalea_core::position::ChunkPos;
    let long = u64::from_be_bytes(body.get(0..8)?.try_into().ok()?);
    let pos = ChunkPos::from(long);
    Some((pos.x, pos.z))
}

pub fn chunk_center(body: &[u8]) -> Option<(i32, i32)> {
    use azalea_buf::AzBufVar;
    use std::io::Cursor;

    let mut cursor = Cursor::new(body);
    Some((
        i32::azalea_read_var(&mut cursor).ok()?,
        i32::azalea_read_var(&mut cursor).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea_protocol::packets::{Packet, ProtocolPacket};

    #[test]
    fn pinned_ids_match_azalea() {
        use azalea_protocol::packets::config::{
            c_finish_configuration::ClientboundFinishConfiguration,
            c_keep_alive::ClientboundKeepAlive as ConfigKeepAlive, c_ping::ClientboundPing,
            s_finish_configuration::ServerboundFinishConfiguration,
        };
        use azalea_protocol::packets::game::c_start_configuration::ClientboundStartConfiguration;

        assert_eq!(
            ClientboundFinishConfiguration.into_variant().id(),
            CB_CONFIG_FINISH
        );
        assert_eq!(
            ServerboundFinishConfiguration.into_variant().id(),
            SB_CONFIG_FINISH
        );
        assert_eq!(
            ConfigKeepAlive { id: 0 }.into_variant().id(),
            CB_CONFIG_KEEP_ALIVE
        );
        assert_eq!(
            ClientboundPing { id: 0 }.into_variant().id(),
            CB_CONFIG_PING
        );
        assert_eq!(
            ClientboundStartConfiguration.into_variant().id(),
            CB_GAME_START_CONFIGURATION
        );
    }

    #[test]
    fn pinned_game_ids_match_azalea() {
        use azalea_core::position::ChunkPos;
        use azalea_protocol::packets::game::{
            c_forget_level_chunk::ClientboundForgetLevelChunk,
            c_game_event::{ClientboundGameEvent, EventType},
            c_set_chunk_cache_center::ClientboundSetChunkCacheCenter,
            c_set_chunk_cache_radius::ClientboundSetChunkCacheRadius,
        };

        assert_eq!(
            ClientboundGameEvent {
                event: EventType::WaitForLevelChunks,
                param: 0.0
            }
            .into_variant()
            .id(),
            CB_GAME_GAME_EVENT
        );
        assert_eq!(
            ClientboundForgetLevelChunk {
                pos: ChunkPos::new(0, 0)
            }
            .into_variant()
            .id(),
            CB_GAME_FORGET_LEVEL_CHUNK
        );
        assert_eq!(
            ClientboundSetChunkCacheCenter { x: 0, z: 0 }
                .into_variant()
                .id(),
            CB_GAME_SET_CHUNK_CACHE_CENTER
        );
        assert_eq!(
            ClientboundSetChunkCacheRadius { radius: 0 }
                .into_variant()
                .id(),
            CB_GAME_SET_CHUNK_CACHE_RADIUS
        );
    }

    #[test]
    fn pinned_movement_ids_match_azalea() {
        use azalea_core::entity_id::MinecraftEntityId;
        use azalea_core::position::Vec3;
        use azalea_entity::LookDirection;
        use azalea_protocol::common::movements::MoveFlags;
        use azalea_protocol::packets::game::{
            c_player_abilities::{ClientboundPlayerAbilities, PlayerAbilitiesFlags},
            c_player_look_at::{Anchor, ClientboundPlayerLookAt},
            c_player_rotation::ClientboundPlayerRotation,
            s_interact::InteractionHand,
            s_move_player_pos::ServerboundMovePlayerPos,
            s_move_player_pos_rot::ServerboundMovePlayerPosRot,
            s_move_player_rot::ServerboundMovePlayerRot,
            s_player_action::{Action as PlayerAction, ServerboundPlayerAction},
            s_player_command::{Action as PlayerCommand, ServerboundPlayerCommand},
            s_player_input::ServerboundPlayerInput,
            s_set_carried_item::ServerboundSetCarriedItem,
            s_swing::ServerboundSwing,
            s_use_item::ServerboundUseItem,
        };

        let flags = MoveFlags {
            on_ground: false,
            horizontal_collision: false,
        };
        assert_eq!(
            ServerboundMovePlayerPos {
                pos: Vec3::default(),
                flags
            }
            .into_variant()
            .id(),
            SB_GAME_MOVE_PLAYER_POS
        );
        assert_eq!(
            ServerboundMovePlayerPosRot {
                pos: Vec3::default(),
                look_direction: LookDirection::default(),
                flags
            }
            .into_variant()
            .id(),
            SB_GAME_MOVE_PLAYER_POS_ROT
        );
        assert_eq!(
            ServerboundMovePlayerRot {
                look_direction: LookDirection::default(),
                flags
            }
            .into_variant()
            .id(),
            SB_GAME_MOVE_PLAYER_ROT
        );
        assert_eq!(
            ClientboundPlayerAbilities {
                flags: PlayerAbilitiesFlags {
                    invulnerable: false,
                    flying: false,
                    can_fly: false,
                    instant_break: false
                },
                flying_speed: 0.0,
                walking_speed: 0.0
            }
            .into_variant()
            .id(),
            CB_GAME_PLAYER_ABILITIES
        );
        assert_eq!(
            ClientboundPlayerLookAt {
                from_anchor: Anchor::Eyes,
                pos: Vec3::default(),
                entity: None,
            }
            .into_variant()
            .id(),
            CB_GAME_PLAYER_LOOK_AT
        );
        assert_eq!(
            ClientboundPlayerRotation {
                y_rot: 0.0,
                relative_y: false,
                x_rot: 0.0,
                relative_x: false,
            }
            .into_variant()
            .id(),
            CB_GAME_PLAYER_ROTATION
        );
        assert_eq!(
            ServerboundPlayerAction {
                action: PlayerAction::ReleaseUseItem,
                pos: Default::default(),
                direction: Default::default(),
                seq: 0,
            }
            .into_variant()
            .id(),
            SB_GAME_PLAYER_ACTION
        );
        assert_eq!(
            ServerboundPlayerCommand {
                id: MinecraftEntityId(0),
                action: PlayerCommand::StartSprinting,
                data: 0,
            }
            .into_variant()
            .id(),
            SB_GAME_PLAYER_COMMAND
        );
        assert_eq!(
            ServerboundPlayerInput::default().into_variant().id(),
            SB_GAME_PLAYER_INPUT
        );
        assert_eq!(
            ServerboundSetCarriedItem { slot: 0 }.into_variant().id(),
            SB_GAME_SET_CARRIED_ITEM
        );
        assert_eq!(
            ServerboundSwing {
                hand: InteractionHand::MainHand,
            }
            .into_variant()
            .id(),
            SB_GAME_SWING
        );
        assert_eq!(
            ServerboundUseItem {
                hand: InteractionHand::MainHand,
                seq: 0,
                y_rot: 0.0,
                x_rot: 0.0,
            }
            .into_variant()
            .id(),
            SB_GAME_USE_ITEM
        );
    }

    #[test]
    fn pinned_snapshot_and_handoff_ids_match_azalea() {
        use azalea_chat::FormattedText;
        use azalea_core::delta::{LpVec3, PositionDelta8};
        use azalea_core::entity_id::MinecraftEntityId;
        use azalea_core::position::{BlockPos, ChunkSectionPos, Vec3};
        use azalea_entity::LookDirection;
        use azalea_protocol::common::movements::{PositionMoveRotation, RelativeMovements};
        use azalea_protocol::packets::game::{
            c_add_entity::ClientboundAddEntity,
            c_block_entity_data::ClientboundBlockEntityData,
            c_block_update::ClientboundBlockUpdate,
            c_boss_event::{ClientboundBossEvent, Operation},
            c_entity_position_sync::ClientboundEntityPositionSync,
            c_keep_alive::ClientboundKeepAlive,
            c_move_entity_pos::ClientboundMoveEntityPos,
            c_move_entity_pos_rot::{ClientboundMoveEntityPosRot, CompactLookDirection},
            c_move_entity_rot::ClientboundMoveEntityRot,
            c_player_info_remove::ClientboundPlayerInfoRemove,
            c_remove_entities::ClientboundRemoveEntities,
            c_reset_score::ClientboundResetScore,
            c_rotate_head::ClientboundRotateHead,
            c_section_blocks_update::ClientboundSectionBlocksUpdate,
            c_set_health::ClientboundSetHealth,
            c_set_passengers::ClientboundSetPassengers,
            c_set_player_inventory::ClientboundSetPlayerInventory,
            c_system_chat::ClientboundSystemChat,
            c_teleport_entity::ClientboundTeleportEntity,
            s_accept_teleportation::ServerboundAcceptTeleportation,
            s_keep_alive::ServerboundKeepAlive,
        };
        use azalea_registry::builtin::{BlockEntityKind, EntityKind};
        use uuid::Uuid;

        let eid = MinecraftEntityId(0);
        let delta = PositionDelta8 {
            xa: 0,
            ya: 0,
            za: 0,
        };
        let compact = CompactLookDirection { y_rot: 0, x_rot: 0 };
        let pmr = PositionMoveRotation {
            pos: Vec3::default(),
            delta: Vec3::default(),
            look_direction: LookDirection::default(),
        };

        assert_eq!(
            ClientboundAddEntity {
                id: eid,
                uuid: Uuid::nil(),
                entity_type: EntityKind::Player,
                position: Vec3::default(),
                movement: LpVec3::Zero,
                x_rot: 0,
                y_rot: 0,
                y_head_rot: 0,
                data: 0
            }
            .into_variant()
            .id(),
            CB_GAME_ADD_ENTITY
        );
        assert_eq!(
            ClientboundBlockEntityData {
                pos: BlockPos::default(),
                block_entity_type: BlockEntityKind::Furnace,
                tag: Default::default(),
            }
            .into_variant()
            .id(),
            CB_GAME_BLOCK_ENTITY_DATA
        );
        assert_eq!(
            ClientboundBlockUpdate {
                pos: BlockPos::default(),
                block_state: Default::default(),
            }
            .into_variant()
            .id(),
            CB_GAME_BLOCK_UPDATE
        );
        assert_eq!(
            ClientboundEntityPositionSync {
                id: eid,
                values: pmr.clone(),
                on_ground: false
            }
            .into_variant()
            .id(),
            CB_GAME_ENTITY_POSITION_SYNC
        );
        assert_eq!(
            ClientboundKeepAlive { id: 0 }.into_variant().id(),
            CB_GAME_KEEP_ALIVE
        );
        assert_eq!(
            ClientboundMoveEntityPos {
                entity_id: eid,
                delta,
                on_ground: false
            }
            .into_variant()
            .id(),
            CB_GAME_MOVE_ENTITY_POS
        );
        assert_eq!(
            ClientboundMoveEntityPosRot {
                entity_id: eid,
                delta,
                look_direction: compact,
                on_ground: false
            }
            .into_variant()
            .id(),
            CB_GAME_MOVE_ENTITY_POS_ROT
        );
        assert_eq!(
            ClientboundMoveEntityRot {
                entity_id: eid,
                look_direction: compact,
                on_ground: false
            }
            .into_variant()
            .id(),
            CB_GAME_MOVE_ENTITY_ROT
        );
        assert_eq!(
            ClientboundPlayerInfoRemove {
                profile_ids: vec![]
            }
            .into_variant()
            .id(),
            CB_GAME_PLAYER_INFO_REMOVE
        );
        assert_eq!(
            ClientboundRemoveEntities { entity_ids: vec![] }
                .into_variant()
                .id(),
            CB_GAME_REMOVE_ENTITIES
        );
        assert_eq!(
            ClientboundResetScore {
                owner: String::new(),
                objective_name: None
            }
            .into_variant()
            .id(),
            CB_GAME_RESET_SCORE
        );
        assert_eq!(
            ClientboundRotateHead {
                entity_id: eid,
                y_head_rot: 0
            }
            .into_variant()
            .id(),
            CB_GAME_ROTATE_HEAD
        );
        assert_eq!(
            ClientboundSectionBlocksUpdate {
                section_pos: ChunkSectionPos::default(),
                states: vec![],
            }
            .into_variant()
            .id(),
            CB_GAME_SECTION_BLOCKS_UPDATE
        );
        assert_eq!(
            ClientboundSetHealth {
                health: 0.0,
                food: 0,
                saturation: 0.0
            }
            .into_variant()
            .id(),
            CB_GAME_SET_HEALTH
        );
        assert_eq!(
            ClientboundSetPlayerInventory {
                slot: 0,
                contents: azalea_inventory::ItemStack::Empty,
            }
            .into_variant()
            .id(),
            CB_GAME_SET_PLAYER_INVENTORY
        );
        assert_eq!(
            ClientboundSetPassengers {
                vehicle: eid,
                passengers: vec![],
            }
            .into_variant()
            .id(),
            CB_GAME_SET_PASSENGERS
        );
        assert_eq!(
            ClientboundBossEvent {
                id: Uuid::nil(),
                operation: Operation::Remove,
            }
            .into_variant()
            .id(),
            CB_GAME_BOSS_EVENT
        );
        assert_eq!(
            ClientboundSystemChat {
                content: FormattedText::from(""),
                overlay: false
            }
            .into_variant()
            .id(),
            121
        );
        assert_eq!(
            ClientboundTeleportEntity {
                id: eid,
                change: pmr,
                relative: RelativeMovements::all_absolute(),
                on_ground: false
            }
            .into_variant()
            .id(),
            CB_GAME_TELEPORT_ENTITY
        );
        assert_eq!(
            ServerboundAcceptTeleportation { id: 0 }.into_variant().id(),
            SB_GAME_ACCEPT_TELEPORTATION
        );
        assert_eq!(
            ServerboundKeepAlive { id: 0 }.into_variant().id(),
            SB_GAME_KEEP_ALIVE
        );
        {
            use azalea_protocol::packets::config::s_keep_alive::ServerboundKeepAlive as ConfigKeepAlive;
            use azalea_protocol::packets::game::c_set_camera::ClientboundSetCamera;
            assert_eq!(
                ClientboundSetCamera { camera_id: eid }.into_variant().id(),
                CB_GAME_SET_CAMERA
            );
            assert_eq!(
                ConfigKeepAlive { id: 0 }.into_variant().id(),
                SB_CONFIG_KEEP_ALIVE
            );
        }
    }

    #[test]
    fn pinned_qol_snapshot_ids_match_azalea() {
        use azalea_core::{delta::LpVec3, difficulty::Difficulty, entity_id::MinecraftEntityId};
        use azalea_inventory::ItemStack;
        use azalea_protocol::packets::game::{
            c_change_difficulty::ClientboundChangeDifficulty,
            c_chunks_biomes::ClientboundChunksBiomes,
            c_cooldown::ClientboundCooldown,
            c_game_rule_values::ClientboundGameRuleValues,
            c_initialize_border::ClientboundInitializeBorder,
            c_light_update::ClientboundLightUpdate,
            c_map_item_data::{ClientboundMapItemData, OptionalMapPatch},
            c_set_border_center::ClientboundSetBorderCenter,
            c_set_border_lerp_size::ClientboundSetBorderLerpSize,
            c_set_border_size::ClientboundSetBorderSize,
            c_set_border_warning_delay::ClientboundSetBorderWarningDelay,
            c_set_border_warning_distance::ClientboundSetBorderWarningDistance,
            c_set_cursor_item::ClientboundSetCursorItem,
            c_set_entity_link::ClientboundSetEntityLink,
            c_set_entity_motion::ClientboundSetEntityMotion,
            c_set_simulation_distance::ClientboundSetSimulationDistance,
        };
        use azalea_registry::builtin::ItemKind;

        assert_eq!(
            ClientboundChangeDifficulty {
                difficulty: Difficulty::Normal,
                locked: false,
            }
            .into_variant()
            .id(),
            CB_GAME_CHANGE_DIFFICULTY
        );
        assert_eq!(
            ClientboundChunksBiomes {
                chunk_biome_data: vec![],
            }
            .into_variant()
            .id(),
            CB_GAME_CHUNKS_BIOMES
        );
        assert_eq!(
            ClientboundCooldown {
                item: ItemKind::Stone,
                duration: 1,
            }
            .into_variant()
            .id(),
            CB_GAME_COOLDOWN
        );
        assert_eq!(
            ClientboundGameRuleValues {
                values: Default::default(),
            }
            .into_variant()
            .id(),
            CB_GAME_GAME_RULE_VALUES
        );
        assert_eq!(
            ClientboundInitializeBorder {
                new_center_x: 0.0,
                new_center_z: 0.0,
                old_size: 1.0,
                new_size: 1.0,
                lerp_time: 0,
                new_absolute_max_size: 1,
                warning_blocks: 1,
                warning_time: 1,
            }
            .into_variant()
            .id(),
            CB_GAME_INITIALIZE_BORDER
        );
        assert_eq!(
            ClientboundLightUpdate {
                x: 0,
                z: 0,
                light_data: Default::default(),
            }
            .into_variant()
            .id(),
            CB_GAME_LIGHT_UPDATE
        );
        assert_eq!(
            ClientboundMapItemData {
                map_id: 0,
                scale: 0,
                locked: false,
                decorations: None,
                color_patch: OptionalMapPatch(None),
            }
            .into_variant()
            .id(),
            CB_GAME_MAP_ITEM_DATA
        );
        assert_eq!(
            ClientboundSetBorderCenter {
                new_center_x: 0.0,
                new_center_z: 0.0,
            }
            .into_variant()
            .id(),
            CB_GAME_SET_BORDER_CENTER
        );
        assert_eq!(
            ClientboundSetBorderLerpSize {
                old_size: 1.0,
                new_size: 2.0,
                lerp_time: 1,
            }
            .into_variant()
            .id(),
            CB_GAME_SET_BORDER_LERP_SIZE
        );
        assert_eq!(
            ClientboundSetBorderSize { size: 1.0 }.into_variant().id(),
            CB_GAME_SET_BORDER_SIZE
        );
        assert_eq!(
            ClientboundSetBorderWarningDelay { warning_delay: 1 }
                .into_variant()
                .id(),
            CB_GAME_SET_BORDER_WARNING_DELAY
        );
        assert_eq!(
            ClientboundSetBorderWarningDistance { warning_blocks: 1 }
                .into_variant()
                .id(),
            CB_GAME_SET_BORDER_WARNING_DISTANCE
        );
        assert_eq!(
            ClientboundSetCursorItem {
                contents: ItemStack::Empty,
            }
            .into_variant()
            .id(),
            CB_GAME_SET_CURSOR_ITEM
        );
        assert_eq!(
            ClientboundSetEntityLink {
                source_id: MinecraftEntityId(1),
                dest_id: MinecraftEntityId(2),
            }
            .into_variant()
            .id(),
            CB_GAME_SET_ENTITY_LINK
        );
        assert_eq!(
            ClientboundSetEntityMotion {
                id: MinecraftEntityId(1),
                delta: LpVec3::Zero,
            }
            .into_variant()
            .id(),
            CB_GAME_SET_ENTITY_MOTION
        );
        assert_eq!(
            ClientboundSetSimulationDistance {
                simulation_distance: 8,
            }
            .into_variant()
            .id(),
            CB_GAME_SET_SIMULATION_DISTANCE
        );
    }

    fn info_packet(
        list_order: bool,
    ) -> azalea_protocol::packets::game::c_player_info_update::ClientboundPlayerInfoUpdate {
        use azalea_auth::game_profile::GameProfile;
        use azalea_chat::FormattedText;
        use azalea_core::game_type::GameMode;
        use azalea_protocol::packets::game::c_player_info_update::{
            ActionEnumSet, ClientboundPlayerInfoUpdate, PlayerInfoEntry,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        ClientboundPlayerInfoUpdate {
            actions: ActionEnumSet {
                add_player: true,
                initialize_chat: false,
                update_game_mode: true,
                update_listed: true,
                update_latency: true,
                update_display_name: true,
                update_list_order: list_order,
                update_hat: false,
            },
            entries: vec![PlayerInfoEntry {
                profile: GameProfile {
                    uuid: Uuid::from_u128(42),
                    name: "bot".into(),
                    properties: Arc::new(Default::default()),
                },
                chat_session: None,
                game_mode: GameMode::Survival,
                listed: true,
                latency: 12,
                display_name: Some(Box::new(FormattedText::from("Bot"))),
                list_order: 7,
                update_hat: false,
            }],
        }
    }

    /// Every player_info_update shape the proxy synthesizes must survive
    /// a write→read round trip through azalea (whose reader demonstrably
    /// matches vanilla — it parses live servers).
    #[test]
    fn synthesized_player_info_roundtrips() {
        use azalea_protocol::packets::game::ClientboundGamePacket;
        use std::io::Cursor;

        let f = frame_of(info_packet(false));
        assert_eq!(f.packet_id, CB_GAME_PLAYER_INFO_UPDATE);
        let parsed = ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..]))
            .expect("must decode");
        let ClientboundGamePacket::PlayerInfoUpdate(p) = parsed else {
            panic!("wrong variant");
        };
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].profile.name, "bot");
        assert_eq!(p.entries[0].latency, 12);
        assert!(p.entries[0].listed);
    }

    /// Canary for the azalea 0.16 bug that motivated update_list_order
    /// staying false in snapshot.rs: azalea_write sets the list_order
    /// action bit but never writes its entry data, so its own reader
    /// chokes on the result. When this test FAILS, azalea fixed the bug
    /// and list_order replay can be re-enabled.
    #[test]
    fn azalea_player_info_list_order_write_still_broken() {
        use azalea_protocol::packets::game::ClientboundGamePacket;
        use std::io::Cursor;

        let f = frame_of(info_packet(true));
        assert!(
            ClientboundGamePacket::read(f.packet_id, &mut Cursor::new(&f.body[..])).is_err(),
            "azalea now round-trips update_list_order — re-enable it in snapshot.rs replay()"
        );
    }

    #[test]
    fn finish_frame_is_empty_bodied() {
        let f = finish_config_frame();
        assert_eq!(f.packet_id, CB_CONFIG_FINISH);
        assert!(f.body.is_empty());
    }

    #[test]
    fn forget_chunk_key_roundtrips_through_azalea_encoder() {
        use azalea_core::position::ChunkPos;
        use azalea_protocol::packets::game::c_forget_level_chunk::ClientboundForgetLevelChunk;

        let f = frame_of(ClientboundForgetLevelChunk {
            pos: ChunkPos::new(5, -3),
        });
        assert_eq!(forget_chunk_key(&f.body), Some((5, -3)));
    }

    #[test]
    fn chunk_key_matches_azalea_i32_encoding() {
        use azalea_buf::AzBuf;

        // LevelChunkWithLight can't be constructed cheaply; verify the
        // leading x/z layout against azalea-buf's i32 encoder instead.
        let mut body = Vec::new();
        (-7i32).azalea_write(&mut body).unwrap();
        (12i32).azalea_write(&mut body).unwrap();
        body.extend_from_slice(&[0xAA; 16]); // rest of the packet, irrelevant
        assert_eq!(chunk_key(&body), Some((-7, 12)));
    }

    #[test]
    fn chunk_center_roundtrips_through_azalea_encoder() {
        use azalea_protocol::packets::game::c_set_chunk_cache_center::ClientboundSetChunkCacheCenter;

        let frame = frame_of(ClientboundSetChunkCacheCenter { x: -12, z: 34 });
        assert_eq!(chunk_center(&frame.body), Some((-12, 34)));
    }
}
