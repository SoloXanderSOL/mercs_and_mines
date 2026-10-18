//! The casualty pipeline for named individuals (brick 7d-1): casualty class, chrome fate,
//! the casualty lottery and the Equipment Recovery roll.
//! Ref: Bespoke_Augmentation_System.md §3; Casualties_Reinforcements_and_Veterancy.md §2a, §6;
//!      Mining_Crew_Casualties_and_Veterancy.md §2a, §5; Desertion_System.md §1, §6.
//!
//! Pure: no I/O, no serde, no wall clock. Every draw comes from the caller's `Rng`, so the
//! caller (8b-1) chooses the stream. Nothing here is reached from a logged sim entry point
//! yet; the brick that calls it inside one owns the R7 version bump.

use crate::game_types::{OutcomeType, UnitStatus};
use crate::rng::Rng;
use crate::roster::CasualtyClass;

/// What happens to a carrier's Bespoke Augment (Bespoke §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromeFate {
    /// Still fitted: the carrier is serving or Wounded.
    Kept,
    /// Gone with a KIA carrier. Never part of an Equipment Recovery roll.
    Destroyed,
    /// Missing with an MIA carrier; recoverable together by a future Story Event.
    Limbo,
    /// Left with a Deserted carrier (Severance In Kind).
    WalkedOut,
}

/// The sim's battle status mapped to a persisted casualty class (7d-1 D1). Exhaustive, no
/// wildcard. `Ready`/`OnMission` are not casualties. Nothing maps to `Deserted`: desertion
/// is campaign-layer (Desertion §1), never a battle result.
pub fn from_unit_status(status: &UnitStatus) -> Option<CasualtyClass> {
    match status {
        UnitStatus::Ready => None,
        UnitStatus::OnMission => None,
        UnitStatus::Wounded => Some(CasualtyClass::Wounded),
        UnitStatus::Mia => Some(CasualtyClass::Mia),
        UnitStatus::Kia => Some(CasualtyClass::Kia),
    }
}

/// The chrome's fate follows the carrier's class alone (ODQ-43, ruled 2026-10-18: per
/// carrier, in a Wipeout too, so there is no outcome argument). Exhaustive, no wildcard.
pub fn chrome_fate(class: CasualtyClass) -> ChromeFate {
    match class {
        CasualtyClass::Wounded => ChromeFate::Kept,
        CasualtyClass::Kia => ChromeFate::Destroyed,
        CasualtyClass::Mia => ChromeFate::Limbo,
        CasualtyClass::Deserted => ChromeFate::WalkedOut,
    }
}

/// One group's (Section's or Crew's) casualties for one mission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotteryInput {
    /// Members before the mission, 0..=8 (the headcount model).
    pub headcount: u8,
    /// Raw KIA, before any conversion.
    pub kia: u8,
    pub wounded: u8,
    pub mia: u8,
    /// How many of the raw KIA convert to Wounded (Medkit, SAWBONES, LIFELINE, Field
    /// Grafts, Trauma Center). The caller computes it; at most `kia`.
    pub kia_to_wounded: u8,
    /// Named augmentees in the group, 0..=2 (the Chrome Ceiling).
    pub named: u8,
}

/// A named member's lottery result: their class (`None` = not a casualty) and their chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedCasualty {
    pub class: Option<CasualtyClass>,
    pub chrome: ChromeFate,
}

/// The casualty lottery (Bespoke §3): casualties are drawn uniformly from the roster,
/// **after** KIA→Wounded conversion, so a converted augmentee keeps their chrome.
///
/// 1. Refuses bad input with `assert!`, before any draw.
/// 2. Converts counts: `kia' = kia − kia_to_wounded`, `wounded' = wounded + kia_to_wounded`.
/// 3. Draws `k = kia' + wounded' + mia` positions without replacement (partial
///    Fisher–Yates): exactly `k` draws, `roll_int(i, headcount − 1)` for `i in 0..k`, even
///    when the last draw is forced.
/// 4. Slots `0..kia'` are KIA, the next `wounded'` Wounded, the next `mia` MIA.
///
/// Returns one entry per named member, in order; named member `i` holds roster position
/// `i`. Never produces `Deserted`.
pub fn run_casualty_lottery(input: &LotteryInput, rng: &mut Rng) -> Vec<NamedCasualty> {
    let headcount = u16::from(input.headcount);
    let (kia, wounded, mia) = (u16::from(input.kia), u16::from(input.wounded), u16::from(input.mia));
    let converted = u16::from(input.kia_to_wounded);
    let named = u16::from(input.named);
    assert!(headcount <= 8, "lottery: headcount {headcount} is above 8");
    assert!(kia + wounded + mia <= headcount, "lottery: {} casualties exceed headcount {headcount}", kia + wounded + mia);
    assert!(converted <= kia, "lottery: {converted} conversions exceed {kia} KIA");
    assert!(named <= 2, "lottery: {named} named members exceed the Chrome Ceiling of 2");
    assert!(named <= headcount, "lottery: {named} named members exceed headcount {headcount}");

    let kia = kia - converted;
    let wounded = wounded + converted;
    let k = kia + wounded + mia;

    let mut positions: Vec<u16> = (0..headcount).collect();
    for i in 0..k {
        let j = rng.roll_int(u32::from(i), u32::from(headcount - 1)) as usize;
        positions.swap(i as usize, j);
    }

    (0..named)
        .map(|member| {
            let slot = positions.iter().position(|&p| p == member).expect("every member holds a position") as u16;
            let class = if slot < kia {
                Some(CasualtyClass::Kia)
            } else if slot < kia + wounded {
                Some(CasualtyClass::Wounded)
            } else if slot < k {
                Some(CasualtyClass::Mia)
            } else {
                None
            };
            NamedCasualty { class, chrome: class.map_or(ChromeFate::Kept, chrome_fate) }
        })
        .collect()
}

/// The Equipment Recovery chance per KIA (Casualties §2a; Mining Crew §2a uses the same
/// table). Literal canon, exhaustive: Success 75, TacticalRetreat 40, Defeat 15, Wipeout 0.
/// Waste-Not's bonus (`FatalFlawEffect::EquipmentRecovery`) is Phase 2 §13's to add.
pub fn recovery_chance_percent(outcome: &OutcomeType) -> u8 {
    match outcome {
        OutcomeType::Success => 75,
        OutcomeType::TacticalRetreat => 40,
        OutcomeType::Defeat => 15,
        OutcomeType::Wipeout => 0,
    }
}

/// Rolls Equipment Recovery for one group's `kia` dead: how many full loadouts return.
/// One `roll_d100` per KIA, always, even at 0% or when the group is wiped, so no draw
/// moves. A loadout is recovered when `group_has_survivor && roll <= chance_percent`.
/// "Wiped entirely" means no survivor in the group per `is_survivor` (neither KIA nor
/// MIA; Q1). An augmentee's chrome is never part of this roll (chrome is not lootable,
/// Casualties §6); their ordinary loadout is, as one KIA among the others.
pub fn roll_equipment_recovery(chance_percent: u8, kia: u8, group_has_survivor: bool, rng: &mut Rng) -> u8 {
    assert!(chance_percent <= 100, "recovery chance {chance_percent} is above 100");
    let mut recovered = 0;
    for _ in 0..kia {
        let roll = rng.roll_d100();
        if group_has_survivor && roll <= u32::from(chance_percent) {
            recovered += 1;
        }
    }
    recovered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::{Augment, AugmentCatalogue, FittingSource};

    fn input(headcount: u8, kia: u8, wounded: u8, mia: u8, kia_to_wounded: u8, named: u8) -> LotteryInput {
        LotteryInput { headcount, kia, wounded, mia, kia_to_wounded, named }
    }

    #[test]
    fn casualty_class_from_unit_status_is_exhaustive_and_canon() {
        assert_eq!(from_unit_status(&UnitStatus::Ready), None);
        assert_eq!(from_unit_status(&UnitStatus::OnMission), None);
        assert_eq!(from_unit_status(&UnitStatus::Wounded), Some(CasualtyClass::Wounded));
        assert_eq!(from_unit_status(&UnitStatus::Mia), Some(CasualtyClass::Mia));
        assert_eq!(from_unit_status(&UnitStatus::Kia), Some(CasualtyClass::Kia));
    }

    #[test]
    fn chrome_fate_follows_the_carrier_class() {
        assert_eq!(chrome_fate(CasualtyClass::Wounded), ChromeFate::Kept);
        assert_eq!(chrome_fate(CasualtyClass::Kia), ChromeFate::Destroyed);
        assert_eq!(chrome_fate(CasualtyClass::Mia), ChromeFate::Limbo);
        assert_eq!(chrome_fate(CasualtyClass::Deserted), ChromeFate::WalkedOut);
    }

    /// The augmentee is the whole roster and the one KIA converts, so conversion-first gives
    /// Wounded with chrome kept for every seed (Bespoke §3, 7d-1 DoD).
    #[test]
    fn lottery_applies_conversion_before_chrome_fate() {
        for seed in 0..100 {
            let got = run_casualty_lottery(&input(1, 1, 0, 0, 1, 1), &mut Rng::new(seed));
            assert_eq!(
                got,
                vec![NamedCasualty { class: Some(CasualtyClass::Wounded), chrome: ChromeFate::Kept }],
                "seed {seed}"
            );
        }
    }

    #[test]
    fn lottery_replays_from_the_same_seed() {
        let lottery = input(8, 3, 1, 1, 1, 2);
        for seed in [0, 7, 42, 9001] {
            let a = run_casualty_lottery(&lottery, &mut Rng::new(seed));
            let b = run_casualty_lottery(&lottery, &mut Rng::new(seed));
            assert_eq!(a, b, "seed {seed}");
            assert_eq!(a.len(), 2);
        }
    }

    #[test]
    fn lottery_draws_once_per_casualty() {
        for seed in [1, 42, 777] {
            let mut rng = Rng::new(seed);
            run_casualty_lottery(&input(8, 3, 1, 1, 1, 2), &mut rng);
            let mut reference = Rng::new(seed);
            for i in 0..5 {
                reference.roll_int(i, 7);
            }
            assert_eq!(rng.next_u32(), reference.next_u32(), "seed {seed}: 5 casualties → 5 draws");

            // The whole roster: the last draw is forced (roll_int(7, 7)) and still made.
            let mut rng = Rng::new(seed);
            run_casualty_lottery(&input(8, 6, 1, 1, 0, 2), &mut rng);
            let mut reference = Rng::new(seed);
            for i in 0..8 {
                reference.roll_int(i, 7);
            }
            assert_eq!(rng.next_u32(), reference.next_u32(), "seed {seed}: 8 casualties → 8 draws");

            let mut rng = Rng::new(seed);
            run_casualty_lottery(&input(8, 0, 0, 0, 0, 2), &mut rng);
            assert_eq!(rng.next_u32(), Rng::new(seed).next_u32(), "seed {seed}: no casualties → no draws");
        }
    }

    /// 2 KIA among 8: a named member is KIA 2/8 = 25% of the time.
    #[test]
    fn lottery_hits_a_named_member_uniformly() {
        let lottery = input(8, 2, 0, 0, 0, 1);
        let hits = (0..10_000)
            .filter(|&seed| run_casualty_lottery(&lottery, &mut Rng::new(seed))[0].class == Some(CasualtyClass::Kia))
            .count();
        let rate = hits as f64 / 10_000.0;
        println!("named member KIA rate: {rate}");
        assert!((rate - 0.25).abs() <= 0.02, "rate {rate} is not 25% ± 2 pp");
    }

    #[test]
    #[should_panic(expected = "exceed headcount")]
    fn lottery_refuses_more_casualties_than_headcount() {
        run_casualty_lottery(&input(4, 3, 1, 1, 0, 0), &mut Rng::new(1));
    }

    #[test]
    #[should_panic(expected = "is above 8")]
    fn lottery_refuses_headcount_above_eight() {
        run_casualty_lottery(&input(9, 0, 0, 0, 0, 0), &mut Rng::new(1));
    }

    #[test]
    #[should_panic(expected = "conversions exceed")]
    fn lottery_refuses_conversions_above_kia() {
        run_casualty_lottery(&input(8, 1, 0, 0, 2, 0), &mut Rng::new(1));
    }

    #[test]
    #[should_panic(expected = "Chrome Ceiling")]
    fn lottery_refuses_more_named_than_the_ceiling() {
        run_casualty_lottery(&input(8, 0, 0, 0, 0, 3), &mut Rng::new(1));
    }

    #[test]
    #[should_panic(expected = "named members exceed headcount")]
    fn lottery_refuses_more_named_than_headcount() {
        run_casualty_lottery(&input(1, 0, 0, 0, 0, 2), &mut Rng::new(1));
    }

    #[test]
    fn recovery_chance_matches_the_canon_table() {
        assert_eq!(recovery_chance_percent(&OutcomeType::Success), 75);
        assert_eq!(recovery_chance_percent(&OutcomeType::TacticalRetreat), 40);
        assert_eq!(recovery_chance_percent(&OutcomeType::Defeat), 15);
        assert_eq!(recovery_chance_percent(&OutcomeType::Wipeout), 0);
    }

    #[test]
    fn recovery_on_a_wiped_group_is_zero_but_still_draws() {
        for seed in 0..100 {
            let mut rng = Rng::new(seed);
            assert_eq!(roll_equipment_recovery(75, 3, false, &mut rng), 0, "seed {seed}");
            let mut reference = Rng::new(seed);
            for _ in 0..3 {
                reference.roll_d100();
            }
            assert_eq!(rng.next_u32(), reference.next_u32(), "seed {seed}: 3 KIA → 3 draws");
        }
    }

    #[test]
    fn recovery_at_zero_and_hundred_percent() {
        for seed in 0..100 {
            assert_eq!(roll_equipment_recovery(0, 8, true, &mut Rng::new(seed)), 0, "seed {seed}");
            assert_eq!(roll_equipment_recovery(100, 8, true, &mut Rng::new(seed)), 8, "seed {seed}");
        }
    }

    #[test]
    fn recovery_success_rate_is_seventy_five_percent() {
        let recovered: u32 = (0..10_000).map(|seed| u32::from(roll_equipment_recovery(75, 1, true, &mut Rng::new(seed)))).sum();
        let rate = f64::from(recovered) / 10_000.0;
        println!("recovery rate at 75%: {rate}");
        assert!((rate - 0.75).abs() <= 0.02, "rate {rate} is not 75% ± 2 pp");
    }

    /// Bespoke §6–7, written out as the oracle.
    #[test]
    fn augment_catalogue_matches_canon() {
        let of = |c: AugmentCatalogue| Augment::ALL.iter().filter(|a| a.catalogue() == c).map(|a| a.as_str()).collect::<Vec<_>>();
        assert_eq!(
            of(AugmentCatalogue::Combat),
            [
                "Integral Gun-Arm",
                "Thermal Lance-Arm",
                "Coherent-Light Oculars",
                "Cortex Intrusion Jack",
                "Myomer-Bundle Arms",
                "Arrestor-Coil Legs",
                "Doppler Cochlea",
                "Gloom-Sight Oculars (Tactical)",
            ]
        );
        assert_eq!(
            of(AugmentCatalogue::Industrial),
            ["Geophone Cochlea", "Gloom-Sight Oculars (Industrial)", "Load-Bearer Frame"]
        );
        for a in Augment::ALL {
            assert_eq!(Augment::from_db_str(a.as_str()), Some(a), "{a:?} must round-trip");
        }
    }

    #[test]
    fn fitting_source_and_casualty_class_strings_match_canon() {
        assert_eq!(FittingSource::ALL.map(FittingSource::as_str), ["Church", "Secular"]);
        assert_eq!(CasualtyClass::ALL.map(CasualtyClass::as_str), ["KIA", "Wounded", "MIA", "Deserted"]);
        for c in CasualtyClass::ALL {
            assert_eq!(CasualtyClass::from_db_str(c.as_str()), Some(c));
        }
    }

    #[test]
    #[should_panic(expected = "is above 100")]
    fn recovery_refuses_a_chance_above_100() {
        roll_equipment_recovery(101, 1, true, &mut Rng::new(1));
    }
}
