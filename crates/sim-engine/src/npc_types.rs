// NPC unit constants and factory constructors.
// Ref: Detection_and_Fog_of_War.md §2 · Hex_Map_and_Travel.md §6

use crate::game_types::{Coordinates, NpcState, NpcType, NpcUnit};
use crate::types::Pack;

// ── Movement ──────────────────────────────────────────────────────────────────

/// Minutes per hex for a War Boar (heavy mount, slow).
pub const WAR_BOAR_MOVE_MINS_PER_HEX: u32 = 40;

/// Minutes per hex for a Raccoon Biker (fast bike, very quick).
pub const RACCOON_MOVE_MINS_PER_HEX: u32 = 10;

// ── Detection / Sensor ────────────────────────────────────────────────────────

/// How many hexes a War Boar's sensors sweep.
pub const WAR_BOAR_DETECTION_RADIUS: u32 = 5;

/// How many hexes a Raccoon's sensors sweep.
pub const RACCOON_DETECTION_RADIUS: u32 = 2;

/// The radar footprint of a Raccoon (for player recon to detect).
pub const RACCOON_RADAR_SIGNATURE: u32 = 1;

// ── Respawn ───────────────────────────────────────────────────────────────────

/// Minutes before a defeated War Boar respawns at its anchor_hex.
pub const WAR_BOAR_RESPAWN_COOLDOWN_MINS: u32 = 60;

/// Minutes before a defeated Raccoon Pack respawns at its anchor_hex.
pub const RACCOON_RESPAWN_COOLDOWN_MINS: u32 = 30;

// ── Patrol ────────────────────────────────────────────────────────────────────

/// Maximum hexes a Raccoon may wander from its anchor_hex while PATROL.
/// If a random step would exceed this, step back toward anchor instead.
pub const RACCOON_PATROL_RADIUS: u32 = 4;

// ── Constructors ──────────────────────────────────────────────────────────────

/// Creates a War Boar NPC unit in its default ANCHORED state.
///
/// The War Boar holds territory around a collapsed city. It does not move
/// until a convoy enters its detection radius, then switches to INTERCEPT.
pub fn create_war_boar(id: String, anchor_hex: Coordinates, pack: Option<Pack>) -> NpcUnit {
    NpcUnit {
        id,
        npc_type:                  NpcType::WarBoar,
        current_hex:               anchor_hex.clone(),
        anchor_hex,
        state:                     NpcState::Anchored,
        movement_speed:            WAR_BOAR_MOVE_MINS_PER_HEX,
        detection_radius:          WAR_BOAR_DETECTION_RADIUS,
        radar_signature:           0, // War Boars are ambush predators — very low signature
        pack,
        was_spotted_during_approach: false,
        target_convoy_id:          None,
        respawn_cooldown_minutes:  WAR_BOAR_RESPAWN_COOLDOWN_MINS,
    }
}

/// Creates a Raccoon Biker NPC unit in its default PATROL state.
///
/// Raccoons are nomadic opportunists. They wander within RACCOON_PATROL_RADIUS
/// of their anchor_hex until a loud convoy triggers INTERCEPT.
pub fn create_raccoon(id: String, anchor_hex: Coordinates, pack: Option<Pack>) -> NpcUnit {
    NpcUnit {
        id,
        npc_type:                  NpcType::RaccoonBiker,
        current_hex:               anchor_hex.clone(),
        anchor_hex,
        state:                     NpcState::Patrol,
        movement_speed:            RACCOON_MOVE_MINS_PER_HEX,
        detection_radius:          RACCOON_DETECTION_RADIUS,
        radar_signature:           RACCOON_RADAR_SIGNATURE,
        pack,
        was_spotted_during_approach: false,
        target_convoy_id:          None,
        respawn_cooldown_minutes:  RACCOON_RESPAWN_COOLDOWN_MINS,
    }
}
