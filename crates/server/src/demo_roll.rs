//! The Breakpoint demo's roll rows (BP-14a, §4e rank 4, B5; **demo-only**). The approach the
//! player picks against the mission's stance, and the record Commander's rank, Specialisation
//! and Fatal Flaw, and (BP-16) the Gunsmith at the merc base, as flat points on the mission
//! roll, added by
//! `resolve_mission_with_modifiers` before the 5–95 clamp. Breakpoint stand-ins: Section 8 owns
//! canon combat, and the `dev` port never picks this file.
//!
//! **Logged inputs, additive only.** `MissionApproach` rides in the logged
//! `MissionResolveRequest`, and `RollInputs` is logged in `session_start` as `roll_inputs`
//! (roster strings by `as_str()`), so `replay_report` rebuilds the same rows. Variants are
//! appended, never renamed; fields are never removed. The points themselves are never logged:
//! they are derived (the recomputability rule).
//!
//! Not `sim_engine::scenario::Approach`: that is the combat engine's canon approach, a
//! different system. Write `MissionApproach::Assault` fully qualified beside
//! `MissionCategory::Assault`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sim_engine::game_types::{MissionCategory, MissionDefinition, QualityGrade, RollModifier};
use sim_engine::missions::missions;
use sim_engine::roster::{CommanderFlaw, CommanderSpecialization};

use crate::demo_ops::{stress_band, StressBand};
use crate::repository::commander::CommanderRecord;

/// The player's approach on the briefing (BP-14a). On the wire: `"Assault" | "Flank" | "DigIn"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionApproach {
    Assault,
    Flank,
    DigIn,
}

impl MissionApproach {
    pub const ALL: [Self; 3] = [Self::Assault, Self::Flank, Self::DigIn];

    fn label(self) -> &'static str {
        match self {
            Self::Assault => "Assault",
            Self::Flank => "Flank",
            Self::DigIn => "Dig in",
        }
    }
}

/// The enemy's posture, hinted truthfully on the briefing. On the wire (`/api/missions`'
/// `stance`): `"DugIn" | "Aggressive" | "SpreadOut"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Stance {
    DugIn,
    Aggressive,
    SpreadOut,
}

impl Stance {
    pub const ALL: [Self; 3] = [Self::DugIn, Self::Aggressive, Self::SpreadOut];

    fn phrase(self) -> &'static str {
        match self {
            Self::DugIn => "a dug-in enemy",
            Self::Aggressive => "an enemy on the attack",
            Self::SpreadOut => "a spread-out enemy",
        }
    }
}

/// One stance per mission, by category, with one override: a supply convoy is on the move.
pub fn mission_stance(mission: &MissionDefinition) -> Stance {
    if mission.id == "SUPPLY_RAID" {
        return Stance::SpreadOut;
    }
    match mission.category {
        MissionCategory::Defense => Stance::Aggressive,
        MissionCategory::Assault => Stance::DugIn,
        MissionCategory::Escort
        | MissionCategory::Extermination
        | MissionCategory::Sabotage
        | MissionCategory::Extraction => Stance::SpreadOut,
    }
}

/// The square: each approach wins once (+15), loses once (−15) and draws once (0).
pub fn approach_points(approach: MissionApproach, stance: Stance) -> i32 {
    use MissionApproach as A;
    use Stance as S;
    match (approach, stance) {
        (A::Assault, S::DugIn) => -15,
        (A::Assault, S::Aggressive) => 0,
        (A::Assault, S::SpreadOut) => 15,
        (A::Flank, S::DugIn) => 15,
        (A::Flank, S::Aggressive) => -15,
        (A::Flank, S::SpreadOut) => 0,
        (A::DigIn, S::DugIn) => 0,
        (A::DigIn, S::Aggressive) => 15,
        (A::DigIn, S::SpreadOut) => -15,
    }
}

/// +2 per rank above 1.
pub fn rank_points(rank: i16) -> i32 {
    2 * (rank as i32 - 1)
}

/// The one mission category each Specialisation suits (+5 there, nothing elsewhere).
fn spec_category(spec: CommanderSpecialization) -> MissionCategory {
    use CommanderSpecialization as S;
    match spec {
        S::TrenchSweeper | S::Breacher | S::AntiArmorAdept | S::SuppressionDoctrine => MissionCategory::Assault,
        S::ArmorAdept | S::Cannoneer | S::MarksmansEye | S::BattlefieldEngineer => MissionCategory::Defense,
        S::NightStalker | S::QuartermastersGhost | S::CorpsemansTouch | S::VeteransInstinct => MissionCategory::Extraction,
    }
}

pub fn spec_suits(spec: CommanderSpecialization, category: &MissionCategory) -> bool {
    std::mem::discriminant(&spec_category(spec)) == std::mem::discriminant(category)
}

/// The Flaw's points. Several key on the approach, so the Commander can change which approach
/// is right; The Gambler doubles the approach row (`approach_row` is that row's points).
pub fn flaw_points(flaw: CommanderFlaw, approach: Option<MissionApproach>, approach_row: i32, difficulty: u8) -> i32 {
    use CommanderFlaw as F;
    use MissionApproach as A;
    match (flaw, approach) {
        (F::Embezzler, _) => -3,
        (F::Bloodthirsty, Some(A::Assault)) => 10,
        (F::Bloodthirsty, Some(A::DigIn)) => -10,
        (F::Cowardly, Some(A::DigIn)) => 10,
        (F::Cowardly, Some(A::Assault)) => -10,
        (F::Paranoid, Some(A::DigIn)) => 6,
        (F::Paranoid, Some(A::Flank)) => -10,
        (F::CultOfBob, _) => -2,
        (F::TheInformant, _) => -5,
        (F::GloryHound, _) if difficulty >= 7 => 5,
        (F::GloryHound, _) if difficulty <= 3 => -3,
        (F::TheGambler, _) => approach_row,
        (F::ChromeChaser, _) => 3,
        (F::Martinet, Some(A::DigIn)) => 10,
        (F::Martinet, Some(A::Flank)) => -6,
        (F::SqueakTouched, _) => 3,
        (F::Doomsayer, _) => -3,
        _ => 0,
    }
}

/// The Flaw row's words, by the sign of its points.
fn flaw_line(flaw: CommanderFlaw, points: i32) -> &'static str {
    use CommanderFlaw as F;
    let up = points > 0;
    match flaw {
        F::Embezzler => "sold the good ammo",
        F::Bloodthirsty => if up { "loves a charge" } else { "hates sitting still" },
        F::Cowardly => if up { "happy behind a wall" } else { "not keen on charging" },
        F::Paranoid => if up { "likes a wall at their back" } else { "won't split the squad" },
        F::CultOfBob => "stopped to bless the rifles",
        F::TheInformant => "the other side was warned",
        F::GloryHound => if up { "smells glory" } else { "bored by small jobs" },
        F::TheGambler => "doubled down",
        F::ChromeChaser => "the chrome works, mostly",
        F::Martinet => if up { "drilled to hold a line" } else { "flanking isn't in the manual" },
        F::SqueakTouched => "the hamsters sent word",
        F::Doomsayer => "the squad believes the prophecies",
    }
}

/// The Gunsmith's points on the roll (BP-16, §4e C1: mercs hit harder), last of the rows.
pub const DEMO_GUNSMITH_ROLL_POINTS: i32 = 10;

/// The stress row while Strained (BP-19b): Command Presence at half, on the roll.
pub const DEMO_STRAINED_ROLL_CUT: i32 = 3;
/// The stress row from Breaking Point (BP-19b): Command Presence inverted.
pub const DEMO_BREAKING_ROLL_CUT: i32 = 10;

/// An armoury item's grade as the roll sees it (BP-23a, §4e C6): the canon `QualityGrade`'s five
/// steps, demo-local so `RollInputs` stays `Copy` and `Eq` (Linux D1). Logged by its canon name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitGrade {
    Basic,
    Standard,
    Specialized,
    Superior,
    Elite,
}

impl KitGrade {
    pub fn from_quality(q: &QualityGrade) -> Self {
        match q {
            QualityGrade::Basic => Self::Basic,
            QualityGrade::Standard => Self::Standard,
            QualityGrade::Specialized => Self::Specialized,
            QualityGrade::Superior => Self::Superior,
            QualityGrade::Elite => Self::Elite,
        }
    }

    /// The canon name, as `QualityGrade` serialises it and as `roll_inputs` logs it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "Basic",
            Self::Standard => "Standard",
            Self::Specialized => "Specialized",
            Self::Superior => "Superior",
            Self::Elite => "Elite",
        }
    }

    /// `as_str` back; `None` for anything else.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "Basic" => Some(Self::Basic),
            "Standard" => Some(Self::Standard),
            "Specialized" => Some(Self::Specialized),
            "Superior" => Some(Self::Superior),
            "Elite" => Some(Self::Elite),
            _ => None,
        }
    }

    /// The kit's points on the roll (BP-23a, Wiki ruling): 4, 6, 8, 10, 12 by grade. About 85 % of
    /// the booth's drops are Basic or Standard (Linux check 11), so the floor is kept visible.
    pub fn points(self) -> i32 {
        match self {
            Self::Basic => 4,
            Self::Standard => 6,
            Self::Specialized => 8,
            Self::Superior => 10,
            Self::Elite => 12,
        }
    }
}

/// The record Commander's roll inputs as they were at fight time (BP-14a), logged in
/// `session_start` as `roll_inputs`. `gunsmith` (BP-16): whether the Gunsmith stood at the
/// campaign's base when the route read it; logged only when true (`"gunsmith": true`), so an
/// absent key, every log before BP-16, reads as no Gunsmith and replays unchanged.
/// `stress` (BP-19b): the record's stress at fight time, before this fight's +10; logged only
/// when above 0, so an absent key (every log before BP-19b) reads as 0, Rested, and replays
/// unchanged.
/// `armoury` (BP-23a): the grade of the kit the fight's leading Section (`section_ids[0]`, the
/// one the latch charges) held when the route read it; logged only when set
/// (`"armoury": "Basic"`), so an absent key, every log before BP-23a, reads as no kit and
/// replays unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollInputs {
    pub rank: i16,
    pub specialization: CommanderSpecialization,
    pub fatal_flaw: CommanderFlaw,
    pub gunsmith: bool,
    pub stress: i16,
    pub armoury: Option<KitGrade>,
}

impl RollInputs {
    /// `None` for a rank outside 1..=5 or a trait off the roster (the route answers `500`). The
    /// record knows nothing of the base or the armoury: `gunsmith` is false and `armoury` `None`,
    /// and the route sets both.
    pub fn from_record(rec: &CommanderRecord) -> Option<Self> {
        Self::parse(rec.rank as i64, &rec.specialization, &rec.fatal_flaw).map(|i| Self { stress: rec.stress, ..i })
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "rank": self.rank,
            "specialization": self.specialization.as_str(),
            "fatal_flaw": self.fatal_flaw.as_str(),
        });
        if self.gunsmith {
            v["gunsmith"] = json!(true);
        }
        if self.stress > 0 {
            v["stress"] = json!(self.stress);
        }
        if let Some(k) = self.armoury {
            v["armoury"] = json!(k.as_str());
        }
        v
    }

    /// The logged form back. `None` for a malformed log: a rank outside 1..=5, a trait off the
    /// roster, or a present optional key of the wrong type or range (`gunsmith` a bool,
    /// `stress` 0..=100, `armoury` a grade name). An absent optional key is its default: no
    /// Gunsmith (before BP-16), stress 0 (before BP-19b), no kit (before BP-23a).
    pub fn from_json(v: &Value) -> Option<Self> {
        let gunsmith = match v.get("gunsmith") {
            None => false,
            Some(g) => g.as_bool()?,
        };
        let stress = match v.get("stress") {
            None => 0,
            Some(s) => i16::try_from(s.as_i64()?).ok().filter(|s| (0..=100).contains(s))?,
        };
        let armoury = match v.get("armoury") {
            None => None,
            Some(a) => Some(KitGrade::from_name(a.as_str()?)?),
        };
        let inputs = Self::parse(v["rank"].as_i64()?, v["specialization"].as_str()?, v["fatal_flaw"].as_str()?)?;
        Some(Self { gunsmith, stress, armoury, ..inputs })
    }

    fn parse(rank: i64, specialization: &str, fatal_flaw: &str) -> Option<Self> {
        if !(1..=5).contains(&rank) {
            return None;
        }
        Some(Self {
            rank: rank as i16,
            specialization: CommanderSpecialization::from_db_str(specialization)?,
            fatal_flaw: CommanderFlaw::from_db_str(fatal_flaw)?,
            gunsmith: false,
            stress: 0,
            armoury: None,
        })
    }
}

/// The fight's rows, in a fixed order (approach, rank, Specialisation, stress (BP-19b), Flaw,
/// then the Gunsmith, BP-16, then the armoury kit, BP-23a); a 0-point row is
/// left out. `inputs` is `None` for a fight logged before BP-14a: it resolved with no rows.
pub fn roll_modifiers(inputs: Option<&RollInputs>, approach: Option<MissionApproach>, mission: &MissionDefinition) -> Vec<RollModifier> {
    let mut rows = Vec::new();
    let mut push = |key: &str, label: String, points: i32| {
        if points != 0 {
            rows.push(RollModifier { key: key.to_string(), label, points });
        }
    };
    let stance = mission_stance(mission);
    let approach_row = approach.map_or(0, |a| approach_points(a, stance));
    if let Some(a) = approach {
        push("approach", format!("Approach: {} against {}", a.label(), stance.phrase()), approach_row);
    }
    if let Some(i) = inputs {
        push("commander_rank", format!("Commander rank {}", i.rank), rank_points(i.rank));
        let band = stress_band(i.stress, false);
        let (spec_points, spec_label) = match band {
            StressBand::Rested => (5, "suits this job"),
            StressBand::Strained => (2, "suits this job, at half while Strained"),
            StressBand::BreakingPoint | StressBand::Shattered => (0, "suits this job"),
        };
        let spec = if spec_suits(i.specialization, &mission.category) { spec_points } else { 0 };
        push("specialization", format!("{}: {}", i.specialization.as_str(), spec_label), spec);
        let (stress_points, stress_word) = match band {
            StressBand::Rested => (0, "Rested"),
            StressBand::Strained => (-DEMO_STRAINED_ROLL_CUT, "Strained"),
            StressBand::BreakingPoint | StressBand::Shattered => (-DEMO_BREAKING_ROLL_CUT, "Breaking Point"),
        };
        push("stress", format!("Commander stress {}: {}", i.stress, stress_word), stress_points);
        let flaw = flaw_points(i.fatal_flaw, approach, approach_row, mission.difficulty);
        push("fatal_flaw", format!("{}: {}", i.fatal_flaw.as_str(), flaw_line(i.fatal_flaw, flaw)), flaw);
        let gunsmith = if i.gunsmith { DEMO_GUNSMITH_ROLL_POINTS } else { 0 };
        push("gunsmith", "Gunsmith (your base)".to_string(), gunsmith);
        if let Some(k) = i.armoury {
            push("armoury", format!("Armoury kit ({})", k.as_str()), k.points());
        }
    }
    rows
}

/// Adds each mission's `stance` to `/api/missions`' body (an object keyed by mission id), for
/// the briefing's hint. An entry with no matching mission is left as it is.
pub fn add_stances(body: &mut Value) {
    for (id, mission) in missions() {
        if let Some(entry) = body.get_mut(*id) {
            entry["stance"] = json!(mission_stance(mission));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CommanderFlaw as F;
    use MissionApproach as A;

    fn mission(id: &str) -> &'static MissionDefinition {
        missions().get(id).expect("mission")
    }

    fn rows(m: &[RollModifier]) -> Vec<(&str, i32)> {
        m.iter().map(|r| (r.key.as_str(), r.points)).collect()
    }

    #[test]
    fn approach_square_pins_every_cell_and_each_approach_wins_once() {
        let want = [
            (A::Assault, Stance::DugIn, -15), (A::Assault, Stance::Aggressive, 0), (A::Assault, Stance::SpreadOut, 15),
            (A::Flank, Stance::DugIn, 15), (A::Flank, Stance::Aggressive, -15), (A::Flank, Stance::SpreadOut, 0),
            (A::DigIn, Stance::DugIn, 0), (A::DigIn, Stance::Aggressive, 15), (A::DigIn, Stance::SpreadOut, -15),
        ];
        for (a, s, p) in want {
            assert_eq!(approach_points(a, s), p, "{a:?} against {s:?}");
        }
        for a in MissionApproach::ALL {
            let mut v: Vec<i32> = Stance::ALL.iter().map(|&s| approach_points(a, s)).collect();
            v.sort();
            assert_eq!(v, [-15, 0, 15], "{a:?}");
        }
        for s in Stance::ALL {
            let mut v: Vec<i32> = MissionApproach::ALL.iter().map(|&a| approach_points(a, s)).collect();
            v.sort();
            assert_eq!(v, [-15, 0, 15], "{s:?}");
        }
    }

    #[test]
    fn every_mission_has_its_stance() {
        let want = [
            ("GUARD_DUTY", Stance::Aggressive), ("RECON_RUN", Stance::SpreadOut), ("ORE_EXTRACTION", Stance::SpreadOut),
            ("SUPPLY_RAID", Stance::SpreadOut), ("INDUSTRIAL_SABOTAGE", Stance::SpreadOut), ("DEEP_CORE_SURVEY", Stance::SpreadOut),
            ("ASSASSINATION", Stance::DugIn), ("CRASHED_SHIP_RAID", Stance::SpreadOut), ("ORE_HEIST", Stance::DugIn),
            ("BLACK_SITE_RAID", Stance::DugIn), ("SECTOR_9_ASSAULT", Stance::DugIn),
        ];
        assert_eq!(missions().len(), want.len());
        for (id, s) in want {
            assert_eq!(mission_stance(mission(id)), s, "{id}");
        }
        let mut body = serde_json::to_value(missions()).expect("missions serialise");
        add_stances(&mut body);
        assert_eq!(body["GUARD_DUTY"]["stance"], "Aggressive");
        assert_eq!(body["BLACK_SITE_RAID"]["stance"], "DugIn");
        assert_eq!(body["SUPPLY_RAID"]["stance"], "SpreadOut");
        assert!(body.as_object().expect("an object").values().all(|e| e["stance"].is_string()));
    }

    #[test]
    fn each_specialization_suits_exactly_its_one_category() {
        use CommanderSpecialization as S;
        let cats = [
            MissionCategory::Assault, MissionCategory::Defense, MissionCategory::Escort,
            MissionCategory::Extermination, MissionCategory::Sabotage, MissionCategory::Extraction,
        ];
        let want = [
            (S::TrenchSweeper, 0), (S::ArmorAdept, 1), (S::NightStalker, 5), (S::Cannoneer, 1),
            (S::MarksmansEye, 1), (S::Breacher, 0), (S::QuartermastersGhost, 5), (S::CorpsemansTouch, 5),
            (S::SuppressionDoctrine, 0), (S::VeteransInstinct, 5), (S::AntiArmorAdept, 0), (S::BattlefieldEngineer, 1),
        ];
        assert_eq!(want.len(), CommanderSpecialization::ALL.len());
        for (spec, at) in want {
            for (i, c) in cats.iter().enumerate() {
                assert_eq!(spec_suits(spec, c), i == at, "{spec:?} on {c:?}");
            }
        }
    }

    /// Every Flaw on Supply Raid (spread out, difficulty 5: the approach rows are +15 / 0 / −15),
    /// with no approach and with each one; then Glory Hound on the other difficulties.
    #[test]
    fn flaw_table_pins_every_flaw_against_every_approach() {
        let m = mission("SUPPLY_RAID");
        //            None, Assault, Flank, DigIn
        let want = [
            (F::Embezzler, [-3, -3, -3, -3]),
            (F::Bloodthirsty, [0, 10, 0, -10]),
            (F::Cowardly, [0, -10, 0, 10]),
            (F::Paranoid, [0, 0, -10, 6]),
            (F::CultOfBob, [-2, -2, -2, -2]),
            (F::TheInformant, [-5, -5, -5, -5]),
            (F::GloryHound, [0, 0, 0, 0]),
            (F::TheGambler, [0, 15, 0, -15]),
            (F::ChromeChaser, [3, 3, 3, 3]),
            (F::Martinet, [0, 0, -6, 10]),
            (F::SqueakTouched, [3, 3, 3, 3]),
            (F::Doomsayer, [-3, -3, -3, -3]),
        ];
        assert_eq!(want.len(), CommanderFlaw::ALL.len());
        let approaches = [None, Some(A::Assault), Some(A::Flank), Some(A::DigIn)];
        for (flaw, points) in want {
            for (a, p) in approaches.iter().zip(points) {
                let row = a.map_or(0, |a| approach_points(a, mission_stance(m)));
                assert_eq!(flaw_points(flaw, *a, row, m.difficulty), p, "{flaw:?} with {a:?}");
            }
        }
        for (id, p) in [("GUARD_DUTY", -3), ("CRASHED_SHIP_RAID", 5), ("BLACK_SITE_RAID", 5)] {
            assert_eq!(flaw_points(F::GloryHound, None, 0, mission(id).difficulty), p, "{id}");
        }
    }

    /// The route test's Commander (seed 42: Cannoneer, Paranoid) at rank 3, flanking on Guard
    /// Duty: every row, with its words. Then the rows that drop out: rank 1, a Specialisation off
    /// its category, no approach, and a fight logged before BP-14a.
    #[test]
    fn roll_modifiers_build_the_rows_in_order() {
        let i = RollInputs { rank: 3, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid, gunsmith: false, stress: 0, armoury: None };
        let got = roll_modifiers(Some(&i), Some(A::Flank), mission("GUARD_DUTY"));
        assert_eq!(rows(&got), [("approach", -15), ("commander_rank", 4), ("specialization", 5), ("fatal_flaw", -10)]);
        let labels: Vec<&str> = got.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, [
            "Approach: Flank against an enemy on the attack",
            "Commander rank 3",
            "Cannoneer: suits this job",
            "Paranoid: won't split the squad",
        ]);
        let g = roll_modifiers(Some(&RollInputs { gunsmith: true, ..i }), Some(A::Flank), mission("GUARD_DUTY"));
        assert_eq!(rows(&g), [("approach", -15), ("commander_rank", 4), ("specialization", 5), ("fatal_flaw", -10), ("gunsmith", 10)]);
        assert_eq!(g[4].label, "Gunsmith (your base)");

        let i = RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Embezzler, gunsmith: false, stress: 0, armoury: None };
        assert_eq!(rows(&roll_modifiers(Some(&i), None, mission("SUPPLY_RAID"))), [("fatal_flaw", -3)]);
        let i = RollInputs { rank: 2, specialization: CommanderSpecialization::Breacher, fatal_flaw: F::TheGambler, gunsmith: false, stress: 0, armoury: None };
        assert_eq!(rows(&roll_modifiers(Some(&i), Some(A::Flank), mission("BLACK_SITE_RAID"))),
                   [("approach", 15), ("commander_rank", 2), ("specialization", 5), ("fatal_flaw", 15)]);
        assert_eq!(rows(&roll_modifiers(None, Some(A::DigIn), mission("GUARD_DUTY"))), [("approach", 15)]);
        assert!(roll_modifiers(None, None, mission("GUARD_DUTY")).is_empty());
    }

    #[test]
    fn roll_inputs_round_trip_and_refuse_what_was_never_written() {
        let rec = crate::commander_gen::generate_commander(vec![7; 32], uuid::Uuid::nil(), 42);
        let i = RollInputs::from_record(&rec).expect("a generated Commander is on the roster");
        assert_eq!(i, RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid, gunsmith: false, stress: 0, armoury: None });
        assert_eq!(i.to_json(), json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }));
        assert_eq!(RollInputs::from_json(&i.to_json()), Some(i), "a log from before BP-16 has no key");
        let g = RollInputs { gunsmith: true, ..i };
        assert_eq!(g.to_json(), json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "gunsmith": true }));
        assert_eq!(RollInputs::from_json(&g.to_json()), Some(g));
        assert_eq!(RollInputs::from_json(&json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "gunsmith": false })), Some(i));
        for bad in [
            json!({ "rank": 0, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }),
            json!({ "rank": 6, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }),
            json!({ "rank": "3", "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }),
            json!({ "rank": 3, "specialization": "Pilot", "fatal_flaw": "Paranoid" }),
            json!({ "rank": 3, "specialization": "Cannoneer", "fatal_flaw": "Vain" }),
            json!({ "rank": 3, "specialization": "Cannoneer" }),
            json!({ "rank": 3, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "gunsmith": 1 }),
        ] {
            assert_eq!(RollInputs::from_json(&bad), None, "{bad}");
        }
    }

    /// Y1 (BP-19b): the Specialisation and stress rows by band, at the cuts 30/31, 70/71 and 99,
    /// on Guard Duty with a Cannoneer (suits it), flanking at rank 3 as in the rows test: Rested
    /// +5 and no stress row; Strained +2 and −3; from Breaking Point no Specialisation row and
    /// −10. The stress row sits right after Specialisation (after rank when that is omitted). An
    /// unsuited mission at 40 shows the stress row alone.
    #[test]
    fn stress_rows_follow_the_band() {
        let base = RollInputs { rank: 3, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid, gunsmith: false, stress: 0, armoury: None };
        let at = |stress: i16| roll_modifiers(Some(&RollInputs { stress, ..base }), Some(A::Flank), mission("GUARD_DUTY"));
        assert_eq!(rows(&at(30)), [("approach", -15), ("commander_rank", 4), ("specialization", 5), ("fatal_flaw", -10)]);
        for s in [31, 70] {
            let got = at(s);
            assert_eq!(rows(&got), [("approach", -15), ("commander_rank", 4), ("specialization", 2), ("stress", -3), ("fatal_flaw", -10)], "{s}");
            assert_eq!(got[2].label, "Cannoneer: suits this job, at half while Strained");
            assert_eq!(got[3].label, format!("Commander stress {s}: Strained"));
        }
        for s in [71, 99] {
            let got = at(s);
            assert_eq!(rows(&got), [("approach", -15), ("commander_rank", 4), ("stress", -10), ("fatal_flaw", -10)], "{s}");
            assert_eq!(got[2].label, format!("Commander stress {s}: Breaking Point"));
        }
        let i = RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Embezzler, gunsmith: false, stress: 40, armoury: None };
        assert_eq!(rows(&roll_modifiers(Some(&i), None, mission("SUPPLY_RAID"))), [("stress", -3), ("fatal_flaw", -3)]);
    }

    /// Y2 (BP-19b): `stress` is logged only above 0; an absent key reads as 0 with the same rows
    /// as an explicit 0; anything `to_json` can't write is refused.
    #[test]
    fn roll_inputs_stress_is_logged_only_above_0() {
        let i = RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid, gunsmith: false, stress: 0, armoury: None };
        assert!(i.to_json().get("stress").is_none());
        let s = RollInputs { stress: 40, ..i };
        assert_eq!(s.to_json()["stress"], json!(40));
        assert_eq!(RollInputs::from_json(&s.to_json()), Some(s));
        let absent = RollInputs::from_json(&json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" })).expect("absent");
        let zero = RollInputs::from_json(&json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "stress": 0 })).expect("zero");
        assert_eq!(absent.stress, 0);
        assert_eq!(roll_modifiers(Some(&absent), None, mission("GUARD_DUTY")), roll_modifiers(Some(&zero), None, mission("GUARD_DUTY")));
        for bad in [json!(101), json!(-1), json!("x")] {
            let v = json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "stress": bad });
            assert_eq!(RollInputs::from_json(&v), None, "{v}");
        }
    }

    /// BP-23a test 1: the armoury row comes last, after the Gunsmith, at 4/6/8/10/12 by grade
    /// (Wiki's ruling, pinned as literals), labelled with the canon grade name; no kit, no row.
    /// The logged name is `QualityGrade`'s own serde name for all five grades.
    #[test]
    fn armoury_row_is_last_and_scales_by_grade() {
        use sim_engine::game_types::QualityGrade as Q;
        let i = RollInputs { rank: 3, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid, gunsmith: true, stress: 0, armoury: None };
        let grades = [
            (KitGrade::Basic, 4, "Armoury kit (Basic)"),
            (KitGrade::Standard, 6, "Armoury kit (Standard)"),
            (KitGrade::Specialized, 8, "Armoury kit (Specialized)"),
            (KitGrade::Superior, 10, "Armoury kit (Superior)"),
            (KitGrade::Elite, 12, "Armoury kit (Elite)"),
        ];
        for (g, p, label) in grades {
            let got = roll_modifiers(Some(&RollInputs { armoury: Some(g), ..i }), Some(A::Flank), mission("GUARD_DUTY"));
            assert_eq!(rows(&got), [("approach", -15), ("commander_rank", 4), ("specialization", 5), ("fatal_flaw", -10), ("gunsmith", 10), ("armoury", p)], "{g:?}");
            assert_eq!(got[5].label, label);
        }
        let none = roll_modifiers(Some(&i), Some(A::Flank), mission("GUARD_DUTY"));
        assert!(none.iter().all(|r| r.key != "armoury"), "no kit, no row");
        for q in [Q::Basic, Q::Standard, Q::Specialized, Q::Superior, Q::Elite] {
            assert_eq!(json!(KitGrade::from_quality(&q).as_str()), serde_json::to_value(&q).expect("serialise"), "{q:?}");
        }
    }

    /// BP-23a test 2: `armoury` is logged only when set, by its grade name, and round-trips; an
    /// absent key reads as no kit; a present key that is not a grade name is a malformed log.
    #[test]
    fn roll_inputs_log_the_armoury_only_when_set() {
        let i = RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid, gunsmith: false, stress: 0, armoury: None };
        assert!(i.to_json().get("armoury").is_none());
        let e = RollInputs { armoury: Some(KitGrade::Elite), ..i };
        assert_eq!(e.to_json(), json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "armoury": "Elite" }));
        assert_eq!(RollInputs::from_json(&e.to_json()), Some(e));
        for bad in [json!("Shiny"), json!(3)] {
            let v = json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid", "armoury": bad });
            assert_eq!(RollInputs::from_json(&v), None, "{v}");
        }
    }
}
