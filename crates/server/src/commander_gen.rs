use chrono::Utc;
use uuid::Uuid;

use sim_engine::game_types::OutcomeType;
use sim_engine::grade::{commander_combinations, commander_tqs};
use sim_engine::rng::Rng;
use sim_engine::roster::CommanderGrade;

use crate::field_leader_gen::{pick, pick_band, pick_faction_bias, pick_name};
use crate::repository::commander::CommanderRecord;

pub use crate::field_leader_gen::DEFAULT_GRADE_WEIGHTS;

/// `generate_commander_with_weights` with `DEFAULT_GRADE_WEIGHTS`.
pub fn generate_commander(
    player_wallet: Vec<u8>,
    campaign_id:   Uuid,
    rng_seed:      u32,
) -> CommanderRecord {
    generate_commander_with_weights(player_wallet, campaign_id, rng_seed, &DEFAULT_GRADE_WEIGHTS)
}

/// Generate a fully-populated `CommanderRecord` from the seeded PRNG and the canonical
/// roster (`sim_engine::roster`), band-first (4d-2). Pure function — no I/O, no DB access.
///
/// Roll order (deterministic; six draws for every seed):
///   1. Grade band      (weighted by `weights`, in `CommanderGrade::ALL` order)
///   2. combination     (uniform within `commander_combinations(band)`: origin,
///                       specialization and fatal flaw together, all 12 flaws)
///   3. bias parent     (`FactionBias::GROUPS`, 9)
///   4. bias sect       (within that parent's group; drawn for every parent, so the
///                       count is fixed — a one-value group still consumes a draw)
///   5. first name
///   6. last name
///
/// The stored Grade is still read from the traits through `sim_engine::grade`; it equals
/// the drawn band by construction.
///
/// Per-seed output is **not** a contract (CF4). `prng_seed_state` is provenance, not a
/// reproduction key: the row is the record, and nothing regenerates a Commander from its
/// seed, so the order of `ALL`/`GROUPS` and the draw sequence may change between releases.
pub fn generate_commander_with_weights(
    player_wallet: Vec<u8>,
    campaign_id:   Uuid,
    rng_seed:      u32,
    weights:       &[u32; 5],
) -> CommanderRecord {
    let mut rng = Rng::new(rng_seed);

    let band_sizes = CommanderGrade::ALL.map(|g| commander_combinations(g).len());
    let band = pick_band(weights, band_sizes, &mut rng);
    let (origin, specialization, fatal_flaw) = pick(&commander_combinations(band), &mut rng);
    let faction_bias = pick_faction_bias(&mut rng);
    let name = pick_name(origin, &mut rng);

    // TQS is computed and discarded; only the Grade is stored.
    let grade = CommanderGrade::from_tqs(commander_tqs(origin, specialization, fatal_flaw));
    debug_assert_eq!(grade, band, "a combination must grade to the band it was drawn from");
    // TODO(phase-1-§5): broadcast sector notification when grade == Heroic via WebSocket once Multiplayer Sync is live
    // TODO(phase-2-§6c): include commander_grade in the memorial snapshot payload (GAP-22)
    // TODO(phase-2-§10b): expose grade as filterable field on Headhunter Market listings

    let origin = origin.as_str().to_string();
    let faction_bias = faction_bias.as_str().to_string();
    let specialization = specialization.as_str().to_string();
    let fatal_flaw = fatal_flaw.as_str().to_string();

    // TODO(phase-2): if full replay of generation is ever needed, per-seed output becomes a
    // contract here: capture the post-roll RNG state, and freeze the order of `ALL`/`GROUPS`
    // and the draw sequence (CF4).
    CommanderRecord {
        commander_id:    Uuid::new_v4(),
        campaign_id,
        player_wallet,
        name,
        rank:            1,
        xp:              0,
        stress:          0,
        origin,
        faction_bias,
        specialization,
        fatal_flaw,
        veteran_trait:   None,
        is_shattered:    false,
        is_kia:          false,
        prng_seed_state: rng_seed as i64,
        commander_grade: Some(grade.as_str().to_string()),
        created_at:      Utc::now(),
        updated_at:      Utc::now(),
    }
}

// ── XP and rank (brick 4b-1) ─────────────────────────────────────────────────

/// Canon XP table — Commander_and_Advisor_System.md § Mission XP Awards.
/// Rows: tier 1..=5. Columns: Success / Defeat / TacticalRetreat / Wipeout.
/// The TacticalRetreat column is authored and live here, but nothing on the
/// mission path assigns that variant (ODQ-23, 2026-09-03): it is reachable
/// only by direct call, which is why its test calls this function directly.
const XP_TABLE: [[i32; 4]; 5] = [
    [ 12,  7,  4, 0],
    [ 25, 15,  8, 0],
    [ 50, 30, 16, 0],
    [ 90, 54, 28, 0],
    [150, 90, 45, 0],
];

pub fn xp_award(tier: u8, outcome: &OutcomeType) -> i32 {
    debug_assert!((1..=5).contains(&tier), "mission tier {} outside 1..=5", tier);
    if !(1..=5).contains(&tier) {
        return 0;
    }
    let col = match outcome {
        OutcomeType::Success         => 0,
        OutcomeType::Defeat          => 1,
        OutcomeType::TacticalRetreat => 2,
        OutcomeType::Wipeout         => 3,
    };
    XP_TABLE[(tier - 1) as usize][col]
}

/// Mission tier (T1–T5) from a Phase 0 mission's `difficulty` (1–10): 1–2 → T1, 3–4 → T2,
/// 5–6 → T3, 7–8 → T4, 9–10 → T5, clamped at both ends so `xp_award` never sees 0 or 6.
/// Phase 0 bridge (§4g item 10); retire when the mission route reads a contract tier.
pub fn mission_tier(difficulty: u8) -> u8 {
    (difficulty.saturating_add(1) / 2).clamp(1, 5)
}

/// Rank derived from cumulative XP — never incremented. Thresholds 100 / 400 /
/// 1100 / 2600 give ranks 2..=5; hard cap 5. Idempotent, so back-filled XP
/// cannot drift the stored rank.
pub const RANK_THRESHOLDS: [i32; 4] = [100, 400, 1100, 2600];

pub fn rank_for_xp(xp: i32) -> i16 {
    1 + RANK_THRESHOLDS.iter().filter(|&&t| xp >= t).count() as i16
}

/// What the Glorious check reads about a death (§4e step 3).
/// `ticker_contribution_pct` is the mission's own Victory Ticker contribution in
/// percentage points, the unit `apply_ticker_delta` uses. A Wipeout that met its
/// objective passes its contribution through rather than zeroing it (ODQ-31).
#[derive(Debug, Clone, Copy)]
pub struct GloriousDeathCriteria {
    pub rank: i16,
    pub mission_tier: u8,
    pub ticker_contribution_pct: f64,
}

/// Card_Game_Design_Overview §1a, Phase 1 criteria only (PvP, Story Event and Redemption Race are Phase 2+).
pub fn is_glorious_death(c: &GloriousDeathCriteria) -> bool {
    (c.rank >= 3 && c.mission_tier >= 4) || c.rank == 5 || c.ticker_contribution_pct >= 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_engine::roster::{CommanderFlaw, CommanderOrigin, CommanderSpecialization, FactionBias};

    fn glorious(rank: i16, mission_tier: u8, ticker_contribution_pct: f64) -> bool {
        is_glorious_death(&GloriousDeathCriteria { rank, mission_tier, ticker_contribution_pct })
    }

    #[test]
    fn test_is_glorious_death_rank_three_on_tier_four_or_five() {
        assert!(glorious(3, 4, 0.0));
        assert!(glorious(3, 5, 0.0));
        assert!(!glorious(3, 3, 0.0));
    }

    #[test]
    fn test_is_glorious_death_rank_five_always() {
        assert!(glorious(5, 1, 0.0));
    }

    #[test]
    fn test_is_glorious_death_ticker_boundary() {
        assert!(glorious(1, 1, 0.5));
        assert!(!glorious(1, 1, 0.49));
    }

    /// ODQ-31: a pyrrhic win (objective taken, nobody walked home) is a Wipeout whose
    /// ticker contribution still lands, so it can make the death Glorious.
    #[test]
    fn test_is_glorious_death_pyrrhic_wipeout_counts() {
        assert!(glorious(1, 1, 0.75));
    }

    #[test]
    fn test_is_glorious_death_ordinary_death_is_not() {
        assert!(!glorious(2, 5, 0.0));
        assert!(!glorious(4, 3, 0.0));
        assert!(!glorious(1, 1, 0.0));
    }

    #[test]
    fn mission_tier_maps_difficulty_bands() {
        let expected: [(u8, u8); 10] = [(1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (6, 3), (7, 4), (8, 4), (9, 5), (10, 5)];
        for (difficulty, tier) in expected {
            assert_eq!(mission_tier(difficulty), tier, "difficulty {}", difficulty);
        }
        assert_eq!(mission_tier(0), 1, "0 clamps up to T1");
        assert_eq!(mission_tier(11), 5, "11 clamps down to T5");
        assert_eq!(mission_tier(255), 5, "255 must not overflow");
    }

    #[test]
    fn every_live_mission_has_a_tier_in_range() {
        let missions = sim_engine::missions::missions();
        assert!(!missions.is_empty());
        for (id, m) in missions.iter() {
            let tier = mission_tier(m.difficulty);
            assert!((1..=5).contains(&tier), "mission {} difficulty {} → tier {}", id, m.difficulty, tier);
            assert_eq!(xp_award(tier, &OutcomeType::Wipeout), 0);
        }
    }

    #[test]
    fn generate_commander_is_deterministic() {
        let wallet = vec![1u8; 32];
        let campaign_id = Uuid::nil();
        let a = generate_commander(wallet.clone(), campaign_id, 42);
        let b = generate_commander(wallet.clone(), campaign_id, 42);
        assert_eq!(a.name, b.name);
        assert_eq!(a.origin, b.origin);
        assert_eq!(a.faction_bias, b.faction_bias);
        assert_eq!(a.specialization, b.specialization);
        assert_eq!(a.fatal_flaw, b.fatal_flaw);
        assert_eq!(a.prng_seed_state, 42);
        assert_ne!(a.commander_id, b.commander_id, "commander_id must use OS entropy, not the PRNG");
    }

    #[test]
    fn generate_commander_different_seeds_can_differ() {
        let wallet = vec![1u8; 32];
        let campaign_id = Uuid::nil();
        // Over 10 seeds, at least one pair of trait rolls must differ
        let records: Vec<_> = (0..10u32)
            .map(|s| generate_commander(wallet.clone(), campaign_id, s))
            .collect();
        let all_same_origin = records.windows(2).all(|w| w[0].origin == w[1].origin);
        assert!(
            !all_same_origin,
            "different seeds should produce at least some origin variation"
        );
    }

    #[test]
    fn generate_commander_origin_is_canonical() {
        for seed in 0..50u32 {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            assert!(
                CommanderOrigin::from_db_str(&r.origin).is_some(),
                "origin '{}' not in canonical list",
                r.origin
            );
        }
    }

    #[test]
    fn generate_commander_faction_bias_is_canonical() {
        for seed in 0..50u32 {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            assert!(
                FactionBias::from_db_str(&r.faction_bias).is_some(),
                "faction_bias '{}' not in canonical list",
                r.faction_bias
            );
        }
    }

    #[test]
    fn generate_commander_fatal_flaw_is_canonical() {
        for seed in 0..50u32 {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            assert!(
                CommanderFlaw::from_db_str(&r.fatal_flaw).is_some(),
                "fatal_flaw '{}' not in canonical list",
                r.fatal_flaw
            );
        }
    }

    #[test]
    fn generate_commander_specialization_is_canonical() {
        for seed in 0..50u32 {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            assert!(
                CommanderSpecialization::from_db_str(&r.specialization).is_some(),
                "specialization '{}' not in canonical list",
                r.specialization
            );
        }
    }

    #[test]
    fn generate_commander_initial_values_are_correct() {
        let r = generate_commander(vec![5u8; 32], Uuid::nil(), 999);
        assert_eq!(r.rank, 1);
        assert_eq!(r.xp, 0);
        assert_eq!(r.stress, 0);
        assert!(!r.is_shattered);
        assert!(!r.is_kia);
        assert!(r.veteran_trait.is_none());
        assert_eq!(r.prng_seed_state, 999);
        assert!(!r.name.is_empty());
    }

    #[test]
    fn generate_commander_emits_only_canonical_values() {
        use std::collections::HashSet;
        let (mut origins, mut biases, mut specs, mut flaws) = (HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new());
        for seed in 0..2000u32 {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            let origin = CommanderOrigin::from_db_str(&r.origin).unwrap_or_else(|| panic!("seed {seed}: origin {:?}", r.origin));
            let bias = FactionBias::from_db_str(&r.faction_bias).unwrap_or_else(|| panic!("seed {seed}: bias {:?}", r.faction_bias));
            let spec = CommanderSpecialization::from_db_str(&r.specialization)
                .unwrap_or_else(|| panic!("seed {seed}: specialization {:?}", r.specialization));
            let flaw = CommanderFlaw::from_db_str(&r.fatal_flaw).unwrap_or_else(|| panic!("seed {seed}: flaw {:?}", r.fatal_flaw));
            origins.insert(origin);
            biases.insert(bias);
            specs.insert(spec);
            flaws.insert(flaw);
        }
        for f in CommanderFlaw::ALL {
            assert!(flaws.contains(&f), "flaw {f:?} never rolled in 2000 seeds");
        }
        for o in CommanderOrigin::ALL {
            assert!(origins.contains(&o), "origin {o:?} never rolled in 2000 seeds");
        }
        for b in FactionBias::ALL {
            assert!(biases.contains(&b), "bias {b:?} never rolled in 2000 seeds");
        }
        for s in CommanderSpecialization::ALL {
            assert!(specs.contains(&s), "specialization {s:?} never rolled in 2000 seeds");
        }
    }

    /// Every origin gets a Grade, and it is the grading function's readout of the rolled
    /// traits (never a hard-coded band; CF4).
    #[test]
    fn test_generate_commander_grade_set_for_every_origin() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for seed in 0..2000u32 {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            let origin = CommanderOrigin::from_db_str(&r.origin).unwrap();
            let spec = CommanderSpecialization::from_db_str(&r.specialization).unwrap();
            let flaw = CommanderFlaw::from_db_str(&r.fatal_flaw).unwrap();
            let expected = CommanderGrade::from_tqs(commander_tqs(origin, spec, flaw));
            assert_eq!(r.commander_grade.as_deref(), Some(expected.as_str()), "seed {seed}");
            seen.insert(origin);
            if seen.len() == CommanderOrigin::ALL.len() {
                return;
            }
        }
        panic!("not every origin rolled in 2000 seeds: {seen:?}");
    }

    /// `DEFAULT_GRADE_WEIGHTS` is indexed in `CommanderGrade::ALL` order; the band counts
    /// track the canon targets, which the shared helper holds as a literal.
    #[test]
    fn generate_commander_band_frequencies_track_weights() {
        crate::field_leader_gen::assert_band_frequencies("Commander", |seed| {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            CommanderGrade::from_db_str(r.commander_grade.as_deref().unwrap()).unwrap()
        });
    }

    #[test]
    fn generate_commander_with_weights_honours_a_single_band() {
        for (weights, grade) in [([0, 0, 0, 0, 1], CommanderGrade::Heroic), ([1, 0, 0, 0, 0], CommanderGrade::StandardIssue)] {
            for seed in 0..200u32 {
                let r = generate_commander_with_weights(vec![0u8; 32], Uuid::nil(), seed, &weights);
                assert_eq!(r.commander_grade.as_deref(), Some(grade.as_str()), "seed {seed}, weights {weights:?}");
            }
        }
    }

    // ── xp_award / rank_for_xp ───────────────────────────────────────────────

    /// Every cell of the canon table by direct call. This is the only path that
    /// reaches the TacticalRetreat arm — no mission-path sweep can (ODQ-23).
    #[test]
    fn xp_award_matches_canon_table() {
        let expected: [(u8, [i32; 4]); 5] = [
            (1, [ 12,  7,  4, 0]),
            (2, [ 25, 15,  8, 0]),
            (3, [ 50, 30, 16, 0]),
            (4, [ 90, 54, 28, 0]),
            (5, [150, 90, 45, 0]),
        ];
        for (tier, row) in expected {
            assert_eq!(xp_award(tier, &OutcomeType::Success),         row[0], "tier {} Success", tier);
            assert_eq!(xp_award(tier, &OutcomeType::Defeat),          row[1], "tier {} Defeat", tier);
            assert_eq!(xp_award(tier, &OutcomeType::TacticalRetreat), row[2], "tier {} TacticalRetreat", tier);
            assert_eq!(xp_award(tier, &OutcomeType::Wipeout),         row[3], "tier {} Wipeout", tier);
        }
    }

    #[test]
    fn xp_award_orders_success_over_defeat_over_tactical_retreat_over_wipeout() {
        for tier in 1..=5u8 {
            let s = xp_award(tier, &OutcomeType::Success);
            let d = xp_award(tier, &OutcomeType::Defeat);
            let t = xp_award(tier, &OutcomeType::TacticalRetreat);
            let w = xp_award(tier, &OutcomeType::Wipeout);
            assert!(s > d, "tier {}: Success {} must exceed Defeat {}", tier, s, d);
            assert!(d > t, "tier {}: Defeat {} must exceed TacticalRetreat {}", tier, d, t);
            assert!(t > w, "tier {}: TacticalRetreat {} must exceed Wipeout {}", tier, t, w);
            assert_eq!(w, 0, "tier {}: Wipeout must be 0", tier);
        }
    }

    #[test]
    fn xp_award_wipeout_is_zero_at_every_tier() {
        for tier in 1..=5u8 {
            assert_eq!(xp_award(tier, &OutcomeType::Wipeout), 0, "tier {}", tier);
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "outside 1..=5")]
    fn xp_award_tier_zero_panics_in_debug() {
        let _ = xp_award(0, &OutcomeType::Success);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "outside 1..=5")]
    fn xp_award_tier_six_panics_in_debug() {
        let _ = xp_award(6, &OutcomeType::Success);
    }

    #[test]
    fn rank_for_xp_thresholds() {
        let cases: [(i32, i16); 11] = [
            (-5, 1), (0, 1), (99, 1),
            (100, 2), (399, 2),
            (400, 3), (1099, 3),
            (1100, 4), (2599, 4),
            (2600, 5), (1_000_000, 5),
        ];
        for (xp, rank) in cases {
            assert_eq!(rank_for_xp(xp), rank, "xp {} should be rank {}", xp, rank);
        }
    }
}
