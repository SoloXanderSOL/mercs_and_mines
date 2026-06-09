use std::collections::HashMap;
use std::f64::consts::PI;
use serde::{Deserialize, Serialize};
use sim_engine::rng::Rng;
use uuid::Uuid;
use tracing;

pub const MAP_RADIUS_HEXES: u32 = 35;
pub const SAFE_ZONE_RADIUS: u32 = 5;
pub const DMZ_INNER_RADIUS: u32 = 6;
pub const DMZ_OUTER_RADIUS: u32 = 7;
pub const FOB_ZONE_INNER_RADIUS: u32 = 8;
pub const FORWARD_OUTPOST_COUNT_MIN: u32 = 3;
pub const FORWARD_OUTPOST_COUNT_MAX: u32 = 5;
pub const FORWARD_OUTPOST_MIN_RADIUS: u32 = 15;
pub const FORWARD_OUTPOST_MAX_RADIUS: u32 = 20;
pub const SPAWN_SEED_NONCE: u32 = 0x9E37_79B9;

const TERRAIN_CUM_ASHEN_PLAINS: f64     = 0.65;
const TERRAIN_CUM_SCORCHED_DESERT: f64  = 0.75;
const TERRAIN_CUM_RUINED_CITY: f64      = 0.83;
const TERRAIN_CUM_MAGMA_FLOW: f64       = 0.87;
const TERRAIN_CUM_COOLED_MAGMA: f64     = 0.91;
const TERRAIN_CUM_VOLCANIC_CALDERA: f64 = 0.93;
const TERRAIN_CUM_MOUNTAIN: f64         = 0.95;
const TERRAIN_CUM_RAVINE: f64           = 0.97;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HexTerrain {
    AshenPlains,
    ScorchedDesert,
    RuinedCity,
    CityState,
    GunOutpost,
    MagmaFlow,
    CooledMagma,
    VolcanicCaldera,
    Mountain,
    Ravine,
    ToxicZone,
}

impl HexTerrain {
    pub fn is_impassable(&self) -> bool {
        matches!(
            self,
            HexTerrain::MagmaFlow
                | HexTerrain::VolcanicCaldera
                | HexTerrain::Mountain
                | HexTerrain::Ravine
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagmaVeinNode {
    pub q: i32,
    pub r: i32,
    pub tier: u8,
    pub is_tectonic_anchor: bool,
}

#[derive(Debug, Clone)]
pub struct SafeZoneLayout {
    pub centre: (i32, i32),
    pub hexes: Vec<(i32, i32)>,
    pub rim_hexes: Vec<(i32, i32)>,
}

#[derive(Debug, Clone)]
pub struct SectorMap {
    pub radius: u32,
    pub terrain: HashMap<String, HexTerrain>,
    pub magma_veins: Vec<MagmaVeinNode>,
    pub safe_zone: SafeZoneLayout,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpawnType {
    SoloQueue,
    CorporateCharter { charter_id: Uuid },
}

#[derive(Debug, Clone)]
pub struct PlayerSpawnRequest {
    pub wallet_address: Vec<u8>,
    pub spawn_type: SpawnType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GatewayAssignment {
    pub wallet_address: Vec<u8>,
    pub q: i32,
    pub r: i32,
}

#[derive(Debug, Clone, PartialEq)]
enum Quadrant {
    NE,
    SE,
    SW,
    NW,
}

fn classify_quadrant(q: i32, r: i32) -> Quadrant {
    if q > 0 && r <= 0 {
        Quadrant::NE
    } else if q >= 0 && r > 0 {
        Quadrant::SE
    } else if q < 0 && r >= 0 {
        Quadrant::SW
    } else {
        Quadrant::NW
    }
}

fn next_quadrant_clockwise(q: Quadrant) -> Quadrant {
    match q {
        Quadrant::NE => Quadrant::SE,
        Quadrant::SE => Quadrant::SW,
        Quadrant::SW => Quadrant::NW,
        Quadrant::NW => Quadrant::NE,
    }
}

/// Assigns a gateway rim hex to each player. Solo players get a uniform random draw.
/// Charter groups are placed together in one quadrant with minimum 3-hex axial separation;
/// overflow spills clockwise to the next quadrant.
pub fn assign_gateway_hexes(
    safe_zone: &SafeZoneLayout,
    players: &[PlayerSpawnRequest],
    rng: &mut Rng,
) -> Vec<GatewayAssignment> {
    // rim_hexes is non-empty for any valid SafeZoneLayout with SAFE_ZONE_RADIUS = 5
    let rim = &safe_zone.rim_hexes;
    let mut results: Vec<GatewayAssignment> = Vec::new();

    let mut solo_players: Vec<&PlayerSpawnRequest> = Vec::new();
    let mut charter_groups: HashMap<Uuid, Vec<&PlayerSpawnRequest>> = HashMap::new();

    for player in players {
        match &player.spawn_type {
            SpawnType::SoloQueue => solo_players.push(player),
            SpawnType::CorporateCharter { charter_id } => {
                charter_groups.entry(*charter_id).or_default().push(player);
            }
        }
    }

    let mut rim_indices: Vec<usize> = (0..rim.len()).collect();
    for i in (1..rim.len()).rev() {
        let j = rng.roll_int(0, i as u32) as usize;
        rim_indices.swap(i, j);
    }
    for (slot, player) in solo_players.iter().enumerate() {
        let idx = if slot < rim_indices.len() {
            rim_indices[slot]
        } else {
            tracing::warn!(
                "solo player count ({}) exceeds rim hex count ({}) — reusing slot 0",
                solo_players.len(), rim.len()
            );
            rim_indices[0]
        };
        results.push(GatewayAssignment {
            wallet_address: player.wallet_address.clone(),
            q: rim[idx].0,
            r: rim[idx].1,
        });
    }

    for (charter_id, members) in &charter_groups {
        let q_idx = rng.roll_int(0, 3);
        let mut current_quadrant = match q_idx {
            0 => Quadrant::NE,
            1 => Quadrant::SE,
            2 => Quadrant::SW,
            _ => Quadrant::NW,
        };
        let mut quadrant_hexes: Vec<(i32, i32)> = rim
            .iter()
            .copied()
            .filter(|&(q, r)| classify_quadrant(q, r) == current_quadrant)
            .collect();
        let mut assigned: Vec<(i32, i32)> = Vec::new();

        for member in members {
            let valid: Vec<(i32, i32)> = quadrant_hexes
                .iter()
                .copied()
                .filter(|&(hq, hr)| {
                    !assigned.contains(&(hq, hr))
                        && assigned
                            .iter()
                            .all(|&(aq, ar)| hex_axial_distance(hq, hr, aq, ar) >= 3)
                })
                .collect();

            let chosen = if !valid.is_empty() {
                let idx = rng.roll_int(0, valid.len() as u32 - 1) as usize;
                valid[idx]
            } else {
                current_quadrant = next_quadrant_clockwise(current_quadrant);
                quadrant_hexes = rim
                    .iter()
                    .copied()
                    .filter(|&(q, r)| classify_quadrant(q, r) == current_quadrant)
                    .collect();
                tracing::warn!(
                    "Charter {:?} spilling to adjacent quadrant — insufficient valid \
                     hexes with 3-hex separation in starting quadrant",
                    charter_id
                );
                let valid2: Vec<(i32, i32)> = quadrant_hexes
                    .iter()
                    .copied()
                    .filter(|&(hq, hr)| {
                        !assigned.contains(&(hq, hr))
                            && assigned
                                .iter()
                                .all(|&(aq, ar)| hex_axial_distance(hq, hr, aq, ar) >= 3)
                    })
                    .collect();
                if !valid2.is_empty() {
                    let idx = rng.roll_int(0, valid2.len() as u32 - 1) as usize;
                    valid2[idx]
                } else {
                    *rim.iter()
                        .find(|&&hex| !assigned.contains(&hex))
                        .unwrap_or(&rim[0])
                }
            };

            assigned.push(chosen);
            results.push(GatewayAssignment {
                wallet_address: member.wallet_address.clone(),
                q: chosen.0,
                r: chosen.1,
            });
        }
    }

    results
}

/// Axial distance between two hex coordinates using cube coordinate formula.
pub fn hex_axial_distance(q1: i32, r1: i32, q2: i32, r2: i32) -> u32 {
    let dq = q1 - q2;
    let dr = r1 - r2;
    ((dq.abs() + (dq + dr).abs() + dr.abs()) / 2) as u32
}

/// Convert polar coordinates to the nearest valid axial hex coordinate.
/// Uses cube-coordinate rounding to snap to the hex grid.
fn polar_to_axial(angle_rad: f64, radius: f64) -> (i32, i32) {
    let fx = radius * angle_rad.cos();
    let fz = radius * angle_rad.sin();
    let fy = -fx - fz;
    let rx = fx.round() as i32;
    let ry = fy.round() as i32;
    let rz = fz.round() as i32;
    let dx = (rx as f64 - fx).abs();
    let dy = (ry as f64 - fy).abs();
    let dz = (rz as f64 - fz).abs();
    if dx > dy && dx > dz {
        (-ry - rz, rz)
    } else if dy > dz {
        (rx, rz)
    } else {
        (rx, -rx - ry)
    }
}

fn vein_exists_at(veins: &[MagmaVeinNode], q: i32, r: i32) -> bool {
    veins.iter().any(|v| v.q == q && v.r == r)
}

fn terrain_passable_for_vein(
    q: i32,
    r: i32,
    terrain: &HashMap<String, HexTerrain>,
) -> bool {
    let key = format!("{},{}", q, r);
    !terrain.get(&key).map(|t| t.is_impassable()).unwrap_or(true)
}

pub fn generate_sector_map(seed: u32, radius: u32) -> SectorMap {
    let mut rng = Rng::new(seed);
    let mut terrain: HashMap<String, HexTerrain> = HashMap::new();
    let mut safe_zone_hexes: Vec<(i32, i32)> = Vec::new();
    let mut safe_zone_rim: Vec<(i32, i32)> = Vec::new();

    // Phase A: terrain fill
    for q in -(radius as i32)..=(radius as i32) {
        let r_min = (-(radius as i32)).max(-q - radius as i32);
        let r_max = (radius as i32).min(-q + radius as i32);
        for r in r_min..=r_max {
            let dist = hex_axial_distance(q, r, 0, 0);
            let hex_terrain = if dist == radius {
                HexTerrain::Mountain
            } else if dist <= SAFE_ZONE_RADIUS {
                HexTerrain::CityState
            } else {
                let roll = rng.next_f64();
                if roll < TERRAIN_CUM_ASHEN_PLAINS {
                    HexTerrain::AshenPlains
                } else if roll < TERRAIN_CUM_SCORCHED_DESERT {
                    HexTerrain::ScorchedDesert
                } else if roll < TERRAIN_CUM_RUINED_CITY {
                    HexTerrain::RuinedCity
                } else if roll < TERRAIN_CUM_MAGMA_FLOW {
                    HexTerrain::MagmaFlow
                } else if roll < TERRAIN_CUM_COOLED_MAGMA {
                    HexTerrain::CooledMagma
                } else if roll < TERRAIN_CUM_VOLCANIC_CALDERA {
                    HexTerrain::VolcanicCaldera
                } else if roll < TERRAIN_CUM_MOUNTAIN {
                    HexTerrain::Mountain
                } else if roll < TERRAIN_CUM_RAVINE {
                    HexTerrain::Ravine
                } else {
                    HexTerrain::ToxicZone
                }
            };
            if dist <= SAFE_ZONE_RADIUS {
                safe_zone_hexes.push((q, r));
                if dist == SAFE_ZONE_RADIUS {
                    safe_zone_rim.push((q, r));
                }
            }
            terrain.insert(format!("{},{}", q, r), hex_terrain);
        }
    }

    let safe_zone = SafeZoneLayout {
        centre: (0, 0),
        hexes: safe_zone_hexes,
        rim_hexes: safe_zone_rim,
    };

    let mut magma_veins: Vec<MagmaVeinNode> = Vec::new();

    // Phase B: magma vein placement

    // Band 1: T1–T2, distances 6–12
    let band1_count = rng.roll_int(20, 30);
    for _ in 0..band1_count {
        let angle = rng.next_f64() * 2.0 * PI;
        let dist = rng.roll_int(6, 12);
        let (q, r) = polar_to_axial(angle, dist as f64);
        let actual_dist = hex_axial_distance(q, r, 0, 0);
        if !terrain_passable_for_vein(q, r, &terrain)
            || actual_dist <= SAFE_ZONE_RADIUS
            || vein_exists_at(&magma_veins, q, r)
        {
            continue;
        }
        let tier = match actual_dist {
            6..=12 => 1,
            13..=20 => 3,
            _ => 5,
        };
        magma_veins.push(MagmaVeinNode { q, r, tier, is_tectonic_anchor: false });
    }

    // Band 2: T3–T4, distances 13–20
    let band2_count = rng.roll_int(15, 25);
    for _ in 0..band2_count {
        let angle = rng.next_f64() * 2.0 * PI;
        let dist = rng.roll_int(13, 20);
        let (q, r) = polar_to_axial(angle, dist as f64);
        let actual_dist = hex_axial_distance(q, r, 0, 0);
        if !terrain_passable_for_vein(q, r, &terrain)
            || actual_dist <= SAFE_ZONE_RADIUS
            || vein_exists_at(&magma_veins, q, r)
        {
            continue;
        }
        let tier = match actual_dist {
            6..=12 => 1,
            13..=20 => 3,
            _ => 5,
        };
        magma_veins.push(MagmaVeinNode { q, r, tier, is_tectonic_anchor: false });
    }

    // Band 3: T5, distances 21 to radius-2
    let band3_count = rng.roll_int(10, 20);
    let band3_max = radius.saturating_sub(2);
    for _ in 0..band3_count {
        let angle = rng.next_f64() * 2.0 * PI;
        let dist = rng.roll_int(21, band3_max);
        let (q, r) = polar_to_axial(angle, dist as f64);
        let actual_dist = hex_axial_distance(q, r, 0, 0);
        if !terrain_passable_for_vein(q, r, &terrain)
            || actual_dist <= SAFE_ZONE_RADIUS
            || vein_exists_at(&magma_veins, q, r)
        {
            continue;
        }
        let tier = match actual_dist {
            6..=12 => 1,
            13..=20 => 3,
            _ => 5,
        };
        magma_veins.push(MagmaVeinNode { q, r, tier, is_tectonic_anchor: false });
    }

    // Phase C: forward outpost placement
    let outpost_count = rng.roll_int(FORWARD_OUTPOST_COUNT_MIN, FORWARD_OUTPOST_COUNT_MAX);
    let sector_angle = 2.0 * PI / outpost_count as f64;

    const NEIGHBOURS: [(i32, i32); 6] =
        [(1, 0), (-1, 0), (0, 1), (0, -1), (1, -1), (-1, 1)];

    for i in 0..outpost_count {
        let base_angle = i as f64 * sector_angle;
        let angular_offset = rng.next_f64() * sector_angle;
        let angle = base_angle + angular_offset;
        let dist = rng.roll_int(FORWARD_OUTPOST_MIN_RADIUS, FORWARD_OUTPOST_MAX_RADIUS);
        let (q, r) = polar_to_axial(angle, dist as f64);

        let (oq, or_) = {
            let key = format!("{},{}", q, r);
            if terrain.get(&key).map(|t| t.is_impassable()).unwrap_or(false) {
                polar_to_axial(base_angle + sector_angle * 0.5, dist as f64)
            } else {
                (q, r)
            }
        };

        terrain.insert(format!("{},{}", oq, or_), HexTerrain::GunOutpost);

        // Place tectonic anchor on a valid neighbour — immediate ring first
        let mut anchor_placed = false;
        for &(dq, dr) in &NEIGHBOURS {
            let nq = oq + dq;
            let nr = or_ + dr;
            let ndist = hex_axial_distance(nq, nr, 0, 0);
            if ndist > radius - 1 { continue; }
            if !terrain_passable_for_vein(nq, nr, &terrain) { continue; }
            if ndist <= SAFE_ZONE_RADIUS { continue; }
            if vein_exists_at(&magma_veins, nq, nr) { continue; }
            magma_veins.push(MagmaVeinNode { q: nq, r: nr, tier: 3, is_tectonic_anchor: true });
            anchor_placed = true;
            break;
        }

        // Fall back to distance-2 ring if no immediate neighbour was valid
        if !anchor_placed {
            'outer: for dq in -2i32..=2 {
                for dr in -2i32..=2 {
                    let nq = oq + dq;
                    let nr = or_ + dr;
                    if hex_axial_distance(oq, or_, nq, nr) != 2 { continue; }
                    let ndist = hex_axial_distance(nq, nr, 0, 0);
                    if ndist > radius - 1 { continue; }
                    if !terrain_passable_for_vein(nq, nr, &terrain) { continue; }
                    if ndist <= SAFE_ZONE_RADIUS { continue; }
                    if vein_exists_at(&magma_veins, nq, nr) { continue; }
                    magma_veins.push(MagmaVeinNode { q: nq, r: nr, tier: 3, is_tectonic_anchor: true });
                    break 'outer;
                }
            }
        }
    }

    SectorMap { radius, terrain, magma_veins, safe_zone }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_safe_zone() -> SafeZoneLayout {
        generate_sector_map(42, MAP_RADIUS_HEXES).safe_zone
    }

    #[test]
    fn test_determinism() {
        let map1 = generate_sector_map(12345, MAP_RADIUS_HEXES);
        let map2 = generate_sector_map(12345, MAP_RADIUS_HEXES);
        assert_eq!(map1.terrain.len(), map2.terrain.len());
        for (k, v) in &map1.terrain {
            assert_eq!(
                map2.terrain.get(k),
                Some(v),
                "terrain mismatch at {}",
                k
            );
        }
        assert_eq!(map1.magma_veins.len(), map2.magma_veins.len());
        for (i, (v1, v2)) in map1.magma_veins.iter().zip(map2.magma_veins.iter()).enumerate() {
            assert_eq!(v1, v2, "vein {} differs", i);
        }
    }

    #[test]
    fn test_safe_zone_is_city_state() {
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        for &(q, r) in &map.safe_zone.hexes {
            let key = format!("{},{}", q, r);
            assert_eq!(
                map.terrain.get(&key),
                Some(&HexTerrain::CityState),
                "safe zone hex ({},{}) is not CityState",
                q, r
            );
        }
    }

    #[test]
    fn test_rim_hexes_at_distance_5() {
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        for &(q, r) in &map.safe_zone.rim_hexes {
            assert_eq!(
                hex_axial_distance(q, r, 0, 0),
                SAFE_ZONE_RADIUS,
                "rim hex ({},{}) is not at distance {}",
                q, r, SAFE_ZONE_RADIUS
            );
        }
    }

    #[test]
    fn test_vein_tiers_match_distance_bands() {
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        for vein in &map.magma_veins {
            if vein.is_tectonic_anchor {
                continue;
            }
            let dist = hex_axial_distance(vein.q, vein.r, 0, 0);
            let expected_tier: u8 = match dist {
                6..=12 => 1,
                13..=20 => 3,
                _ => 5,
            };
            assert_eq!(
                vein.tier, expected_tier,
                "vein at ({},{}) dist {} has tier {} but expected {}",
                vein.q, vein.r, dist, vein.tier, expected_tier
            );
        }
    }

    #[test]
    fn test_no_vein_on_impassable_terrain() {
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        for vein in &map.magma_veins {
            let key = format!("{},{}", vein.q, vein.r);
            let terrain = map.terrain.get(&key).expect("vein at hex with no terrain entry");
            assert!(
                !terrain.is_impassable(),
                "vein at ({},{}) is on impassable terrain {:?}",
                vein.q, vein.r, terrain
            );
        }
    }

    #[test]
    fn test_tectonic_anchors_exist_at_expected_radius() {
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        let anchors: Vec<&MagmaVeinNode> =
            map.magma_veins.iter().filter(|v| v.is_tectonic_anchor).collect();
        assert!(!anchors.is_empty(), "no tectonic anchors placed");
        // polar_to_axial cube-rounding can snap an outpost ±1 beyond the rolled radius,
        // and the neighbour step adds another ±1, so effective tolerance is ±2.
        let lo = FORWARD_OUTPOST_MIN_RADIUS.saturating_sub(2);
        let hi = FORWARD_OUTPOST_MAX_RADIUS + 2;
        for anchor in anchors {
            let dist = hex_axial_distance(anchor.q, anchor.r, 0, 0);
            assert!(
                dist >= lo && dist <= hi,
                "tectonic anchor at ({},{}) dist {} is outside expected range {}-{}",
                anchor.q, anchor.r, dist, lo, hi
            );
        }
    }

    #[test]
    fn test_gun_outpost_count_in_range() {
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        let count = map
            .terrain
            .values()
            .filter(|t| **t == HexTerrain::GunOutpost)
            .count() as u32;
        assert!(
            count >= FORWARD_OUTPOST_COUNT_MIN && count <= FORWARD_OUTPOST_COUNT_MAX,
            "GunOutpost count {} not in [{}, {}]",
            count, FORWARD_OUTPOST_COUNT_MIN, FORWARD_OUTPOST_COUNT_MAX
        );
    }

    #[test]
    fn test_every_gun_outpost_adjacent_to_tectonic_anchor() {
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        let anchor_coords: Vec<(i32, i32)> = map
            .magma_veins
            .iter()
            .filter(|v| v.is_tectonic_anchor)
            .map(|v| (v.q, v.r))
            .collect();

        for (key, terrain) in &map.terrain {
            if *terrain != HexTerrain::GunOutpost {
                continue;
            }
            let parts: Vec<i32> = key.split(',').map(|s| s.parse().unwrap()).collect();
            let (q, r) = (parts[0], parts[1]);
            let neighbours = [
                (q + 1, r), (q - 1, r),
                (q, r + 1), (q, r - 1),
                (q + 1, r - 1), (q - 1, r + 1),
            ];
            let has_anchor = neighbours.iter().any(|pos| anchor_coords.contains(pos));
            assert!(
                has_anchor,
                "GunOutpost at ({},{}) has no adjacent tectonic anchor",
                q, r
            );
        }
    }

    #[test]
    fn test_no_gun_outpost_on_impassable_terrain() {
        assert!(
            !HexTerrain::GunOutpost.is_impassable(),
            "GunOutpost.is_impassable() must return false"
        );
        let map = generate_sector_map(12345, MAP_RADIUS_HEXES);
        for (key, terrain) in &map.terrain {
            if *terrain == HexTerrain::GunOutpost {
                assert!(
                    !terrain.is_impassable(),
                    "GunOutpost at {} is marked impassable",
                    key
                );
            }
        }
    }

    #[test]
    fn test_solo_queue_lands_on_rim_hex() {
        let safe_zone = test_safe_zone();
        let players = vec![PlayerSpawnRequest {
            wallet_address: vec![1, 2, 3],
            spawn_type: SpawnType::SoloQueue,
        }];
        let mut rng = sim_engine::rng::Rng::new(42);
        let assignments = assign_gateway_hexes(&safe_zone, &players, &mut rng);
        assert_eq!(assignments.len(), 1);
        let a = &assignments[0];
        assert!(
            safe_zone.rim_hexes.contains(&(a.q, a.r)),
            "assignment ({},{}) not in rim_hexes",
            a.q, a.r
        );
    }

    #[test]
    fn test_charter_members_in_same_quadrant() {
        let safe_zone = test_safe_zone();
        let charter_id = Uuid::nil();
        let players = vec![
            PlayerSpawnRequest { wallet_address: vec![1], spawn_type: SpawnType::CorporateCharter { charter_id } },
            PlayerSpawnRequest { wallet_address: vec![2], spawn_type: SpawnType::CorporateCharter { charter_id } },
            PlayerSpawnRequest { wallet_address: vec![3], spawn_type: SpawnType::CorporateCharter { charter_id } },
        ];
        let mut rng = sim_engine::rng::Rng::new(42);
        let assignments = assign_gateway_hexes(&safe_zone, &players, &mut rng);
        assert_eq!(assignments.len(), 3);
        let first_quadrant = super::classify_quadrant(assignments[0].q, assignments[0].r);
        for a in &assignments {
            assert_eq!(
                super::classify_quadrant(a.q, a.r),
                first_quadrant,
                "assignment ({},{}) is in a different quadrant",
                a.q, a.r
            );
        }
    }

    #[test]
    fn test_charter_pairs_have_min_3_hex_separation() {
        let safe_zone = test_safe_zone();
        let charter_id = Uuid::nil();
        let players = vec![
            PlayerSpawnRequest { wallet_address: vec![1], spawn_type: SpawnType::CorporateCharter { charter_id } },
            PlayerSpawnRequest { wallet_address: vec![2], spawn_type: SpawnType::CorporateCharter { charter_id } },
            PlayerSpawnRequest { wallet_address: vec![3], spawn_type: SpawnType::CorporateCharter { charter_id } },
            PlayerSpawnRequest { wallet_address: vec![4], spawn_type: SpawnType::CorporateCharter { charter_id } },
        ];
        let mut rng = sim_engine::rng::Rng::new(42);
        let assignments = assign_gateway_hexes(&safe_zone, &players, &mut rng);
        assert_eq!(assignments.len(), 4);
        for i in 0..assignments.len() {
            for j in (i + 1)..assignments.len() {
                let a = &assignments[i];
                let b = &assignments[j];
                let dist = hex_axial_distance(a.q, a.r, b.q, b.r);
                assert!(
                    dist >= 3,
                    "pair ({},{}) and ({},{}) have separation {} < 3",
                    a.q, a.r, b.q, b.r, dist
                );
            }
        }
    }

    #[test]
    fn test_five_member_charter_fully_placed() {
        let safe_zone = test_safe_zone();
        let charter_id = Uuid::nil();
        let players: Vec<PlayerSpawnRequest> = (1u8..=5)
            .map(|i| PlayerSpawnRequest {
                wallet_address: vec![i],
                spawn_type: SpawnType::CorporateCharter { charter_id },
            })
            .collect();
        let mut rng = sim_engine::rng::Rng::new(42);
        let assignments = assign_gateway_hexes(&safe_zone, &players, &mut rng);
        assert_eq!(assignments.len(), 5, "expected 5 assignments");
        for a in &assignments {
            assert!(
                safe_zone.rim_hexes.contains(&(a.q, a.r)),
                "assignment ({},{}) not in rim_hexes",
                a.q, a.r
            );
        }
    }

    #[test]
    fn test_assign_gateway_hexes_is_deterministic() {
        let safe_zone = test_safe_zone();
        let charter_id = Uuid::nil();
        let players = vec![
            PlayerSpawnRequest { wallet_address: vec![1], spawn_type: SpawnType::SoloQueue },
            PlayerSpawnRequest { wallet_address: vec![2], spawn_type: SpawnType::SoloQueue },
            PlayerSpawnRequest { wallet_address: vec![3], spawn_type: SpawnType::CorporateCharter { charter_id } },
            PlayerSpawnRequest { wallet_address: vec![4], spawn_type: SpawnType::CorporateCharter { charter_id } },
            PlayerSpawnRequest { wallet_address: vec![5], spawn_type: SpawnType::CorporateCharter { charter_id } },
        ];
        let mut rng1 = sim_engine::rng::Rng::new(42);
        let result1 = assign_gateway_hexes(&safe_zone, &players, &mut rng1);
        let mut rng2 = sim_engine::rng::Rng::new(42);
        let result2 = assign_gateway_hexes(&safe_zone, &players, &mut rng2);
        assert_eq!(result1, result2);
    }
}
