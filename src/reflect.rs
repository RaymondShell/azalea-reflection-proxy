//! The reflected bot entity — what turns a viewer from a confused clone
//! of the bot into a spectator.
//!
//! A viewer's client believes it IS the session player (it got the
//! session's Login packet), so the server-side player is invisible to
//! it: the server never echoes your own movement back. This module
//! synthesizes the bot as a SEPARATE player entity for viewers, fed
//! from the controller's serverbound movement packets — which all pass
//! through the proxy, the original project's whole reason for routing
//! the bot through it. Viewers themselves are switched to spectator
//! game mode so they free-fly and can't interact.

use azalea_core::entity_id::MinecraftEntityId;
use azalea_core::position::Vec3;
use azalea_entity::LookDirection;
use std::io::Cursor;
use uuid::Uuid;

use crate::ids::{self, frame_of};
use crate::plugin::Frame;

/// Entity id for the synthesized bot entity on viewer clients. Vanilla
/// servers allocate ids incrementally from 0, so a value this large
/// cannot collide in practice.
pub const REFLECTED_ENTITY_ID: i32 = 1_999_999_999;

/// Last known pose of the controlled player, updated from the
/// controller's serverbound movement stream and clientbound teleports.
#[derive(Default)]
pub struct BotPose {
    pub pos: Option<Vec3>,
    pub look: LookDirection,
    pub on_ground: bool,
}

fn angle_byte(degrees: f32) -> i8 {
    (degrees.rem_euclid(360.0) / 360.0 * 256.0) as i32 as i8
}

/// Game Event 3 (ChangeGameMode) for an arbitrary mode id.
pub fn gamemode_event_frame(mode: u8) -> Frame {
    use azalea_protocol::packets::game::c_game_event::{ClientboundGameEvent, EventType};
    frame_of(ClientboundGameEvent {
        event: EventType::ChangeGameMode,
        param: mode as f32,
    })
}

/// Player-info entry for the client's OWN uuid with a game mode. Modern
/// vanilla clients key their game-mode state off this entry, not just
/// the game event — sending only the event is unreliable (the cause of
/// the "viewer not in spectator" bug).
fn own_info_frame(uuid: Uuid, name: &str, mode: u8) -> Frame {
    use azalea_auth::game_profile::GameProfile;
    use azalea_core::game_type::GameMode;
    use azalea_protocol::packets::game::c_player_info_update::{
        ActionEnumSet, ClientboundPlayerInfoUpdate, PlayerInfoEntry,
    };
    use std::sync::Arc;

    frame_of(ClientboundPlayerInfoUpdate {
        actions: ActionEnumSet {
            add_player: true,
            initialize_chat: false,
            update_game_mode: true,
            update_listed: true,
            update_latency: false,
            update_display_name: false,
            update_list_order: false,
            update_hat: false,
        },
        entries: vec![PlayerInfoEntry {
            profile: GameProfile {
                uuid,
                name: name.to_string(),
                properties: Arc::new(Default::default()),
            },
            chat_session: None,
            game_mode: GameMode::from_id(mode).unwrap_or(GameMode::Spectator),
            listed: false,
            latency: 0,
            display_name: None,
            list_order: 0,
            update_hat: false,
        }],
    })
}

fn abilities_frame(flying: bool) -> Frame {
    use azalea_protocol::packets::game::c_player_abilities::{
        ClientboundPlayerAbilities, PlayerAbilitiesFlags,
    };
    frame_of(ClientboundPlayerAbilities {
        flags: PlayerAbilitiesFlags {
            invulnerable: flying,
            flying,
            can_fly: flying,
            instant_break: false,
        },
        flying_speed: 0.05,
        walking_speed: 0.1,
    })
}

/// The full "become a spectator" sequence for a viewer: own-uuid player
/// info, the game event, and flight abilities. Re-send after every
/// Login/Respawn the viewer receives — those reset client game mode.
pub fn spectator_kit(viewer_uuid: Uuid, viewer_name: &str) -> Vec<Frame> {
    vec![
        own_info_frame(viewer_uuid, viewer_name, 3),
        gamemode_event_frame(3),
        abilities_frame(true),
    ]
}

/// The reverse: restore a client to the session's real game mode when
/// it acquires control.
pub fn controller_kit(uuid: Uuid, name: &str, real_mode: u8) -> Vec<Frame> {
    vec![
        own_info_frame(uuid, name, real_mode),
        gamemode_event_frame(real_mode),
        abilities_frame(false),
        remove_reflected_frame(),
    ]
}

/// Despawn the reflected bot entity (for a client becoming controller —
/// it must not see a ghost of itself).
pub fn remove_reflected_frame() -> Frame {
    use azalea_protocol::packets::game::c_remove_entities::ClientboundRemoveEntities;
    frame_of(ClientboundRemoveEntities {
        entity_ids: vec![MinecraftEntityId(REFLECTED_ENTITY_ID)],
    })
}

/// Proxy-issued chat feedback for `,commands`.
pub fn system_chat_frame(msg: &str) -> Frame {
    use azalea_chat::FormattedText;
    use azalea_protocol::packets::game::c_system_chat::ClientboundSystemChat;
    frame_of(ClientboundSystemChat {
        content: FormattedText::from(format!("[proxy] {msg}")),
        overlay: false,
    })
}

/// Teleport id for proxy-synthesized position syncs during handoff; the
/// new controller's matching accept is swallowed, never forwarded.
pub const HANDOFF_TELEPORT_ID: u32 = 0x5EC7A11;

/// Align a new controller's client to the bot's pose (the GrimAC-style
/// teleport option from the original's README, minus the
/// explosion-velocity trick).
pub fn handoff_teleport_frame(pose: &BotPose) -> Option<Frame> {
    use azalea_protocol::common::movements::{PositionMoveRotation, RelativeMovements};
    use azalea_protocol::packets::game::c_player_position::ClientboundPlayerPosition;
    Some(frame_of(ClientboundPlayerPosition {
        id: HANDOFF_TELEPORT_ID,
        change: PositionMoveRotation {
            pos: pose.pos?,
            delta: Vec3::default(),
            look_direction: pose.look,
        },
        relative: RelativeMovements::all_absolute(),
    }))
}

/// Serverbound keepalive reply, for when no controller is attached and
/// the proxy must keep the session alive itself.
pub fn keepalive_reply(id: u64) -> Frame {
    use azalea_protocol::packets::game::s_keep_alive::ServerboundKeepAlive;
    frame_of(ServerboundKeepAlive { id })
}

/// Config-state variant of the keepalive reply.
pub fn config_keepalive_reply(id: u64) -> Frame {
    use azalea_protocol::packets::config::s_keep_alive::ServerboundKeepAlive;
    frame_of(ServerboundKeepAlive { id })
}

/// Serverbound teleport accept, for controllerless position syncs.
pub fn accept_teleport_frame(id: u32) -> Frame {
    use azalea_protocol::packets::game::s_accept_teleportation::ServerboundAcceptTeleportation;
    frame_of(ServerboundAcceptTeleportation { id })
}

/// Extract the text of a serverbound chat frame, if it is one.
pub fn chat_text(frame: &Frame) -> Option<String> {
    use azalea_protocol::packets::ProtocolPacket;
    use azalea_protocol::packets::game::ServerboundGamePacket;
    if frame.packet_id != ids::SB_GAME_CHAT {
        return None;
    }
    match ServerboundGamePacket::read(frame.packet_id, &mut Cursor::new(&frame.body[..])) {
        Ok(ServerboundGamePacket::Chat(p)) => Some(p.message),
        _ => None,
    }
}

/// Teleport id of a clientbound PlayerPosition frame (for controllerless
/// auto-accept).
pub fn teleport_id(frame: &Frame) -> Option<u32> {
    use azalea_buf::AzBufVar;
    u32::azalea_read_var(&mut Cursor::new(&frame.body[..])).ok()
}

/// Keepalive id of a clientbound KeepAlive frame (both states: body is
/// a bare u64).
pub fn keepalive_id(frame: &Frame) -> Option<u64> {
    Some(u64::from_be_bytes(frame.body.get(0..8)?.try_into().ok()?))
}

/// Tab-list entry for the bot. Required: the client refuses to render a
/// player entity whose uuid it has no player-info for. No skin textures
/// yet (they need a signed sessionserver profile lookup) — the bot shows
/// with a default skin.
pub fn bot_info_frame(uuid: Uuid, name: &str) -> Frame {
    use azalea_auth::game_profile::GameProfile;
    use azalea_core::game_type::GameMode;
    use azalea_protocol::packets::game::c_player_info_update::{
        ActionEnumSet, ClientboundPlayerInfoUpdate, PlayerInfoEntry,
    };
    use std::sync::Arc;

    frame_of(ClientboundPlayerInfoUpdate {
        actions: ActionEnumSet {
            add_player: true,
            initialize_chat: false,
            update_game_mode: true,
            update_listed: true,
            update_latency: true,
            update_display_name: false,
            update_list_order: false,
            update_hat: false,
        },
        entries: vec![PlayerInfoEntry {
            profile: GameProfile {
                uuid,
                name: name.to_string(),
                properties: Arc::new(Default::default()),
            },
            chat_session: None,
            game_mode: GameMode::Survival,
            listed: true,
            latency: 0,
            display_name: None,
            list_order: 0,
            update_hat: false,
        }],
    })
}

/// Spawn the reflected entity at the bot's pose (AddEntity + RotateHead).
pub fn spawn_frames(uuid: Uuid, pose: &BotPose) -> Vec<Frame> {
    use azalea_core::delta::LpVec3;
    use azalea_protocol::packets::game::c_add_entity::ClientboundAddEntity;
    use azalea_protocol::packets::game::c_rotate_head::ClientboundRotateHead;
    use azalea_registry::builtin::EntityKind;

    let Some(pos) = pose.pos else {
        return Vec::new();
    };
    vec![
        frame_of(ClientboundAddEntity {
            id: MinecraftEntityId(REFLECTED_ENTITY_ID),
            uuid,
            entity_type: EntityKind::Player,
            position: pos,
            movement: LpVec3::Zero,
            x_rot: angle_byte(pose.look.x_rot()),
            y_rot: angle_byte(pose.look.y_rot()),
            y_head_rot: angle_byte(pose.look.y_rot()),
            data: 0,
        }),
        frame_of(ClientboundRotateHead {
            entity_id: MinecraftEntityId(REFLECTED_ENTITY_ID),
            y_head_rot: angle_byte(pose.look.y_rot()),
        }),
    ]
}

/// Move the reflected entity to the bot's pose (absolute teleport +
/// head rotation; sent per controller movement packet, ~20/s).
pub fn move_frames(pose: &BotPose) -> Vec<Frame> {
    use azalea_protocol::common::movements::PositionMoveRotation;
    use azalea_protocol::packets::game::c_entity_position_sync::ClientboundEntityPositionSync;
    use azalea_protocol::packets::game::c_rotate_head::ClientboundRotateHead;

    let Some(pos) = pose.pos else {
        return Vec::new();
    };
    vec![
        frame_of(ClientboundEntityPositionSync {
            id: MinecraftEntityId(REFLECTED_ENTITY_ID),
            values: PositionMoveRotation {
                pos,
                delta: Vec3::default(),
                look_direction: pose.look,
            },
            on_ground: pose.on_ground,
        }),
        frame_of(ClientboundRotateHead {
            entity_id: MinecraftEntityId(REFLECTED_ENTITY_ID),
            y_head_rot: angle_byte(pose.look.y_rot()),
        }),
    ]
}

/// Update the pose from a controller serverbound movement frame.
/// Returns true when the frame was a movement packet (pose changed).
pub fn apply_controller_move(pose: &mut BotPose, frame: &Frame) -> bool {
    use azalea_protocol::packets::ProtocolPacket;
    use azalea_protocol::packets::game::ServerboundGamePacket;

    if !matches!(
        frame.packet_id,
        ids::SB_GAME_MOVE_PLAYER_POS | ids::SB_GAME_MOVE_PLAYER_POS_ROT | ids::SB_GAME_MOVE_PLAYER_ROT
    ) {
        return false;
    }
    let Ok(pkt) = ServerboundGamePacket::read(frame.packet_id, &mut Cursor::new(&frame.body[..]))
    else {
        return false;
    };
    match pkt {
        ServerboundGamePacket::MovePlayerPos(p) => {
            pose.pos = Some(p.pos);
            pose.on_ground = p.flags.on_ground;
        }
        ServerboundGamePacket::MovePlayerPosRot(p) => {
            pose.pos = Some(p.pos);
            pose.look = p.look_direction;
            pose.on_ground = p.flags.on_ground;
        }
        ServerboundGamePacket::MovePlayerRot(p) => {
            pose.look = p.look_direction;
            pose.on_ground = p.flags.on_ground;
        }
        _ => return false,
    }
    true
}

/// Update the pose from a clientbound PlayerPosition (server teleport of
/// the controlled player). Only absolute teleports are applied — the
/// relative-flag math isn't worth modelling here, and the controller's
/// next movement packet corrects the pose within a tick anyway.
pub fn apply_server_teleport(pose: &mut BotPose, frame: &Frame) {
    use azalea_protocol::packets::ProtocolPacket;
    use azalea_protocol::packets::game::ClientboundGamePacket;

    let Ok(ClientboundGamePacket::PlayerPosition(p)) =
        ClientboundGamePacket::read(frame.packet_id, &mut Cursor::new(&frame.body[..]))
    else {
        return;
    };
    if !p.relative.x && !p.relative.y && !p.relative.z {
        pose.pos = Some(p.change.pos);
        pose.look = p.change.look_direction;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea_protocol::common::movements::MoveFlags;
    use azalea_protocol::packets::game::s_move_player_pos_rot::ServerboundMovePlayerPosRot;

    #[test]
    fn controller_move_roundtrips_through_azalea_encoder() {
        let frame = frame_of(ServerboundMovePlayerPosRot {
            pos: Vec3 {
                x: 100.5,
                y: 64.0,
                z: -20.25,
            },
            look_direction: LookDirection::new(90.0, -10.0),
            flags: MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        });
        let mut pose = BotPose::default();
        assert!(apply_controller_move(&mut pose, &frame));
        assert_eq!(
            pose.pos,
            Some(Vec3 {
                x: 100.5,
                y: 64.0,
                z: -20.25
            })
        );
        assert_eq!(pose.look.y_rot(), 90.0);
        assert_eq!(pose.look.x_rot(), -10.0);
        assert!(pose.on_ground);
    }

    #[test]
    fn angle_byte_wraps() {
        assert_eq!(angle_byte(0.0), 0);
        assert_eq!(angle_byte(360.0), 0);
        // 90° = quarter turn = 64/256
        assert_eq!(angle_byte(90.0), 64);
        // -90° wraps to 270° = 192/256 = -64 as i8
        assert_eq!(angle_byte(-90.0), -64);
    }

    #[test]
    fn spawn_and_move_need_a_position() {
        let pose = BotPose::default();
        assert!(spawn_frames(Uuid::nil(), &pose).is_empty());
        assert!(move_frames(&pose).is_empty());
    }
}
