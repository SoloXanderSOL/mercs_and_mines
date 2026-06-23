use chrono::Utc;
use uuid::Uuid;

use sim_engine::rng::Rng;

use crate::repository::commander::CommanderRecord;

const ORIGINS: &[&str] = &[
    "Human (Corporate)",
    "Human (Underhive)",
    "Raccoon (Mod-Moped)",
    "Raccoon (Rocker-Chopper)",
    "Hamster Attachment",
];

const FACTION_BIAS: &[&str] = &[
    "GUN Loyalist",
    "Union Agitator",
    "Cult of Bob Initiate",
];

const SPECS_MERC: &[&str] = &["Trench-Sweeper", "Armor-Adept", "Night-Stalker"];
const SPECS_MINER: &[&str] = &["Deep-Driller", "Coolant-Jockey", "Shoring-Expert"];

const FATAL_FLAWS: &[&str] = &["Embezzler", "Bloodthirsty", "Cowardly"];

fn pick_name(origin: &str, rng: &mut Rng) -> String {
    let first_names: &[&str] = match origin {
        "Raccoon (Mod-Moped)" | "Raccoon (Rocker-Chopper)" => {
            &["Scratch", "Gnash", "Rev", "Bolt", "Skid", "Crank"]
        }
        "Hamster Attachment" => &["Squeaks", "Nibbles", "Fluff", "Scurry", "Hatch", "Pinch"],
        _ => &["Vance", "Kira", "Dolan", "Metz", "Shen", "Yula"],
    };
    let surnames: &[&str] = &["Ashmore", "Drek", "Volkov", "Jin", "Osei", "Colt"];
    let first = first_names[rng.roll_int(0, (first_names.len() - 1) as u32) as usize];
    let last = surnames[rng.roll_int(0, (surnames.len() - 1) as u32) as usize];
    format!("{} {}", first, last)
}

/// Generate a fully-populated `CommanderRecord` by rolling all five trait pillars
/// from the seeded PRNG. Pure function — no I/O, no DB access.
///
/// Roll order (deterministic):
///   1. origin        (5 variants)
///   2. faction_bias  (3 variants)
///   3. branch        (0 = Merc, 1 = Miner)
///   4. specialization (3 variants in chosen branch)
///   5. fatal_flaw    (3 variants)
///   6. name          (2 rng calls inside pick_name)
pub fn generate_commander(
    player_wallet: Vec<u8>,
    campaign_id:   Uuid,
    rng_seed:      u32,
) -> CommanderRecord {
    let mut rng = Rng::new(rng_seed);

    let origin = ORIGINS[rng.roll_int(0, (ORIGINS.len() - 1) as u32) as usize].to_string();
    let faction_bias = FACTION_BIAS[rng.roll_int(0, (FACTION_BIAS.len() - 1) as u32) as usize].to_string();

    let branch = rng.roll_int(0, 1);
    let spec_pool = if branch == 0 { SPECS_MERC } else { SPECS_MINER };
    let specialization = spec_pool[rng.roll_int(0, (spec_pool.len() - 1) as u32) as usize].to_string();

    let fatal_flaw = FATAL_FLAWS[rng.roll_int(0, (FATAL_FLAWS.len() - 1) as u32) as usize].to_string();

    let name = pick_name(&origin, &mut rng);

    // TODO(phase-2): capture post-roll RNG state for full replay support
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
        is_nft:          false,
        prng_seed_state: rng_seed as i64,
        created_at:      Utc::now(),
        updated_at:      Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ORIGINS.contains(&r.origin.as_str()),
                "origin '{}' not in canonical list",
                r.origin
            );
        }
    }

    #[test]
    fn generate_commander_specialization_is_canonical() {
        for seed in 0..50u32 {
            let r = generate_commander(vec![0u8; 32], Uuid::nil(), seed);
            let all_specs: Vec<&str> = SPECS_MERC.iter().chain(SPECS_MINER.iter()).copied().collect();
            assert!(
                all_specs.contains(&r.specialization.as_str()),
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
        assert!(!r.is_nft);
        assert!(r.veteran_trait.is_none());
        assert_eq!(r.prng_seed_state, 999);
        assert!(!r.name.is_empty());
    }
}
