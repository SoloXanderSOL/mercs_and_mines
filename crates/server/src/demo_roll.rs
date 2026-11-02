//! The Breakpoint demo's roll rows (BP-14a, §4e rank 4, B5; **demo-only**). The approach the
//! player picks against the mission's stance, and the record Commander's rank, Specialisation
//! and Fatal Flaw, as flat points on the mission roll, added by
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
use sim_engine::game_types::{MissionCategory, MissionDefinition, RollModifier};
use sim_engine::missions::missions;
use sim_engine::roster::{CommanderFlaw, CommanderSpecialization};

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

/// The record Commander's roll inputs as they were at fight time (BP-14a), logged in
/// `session_start` as `roll_inputs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollInputs {
    pub rank: i16,
    pub specialization: CommanderSpecialization,
    pub fatal_flaw: CommanderFlaw,
}

impl RollInputs {
    /// `None` for a rank outside 1..=5 or a trait off the roster (the route answers `500`).
    pub fn from_record(rec: &CommanderRecord) -> Option<Self> {
        Self::parse(rec.rank as i64, &rec.specialization, &rec.fatal_flaw)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "rank": self.rank,
            "specialization": self.specialization.as_str(),
            "fatal_flaw": self.fatal_flaw.as_str(),
        })
    }

    /// The logged form back. `None` for anything `to_json` can't have written.
    pub fn from_json(v: &Value) -> Option<Self> {
        Self::parse(v["rank"].as_i64()?, v["specialization"].as_str()?, v["fatal_flaw"].as_str()?)
    }

    fn parse(rank: i64, specialization: &str, fatal_flaw: &str) -> Option<Self> {
        if !(1..=5).contains(&rank) {
            return None;
        }
        Some(Self {
            rank: rank as i16,
            specialization: CommanderSpecialization::from_db_str(specialization)?,
            fatal_flaw: CommanderFlaw::from_db_str(fatal_flaw)?,
        })
    }
}

/// The fight's rows, in a fixed order (approach, rank, Specialisation, Flaw); a 0-point row is
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
        let spec = if spec_suits(i.specialization, &mission.category) { 5 } else { 0 };
        push("specialization", format!("{}: suits this job", i.specialization.as_str()), spec);
        let flaw = flaw_points(i.fatal_flaw, approach, approach_row, mission.difficulty);
        push("fatal_flaw", format!("{}: {}", i.fatal_flaw.as_str(), flaw_line(i.fatal_flaw, flaw)), flaw);
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
        let i = RollInputs { rank: 3, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid };
        let got = roll_modifiers(Some(&i), Some(A::Flank), mission("GUARD_DUTY"));
        assert_eq!(rows(&got), [("approach", -15), ("commander_rank", 4), ("specialization", 5), ("fatal_flaw", -10)]);
        let labels: Vec<&str> = got.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, [
            "Approach: Flank against an enemy on the attack",
            "Commander rank 3",
            "Cannoneer: suits this job",
            "Paranoid: won't split the squad",
        ]);

        let i = RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Embezzler };
        assert_eq!(rows(&roll_modifiers(Some(&i), None, mission("SUPPLY_RAID"))), [("fatal_flaw", -3)]);
        let i = RollInputs { rank: 2, specialization: CommanderSpecialization::Breacher, fatal_flaw: F::TheGambler };
        assert_eq!(rows(&roll_modifiers(Some(&i), Some(A::Flank), mission("BLACK_SITE_RAID"))),
                   [("approach", 15), ("commander_rank", 2), ("specialization", 5), ("fatal_flaw", 15)]);
        assert_eq!(rows(&roll_modifiers(None, Some(A::DigIn), mission("GUARD_DUTY"))), [("approach", 15)]);
        assert!(roll_modifiers(None, None, mission("GUARD_DUTY")).is_empty());
    }

    #[test]
    fn roll_inputs_round_trip_and_refuse_what_was_never_written() {
        let rec = crate::commander_gen::generate_commander(vec![7; 32], uuid::Uuid::nil(), 42);
        let i = RollInputs::from_record(&rec).expect("a generated Commander is on the roster");
        assert_eq!(i, RollInputs { rank: 1, specialization: CommanderSpecialization::Cannoneer, fatal_flaw: F::Paranoid });
        assert_eq!(i.to_json(), json!({ "rank": 1, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }));
        assert_eq!(RollInputs::from_json(&i.to_json()), Some(i));
        for bad in [
            json!({ "rank": 0, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }),
            json!({ "rank": 6, "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }),
            json!({ "rank": "3", "specialization": "Cannoneer", "fatal_flaw": "Paranoid" }),
            json!({ "rank": 3, "specialization": "Pilot", "fatal_flaw": "Paranoid" }),
            json!({ "rank": 3, "specialization": "Cannoneer", "fatal_flaw": "Vain" }),
            json!({ "rank": 3, "specialization": "Cannoneer" }),
        ] {
            assert_eq!(RollInputs::from_json(&bad), None, "{bad}");
        }
    }
}
