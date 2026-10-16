// Mining Foreman Fatal Flaw effects (brick 7a-1).
// Ref: Mining_Foreman_System.md Pillar 4 and its ODQ-38 tier table (the seven exclusive flaws);
//      Commander_Stress_System.md §2 (the three shared flaws, ODQ-36).
//
// Pure and declarative, like `crate::fatal_flaw`, and returning the same `FatalFlawEffect`.
// Nothing in production calls `evaluate_foreman_flaw`; the extraction consumers are Phase 2
// §13's. The rules in `fatal_flaw.rs`'s header (a)–(c) apply unchanged.
//
// The shared flaws route through the Commander functions, so they cannot drift between the
// classes (Reviewer 4d-2 F3). A flaw's effects persist into higher tiers, except where a
// higher tier escalates the same mechanic, which replaces it (W1).

use crate::fatal_flaw::{cult_of_bob, doomsayer, squeak_touched, then, FatalFlawContext, FatalFlawEffect};
use crate::game_types::StressTier;
use crate::roster::ForemanFlaw;

use FatalFlawEffect::*;

/// The effects of a Foreman's `flaw` at `tier`, in a fixed order.
///
/// Exhaustive on both axes: an eleventh flaw or a fifth `StressTier` is a compile error. A
/// Shattered Foreman cannot deploy, so `Shattered` has no effects for any flaw.
pub fn evaluate_foreman_flaw(flaw: ForemanFlaw, tier: &StressTier, _context: &FatalFlawContext) -> Vec<FatalFlawEffect> {
    match flaw {
        ForemanFlaw::Superstitious   => superstitious(tier),
        ForemanFlaw::Doomsayer       => doomsayer(tier),
        ForemanFlaw::CultOfBob       => cult_of_bob(tier),
        ForemanFlaw::SqueakTouched   => squeak_touched(tier),
        ForemanFlaw::WasteNot        => waste_not(tier),
        ForemanFlaw::CaveShy         => cave_shy(tier),
        ForemanFlaw::ProspectorsItch => prospectors_itch(tier),
        ForemanFlaw::DeepEar         => deep_ear(tier),
        ForemanFlaw::HighGrader      => high_grader(tier),
        ForemanFlaw::SeamFever       => seam_fever(tier),
    }
}

/// Mirrors Embezzler's 2% / 10% / 10%, on ore rather than Credits.
fn high_grader(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![OreSkim { basis_points: 200 }],
        StressTier::Strained      => vec![OreSkim { basis_points: 1000 }],
        StressTier::BreakingPoint => high_grader(&StressTier::Strained),
        StressTier::Shattered     => vec![],
    }
}

fn seam_fever(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![YieldModifier { basis_points: 1000 }, AccidentCheck { percent_points: 5 }],
        StressTier::Strained      => vec![YieldModifier { basis_points: 1000 }, AccidentCheck { percent_points: 10 }],
        StressTier::BreakingPoint => vec![YieldModifier { basis_points: 1000 }, AccidentCheck { percent_points: 20 }],
        StressTier::Shattered     => vec![],
    }
}

fn cave_shy(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![MinerKiaReduction { basis_points: 2500 }, HoldOrFoldEarlyEnd { percent: 10 }],
        StressTier::Strained      => vec![MinerKiaReduction { basis_points: 2500 }, HoldOrFoldEarlyEnd { percent: 25 }],
        StressTier::BreakingPoint => vec![MinerKiaReduction { basis_points: 2500 }, HoldOrFoldEarlyEnd { percent: 50 }],
        StressTier::Shattered     => vec![],
    }
}

fn superstitious(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![CrewMorale { delta: 5 }, ExtractionDelay { ticks: 1, percent: 10 }],
        StressTier::Strained      => vec![CrewMorale { delta: 5 }, ExtractionDelay { ticks: 1, percent: 25 }],
        StressTier::BreakingPoint => vec![CrewMorale { delta: 5 }, ExtractionDelay { ticks: 1, percent: 50 }],
        StressTier::Shattered     => vec![],
    }
}

/// At Breaking Point the unscouted-site conversion replaces the rig relocation.
fn prospectors_itch(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![VeinLead { percent: 10 }, ExtractionDelay { ticks: 1, percent: 10 }],
        StressTier::Strained      => vec![VeinLead { percent: 10 }, ExtractionDelay { ticks: 1, percent: 25 }],
        StressTier::BreakingPoint => vec![VeinLead { percent: 10 }, UnscoutedSiteConversion { percent: 25 }],
        StressTier::Shattered     => vec![],
    }
}

/// Breaking Point keeps Strained's +4 step (W1) and adds the rig-failure check.
fn waste_not(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested => vec![
            EquipmentRecovery { percent_points: 15 },
            RefurbishedBreakdown { start_percent: 2, step_percent_points: 2, cap_percent: 20 },
        ],
        StressTier::Strained => vec![
            EquipmentRecovery { percent_points: 15 },
            RefurbishedBreakdown { start_percent: 2, step_percent_points: 4, cap_percent: 20 },
        ],
        StressTier::BreakingPoint => then(waste_not(&StressTier::Strained), &[RigFailureCheck { percent: 10 }]),
        StressTier::Shattered     => vec![],
    }
}

fn deep_ear(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested => vec![
            WarningLead { ticks: 1 },
            VoidCallerRelationPerDay { tenths: 1 },
            ExtractionDelay { ticks: 1, percent: 10 },
        ],
        StressTier::Strained      => then(deep_ear(&StressTier::Rested), &[YieldModifier { basis_points: -1000 }]),
        StressTier::BreakingPoint => then(
            deep_ear(&StressTier::Strained),
            &[SiteEventRisk { percent_points: 25 }, VoidCallerDelegationOnEntry],
        ),
        StressTier::Shattered => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fatal_flaw::evaluate_fatal_flaw;
    use crate::roster::CommanderFlaw;

    const TIERS: [StressTier; 4] = [
        StressTier::Rested,
        StressTier::Strained,
        StressTier::BreakingPoint,
        StressTier::Shattered,
    ];

    fn eval(flaw: ForemanFlaw, tier: StressTier) -> Vec<FatalFlawEffect> {
        evaluate_foreman_flaw(flaw, &tier, &FatalFlawContext::default())
    }

    fn assert_table(flaw: ForemanFlaw, expected: [Vec<FatalFlawEffect>; 4]) {
        for (tier, want) in TIERS.into_iter().zip(expected) {
            assert_eq!(eval(flaw, tier), want, "{flaw:?} at {tier:?}");
        }
    }

    #[test]
    fn evaluate_foreman_flaw_high_grader() {
        assert_table(ForemanFlaw::HighGrader, [
            vec![OreSkim { basis_points: 200 }],
            vec![OreSkim { basis_points: 1000 }],
            vec![OreSkim { basis_points: 1000 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_seam_fever() {
        assert_table(ForemanFlaw::SeamFever, [
            vec![YieldModifier { basis_points: 1000 }, AccidentCheck { percent_points: 5 }],
            vec![YieldModifier { basis_points: 1000 }, AccidentCheck { percent_points: 10 }],
            vec![YieldModifier { basis_points: 1000 }, AccidentCheck { percent_points: 20 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_cave_shy() {
        assert_table(ForemanFlaw::CaveShy, [
            vec![MinerKiaReduction { basis_points: 2500 }, HoldOrFoldEarlyEnd { percent: 10 }],
            vec![MinerKiaReduction { basis_points: 2500 }, HoldOrFoldEarlyEnd { percent: 25 }],
            vec![MinerKiaReduction { basis_points: 2500 }, HoldOrFoldEarlyEnd { percent: 50 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_superstitious() {
        assert_table(ForemanFlaw::Superstitious, [
            vec![CrewMorale { delta: 5 }, ExtractionDelay { ticks: 1, percent: 10 }],
            vec![CrewMorale { delta: 5 }, ExtractionDelay { ticks: 1, percent: 25 }],
            vec![CrewMorale { delta: 5 }, ExtractionDelay { ticks: 1, percent: 50 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_prospectors_itch() {
        assert_table(ForemanFlaw::ProspectorsItch, [
            vec![VeinLead { percent: 10 }, ExtractionDelay { ticks: 1, percent: 10 }],
            vec![VeinLead { percent: 10 }, ExtractionDelay { ticks: 1, percent: 25 }],
            vec![VeinLead { percent: 10 }, UnscoutedSiteConversion { percent: 25 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_waste_not() {
        assert_table(ForemanFlaw::WasteNot, [
            vec![
                EquipmentRecovery { percent_points: 15 },
                RefurbishedBreakdown { start_percent: 2, step_percent_points: 2, cap_percent: 20 },
            ],
            vec![
                EquipmentRecovery { percent_points: 15 },
                RefurbishedBreakdown { start_percent: 2, step_percent_points: 4, cap_percent: 20 },
            ],
            vec![
                EquipmentRecovery { percent_points: 15 },
                RefurbishedBreakdown { start_percent: 2, step_percent_points: 4, cap_percent: 20 },
                RigFailureCheck { percent: 10 },
            ],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_deep_ear() {
        assert_table(ForemanFlaw::DeepEar, [
            vec![
                WarningLead { ticks: 1 },
                VoidCallerRelationPerDay { tenths: 1 },
                ExtractionDelay { ticks: 1, percent: 10 },
            ],
            vec![
                WarningLead { ticks: 1 },
                VoidCallerRelationPerDay { tenths: 1 },
                ExtractionDelay { ticks: 1, percent: 10 },
                YieldModifier { basis_points: -1000 },
            ],
            vec![
                WarningLead { ticks: 1 },
                VoidCallerRelationPerDay { tenths: 1 },
                ExtractionDelay { ticks: 1, percent: 10 },
                YieldModifier { basis_points: -1000 },
                SiteEventRisk { percent_points: 25 },
                VoidCallerDelegationOnEntry,
            ],
            vec![],
        ]);
    }

    /// The shared flaws' numbers, spelled from Commander_Stress_System §2 (ODQ-36).
    #[test]
    fn evaluate_foreman_flaw_cult_of_bob() {
        assert_table(ForemanFlaw::CultOfBob, [
            vec![UpkeepModifier { basis_points: -500 }, TransitLegDelay { ticks: 1, percent: 10 }],
            vec![UpkeepModifier { basis_points: -500 }, TransitLegDelay { ticks: 1, percent: 100 }],
            vec![UpkeepModifier { basis_points: -500 }, TransitLegDelay { ticks: 1, percent: 100 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_squeak_touched() {
        assert_table(ForemanFlaw::SqueakTouched, [
            vec![ShrineOffering { credits_per_day: 50 }, HazardWarning { percent: 25 }],
            vec![
                ShrineOffering { credits_per_day: 150 },
                HazardWarning { percent: 25 },
                TransitLegDelay { ticks: 1, percent: 100 },
            ],
            vec![
                ShrineOffering { credits_per_day: 150 },
                HazardWarning { percent: 25 },
                TransitLegDelay { ticks: 1, percent: 100 },
                SermonOnEntry { conversion_percent: 5 },
            ],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_doomsayer() {
        assert_table(ForemanFlaw::Doomsayer, [
            vec![DeploymentStability { delta: -5 }, CasualtyStabilityLossReduction { basis_points: 2500 }],
            vec![DeploymentStability { delta: -10 }, CasualtyStabilityLossReduction { basis_points: 4000 }],
            vec![
                DeploymentStability { delta: -10 },
                CasualtyStabilityLossReduction { basis_points: 4000 },
                AdvanceOrderRefusal { percent: 25, ticks: 1 },
            ],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_foreman_flaw_shattered_is_empty_for_every_flaw() {
        for flaw in ForemanFlaw::ALL {
            assert!(eval(flaw, StressTier::Shattered).is_empty(), "{flaw:?} at Shattered");
        }
    }

    /// Reviewer 4d-2 F3: a shared flaw behaves identically for both classes at every tier.
    /// The pair list is literal; `grade.rs` pins that exactly three flaw strings are shared.
    #[test]
    fn foreman_shared_flaws_match_commander_at_every_tier() {
        let pairs = [
            (ForemanFlaw::CultOfBob, CommanderFlaw::CultOfBob),
            (ForemanFlaw::SqueakTouched, CommanderFlaw::SqueakTouched),
            (ForemanFlaw::Doomsayer, CommanderFlaw::Doomsayer),
        ];
        let ctx = FatalFlawContext::default();
        for (foreman, commander) in pairs {
            for tier in TIERS {
                assert_eq!(
                    evaluate_foreman_flaw(foreman, &tier, &ctx),
                    evaluate_fatal_flaw(commander, &tier, &ctx),
                    "{foreman:?} at {tier:?}"
                );
            }
        }
    }
}
