use chrono::{DateTime, Utc};
use uuid::Uuid;

// ── ForemanRecord (brick 7a-1) ───────────────────────────────────────────────

/// A Mining Foreman's row in `foreman_records` (migration 0022). The roster columns hold
/// `sim_engine::roster` display strings, pinned by `0022`'s CHECKs. The career columns
/// (rank, xp, stress, is_shattered, is_kia) mirror `commander_records` and have no writer
/// until Phase 2 §13.
#[derive(Debug, Clone)]
pub struct ForemanRecord {
    pub foreman_id:      Uuid,
    pub campaign_id:     Uuid,
    pub player_wallet:   Vec<u8>,
    pub name:            String,
    pub rank:            i16,
    pub xp:              i32,
    pub stress:          i16,
    pub origin:          String,
    pub faction_bias:    String,
    pub specialization:  String,
    pub fatal_flaw:      String,
    /// The spawn Grade's display string (`roster::CommanderGrade::as_str`; Foremen use the
    /// same Grade enum). Stored as given, never recomputed from the traits.
    pub foreman_grade:   String,
    pub is_shattered:    bool,
    pub is_kia:          bool,
    pub prng_seed_state: i64,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
}
