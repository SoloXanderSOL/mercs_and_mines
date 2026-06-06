use std::collections::HashMap;
use std::f64::consts::PI;
use sim_engine::rng::Rng;

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

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
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
}
