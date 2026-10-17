use chrono::Utc;
use uuid::Uuid;

use sim_engine::rng::Rng;
use sim_engine::roster::{generic_pool, AdvisorQuirk, CommanderOrigin, GenericBoard};

use crate::field_leader_gen::{pick, pick_faction_bias, pick_name};
use crate::repository::advisor::AdvisorRecord;

/// Generate a fully-populated `AdvisorRecord` from the seeded PRNG (brick 7a-2). The caller
/// picks the Board (ODQ-39 (d)); the Research Board is not a `GenericBoard`, so it is never
/// rolled. Advisors are ungraded: no band, no grade weights. Pure function — no I/O.
///
/// Roll order (deterministic; seven draws for every seed and either Board):
///   1. origin          (uniform over `CommanderOrigin::ALL`, 5; no origin × spec restriction)
///   2. specialization  (uniform over `generic_pool(board)`, 4 or 7)
///   3. quirk           (uniform over `AdvisorQuirk::ALL`, 12)
///   4. bias parent     (`FactionBias::GROUPS`, 9)
///   5. bias sect       (within that parent's group)
///   6. first name
///   7. last name
///
/// Per-seed output is **not** a contract (Reviewer 4d-2 CF4): `prng_seed_state` is
/// provenance only. Rank starts at 1 and is not rolled (Skill Level = Rank, ODQ-39 (f)).
pub fn generate_advisor(
    player_wallet: Vec<u8>,
    campaign_id:   Uuid,
    board:         GenericBoard,
    rng_seed:      u32,
) -> AdvisorRecord {
    let mut rng = Rng::new(rng_seed);

    let origin = pick(&CommanderOrigin::ALL, &mut rng);
    let specialization = pick(generic_pool(board), &mut rng);
    let quirk = pick(&AdvisorQuirk::ALL, &mut rng);
    let faction_bias = pick_faction_bias(&mut rng);
    let name = pick_name(origin, &mut rng);

    AdvisorRecord {
        advisor_id:      Uuid::new_v4(),
        campaign_id,
        player_wallet,
        name,
        rank:            1,
        origin:          origin.as_str().to_string(),
        faction_bias:    faction_bias.as_str().to_string(),
        specialization:  specialization.as_str().to_string(),
        quirk:           quirk.as_str().to_string(),
        prng_seed_state: rng_seed as i64,
        created_at:      Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_engine::roster::FactionBias;
    use std::collections::HashSet;

    const SEEDS: u32 = 2000;

    /// The spec strings `board` reaches over `SEEDS` seeds.
    fn reached_specs(board: GenericBoard) -> HashSet<String> {
        (0..SEEDS).map(|seed| generate_advisor(vec![0u8; 32], Uuid::nil(), board, seed).specialization).collect()
    }

    #[test]
    fn generate_advisor_is_deterministic() {
        for board in [GenericBoard::Mercenary, GenericBoard::Mining] {
            let a = generate_advisor(vec![1u8; 32], Uuid::nil(), board, 42);
            let b = generate_advisor(vec![1u8; 32], Uuid::nil(), board, 42);
            assert_eq!(a.name, b.name);
            assert_eq!(a.origin, b.origin);
            assert_eq!(a.faction_bias, b.faction_bias);
            assert_eq!(a.specialization, b.specialization);
            assert_eq!(a.quirk, b.quirk);
            assert_eq!(a.prng_seed_state, 42);
            assert_eq!(b.prng_seed_state, 42);
            assert_ne!(a.advisor_id, b.advisor_id, "advisor_id must use OS entropy, not the PRNG");
        }
    }

    #[test]
    fn generate_advisor_emits_only_canonical_values() {
        let (mut origins, mut biases, mut quirks) = (HashSet::new(), HashSet::new(), HashSet::new());
        for board in [GenericBoard::Mercenary, GenericBoard::Mining] {
            for seed in 0..SEEDS {
                let r = generate_advisor(vec![0u8; 32], Uuid::nil(), board, seed);
                origins.insert(CommanderOrigin::from_db_str(&r.origin).unwrap_or_else(|| panic!("seed {seed}: origin {:?}", r.origin)));
                biases.insert(FactionBias::from_db_str(&r.faction_bias).unwrap_or_else(|| panic!("seed {seed}: bias {:?}", r.faction_bias)));
                quirks.insert(AdvisorQuirk::from_db_str(&r.quirk).unwrap_or_else(|| panic!("seed {seed}: quirk {:?}", r.quirk)));
                assert_eq!(r.rank, 1, "seed {seed}: a new Advisor is Rank 1");
            }
        }
        assert_eq!(origins.len(), 5, "all 5 origins must be reached: {origins:?}");
        assert_eq!(biases.len(), 12, "all 12 biases must be reached: {biases:?}");
        assert_eq!(quirks.len(), 12, "all 12 Quirks must be reached: {quirks:?}");
    }

    /// The oracle is the literal canon list (Advisor_System §3a), never `generic_pool`.
    #[test]
    fn generate_advisor_mercenary_rolls_only_the_four() {
        let canon: HashSet<String> = [
            "Intelligence Broker (Spymaster)",
            "Operations Officer (COO)",
            "The Financial Patron",
            "CFO (Mercenary Focus)",
        ]
        .map(String::from)
        .into();
        assert_eq!(reached_specs(GenericBoard::Mercenary), canon);
    }

    /// The oracle is the literal canon list (Advisor_System §3b), never `generic_pool`.
    #[test]
    fn generate_advisor_mining_rolls_only_the_seven() {
        let canon: HashSet<String> = [
            "CFO (Mining Focus)",
            "VP of Tectonic Risk & Planetology",
            "Director of Heavy Engineering",
            "Director of Asset Protection (Corp-Sec)",
            "VP of Labor & Personnel (Anti-Union)",
            "VP of Labor & Personnel (Pro-Union)",
            "Director of Overland Logistics",
        ]
        .map(String::from)
        .into();
        assert_eq!(reached_specs(GenericBoard::Mining), canon);
    }
}
