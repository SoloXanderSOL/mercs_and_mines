// Detection helpers for NPC AI — thin wrappers over resolver::check_detection.
// Ref: Detection_and_Fog_of_War.md §2–§3

use crate::game_types::{EntityVision, NpcUnit};
use crate::resolver::check_detection;

/// Checks whether an NPC can detect a convoy.
///
/// Formula (Detection_and_Fog_of_War.md §3):
///   detection_range = max(npc.detection_radius, convoy_noise_radius)
///   detected        = distance <= detection_range
pub fn npc_detects_convoy(npc: &NpcUnit, convoy_noise_radius: u32, distance: u32) -> bool {
    check_detection(npc.detection_radius, convoy_noise_radius, distance)
}

/// Checks whether a player recon asset can spot an approaching NPC.
///
/// @param recon_vision_radius  The recon asset's vision_radius (Bloodhound: 2, Owl: 5)
pub fn recon_spots_npc(recon_vision_radius: u32, npc: &NpcUnit, distance: u32) -> bool {
    check_detection(recon_vision_radius, npc.radar_signature, distance)
}

/// Returns the best recon vision_radius in the convoy's recon assets, or 0 if none present.
pub fn get_best_recon_vision(recon_assets: &[EntityVision]) -> u32 {
    recon_assets.iter().map(|a| a.vision_radius).max().unwrap_or(0)
}
