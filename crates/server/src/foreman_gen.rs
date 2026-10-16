use chrono::Utc;
use uuid::Uuid;

use sim_engine::grade::{foreman_combinations, foreman_tqs};
use sim_engine::rng::Rng;
use sim_engine::roster::CommanderGrade;

use crate::field_leader_gen::{pick, pick_band, pick_faction_bias, pick_name, DEFAULT_GRADE_WEIGHTS};
use crate::repository::foreman::ForemanRecord;

/// `generate_foreman_with_weights` with `DEFAULT_GRADE_WEIGHTS`.
pub fn generate_foreman(
    player_wallet: Vec<u8>,
    campaign_id:   Uuid,
    rng_seed:      u32,
) -> ForemanRecord {
    generate_foreman_with_weights(player_wallet, campaign_id, rng_seed, &DEFAULT_GRADE_WEIGHTS)
}

/// Generate a fully-populated `ForemanRecord` from the seeded PRNG and the Foreman pools
/// (`sim_engine::roster`), band-first like `generate_commander` (7a-1). Pure function — no
/// I/O, no DB access.
///
/// Roll order (deterministic; six draws for every seed):
///   1. Grade band      (weighted by `weights`, in `CommanderGrade::ALL` order)
///   2. combination     (uniform within `foreman_combinations(band)`: origin,
///                       specialization and fatal flaw together, all 10 flaws)
///   3. bias parent     (`FactionBias::GROUPS`, 9)
///   4. bias sect       (within that parent's group)
///   5. first name
///   6. last name
///
/// Trait marginals follow the band weights, not a flat roll (Reviewer 4d-2 CF7). Per-seed
/// output is **not** a contract (CF4): `prng_seed_state` is provenance only.
pub fn generate_foreman_with_weights(
    player_wallet: Vec<u8>,
    campaign_id:   Uuid,
    rng_seed:      u32,
    weights:       &[u32; 5],
) -> ForemanRecord {
    let mut rng = Rng::new(rng_seed);

    let band_sizes = CommanderGrade::ALL.map(|g| foreman_combinations(g).len());
    let band = pick_band(weights, band_sizes, &mut rng);
    let (origin, specialization, fatal_flaw) = pick(&foreman_combinations(band), &mut rng);
    let faction_bias = pick_faction_bias(&mut rng);
    let name = pick_name(origin, &mut rng);

    // TQS is computed and discarded; only the Grade is stored.
    let grade = CommanderGrade::from_tqs(foreman_tqs(origin, specialization, fatal_flaw));
    debug_assert_eq!(grade, band, "a combination must grade to the band it was drawn from");

    ForemanRecord {
        foreman_id:      Uuid::new_v4(),
        campaign_id,
        player_wallet,
        name,
        rank:            1,
        xp:              0,
        stress:          0,
        origin:          origin.as_str().to_string(),
        faction_bias:    faction_bias.as_str().to_string(),
        specialization:  specialization.as_str().to_string(),
        fatal_flaw:      fatal_flaw.as_str().to_string(),
        foreman_grade:   grade.as_str().to_string(),
        is_shattered:    false,
        is_kia:          false,
        prng_seed_state: rng_seed as i64,
        created_at:      Utc::now(),
        updated_at:      Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_engine::roster::{CommanderOrigin, FactionBias, ForemanFlaw, ForemanSpecialization};

    #[test]
    fn generate_foreman_is_deterministic() {
        let wallet = vec![1u8; 32];
        let a = generate_foreman(wallet.clone(), Uuid::nil(), 42);
        let b = generate_foreman(wallet.clone(), Uuid::nil(), 42);
        assert_eq!(a.name, b.name);
        assert_eq!(a.origin, b.origin);
        assert_eq!(a.faction_bias, b.faction_bias);
        assert_eq!(a.specialization, b.specialization);
        assert_eq!(a.fatal_flaw, b.fatal_flaw);
        assert_eq!(a.foreman_grade, b.foreman_grade);
        assert_eq!(a.prng_seed_state, 42);
        assert_ne!(a.foreman_id, b.foreman_id, "foreman_id must use OS entropy, not the PRNG");
    }

    #[test]
    fn generate_foreman_emits_only_canonical_values() {
        use std::collections::HashSet;
        let (mut origins, mut biases, mut specs, mut flaws, mut grades) =
            (HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new(), HashSet::new());
        for seed in 0..2000u32 {
            let r = generate_foreman(vec![0u8; 32], Uuid::nil(), seed);
            origins.insert(CommanderOrigin::from_db_str(&r.origin).unwrap_or_else(|| panic!("seed {seed}: origin {:?}", r.origin)));
            biases.insert(FactionBias::from_db_str(&r.faction_bias).unwrap_or_else(|| panic!("seed {seed}: bias {:?}", r.faction_bias)));
            specs.insert(
                ForemanSpecialization::from_db_str(&r.specialization)
                    .unwrap_or_else(|| panic!("seed {seed}: specialization {:?}", r.specialization)),
            );
            flaws.insert(ForemanFlaw::from_db_str(&r.fatal_flaw).unwrap_or_else(|| panic!("seed {seed}: flaw {:?}", r.fatal_flaw)));
            grades.insert(CommanderGrade::from_db_str(&r.foreman_grade).unwrap_or_else(|| panic!("seed {seed}: grade {:?}", r.foreman_grade)));
        }
        for o in CommanderOrigin::ALL {
            assert!(origins.contains(&o), "origin {o:?} never rolled in 2000 seeds");
        }
        for b in FactionBias::ALL {
            assert!(biases.contains(&b), "bias {b:?} never rolled in 2000 seeds");
        }
        for s in ForemanSpecialization::ALL {
            assert!(specs.contains(&s), "specialization {s:?} never rolled in 2000 seeds");
        }
        for f in ForemanFlaw::ALL {
            assert!(flaws.contains(&f), "flaw {f:?} never rolled in 2000 seeds");
        }
        for g in CommanderGrade::ALL {
            assert!(grades.contains(&g), "grade {g:?} never rolled in 2000 seeds");
        }
    }

    #[test]
    fn generate_foreman_band_frequencies_track_weights() {
        crate::field_leader_gen::assert_band_frequencies("Foreman", |seed| {
            let r = generate_foreman(vec![0u8; 32], Uuid::nil(), seed);
            CommanderGrade::from_db_str(&r.foreman_grade).unwrap()
        });
    }

    #[test]
    fn generate_foreman_with_weights_honours_a_single_band() {
        for (weights, grade) in [([0, 0, 0, 0, 1], CommanderGrade::Heroic), ([1, 0, 0, 0, 0], CommanderGrade::StandardIssue)] {
            for seed in 0..200u32 {
                let r = generate_foreman_with_weights(vec![0u8; 32], Uuid::nil(), seed, &weights);
                assert_eq!(r.foreman_grade, grade.as_str(), "seed {seed}, weights {weights:?}");
            }
        }
    }
}
