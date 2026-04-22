// NPC AI — patrol movement for PATROL-state units.
// Ref: Detection_and_Fog_of_War.md §2 · Hex_Map_and_Travel.md §6

use crate::game_types::{Coordinates, NpcUnit};
use crate::npc_types::RACCOON_PATROL_RADIUS;
use crate::resolver::calculate_hex_distance;
use crate::rng::Rng;
use shared::hex::{get_neighbors, step_toward};

/// Advances a PATROL Raccoon one hex in a random direction, subject to leash.
///
/// Algorithm:
///   1. Fisher-Yates shuffle the six neighbours using the seeded RNG.
///   2. Pick the first neighbour whose distance from anchor_hex <= RACCOON_PATROL_RADIUS.
///   3. If ALL neighbours violate the leash, step toward anchor_hex instead.
///
/// Mutates `npc.current_hex` in-place and returns the new hex.
pub fn raccoon_patrol_step(npc: &mut NpcUnit, rng: &mut Rng) -> Coordinates {
    let mut neighbors = get_neighbors(&npc.current_hex);

    // Fisher-Yates shuffle using seeded RNG
    let len = neighbors.len();
    for i in (1..len).rev() {
        let j = (rng.next_f64() * (i + 1) as f64) as usize;
        neighbors.swap(i, j);
    }

    // Pick first neighbour within leash radius
    for candidate in &neighbors {
        if calculate_hex_distance(candidate, &npc.anchor_hex) <= RACCOON_PATROL_RADIUS {
            npc.current_hex = candidate.clone();
            return candidate.clone();
        }
    }

    // All neighbours violate leash — step back toward anchor
    let fallback = step_toward(&npc.current_hex, &npc.anchor_hex);
    npc.current_hex = fallback.clone();
    fallback
}
