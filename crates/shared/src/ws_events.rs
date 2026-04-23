use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitSnapshot {
    pub id: String,
    pub name: String,
    pub current_hp: i32,
    pub max_hp: i32,
    pub status: UnitStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitStatus {
    Active,
    Suppressed,
    Retreated,
    Kia,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombatTickEvent {
    pub tick_index: u32,
    pub narrative: Option<String>,
    pub friendly_units: Vec<UnitSnapshot>,
    pub enemy_units: Vec<UnitSnapshot>,
    pub combat_ended: bool,
    /// Some only when combat_ended == true.
    pub outcome: Option<CombatOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatOutcome {
    Victory,
    Defeat,
    Retreated,
    MutualDestruction,
}

/// Client → Server messages over the WS pipe.
#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ClientCommand {
    Retreat,
}
