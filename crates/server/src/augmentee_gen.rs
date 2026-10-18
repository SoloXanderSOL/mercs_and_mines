use chrono::Utc;
use uuid::Uuid;
use sim_engine::rng::Rng;
use sim_engine::roster::{Augment, AugmentCatalogue, CommanderOrigin, FittingSource};
use crate::field_leader_gen::{pick, pick_name};
use crate::repository::augmentee::{AugmenteeOwner, AugmenteeRecord};

/// The callsign pool (Bespoke_Augmentation_System §2, 24, in canon order). Drawn uniformly,
/// whatever the species; two augmentees may share one.
pub const CALLSIGNS: [&str; 24] = [
    "Tinman", "Sparkplug", "Deadbolt", "Overtime", "Lugnut", "Hazard Pay", "Warranty", "Socket",
    "Rivet", "Payroll", "Spare Parts", "Crowbar", "Torque", "Static", "Lockjaw", "Rebate",
    "Fuse", "Gristle", "Recall", "Invoice", "Bolt-Cutter", "Ballast", "Scrapheap", "Grommet",
];

/// Generate a named augmentee at fitting (brick 7d-1). The player buys the SKU and chooses the
/// clinic, so `augment` and `fitting_source` are arguments, as are the owner, the Chrome
/// Ceiling `slot` and the owner's species (`origin`, which picks the name pools and is not
/// stored, Q4). Pure function — no I/O.
///
/// Refused with `assert!` before any draw: a slot other than 1 or 2; an SKU from the other
/// side's catalogue (combat fits Sections, industrial fits Crews; Bespoke §2).
///
/// Roll order (deterministic; three draws for every seed):
///   1. first name  (by `origin`)
///   2. last name   (by `origin`)
///   3. callsign    (uniform over `CALLSIGNS`, 24)
///
/// Per-seed output is **not** a contract (Reviewer 4d-2 CF4): `prng_seed_state` is
/// provenance only. A new augmentee is serving: no casualty class, no `fate_at`.
#[allow(clippy::too_many_arguments)]
pub fn generate_augmentee(
    player_wallet:  Vec<u8>,
    campaign_id:    Uuid,
    owner:          AugmenteeOwner,
    slot:           i16,
    origin:         CommanderOrigin,
    augment:        Augment,
    fitting_source: FittingSource,
    rng_seed:       u32,
) -> AugmenteeRecord {
    assert!(slot == 1 || slot == 2, "augmentee slot {slot} is not 1 or 2");
    let wanted = match owner {
        AugmenteeOwner::Section(_) => AugmentCatalogue::Combat,
        AugmenteeOwner::Crew(_) => AugmentCatalogue::Industrial,
    };
    assert_eq!(augment.catalogue(), wanted, "{augment:?} does not fit {owner:?}");

    let mut rng = Rng::new(rng_seed);
    let name = pick_name(origin, &mut rng);
    let callsign = pick(&CALLSIGNS, &mut rng);
    AugmenteeRecord {
        augmentee_id:    Uuid::new_v4(),
        campaign_id,
        player_wallet,
        owner,
        slot,
        name,
        callsign:        callsign.to_string(),
        augment:         augment.as_str().to_string(),
        fitting_source:  fitting_source.as_str().to_string(),
        casualty_class:  None,
        fate_at:         None,
        prng_seed_state: rng_seed as i64,
        created_at:      Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bespoke §2, typed here as the oracle.
    #[test]
    fn augmentee_callsign_pool_matches_canon() {
        const CANON: [&str; 24] = [
            "Tinman", "Sparkplug", "Deadbolt", "Overtime", "Lugnut", "Hazard Pay", "Warranty", "Socket",
            "Rivet", "Payroll", "Spare Parts", "Crowbar", "Torque", "Static", "Lockjaw", "Rebate",
            "Fuse", "Gristle", "Recall", "Invoice", "Bolt-Cutter", "Ballast", "Scrapheap", "Grommet",
        ];
        assert_eq!(CALLSIGNS, CANON);
    }

    #[test]
    fn generate_augmentee_replays_from_the_same_seed() {
        let owner = AugmenteeOwner::Crew(Uuid::nil());
        let gen = || generate_augmentee(vec![36u8; 32], Uuid::nil(), owner, 2, CommanderOrigin::Duck, Augment::GeophoneCochlea, FittingSource::Church, 42);
        let (a, b) = (gen(), gen());
        assert_ne!(a.augmentee_id, b.augmentee_id, "augmentee_id must use OS entropy, not the PRNG");
        assert_eq!(
            AugmenteeRecord { augmentee_id: Uuid::nil(), created_at: a.created_at, ..a },
            AugmenteeRecord { augmentee_id: Uuid::nil(), created_at: a.created_at, ..b }
        );
    }

    #[test]
    fn generate_augmentee_takes_owner_slot_origin_augment_and_source_from_the_caller() {
        let section = Uuid::new_v4();
        let campaign = Uuid::new_v4();
        let r = generate_augmentee(
            vec![36u8; 32],
            campaign,
            AugmenteeOwner::Section(section),
            1,
            CommanderOrigin::Duck,
            Augment::DopplerCochlea,
            FittingSource::Secular,
            7,
        );
        assert_eq!(r.owner, AugmenteeOwner::Section(section));
        assert_eq!(r.campaign_id, campaign);
        assert_eq!(r.player_wallet, vec![36u8; 32]);
        assert_eq!(r.slot, 1);
        assert_eq!(r.augment, "Doppler Cochlea");
        assert_eq!(r.fitting_source, "Secular");
        assert_eq!(r.casualty_class, None);
        assert_eq!(r.fate_at, None);
        assert_eq!(r.prng_seed_state, 7);
        assert!(CALLSIGNS.contains(&r.callsign.as_str()), "callsign {:?} off the pool", r.callsign);
        // The Duck pools (field_leader_gen::pick_name): a Duck Section's augmentee is a Duck.
        let duck_first = ["Mallardus", "Drakewell", "Pondsworth", "Aylesbury", "Canardo", "Widgeon", "Brantley", "Scaup"];
        let first = r.name.split(' ').next().unwrap();
        assert!(duck_first.contains(&first), "{:?} is not a Duck first name", r.name);
    }

    #[test]
    #[should_panic(expected = "does not fit")]
    fn generate_augmentee_refuses_an_off_catalogue_owner() {
        generate_augmentee(
            vec![36u8; 32],
            Uuid::nil(),
            AugmenteeOwner::Crew(Uuid::nil()),
            1,
            CommanderOrigin::HumanCorporate,
            Augment::IntegralGunArm,
            FittingSource::Church,
            1,
        );
    }
}
