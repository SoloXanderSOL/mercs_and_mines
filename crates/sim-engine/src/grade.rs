//! Commander Grade: the Trait Quality Score (brick 4a-2).
//! Ref: Commander_Grade_System.md §3a–§3e (canon-locked tables).
//!
//! TQS = Synergy (0–40) + Flaw Severity (0–40) + Coherence (0–20). Both Field Leader
//! classes are graded by the same formula, each against its own tables. The tables are
//! data, keyed on the `crate::roster` enums. Pure: no RNG, no I/O. TQS is computed and
//! discarded; only the Grade is ever stored.

use crate::roster::{
    CommanderFlaw, CommanderGrade, CommanderOrigin, CommanderSpecialization, ForemanFlaw, ForemanSpecialization,
};

const SIGNATURE: u8 = 40;
const ALIGNED: u8 = 30;
const NEUTRAL: u8 = 20;
const OFF_PROFILE: u8 = 10;

const COHERENT: u8 = 20;
const STANDARD_COHERENCE: u8 = 12;
const CONTRADICTORY: u8 = 4;

/// One origin's Table A row: two Signature, two Aligned and two Off-Profile
/// Specializations. Everything unlisted is Neutral.
#[derive(Debug, Clone, Copy)]
pub struct SynergyRow<S: 'static> {
    pub signature:   [S; 2],
    pub aligned:     [S; 2],
    pub off_profile: [S; 2],
}

fn synergy<S: PartialEq>(row: &SynergyRow<S>, spec: S) -> u8 {
    if row.signature.contains(&spec) {
        SIGNATURE
    } else if row.aligned.contains(&spec) {
        ALIGNED
    } else if row.off_profile.contains(&spec) {
        OFF_PROFILE
    } else {
        NEUTRAL
    }
}

fn coherence<F: PartialEq, S: PartialEq>(coherent: &[(F, S)], contradictory: &[(F, S)], flaw: F, spec: S) -> u8 {
    let pair = (flaw, spec);
    if coherent.contains(&pair) {
        COHERENT
    } else if contradictory.contains(&pair) {
        CONTRADICTORY
    } else {
        STANDARD_COHERENCE
    }
}

// ── Commander tables ─────────────────────────────────────────────────────────

/// Table A, Commander matrix (§3b).
pub fn commander_synergy_row(origin: CommanderOrigin) -> SynergyRow<CommanderSpecialization> {
    use CommanderSpecialization as S;
    match origin {
        CommanderOrigin::Duck => SynergyRow {
            signature:   [S::Breacher, S::TrenchSweeper],
            aligned:     [S::NightStalker, S::SuppressionDoctrine],
            off_profile: [S::Cannoneer, S::MarksmansEye],
        },
        CommanderOrigin::HumanCorporate => SynergyRow {
            signature:   [S::QuartermastersGhost, S::ArmorAdept],
            aligned:     [S::Cannoneer, S::SuppressionDoctrine],
            off_profile: [S::NightStalker, S::TrenchSweeper],
        },
        CommanderOrigin::HumanUnderhive => SynergyRow {
            signature:   [S::NightStalker, S::TrenchSweeper],
            aligned:     [S::Breacher, S::QuartermastersGhost],
            off_profile: [S::ArmorAdept, S::Cannoneer],
        },
        CommanderOrigin::Bearocrat => SynergyRow {
            signature:   [S::SuppressionDoctrine, S::QuartermastersGhost],
            aligned:     [S::CorpsemansTouch, S::Cannoneer],
            off_profile: [S::NightStalker, S::Breacher],
        },
        CommanderOrigin::ScienceApe => SynergyRow {
            signature:   [S::BattlefieldEngineer, S::AntiArmorAdept],
            aligned:     [S::CorpsemansTouch, S::MarksmansEye],
            off_profile: [S::TrenchSweeper, S::SuppressionDoctrine],
        },
    }
}

/// Table B, Commander pool (§3c). Higher is milder.
pub fn commander_flaw_severity(flaw: CommanderFlaw) -> u8 {
    match flaw {
        CommanderFlaw::Paranoid      => 32,
        CommanderFlaw::Doomsayer     => 30,
        CommanderFlaw::CultOfBob     => 26,
        CommanderFlaw::SqueakTouched => 24,
        CommanderFlaw::Martinet      => 22,
        CommanderFlaw::Cowardly      => 20,
        CommanderFlaw::GloryHound    => 18,
        CommanderFlaw::ChromeChaser  => 16,
        CommanderFlaw::TheGambler    => 14,
        CommanderFlaw::Embezzler     => 12,
        CommanderFlaw::TheInformant  => 10,
        CommanderFlaw::Bloodthirsty  => 8,
    }
}

/// Table C, Commander Coherent pairings (§3d), keyed on (Flaw, Specialization) only.
pub const COMMANDER_COHERENT: &[(CommanderFlaw, CommanderSpecialization)] = &[
    (CommanderFlaw::Bloodthirsty, CommanderSpecialization::TrenchSweeper),
    (CommanderFlaw::Bloodthirsty, CommanderSpecialization::Breacher),
    (CommanderFlaw::Bloodthirsty, CommanderSpecialization::SuppressionDoctrine),
    (CommanderFlaw::Paranoid,     CommanderSpecialization::NightStalker),
    (CommanderFlaw::Cowardly,     CommanderSpecialization::VeteransInstinct),
    (CommanderFlaw::Martinet,     CommanderSpecialization::SuppressionDoctrine),
    (CommanderFlaw::TheInformant, CommanderSpecialization::NightStalker),
    (CommanderFlaw::Doomsayer,    CommanderSpecialization::CorpsemansTouch),
    (CommanderFlaw::Paranoid,     CommanderSpecialization::QuartermastersGhost),
    (CommanderFlaw::Doomsayer,    CommanderSpecialization::BattlefieldEngineer),
];

/// Table C, Commander Contradictory pairings (§3d).
pub const COMMANDER_CONTRADICTORY: &[(CommanderFlaw, CommanderSpecialization)] = &[
    (CommanderFlaw::Cowardly,     CommanderSpecialization::TrenchSweeper),
    (CommanderFlaw::Cowardly,     CommanderSpecialization::Breacher),
    (CommanderFlaw::Cowardly,     CommanderSpecialization::SuppressionDoctrine),
    (CommanderFlaw::Bloodthirsty, CommanderSpecialization::CorpsemansTouch),
    (CommanderFlaw::Bloodthirsty, CommanderSpecialization::NightStalker),
    (CommanderFlaw::Embezzler,    CommanderSpecialization::QuartermastersGhost),
    (CommanderFlaw::CultOfBob,    CommanderSpecialization::QuartermastersGhost),
    (CommanderFlaw::Paranoid,     CommanderSpecialization::MarksmansEye),
    (CommanderFlaw::GloryHound,   CommanderSpecialization::NightStalker),
    (CommanderFlaw::TheGambler,   CommanderSpecialization::QuartermastersGhost),
    (CommanderFlaw::ChromeChaser, CommanderSpecialization::CorpsemansTouch),
];

// ── Foreman tables ───────────────────────────────────────────────────────────

/// Table A, Foreman matrix (§3b).
pub fn foreman_synergy_row(origin: CommanderOrigin) -> SynergyRow<ForemanSpecialization> {
    use ForemanSpecialization as S;
    match origin {
        CommanderOrigin::Duck => SynergyRow {
            signature:   [S::DeepDriller, S::ShoringExpert],
            aligned:     [S::LaborRelations, S::TectonicWhisperer],
            off_profile: [S::BlackMarketAuger, S::ConvoyHardener],
        },
        CommanderOrigin::HumanCorporate => SynergyRow {
            signature:   [S::ConvoyHardener, S::OreGrader],
            aligned:     [S::SalvageOperator, S::CoolantJockey],
            off_profile: [S::LaborRelations, S::BlackMarketAuger],
        },
        CommanderOrigin::HumanUnderhive => SynergyRow {
            signature:   [S::BlackMarketAuger, S::RushDriller],
            aligned:     [S::SalvageOperator, S::LaborRelations],
            off_profile: [S::VeinReader, S::OreGrader],
        },
        CommanderOrigin::Bearocrat => SynergyRow {
            signature:   [S::LaborRelations, S::OreGrader],
            aligned:     [S::ConvoyHardener, S::SalvageOperator],
            off_profile: [S::BlackMarketAuger, S::RushDriller],
        },
        CommanderOrigin::ScienceApe => SynergyRow {
            signature:   [S::VeinReader, S::MagmaTapper],
            aligned:     [S::TectonicWhisperer, S::CoolantJockey],
            off_profile: [S::LaborRelations, S::CrewPsychologist],
        },
    }
}

/// Table B, Foreman pool (§3c). Shared flaws score as in the Commander pool.
pub fn foreman_flaw_severity(flaw: ForemanFlaw) -> u8 {
    match flaw {
        ForemanFlaw::Superstitious   => 32,
        ForemanFlaw::Doomsayer       => 30,
        ForemanFlaw::CultOfBob       => 26,
        ForemanFlaw::SqueakTouched   => 24,
        ForemanFlaw::WasteNot        => 22,
        ForemanFlaw::CaveShy         => 20,
        ForemanFlaw::ProspectorsItch => 18,
        ForemanFlaw::DeepEar         => 14,
        ForemanFlaw::HighGrader      => 12,
        ForemanFlaw::SeamFever       => 8,
    }
}

/// Table C, Foreman Coherent pairings (§3d).
pub const FOREMAN_COHERENT: &[(ForemanFlaw, ForemanSpecialization)] = &[
    (ForemanFlaw::SeamFever,     ForemanSpecialization::RushDriller),
    (ForemanFlaw::SeamFever,     ForemanSpecialization::MagmaTapper),
    (ForemanFlaw::Superstitious, ForemanSpecialization::TectonicWhisperer),
    (ForemanFlaw::Superstitious, ForemanSpecialization::VeinReader),
    (ForemanFlaw::CaveShy,       ForemanSpecialization::ShoringExpert),
    (ForemanFlaw::WasteNot,      ForemanSpecialization::SalvageOperator),
    (ForemanFlaw::DeepEar,       ForemanSpecialization::TectonicWhisperer),
    (ForemanFlaw::Doomsayer,     ForemanSpecialization::CrewPsychologist),
    (ForemanFlaw::HighGrader,    ForemanSpecialization::BlackMarketAuger),
    (ForemanFlaw::Superstitious, ForemanSpecialization::DeepDriller),
    (ForemanFlaw::Doomsayer,     ForemanSpecialization::ConvoyHardener),
];

/// Table C, Foreman Contradictory pairings (§3d).
pub const FOREMAN_CONTRADICTORY: &[(ForemanFlaw, ForemanSpecialization)] = &[
    (ForemanFlaw::CaveShy,         ForemanSpecialization::RushDriller),
    (ForemanFlaw::CaveShy,         ForemanSpecialization::MagmaTapper),
    (ForemanFlaw::SeamFever,       ForemanSpecialization::ShoringExpert),
    (ForemanFlaw::HighGrader,      ForemanSpecialization::OreGrader),
    (ForemanFlaw::CultOfBob,       ForemanSpecialization::ConvoyHardener),
    (ForemanFlaw::ProspectorsItch, ForemanSpecialization::VeinReader),
];

// ── Scoring ──────────────────────────────────────────────────────────────────

/// A Commander's TQS (0–100).
pub fn commander_tqs(origin: CommanderOrigin, spec: CommanderSpecialization, flaw: CommanderFlaw) -> u8 {
    synergy(&commander_synergy_row(origin), spec)
        + commander_flaw_severity(flaw)
        + coherence(COMMANDER_COHERENT, COMMANDER_CONTRADICTORY, flaw, spec)
}

/// A Foreman's TQS (0–100).
pub fn foreman_tqs(origin: CommanderOrigin, spec: ForemanSpecialization, flaw: ForemanFlaw) -> u8 {
    synergy(&foreman_synergy_row(origin), spec)
        + foreman_flaw_severity(flaw)
        + coherence(FOREMAN_COHERENT, FOREMAN_CONTRADICTORY, flaw, spec)
}

impl CommanderGrade {
    /// The Grade band for a TQS (§3a): 0–39 / 40–59 / 60–74 / 75–89 / 90–100.
    pub fn from_tqs(tqs: u8) -> CommanderGrade {
        match tqs {
            0..=39  => CommanderGrade::StandardIssue,
            40..=59 => CommanderGrade::Notable,
            60..=74 => CommanderGrade::Distinguished,
            75..=89 => CommanderGrade::Elite,
            _       => CommanderGrade::Heroic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Per-grade counts in `CommanderGrade::ALL` order, plus every Heroic combination.
    fn enumerate<S: Copy, F: Copy>(
        specs: &[S],
        flaws: &[F],
        tqs: fn(CommanderOrigin, S, F) -> u8,
    ) -> ([usize; 5], Vec<(CommanderOrigin, S, F, u8)>) {
        let mut counts = [0usize; 5];
        let mut heroic = Vec::new();
        for origin in CommanderOrigin::ALL {
            for &spec in specs {
                for &flaw in flaws {
                    let t = tqs(origin, spec, flaw);
                    assert!(t <= 100, "TQS {t} above 100");
                    let grade = CommanderGrade::from_tqs(t);
                    counts[CommanderGrade::ALL.iter().position(|&g| g == grade).unwrap()] += 1;
                    if grade == CommanderGrade::Heroic {
                        heroic.push((origin, spec, flaw, t));
                    }
                }
            }
        }
        (counts, heroic)
    }

    #[test]
    fn grade_bands_at_every_boundary() {
        let cases = [
            (0, CommanderGrade::StandardIssue),
            (39, CommanderGrade::StandardIssue),
            (40, CommanderGrade::Notable),
            (59, CommanderGrade::Notable),
            (60, CommanderGrade::Distinguished),
            (74, CommanderGrade::Distinguished),
            (75, CommanderGrade::Elite),
            (89, CommanderGrade::Elite),
            (90, CommanderGrade::Heroic),
            (100, CommanderGrade::Heroic),
        ];
        for (tqs, grade) in cases {
            assert_eq!(CommanderGrade::from_tqs(tqs), grade, "TQS {tqs}");
        }
    }

    /// §3e: exactly four Commander combinations reach Heroic. The count vector pins the
    /// whole flat distribution, so any table edit shows up here.
    #[test]
    fn commander_enumeration_yields_the_four_heroic_archetypes() {
        let (counts, heroic) = enumerate(&CommanderSpecialization::ALL, &CommanderFlaw::ALL, commander_tqs);
        println!("Commander per-grade counts (of 720): {counts:?}");
        assert_eq!(counts.iter().sum::<usize>(), 720);
        let got: HashSet<_> = heroic.into_iter().collect();
        let want: HashSet<_> = [
            (CommanderOrigin::HumanUnderhive, CommanderSpecialization::NightStalker, CommanderFlaw::Paranoid, 92),
            (CommanderOrigin::HumanCorporate, CommanderSpecialization::QuartermastersGhost, CommanderFlaw::Paranoid, 92),
            (CommanderOrigin::Bearocrat, CommanderSpecialization::QuartermastersGhost, CommanderFlaw::Paranoid, 92),
            (CommanderOrigin::ScienceApe, CommanderSpecialization::BattlefieldEngineer, CommanderFlaw::Doomsayer, 90),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want, "Commander Heroic archetypes");
        assert_eq!(counts, [62, 411, 204, 39, 4], "Commander per-grade counts");
    }

    /// §3e: exactly three Foreman combinations reach Heroic.
    #[test]
    fn foreman_enumeration_yields_the_three_heroic_archetypes() {
        let (counts, heroic) = enumerate(&ForemanSpecialization::ALL, &ForemanFlaw::ALL, foreman_tqs);
        println!("Foreman per-grade counts (of 650): {counts:?}");
        assert_eq!(counts.iter().sum::<usize>(), 650);
        let got: HashSet<_> = heroic.into_iter().collect();
        let want: HashSet<_> = [
            (CommanderOrigin::ScienceApe, ForemanSpecialization::VeinReader, ForemanFlaw::Superstitious, 92),
            (CommanderOrigin::Duck, ForemanSpecialization::DeepDriller, ForemanFlaw::Superstitious, 92),
            (CommanderOrigin::HumanCorporate, ForemanSpecialization::ConvoyHardener, ForemanFlaw::Doomsayer, 90),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want, "Foreman Heroic archetypes");
        assert_eq!(counts, [35, 367, 205, 40, 3], "Foreman per-grade counts");
    }

    fn assert_rows_well_formed<S: Copy + PartialEq + Eq + std::hash::Hash + std::fmt::Debug>(
        class: &str,
        row: fn(CommanderOrigin) -> SynergyRow<S>,
    ) {
        for origin in CommanderOrigin::ALL {
            let r = row(origin);
            let cells: HashSet<S> = r.signature.iter().chain(&r.aligned).chain(&r.off_profile).copied().collect();
            assert_eq!(cells.len(), 6, "{class} {origin:?}: Table A row must be 2/2/2 and disjoint");
        }
    }

    fn assert_pairs_well_formed<F: Copy + Eq + std::hash::Hash + std::fmt::Debug, S: Copy + Eq + std::hash::Hash + std::fmt::Debug>(
        class: &str,
        coherent: &[(F, S)],
        contradictory: &[(F, S)],
    ) {
        let coh: HashSet<_> = coherent.iter().copied().collect();
        let con: HashSet<_> = contradictory.iter().copied().collect();
        assert_eq!(coh.len(), coherent.len(), "{class}: duplicate Coherent pair");
        assert_eq!(con.len(), contradictory.len(), "{class}: duplicate Contradictory pair");
        assert!(coh.is_disjoint(&con), "{class}: a pair is both Coherent and Contradictory");
    }

    #[test]
    fn grade_tables_are_well_formed() {
        assert_rows_well_formed("Commander", commander_synergy_row);
        assert_rows_well_formed("Foreman", foreman_synergy_row);
        assert_pairs_well_formed("Commander", COMMANDER_COHERENT, COMMANDER_CONTRADICTORY);
        assert_pairs_well_formed("Foreman", FOREMAN_COHERENT, FOREMAN_CONTRADICTORY);

        // Table B covers every flaw exactly once: the match tables are exhaustive by
        // construction; every score is in range.
        for f in CommanderFlaw::ALL {
            assert!(commander_flaw_severity(f) <= 40, "{f:?}");
        }
        for f in ForemanFlaw::ALL {
            assert!(foreman_flaw_severity(f) <= 40, "{f:?}");
        }

        // §3a: a shared flaw carries the same Severity in both pools.
        let shared = [
            (CommanderFlaw::Doomsayer, ForemanFlaw::Doomsayer, 30),
            (CommanderFlaw::CultOfBob, ForemanFlaw::CultOfBob, 26),
            (CommanderFlaw::SqueakTouched, ForemanFlaw::SqueakTouched, 24),
        ];
        for (c, f, score) in shared {
            assert_eq!(c.as_str(), f.as_str(), "shared flaw strings");
            assert_eq!(commander_flaw_severity(c), score, "{c:?} Commander severity");
            assert_eq!(foreman_flaw_severity(f), score, "{f:?} Foreman severity");
        }
        // No other flaw string is shared between the pools.
        let foreman: HashSet<&str> = ForemanFlaw::ALL.iter().map(|f| f.as_str()).collect();
        let overlap = CommanderFlaw::ALL.iter().filter(|c| foreman.contains(c.as_str())).count();
        assert_eq!(overlap, shared.len(), "unexpected shared flaw");
    }
}
