// Fatal Flaw behavioural effects (bricks 4d-1, 4d-2).
// Ref: Phase_1_Section_4_Commander_Records.md §4d; Commander_Stress_System.md §2 (ODQ-35, ODQ-36);
//      Mercenary_Commander_System.md Pillar 4 (Rested behaviour).
//
// Pure and declarative: each effect carries its own number and the consumer applies it.
// Nothing in production calls `evaluate_fatal_flaw` yet; the consumers belong to 8d-1,
// Phase 2 §3d and the other host bricks named in §4d.
//
// (a) The first caller parses DB text with `CommanderFlaw::from_db_str`; a `None` is an
//     integrity `500` (CF3).
// (b) The first host brick that calls `evaluate_fatal_flaw` inside a logged sim entry point
//     bumps the workspace version (R7), and logs the flaw and the stress tier in
//     `session_start` before the roll. They are record-sourced inputs, and replay cannot
//     reconstruct them otherwise.
// (c) Each chance is drawn by the consumer, one draw per opportunity while the effect is
//     present, including at 100%. For an effect present at every tier, a tier change never
//     moves the draw count.
//
// A flaw's effects persist into higher tiers, except where a higher tier escalates the same
// mechanic, which replaces it (W1). Values are raw: the §2a buff ladder is the consumer's.

use crate::game_types::StressTier;
use crate::roster::CommanderFlaw;

/// Situational inputs a flaw may need. Empty: every effect is declarative and the consumer
/// applies it. Fields arrive with the consumers that read them (8d-1, …).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FatalFlawContext {}

/// Which deployments an `OppositionWarned` chance covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarnScope {
    /// Missions this Commander leads (The Informant, Rested).
    Missions,
    /// Missions and convoy legs (The Informant, Strained; ODQ-36 fix 3).
    MissionsAndConvoyLegs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatalFlawEffect {
    /// Embezzler: share of the mission's Credit payout skimmed, in basis points.
    PayoutSkim { basis_points: u16 },
    /// Bloodthirsty: chance a Retreat order is obeyed; the consumer rolls.
    RetreatCompliance { percent: u8 },
    /// Cowardly: the armor-strip Stability penalty, replacing the standard −15.
    ArmorStripStability { delta: i32 },
    /// The attached Section's Evasion modifier (Cowardly, Paranoid, Martinet).
    SectionEvasion { delta: i32 },
    /// The attached Section's flat Accuracy modifier (Martinet, Pillar 4).
    SectionAccuracy { delta: i32 },
    /// Paranoid: per-Tick chance of an Accuracy penalty (ODQ-36 fix 1).
    AccuracyPerTick { delta: i32, percent: u8 },
    /// Martinet, Breaking Point: per-Tick chance of a Stability hit (ODQ-36).
    StabilityPerTick { delta: i32, percent: u8 },
    /// The led unit's morale stat at deployment: Section Stability (Commander) or Crew Morale
    /// (Foreman). Glory Hound +, Doomsayer −; Pillar 4, Stress §2. The consumer applies it to
    /// its own unit.
    DeploymentStability { delta: i32 },
    /// Doomsayer: casualty and hazard losses to the led unit's morale stat (Section Stability
    /// or Crew Morale) reduced, in basis points.
    CasualtyStabilityLossReduction { basis_points: u16 },
    /// Martinet: between-mission Stability recovery rate, in basis points (negative = slower).
    StabilityRecovery { basis_points: i32 },
    /// Chrome-Chaser, Strained: Wounded recovery rate, in basis points (negative = slower).
    WoundedRecovery { basis_points: i32 },
    /// Commander upkeep, in basis points (Cult of Bob −, Chrome-Chaser +).
    UpkeepModifier { basis_points: i32 },
    /// The Informant: income from every contract payout, in basis points (ODQ-36 fix 2).
    PayoutKickback { basis_points: u16 },
    /// The Gambler: wagered share of each payout, and the chance it returns doubled.
    PayoutWager { basis_points: u16, doubled_percent: u8 },
    /// Per-leg chance of extra convoy transit Ticks (Cult of Bob, Squeak-Touched).
    TransitLegDelay { ticks: u8, percent: u8 },
    /// The Informant: chance the opposition was warned. What "warned" does is Section 8's.
    OppositionWarned { percent: u8, scope: WarnScope },
    /// The Informant, Breaking Point: the next engagement is warned, once. The consumer owns
    /// the "once" marker.
    NextEngagementWarned,
    /// The Informant, Strained: G.U.N. Negligence Inquiry likelihood, in basis points.
    InquiryLikelihood { basis_points: u16 },
    /// Glory Hound: tier-up offers surface on their deployments; declining costs Stress.
    TierUpOffers { decline_stress: u8 },
    /// Glory Hound, Strained: the first Retreat order of each engagement is refused; the
    /// re-issue on the next Tick is obeyed.
    FirstRetreatOrderRefused,
    /// Glory Hound, Breaking Point: commits to the objective regardless of the math
    /// (Section 8 defines "the math").
    IgnoresWithdrawalMath,
    /// The Gambler, Strained: per-mission chance a random equipped item starts Damaged.
    ItemStartsDamaged { percent: u8 },
    /// The Gambler, Breaking Point: per-unattended-item repossession chance at the Hub.
    UnattendedItemRepossession { percent: u8 },
    /// Chrome-Chaser: +delta to Accuracy or Evasion, alternating. The consumer owns the
    /// alternation key.
    AlternatingAccuracyEvasion { delta: i32 },
    /// Chrome-Chaser: the VC#6 Singularity contribution per deployment, at a weight in basis
    /// points (×0.5 = 5000).
    AugmentTickerContribution { augments: u8, weight_basis_points: u16 },
    /// Chrome-Chaser, Breaking Point: per-deployment chance the buffs are offline for the
    /// first Tick.
    BuffsOfflineFirstTick { percent: u8 },
    /// Martinet: desertion checks roll at +N percentage points.
    DesertionCheck { percent_points: u8 },
    /// Squeak-Touched: the daily Shrine offering, in Credits.
    ShrineOffering { credits_per_day: u32 },
    /// Squeak-Touched: once-per-mission chance of a free 1-Tick hazard or ambush warning.
    HazardWarning { percent: u8 },
    /// Squeak-Touched, Breaking Point: the Sermon fires once, on entering Breaking Point;
    /// each attendee converts at this chance. The consumer owns the "once".
    SermonOnEntry { conversion_percent: u8 },
    /// Doomsayer, Breaking Point: per-advance-order refusal chance, and how long it holds.
    AdvanceOrderRefusal { percent: u8, ticks: u8 },

    // ── Foreman extraction effects (7a-1, ODQ-38). Consumers: Phase 2 §13 Strike Mining,
    //    unless noted. ──
    /// High-Grader: share of each ore haul skimmed, in basis points. Ore, not Credits:
    /// never Embezzler's `PayoutSkim`.
    OreSkim { basis_points: u16 },
    /// Extraction yield, in basis points (Seam-Fever +, Deep-Ear's off-vein drift −).
    YieldModifier { basis_points: i32 },
    /// Seam-Fever: accident and cave-in checks roll at +N percentage points.
    AccidentCheck { percent_points: u8 },
    /// Cave-Shy: miner KIA reduced, in basis points.
    MinerKiaReduction { basis_points: u16 },
    /// Cave-Shy: chance each Hold-or-Fold window ends early.
    HoldOrFoldEarlyEnd { percent: u8 },
    /// Superstitious: flat, passive Crew Morale modifier, always active (not at-deployment).
    /// Consumer: Phase 2 §13 Crew Morale.
    CrewMorale { delta: i32 },
    /// Per-extraction chance of losing Ticks of extraction (Superstitious' omen delay,
    /// Deep-Ear's listening halt, Prospector's Itch's rig relocation).
    ExtractionDelay { ticks: u8, percent: u8 },
    /// Prospector's Itch: per-mission chance of surfacing a bonus vein lead.
    VeinLead { percent: u8 },
    /// Prospector's Itch, Breaking Point: per-extraction chance the dig converts to an
    /// unscouted site, hazard ratings unknown.
    UnscoutedSiteConversion { percent: u8 },
    /// Waste-Not: equipment recovery chance, +N percentage points on all outcomes.
    /// Consumer: the Equipment Recovery roll (7d-1 builds it; Phase 2 §13 applies this).
    EquipmentRecovery { percent_points: u8 },
    /// Waste-Not: each refurbished item's breakdown chance starts at `start_percent` and
    /// rises by `step_percent_points` per mission in service, capped at `cap_percent`. The
    /// consumer owns each item's missions-in-service count.
    RefurbishedBreakdown { start_percent: u8, step_percent_points: u8, cap_percent: u8 },
    /// Waste-Not, Breaking Point: per-extraction rig-failure check.
    RigFailureCheck { percent: u8 },
    /// Deep-Ear: eruption and vein-collapse warnings surface this many Ticks earlier, every
    /// time (unlike Squeak-Touched's `HazardWarning`, which is a chance).
    WarningLead { ticks: u8 },
    /// Deep-Ear: `VOID_CALLER_RELATION` per deployed campaign day, in tenths of a point.
    /// The consumer owns any accumulation onto the relation's own scale.
    VoidCallerRelationPerDay { tenths: i16 },
    /// Deep-Ear, Breaking Point: site event risk, +N percentage points per extraction.
    SiteEventRisk { percent_points: u8 },
    /// Deep-Ear, Breaking Point: a Void-Caller delegation arrives once, on entering Breaking
    /// Point. The consumer owns the "once".
    VoidCallerDelegationOnEntry,
}

use FatalFlawEffect::*;

/// The effects of `flaw` at `tier`, in a fixed order.
///
/// Exhaustive on both axes: a thirteenth flaw or a fifth `StressTier` is a compile error.
/// A Shattered Commander cannot deploy, so `Shattered` has no effects for any flaw.
pub fn evaluate_fatal_flaw(flaw: CommanderFlaw, tier: &StressTier, _context: &FatalFlawContext) -> Vec<FatalFlawEffect> {
    // TODO(phase-2-§10): Maturation (Controlled Aggression: Strained = max(50, own), Breaking Point 40/70; Creative Accounting; Calculated Caution)
    match flaw {
        CommanderFlaw::Embezzler     => embezzler(tier),
        CommanderFlaw::Bloodthirsty  => bloodthirsty(tier),
        CommanderFlaw::Cowardly      => cowardly(tier),
        CommanderFlaw::Paranoid      => paranoid(tier),
        CommanderFlaw::CultOfBob     => cult_of_bob(tier),
        CommanderFlaw::TheInformant  => the_informant(tier),
        CommanderFlaw::GloryHound    => glory_hound(tier),
        CommanderFlaw::TheGambler    => the_gambler(tier),
        CommanderFlaw::ChromeChaser  => chrome_chaser(tier),
        CommanderFlaw::Martinet      => martinet(tier),
        CommanderFlaw::SqueakTouched => squeak_touched(tier),
        CommanderFlaw::Doomsayer     => doomsayer(tier),
    }
}

/// `base`, followed by `more`.
pub(crate) fn then(mut base: Vec<FatalFlawEffect>, more: &[FatalFlawEffect]) -> Vec<FatalFlawEffect> {
    base.extend_from_slice(more);
    base
}

/// Embezzler's "log the discrepancy" belongs to the payout consumer, not here.
fn embezzler(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![PayoutSkim { basis_points: 200 }],
        StressTier::Strained      => vec![PayoutSkim { basis_points: 1000 }],
        StressTier::BreakingPoint => vec![PayoutSkim { basis_points: 1000 }],
        StressTier::Shattered     => vec![],
    }
}

/// The Breaking Point compliance of `0` duplicates the universal Breaking Point Retreat lock
/// on purpose: Controlled Aggression lifts only this case.
fn bloodthirsty(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![RetreatCompliance { percent: 100 }],
        StressTier::Strained      => vec![RetreatCompliance { percent: 50 }],
        StressTier::BreakingPoint => vec![RetreatCompliance { percent: 0 }],
        StressTier::Shattered     => vec![],
    }
}

fn cowardly(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![ArmorStripStability { delta: -30 }],
        StressTier::Strained      => vec![ArmorStripStability { delta: -30 }, SectionEvasion { delta: -10 }],
        StressTier::BreakingPoint => vec![ArmorStripStability { delta: -30 }, SectionEvasion { delta: -10 }],
        StressTier::Shattered     => vec![],
    }
}

fn paranoid(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![SectionEvasion { delta: 5 }, AccuracyPerTick { delta: -5, percent: 25 }],
        StressTier::Strained      => vec![SectionEvasion { delta: 10 }, AccuracyPerTick { delta: -10, percent: 100 }],
        StressTier::BreakingPoint => vec![SectionEvasion { delta: 10 }, AccuracyPerTick { delta: -20, percent: 100 }],
        StressTier::Shattered     => vec![],
    }
}

/// Shared with the Foreman pool: 7a-1's Foreman evaluator reuses this, so the shared flaw
/// cannot drift between the classes.
pub(crate) fn cult_of_bob(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![UpkeepModifier { basis_points: -500 }, TransitLegDelay { ticks: 1, percent: 10 }],
        StressTier::Strained      => vec![UpkeepModifier { basis_points: -500 }, TransitLegDelay { ticks: 1, percent: 100 }],
        StressTier::BreakingPoint => cult_of_bob(&StressTier::Strained),
        StressTier::Shattered     => vec![],
    }
}

fn the_informant(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested => vec![
            PayoutKickback { basis_points: 100 },
            OppositionWarned { percent: 10, scope: WarnScope::Missions },
        ],
        StressTier::Strained => vec![
            PayoutKickback { basis_points: 100 },
            OppositionWarned { percent: 25, scope: WarnScope::MissionsAndConvoyLegs },
            InquiryLikelihood { basis_points: 2500 },
        ],
        StressTier::BreakingPoint => then(the_informant(&StressTier::Strained), &[NextEngagementWarned]),
        StressTier::Shattered     => vec![],
    }
}

fn glory_hound(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![DeploymentStability { delta: 5 }, TierUpOffers { decline_stress: 5 }],
        StressTier::Strained      => then(glory_hound(&StressTier::Rested), &[FirstRetreatOrderRefused]),
        StressTier::BreakingPoint => then(glory_hound(&StressTier::Strained), &[IgnoresWithdrawalMath]),
        StressTier::Shattered     => vec![],
    }
}

fn the_gambler(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested        => vec![PayoutWager { basis_points: 500, doubled_percent: 50 }],
        StressTier::Strained      => vec![PayoutWager { basis_points: 1500, doubled_percent: 50 }, ItemStartsDamaged { percent: 10 }],
        StressTier::BreakingPoint => then(the_gambler(&StressTier::Strained), &[UnattendedItemRepossession { percent: 10 }]),
        StressTier::Shattered     => vec![],
    }
}

fn chrome_chaser(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested => vec![
            AlternatingAccuracyEvasion { delta: 5 },
            UpkeepModifier { basis_points: 2000 },
            AugmentTickerContribution { augments: 1, weight_basis_points: 5000 },
        ],
        StressTier::Strained      => then(chrome_chaser(&StressTier::Rested), &[WoundedRecovery { basis_points: -2500 }]),
        StressTier::BreakingPoint => then(chrome_chaser(&StressTier::Strained), &[BuffsOfflineFirstTick { percent: 25 }]),
        StressTier::Shattered     => vec![],
    }
}

fn martinet(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested => vec![
            SectionAccuracy { delta: 5 },
            StabilityRecovery { basis_points: -5000 },
            DesertionCheck { percent_points: 5 },
        ],
        StressTier::Strained      => then(martinet(&StressTier::Rested), &[SectionEvasion { delta: -10 }]),
        StressTier::BreakingPoint => then(martinet(&StressTier::Strained), &[StabilityPerTick { delta: -5, percent: 10 }]),
        StressTier::Shattered     => vec![],
    }
}

/// Shared with the Foreman pool: 7a-1's Foreman evaluator reuses this, so the shared flaw
/// cannot drift between the classes.
pub(crate) fn squeak_touched(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested => vec![ShrineOffering { credits_per_day: 50 }, HazardWarning { percent: 25 }],
        StressTier::Strained => vec![
            ShrineOffering { credits_per_day: 150 },
            HazardWarning { percent: 25 },
            TransitLegDelay { ticks: 1, percent: 100 },
        ],
        StressTier::BreakingPoint => then(squeak_touched(&StressTier::Strained), &[SermonOnEntry { conversion_percent: 5 }]),
        StressTier::Shattered     => vec![],
    }
}

/// Shared with the Foreman pool: 7a-1's Foreman evaluator reuses this, so the shared flaw
/// cannot drift between the classes.
pub(crate) fn doomsayer(tier: &StressTier) -> Vec<FatalFlawEffect> {
    match tier {
        StressTier::Rested => vec![
            DeploymentStability { delta: -5 },
            CasualtyStabilityLossReduction { basis_points: 2500 },
        ],
        StressTier::Strained => vec![
            DeploymentStability { delta: -10 },
            CasualtyStabilityLossReduction { basis_points: 4000 },
        ],
        StressTier::BreakingPoint => then(doomsayer(&StressTier::Strained), &[AdvanceOrderRefusal { percent: 25, ticks: 1 }]),
        StressTier::Shattered     => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIERS: [StressTier; 4] = [
        StressTier::Rested,
        StressTier::Strained,
        StressTier::BreakingPoint,
        StressTier::Shattered,
    ];

    fn eval(flaw: CommanderFlaw, tier: StressTier) -> Vec<FatalFlawEffect> {
        evaluate_fatal_flaw(flaw, &tier, &FatalFlawContext::default())
    }

    fn assert_table(flaw: CommanderFlaw, expected: [Vec<FatalFlawEffect>; 4]) {
        for (tier, want) in TIERS.into_iter().zip(expected) {
            assert_eq!(eval(flaw, tier), want, "{flaw:?} at {tier:?}");
        }
    }

    #[test]
    fn evaluate_fatal_flaw_embezzler_skims_by_tier() {
        assert_table(CommanderFlaw::Embezzler, [
            vec![PayoutSkim { basis_points: 200 }],
            vec![PayoutSkim { basis_points: 1000 }],
            vec![PayoutSkim { basis_points: 1000 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_bloodthirsty_retreat_compliance_by_tier() {
        assert_table(CommanderFlaw::Bloodthirsty, [
            vec![RetreatCompliance { percent: 100 }],
            vec![RetreatCompliance { percent: 50 }],
            vec![RetreatCompliance { percent: 0 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_cowardly_by_tier() {
        assert_table(CommanderFlaw::Cowardly, [
            vec![ArmorStripStability { delta: -30 }],
            vec![ArmorStripStability { delta: -30 }, SectionEvasion { delta: -10 }],
            vec![ArmorStripStability { delta: -30 }, SectionEvasion { delta: -10 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_paranoid_by_tier() {
        assert_table(CommanderFlaw::Paranoid, [
            vec![SectionEvasion { delta: 5 }, AccuracyPerTick { delta: -5, percent: 25 }],
            vec![SectionEvasion { delta: 10 }, AccuracyPerTick { delta: -10, percent: 100 }],
            vec![SectionEvasion { delta: 10 }, AccuracyPerTick { delta: -20, percent: 100 }],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_cult_of_bob_by_tier() {
        let strained = vec![UpkeepModifier { basis_points: -500 }, TransitLegDelay { ticks: 1, percent: 100 }];
        assert_table(CommanderFlaw::CultOfBob, [
            vec![UpkeepModifier { basis_points: -500 }, TransitLegDelay { ticks: 1, percent: 10 }],
            strained.clone(),
            strained,
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_the_informant_by_tier() {
        assert_table(CommanderFlaw::TheInformant, [
            vec![PayoutKickback { basis_points: 100 }, OppositionWarned { percent: 10, scope: WarnScope::Missions }],
            vec![
                PayoutKickback { basis_points: 100 },
                OppositionWarned { percent: 25, scope: WarnScope::MissionsAndConvoyLegs },
                InquiryLikelihood { basis_points: 2500 },
            ],
            vec![
                PayoutKickback { basis_points: 100 },
                OppositionWarned { percent: 25, scope: WarnScope::MissionsAndConvoyLegs },
                InquiryLikelihood { basis_points: 2500 },
                NextEngagementWarned,
            ],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_glory_hound_by_tier() {
        assert_table(CommanderFlaw::GloryHound, [
            vec![DeploymentStability { delta: 5 }, TierUpOffers { decline_stress: 5 }],
            vec![DeploymentStability { delta: 5 }, TierUpOffers { decline_stress: 5 }, FirstRetreatOrderRefused],
            vec![
                DeploymentStability { delta: 5 },
                TierUpOffers { decline_stress: 5 },
                FirstRetreatOrderRefused,
                IgnoresWithdrawalMath,
            ],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_the_gambler_by_tier() {
        assert_table(CommanderFlaw::TheGambler, [
            vec![PayoutWager { basis_points: 500, doubled_percent: 50 }],
            vec![PayoutWager { basis_points: 1500, doubled_percent: 50 }, ItemStartsDamaged { percent: 10 }],
            vec![
                PayoutWager { basis_points: 1500, doubled_percent: 50 },
                ItemStartsDamaged { percent: 10 },
                UnattendedItemRepossession { percent: 10 },
            ],
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_chrome_chaser_by_tier() {
        let rested = [
            AlternatingAccuracyEvasion { delta: 5 },
            UpkeepModifier { basis_points: 2000 },
            AugmentTickerContribution { augments: 1, weight_basis_points: 5000 },
        ];
        assert_table(CommanderFlaw::ChromeChaser, [
            rested.to_vec(),
            [&rested[..], &[WoundedRecovery { basis_points: -2500 }]].concat(),
            [&rested[..], &[WoundedRecovery { basis_points: -2500 }, BuffsOfflineFirstTick { percent: 25 }]].concat(),
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_martinet_by_tier() {
        let rested = [
            SectionAccuracy { delta: 5 },
            StabilityRecovery { basis_points: -5000 },
            DesertionCheck { percent_points: 5 },
        ];
        assert_table(CommanderFlaw::Martinet, [
            rested.to_vec(),
            [&rested[..], &[SectionEvasion { delta: -10 }]].concat(),
            [&rested[..], &[SectionEvasion { delta: -10 }, StabilityPerTick { delta: -5, percent: 10 }]].concat(),
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_squeak_touched_by_tier() {
        let strained = [
            ShrineOffering { credits_per_day: 150 },
            HazardWarning { percent: 25 },
            TransitLegDelay { ticks: 1, percent: 100 },
        ];
        assert_table(CommanderFlaw::SqueakTouched, [
            vec![ShrineOffering { credits_per_day: 50 }, HazardWarning { percent: 25 }],
            strained.to_vec(),
            [&strained[..], &[SermonOnEntry { conversion_percent: 5 }]].concat(),
            vec![],
        ]);
    }

    #[test]
    fn evaluate_fatal_flaw_doomsayer_by_tier() {
        assert_table(CommanderFlaw::Doomsayer, [
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
    fn evaluate_fatal_flaw_shattered_is_empty_for_every_flaw() {
        for flaw in CommanderFlaw::ALL {
            assert!(eval(flaw, StressTier::Shattered).is_empty(), "{flaw:?} at Shattered");
        }
    }
}
