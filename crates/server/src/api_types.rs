// Request types for all simulation endpoints.
// VehicleClass is canonical (game_types.rs §12) — imported from sim_engine, not redefined here.
// convoy_vehicle_from_class is a server-layer concern: maps the enum to the resolver's ConvoyVehicle.
// Fuel rule: Hauler = 10 He3/hex (Gas Guzzler); all other classes = 5 He3/hex.

use serde::{Deserialize, Serialize};
use sim_engine::game_types::{Commander, Unit, VehicleClass};
use uuid::Uuid;
use sim_engine::types::{CombatInitiationType, ConvoyVehicle, Section, Vehicle};

pub fn convoy_vehicle_from_class(class: &VehicleClass) -> ConvoyVehicle {
    let (name, fuel_cost_per_hex, radar_signature) = match class {
        VehicleClass::LightAttack   => ("Light Attack Vehicle", 5,  2),
        VehicleClass::Transport     => ("Transport",             5,  4),
        VehicleClass::MediumArmor   => ("Medium Armor",          5,  3),
        VehicleClass::HeavySiege    => ("Heavy Siege",           5,  5),
        VehicleClass::Recon         => ("Recon",                 5,  1),
        VehicleClass::ExtractionRig => ("Extraction Rig",        5,  4),
        VehicleClass::MiningSupport => ("Mining Support",        5,  3),
        VehicleClass::Hauler        => ("Hauler",                10, 5),
        VehicleClass::Engineering   => ("Engineering",           5,  3),
    };
    ConvoyVehicle { name: name.into(), fuel_cost_per_hex, radar_signature }
}

/// `POST /api/mission/resolve` (brick 4g-1). The Commander is named by `commander_id` and
/// read from `commander_records`. The two `Option<Value>` fields exist only to be refused:
/// `null` deserialises to `None`, so `Some(_)` means a non-null value was sent, and the
/// route answers `400` rather than silently ignoring it.
#[derive(Debug, Deserialize, Serialize)]
pub struct MissionResolveRequest {
    pub squad: MissionSquadRequest,
    pub mission_id: String,
    pub commander_id: Uuid,
    /// Refused on this route: a client-chosen seed would let a player replay a proven win.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_override: Option<serde_json::Value>,
}

/// The HTTP body of `POST /api/mission/resolve` (7b-2 R1): the logged request plus the
/// Sections the mission sends. Not logged itself: `payload.request` stays the
/// `MissionResolveRequest`, and the selection goes in `payload.selection`. A missing
/// `section_ids` reads as empty, which the route refuses with its own `400` (axum's
/// rejection of a missing field would be a plain-text `422`, the rank cap's status).
#[derive(Debug, Deserialize, Serialize)]
pub struct MissionResolveBody {
    #[serde(flatten)]
    pub request: MissionResolveRequest,
    #[serde(default)]
    pub section_ids: Vec<Uuid>,
}

/// The mission request's squad. The sim `Squad` is built from it with `commander: None`.
#[derive(Debug, Deserialize, Serialize)]
pub struct MissionSquadRequest {
    pub units: Vec<Unit>,
    /// Refused on this route: a body carrying a Commander gets `400`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commander: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CombatResolveRequest {
    pub section: Section,
    pub vehicle: Vehicle,
    pub max_ticks: Option<usize>,
    pub seed_override: Option<u32>,
    pub combat_initiation_type: Option<CombatInitiationType>,
    pub defending_convoy_vehicles: Option<Vec<VehicleClass>>,
    pub commander: Option<Commander>,
}

// Work bounds on a combat request (BP-1e-1, GAP-87). Every client-chosen work count and
// every value that reaches an `i32` sum is bounded, so one request can't run long, allocate
// without limit or overflow (`overflow-checks` is on in release). Over a bound the route
// answers `400`; nothing is clamped, so the logged request is the one that ran (R7).

/// The most ticks a combat may run, checked on the effective value (the request's
/// `max_ticks`, or the server default when it sends none).
pub const COMBAT_MAX_TICKS: usize = 100;
/// A Section is at most 8 soldiers (canon).
pub const MAX_SECTION_STRENGTH: u32 = 8;
pub const MAX_INDIVIDUAL_HP: i32 = 1000;
pub const MAX_VEHICLE_WEAPONS: usize = 4;
pub const MAX_VEHICLE_HP: i32 = 10_000;
/// Every accuracy, evasion, AP, AT, damage and Commander buff lies within ±this.
pub const MAX_COMBAT_STAT: i32 = 1000;
pub const MAX_DEFENDING_VEHICLES: usize = 16;
/// Every client string (ids, names, the Commander's text), in bytes.
pub const MAX_COMBAT_NAME_BYTES: usize = 64;

fn check_stat(value: i32, what: &str) -> Result<(), String> {
    if (-MAX_COMBAT_STAT..=MAX_COMBAT_STAT).contains(&value) {
        Ok(())
    } else {
        Err(format!("{what} {value} is outside ±{MAX_COMBAT_STAT}"))
    }
}

fn check_name(value: &str, what: &str) -> Result<(), String> {
    if value.len() <= MAX_COMBAT_NAME_BYTES {
        Ok(())
    } else {
        Err(format!("{what} is longer than {MAX_COMBAT_NAME_BYTES} bytes"))
    }
}

/// Checks a combat request against the work bounds above. `default_max_ticks` is the
/// server's default, so the check covers the `max_ticks` that would actually run. Returns
/// the first violation. `seed_override` is not checked here: the live routes refuse it
/// themselves, and the AAR replays the logged seed, never the request's.
pub fn validate_combat_request(req: &CombatResolveRequest, default_max_ticks: usize) -> Result<(), String> {
    let ticks = req.max_ticks.unwrap_or(default_max_ticks);
    if ticks > COMBAT_MAX_TICKS {
        return Err(format!("max_ticks {ticks} exceeds {COMBAT_MAX_TICKS}"));
    }
    let s = &req.section;
    if s.max_strength > MAX_SECTION_STRENGTH {
        return Err(format!("section max_strength {} exceeds {MAX_SECTION_STRENGTH}", s.max_strength));
    }
    if s.current_strength > s.max_strength {
        return Err("section current_strength exceeds max_strength".into());
    }
    if !(1..=MAX_INDIVIDUAL_HP).contains(&s.individual_hp) {
        return Err(format!("section individual_hp {} is outside 1..={MAX_INDIVIDUAL_HP}", s.individual_hp));
    }
    check_stat(s.accuracy, "section accuracy")?;
    check_stat(s.evasion, "section evasion")?;
    check_stat(s.armor_at, "section armor_at")?;
    check_stat(s.weapon.ap, "section weapon ap")?;
    check_stat(s.weapon.base_damage, "section weapon base_damage")?;
    check_stat(s.weapon.accuracy, "section weapon accuracy")?;
    check_name(&s.id, "section id")?;
    check_name(&s.name, "section name")?;
    check_name(&s.weapon.name, "section weapon name")?;
    let v = &req.vehicle;
    if v.weapons.len() > MAX_VEHICLE_WEAPONS {
        return Err(format!("vehicle has {} weapons; at most {MAX_VEHICLE_WEAPONS}", v.weapons.len()));
    }
    if !(1..=MAX_VEHICLE_HP).contains(&v.max_hp) {
        return Err(format!("vehicle max_hp {} is outside 1..={MAX_VEHICLE_HP}", v.max_hp));
    }
    if !(0..=v.max_hp).contains(&v.hp) {
        return Err(format!("vehicle hp {} is outside 0..=max_hp", v.hp));
    }
    check_stat(v.at, "vehicle at")?;
    check_stat(v.evasion, "vehicle evasion")?;
    check_name(&v.id, "vehicle id")?;
    check_name(&v.name, "vehicle name")?;
    for w in &v.weapons {
        check_stat(w.ap, "vehicle weapon ap")?;
        check_stat(w.base_damage, "vehicle weapon base_damage")?;
        check_stat(w.accuracy, "vehicle weapon accuracy")?;
        check_name(&w.name, "vehicle weapon name")?;
    }
    if let Some(d) = &req.defending_convoy_vehicles {
        if d.len() > MAX_DEFENDING_VEHICLES {
            return Err(format!("{} defending vehicles; at most {MAX_DEFENDING_VEHICLES}", d.len()));
        }
    }
    if let Some(c) = &req.commander {
        check_stat(c.success_aura, "commander success_aura")?;
        check_stat(c.passive_buffs.accuracy, "commander accuracy buff")?;
        check_stat(c.passive_buffs.evasion, "commander evasion buff")?;
        check_stat(c.passive_buffs.damage_reduction, "commander damage_reduction buff")?;
        check_name(&c.id, "commander id")?;
        check_name(&c.name, "commander name")?;
        check_name(&c.ability, "commander ability")?;
        check_name(&c.flavor_text, "commander flavor_text")?;
        if let Some(u) = &c.attached_unit_id {
            check_name(u, "commander attached_unit_id")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sim `Commander.species` field was removed in 4a-2. A logged `session_start`
    /// payload that still carries the key must deserialise (serde ignores the unknown
    /// key), so the removal cannot break an AAR replay. The value is a removed variant.
    #[test]
    fn combat_resolve_request_ignores_a_legacy_species_key() {
        let logged = serde_json::json!({
            "section": {
                "id": "alpha", "name": "Alpha Section", "max_strength": 5, "current_strength": 5,
                "individual_hp": 10, "accuracy": 60, "evasion": 20,
                "weapon": { "name": "Laser Carbine", "ap": 4, "base_damage": 8, "tag": "Laser", "accuracy": 0 },
                "armor_at": 3, "armor_tag": "LightArmor"
            },
            "vehicle": {
                "id": "scout-1", "name": "Armored Scout", "hp": 40, "max_hp": 40, "at": 6,
                "armor_tag": "HeavyArmor", "evasion": 10,
                "weapons": [{ "name": "Scout Cannon", "ap": 6, "base_damage": 12, "tag": "Slug", "accuracy": 0 }]
            },
            "max_ticks": 25,
            "seed_override": 42,
            "combat_initiation_type": "SPOTTED",
            "defending_convoy_vehicles": null,
            "commander": {
                "id": "cmd-legacy", "name": "Legacy Commander", "species": "Hamster Attachment",
                "rank": 1, "skill": 1, "success_aura": 0, "quality_grade": "Standard",
                "ability": "", "flavor_text": "", "stress_level": 0,
                "is_kia": false, "is_shattered": false, "can_retreat": true,
                "passive_buffs": { "accuracy": 0, "evasion": 30, "damage_reduction": 0 },
                "attached_unit_id": null
            }
        });
        let req: CombatResolveRequest = serde_json::from_value(logged)
            .expect("a logged payload carrying the removed species key must still deserialise");
        assert_eq!(req.commander.expect("commander present").name, "Legacy Commander");
    }

    /// BP-1e-1: every bound admits its edge and refuses one past it.
    #[test]
    fn validate_combat_request_bounds_at_the_edges() {
        let base: CombatResolveRequest = serde_json::from_value(serde_json::json!({
            "section": {
                "id": "alpha", "name": "Alpha Section", "max_strength": 5, "current_strength": 5,
                "individual_hp": 10, "accuracy": 60, "evasion": 20,
                "weapon": { "name": "Laser Carbine", "ap": 4, "base_damage": 8, "tag": "Laser", "accuracy": 0 },
                "armor_at": 3, "armor_tag": "LightArmor"
            },
            "vehicle": {
                "id": "scout-1", "name": "Armored Scout", "hp": 40, "max_hp": 40, "at": 6,
                "armor_tag": "HeavyArmor", "evasion": 10,
                "weapons": [{ "name": "Scout Cannon", "ap": 6, "base_damage": 12, "tag": "Slug", "accuracy": 0 }]
            },
            "max_ticks": 25,
            "seed_override": null,
            "combat_initiation_type": "SPOTTED",
            "defending_convoy_vehicles": null,
            "commander": null
        }))
        .expect("fixture");
        let ok = |r: &CombatResolveRequest| validate_combat_request(r, 50).is_ok();
        assert!(ok(&base), "the fixture is in bounds");

        let mut r = base.clone();
        r.max_ticks = Some(COMBAT_MAX_TICKS);
        assert!(ok(&r));
        r.max_ticks = Some(COMBAT_MAX_TICKS + 1);
        assert!(!ok(&r));
        r.max_ticks = None;
        assert!(validate_combat_request(&r, COMBAT_MAX_TICKS).is_ok());
        assert!(validate_combat_request(&r, COMBAT_MAX_TICKS + 1).is_err(), "the default counts");

        let mut r = base.clone();
        r.section.max_strength = MAX_SECTION_STRENGTH;
        r.section.current_strength = MAX_SECTION_STRENGTH;
        assert!(ok(&r));
        r.section.max_strength = MAX_SECTION_STRENGTH + 1;
        assert!(!ok(&r));
        let mut r = base.clone();
        r.section.current_strength = r.section.max_strength + 1;
        assert!(!ok(&r));

        let mut r = base.clone();
        r.section.individual_hp = MAX_INDIVIDUAL_HP;
        assert!(ok(&r));
        r.section.individual_hp = MAX_INDIVIDUAL_HP + 1;
        assert!(!ok(&r));
        r.section.individual_hp = 0;
        assert!(!ok(&r));

        let mut r = base.clone();
        r.section.accuracy = MAX_COMBAT_STAT;
        r.section.evasion = -MAX_COMBAT_STAT;
        assert!(ok(&r));
        r.section.accuracy = MAX_COMBAT_STAT + 1;
        assert!(!ok(&r));
        let mut r = base.clone();
        r.section.evasion = -MAX_COMBAT_STAT - 1;
        assert!(!ok(&r));

        let mut r = base.clone();
        let w = r.vehicle.weapons[0].clone();
        r.vehicle.weapons = vec![w.clone(); MAX_VEHICLE_WEAPONS];
        assert!(ok(&r));
        r.vehicle.weapons.push(w);
        assert!(!ok(&r));

        let mut r = base.clone();
        r.vehicle.max_hp = MAX_VEHICLE_HP;
        r.vehicle.hp = MAX_VEHICLE_HP;
        assert!(ok(&r));
        r.vehicle.max_hp = MAX_VEHICLE_HP + 1;
        assert!(!ok(&r));
        let mut r = base.clone();
        r.vehicle.hp = r.vehicle.max_hp + 1;
        assert!(!ok(&r));
        let mut r = base.clone();
        r.vehicle.weapons[0].base_damage = MAX_COMBAT_STAT + 1;
        assert!(!ok(&r));

        let mut r = base.clone();
        r.section.name = "x".repeat(MAX_COMBAT_NAME_BYTES);
        assert!(ok(&r));
        r.section.name = "x".repeat(MAX_COMBAT_NAME_BYTES + 1);
        assert!(!ok(&r));

        let mut r = base.clone();
        r.defending_convoy_vehicles = Some(vec![VehicleClass::Hauler; MAX_DEFENDING_VEHICLES]);
        assert!(ok(&r));
        r.defending_convoy_vehicles = Some(vec![VehicleClass::Hauler; MAX_DEFENDING_VEHICLES + 1]);
        assert!(!ok(&r));
    }
}
