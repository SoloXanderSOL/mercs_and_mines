// Fatal Flaw behavioural effects (brick 4d-1).
// Ref: Phase_1_Section_4_Commander_Records.md §4d; Commander_Stress_System.md §2 (ODQ-35).
//
// Pure and declarative: each effect carries its own number and the consumer applies it.
// Nothing in production calls `evaluate_fatal_flaw` yet; the consumers belong to 8d-1,
// Phase 2 §3d and the other host bricks named in §4d.

use crate::game_types::StressTier;

/// Situational inputs a flaw may need. Empty in 4d-1: every effect is declarative and
/// the consumer applies it. Fields arrive with the consumers that read them (8d-1, …).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FatalFlawContext {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatalFlawEffect {
    /// Embezzler: share of the mission's Credit payout skimmed, in basis points.
    PayoutSkim { basis_points: u16 },
    /// Bloodthirsty: chance a Retreat order is obeyed; the consumer rolls.
    RetreatCompliance { percent: u8 },
    /// Cowardly: the armor-strip Stability penalty, replacing the standard −15.
    ArmorStripStability { delta: i32 },
    /// Cowardly, Strained and up: the attached Section's Evasion modifier.
    SectionEvasion { delta: i32 },
}

/// The effects of `flaw` at `tier`, in a fixed order.
///
/// The match is on the exact flaw string (no trimming, no case folding); an unknown flaw
/// has no effects. A Shattered Commander cannot deploy, so `Shattered` has none either.
///
/// Bloodthirsty's Breaking Point compliance of `0` duplicates the universal Breaking Point
/// Retreat lock on purpose: Controlled Aggression lifts only this case. Embezzler's
/// "log the discrepancy" belongs to the payout consumer, not here.
pub fn evaluate_fatal_flaw(flaw: &str, tier: &StressTier, _context: &FatalFlawContext) -> Vec<FatalFlawEffect> {
    use FatalFlawEffect::*;
    if matches!(tier, StressTier::Shattered) {
        return vec![];
    }
    let escalated = matches!(tier, StressTier::Strained | StressTier::BreakingPoint);
    // TODO(phase-2-§10): Maturation (Controlled Aggression: Strained = max(50, own), Breaking Point 40/70; Creative Accounting; Calculated Caution)
    match flaw {
        "Embezzler" => vec![PayoutSkim { basis_points: if escalated { 1000 } else { 200 } }],
        "Bloodthirsty" => vec![RetreatCompliance {
            percent: match tier {
                StressTier::Rested => 100,
                StressTier::Strained => 50,
                _ => 0,
            },
        }],
        "Cowardly" => {
            let mut effects = vec![ArmorStripStability { delta: -30 }];
            if escalated {
                effects.push(SectionEvasion { delta: -10 });
            }
            effects
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FatalFlawEffect::*;

    const TIERS: [StressTier; 4] = [
        StressTier::Rested,
        StressTier::Strained,
        StressTier::BreakingPoint,
        StressTier::Shattered,
    ];

    fn eval(flaw: &str, tier: StressTier) -> Vec<FatalFlawEffect> {
        evaluate_fatal_flaw(flaw, &tier, &FatalFlawContext::default())
    }

    fn assert_table(flaw: &str, expected: [Vec<FatalFlawEffect>; 4]) {
        for (tier, want) in TIERS.into_iter().zip(expected) {
            assert_eq!(eval(flaw, tier), want, "{flaw} at {tier:?}");
        }
    }

    #[test]
    fn evaluate_fatal_flaw_embezzler_skims_by_tier() {
        assert_table("Embezzler", [
            vec![PayoutSkim { basis_points: 200 }],
            vec![PayoutSkim { basis_points: 1000 }],
            vec![PayoutSkim { basis_points: 1000 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_bloodthirsty_retreat_compliance_by_tier() {
        assert_table("Bloodthirsty", [
            vec![RetreatCompliance { percent: 100 }],
            vec![RetreatCompliance { percent: 50 }],
            vec![RetreatCompliance { percent: 0 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_cowardly_by_tier() {
        assert_table("Cowardly", [
            vec![ArmorStripStability { delta: -30 }],
            vec![ArmorStripStability { delta: -30 }, SectionEvasion { delta: -10 }],
            vec![ArmorStripStability { delta: -30 }, SectionEvasion { delta: -10 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_unknown_flaw_is_empty() {
        for flaw in ["Reckless", "none", ""] {
            for tier in TIERS {
                assert!(eval(flaw, tier).is_empty(), "{flaw:?} at {tier:?}");
            }
        }
    }

    #[test]
    fn evaluate_fatal_flaw_is_case_and_whitespace_exact() {
        for flaw in ["embezzler", "The Embezzler", " Cowardly", "BLOODTHIRSTY"] {
            for tier in TIERS {
                assert!(eval(flaw, tier).is_empty(), "{flaw:?} at {tier:?}");
            }
        }
    }
}
