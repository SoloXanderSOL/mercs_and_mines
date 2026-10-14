// Request types for all simulation endpoints.
// VehicleClass is canonical (game_types.rs §12) — imported from sim_engine, not redefined here.
// convoy_vehicle_from_class is a server-layer concern: maps the enum to the resolver's ConvoyVehicle.
// Fuel rule: Hauler = 10 He3/hex (Gas Guzzler); all other classes = 5 He3/hex.

use serde::{Deserialize, Serialize};
use sim_engine::game_types::{Commander, Unit, VehicleClass};
use uuid::Uuid;
use sim_engine::types::{CombatInitiationType, ConvoyVehicle, Pack, Section, Vehicle};

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

#[derive(Debug, Deserialize, Serialize)]
pub struct PackAssaultRequest {
    pub section: Section,
    pub pack: Pack,
    pub combat_initiation_type: CombatInitiationType,
    pub defending_convoy_vehicles: Option<Vec<VehicleClass>>,
    pub max_ticks: Option<usize>,
    pub seed_override: Option<u32>,
}
