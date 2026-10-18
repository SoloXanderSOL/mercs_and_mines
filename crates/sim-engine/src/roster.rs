//! Canonical Commander roster (Brick B; GAP-6, ruling R3).
//! Ref: Commander_and_Advisor_System.md (species roster, Faction Bias table, Sect Layer);
//!      Mercenary_Commander_System.md (Specializations, Fatal Flaws);
//!      Commander_Grade_System.md §2, §3b–§3c (Grades, Foreman pools).
//!
//! The single source of truth for Commander roster strings. Migration `0018`'s CHECK
//! constraints on `commander_records.origin` and `faction_bias` mirror `CommanderOrigin`
//! and `FactionBias`, and `0019`'s `commander_grade` CHECK mirrors `CommanderGrade`;
//! drift-guard tests write every value through them. The Foreman pools live here too:
//! they are graded (`crate::grade`) but have no generator until 7a-1. The Advisor pools
//! (7a-2) are ungraded. `CommanderOrigin` is the origin roster for all three classes
//! (Commanders, Foremen and Advisors), despite its name.
//!
//! No serde derive: the database boundary uses `as_str()`, and these types must never
//! become part of a logged payload by accident.

macro_rules! roster_enum {
    ($(#[$meta:meta])* $name:ident, $n:literal { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every variant, in canon table order.
            pub const ALL: [Self; $n] = [$(Self::$variant),+];

            /// The canonical display string, as stored in Postgres.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// Parses a stored string; `None` for anything off the roster.
            pub fn from_db_str(s: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|v| v.as_str() == s)
            }
        }
    };
}

roster_enum! {
    /// A Commander's species. The hamster attachment is a separate entity, not an origin,
    /// and Raccoons cannot command (the Raccoon Rule).
    CommanderOrigin, 5 {
        HumanCorporate => "Human (Corporate)",
        HumanUnderhive => "Human (Underhive)",
        Duck           => "Duck",
        Bearocrat      => "Bearocrat",
        ScienceApe     => "Science Ape",
    }
}

roster_enum! {
    /// A hidden faction alignment. Sect-tagged parents fold the sect into the string.
    FactionBias, 12 {
        GunLoyalist                 => "G.U.N. Loyalist",
        CapitalistPartisan          => "Capitalist Partisan",
        UnionAgitator               => "Union Agitator",
        SqueakingProphetSympathiser => "Squeaking Prophet Sympathiser",
        VoidCallerLiberationist     => "Void-Caller (Liberationist)",
        VoidCallerAbolitionist      => "Void-Caller (Abolitionist)",
        VoidCallerCollaborationist  => "Void-Caller (Collaborationist)",
        JumpLaneRestorer            => "Jump Lane Restorer",
        CyberkinInitiate            => "Cyberkin Initiate",
        EmperorBobWayOfTheRoad      => "Emperor Bob Devotee (Way of the Road)",
        EmperorBobCrownedFist       => "Emperor Bob Devotee (Crowned Fist)",
        TrustAsset                  => "Trust Asset",
    }
}

impl FactionBias {
    /// One group per canon parent bias, in table order. A sect is a tag on the roll, not a
    /// separate bias: the Void-Caller group holds its three sects, the Emperor Bob group its
    /// two, and every other group a single value.
    pub const GROUPS: [&'static [FactionBias]; 9] = [
        &[Self::GunLoyalist],
        &[Self::CapitalistPartisan],
        &[Self::UnionAgitator],
        &[Self::SqueakingProphetSympathiser],
        &[Self::VoidCallerLiberationist, Self::VoidCallerAbolitionist, Self::VoidCallerCollaborationist],
        &[Self::JumpLaneRestorer],
        &[Self::CyberkinInitiate],
        &[Self::EmperorBobWayOfTheRoad, Self::EmperorBobCrownedFist],
        &[Self::TrustAsset],
    ];
}

roster_enum! {
    /// A Commander's combat Specialization. Commanders never roll from the Foreman pool.
    CommanderSpecialization, 12 {
        TrenchSweeper        => "Trench-Sweeper",
        ArmorAdept           => "Armor-Adept",
        NightStalker         => "Night-Stalker",
        Cannoneer            => "Cannoneer",
        MarksmansEye         => "Marksman's Eye",
        Breacher             => "Breacher",
        QuartermastersGhost  => "Quartermaster's Ghost",
        CorpsemansTouch      => "Corpseman's Touch",
        SuppressionDoctrine  => "Suppression Doctrine",
        VeteransInstinct     => "Veteran's Instinct",
        AntiArmorAdept       => "Anti-Armor Adept",
        BattlefieldEngineer  => "Battlefield Engineer",
    }
}

roster_enum! {
    /// A Commander's Fatal Flaw (the 12-flaw pool, canon 2026-07-08).
    CommanderFlaw, 12 {
        Embezzler     => "Embezzler",
        Bloodthirsty  => "Bloodthirsty",
        Cowardly      => "Cowardly",
        Paranoid      => "Paranoid",
        CultOfBob     => "Cult of Bob",
        TheInformant  => "The Informant",
        GloryHound    => "Glory Hound",
        TheGambler    => "The Gambler",
        ChromeChaser  => "Chrome-Chaser",
        Martinet      => "Martinet",
        SqueakTouched => "Squeak-Touched",
        Doomsayer     => "Doomsayer",
    }
}

roster_enum! {
    /// A Field Leader's spawn Grade, read from the Trait Quality Score (Commander_Grade_System
    /// §2, §3a). Shares the bare name `Elite` with `QualityGrade`: always write the variants
    /// fully qualified.
    CommanderGrade, 5 {
        StandardIssue => "Standard Issue",
        Notable       => "Notable",
        Distinguished => "Distinguished",
        Elite         => "Elite",
        Heroic        => "Heroic",
    }
}

roster_enum! {
    /// A Mining Foreman's Specialization (the 13-spec pool, Commander_Grade_System §3b).
    ForemanSpecialization, 13 {
        DeepDriller       => "Deep-Driller",
        ShoringExpert     => "Shoring-Expert",
        LaborRelations    => "Labor Relations",
        TectonicWhisperer => "Tectonic Whisperer",
        BlackMarketAuger  => "Black Market Auger",
        ConvoyHardener    => "Convoy Hardener",
        OreGrader         => "Ore Grader",
        SalvageOperator   => "Salvage Operator",
        CoolantJockey     => "Coolant-Jockey",
        RushDriller       => "Rush-Driller",
        VeinReader        => "Vein-Reader",
        MagmaTapper       => "Magma Tapper",
        CrewPsychologist  => "Crew Psychologist",
    }
}

roster_enum! {
    /// A Mining Foreman's Fatal Flaw (the 10-flaw pool, Commander_Grade_System §3c). Doomsayer,
    /// Cult of Bob and Squeak-Touched are shared with `CommanderFlaw` under the same strings.
    ForemanFlaw, 10 {
        Superstitious    => "Superstitious",
        Doomsayer        => "Doomsayer",
        CultOfBob        => "Cult of Bob",
        SqueakTouched    => "Squeak-Touched",
        WasteNot         => "Waste-Not",
        CaveShy          => "Cave-Shy",
        ProspectorsItch  => "Prospector's Itch",
        DeepEar          => "Deep-Ear",
        HighGrader       => "High-Grader",
        SeamFever        => "Seam-Fever",
    }
}

roster_enum! {
    /// An Advisor's Specialization (the three Board pools, Advisor_System Part 3; 14 roles).
    AdvisorSpecialization, 14 {
        IntelligenceBroker        => "Intelligence Broker (Spymaster)",
        OperationsOfficer         => "Operations Officer (COO)",
        FinancialPatron           => "The Financial Patron",
        CfoMercenary              => "CFO (Mercenary Focus)",
        CfoMining                 => "CFO (Mining Focus)",
        VpTectonicRisk            => "VP of Tectonic Risk & Planetology",
        DirectorHeavyEngineering  => "Director of Heavy Engineering",
        DirectorAssetProtection   => "Director of Asset Protection (Corp-Sec)",
        VpLaborAntiUnion          => "VP of Labor & Personnel (Anti-Union)",
        VpLaborProUnion           => "VP of Labor & Personnel (Pro-Union)",
        DirectorOverlandLogistics => "Director of Overland Logistics",
        ChiefScienceOfficer       => "Chief Science Officer (CSO)",
        HeadReverseEngineering    => "Head of Reverse Engineering",
        DirectorAppliedInnovation => "Director of Applied Innovation",
    }
}

roster_enum! {
    /// An Advisor's Quirk (Advisor_System Pillar 4; identity only in Phase 1, effects are GAP-64).
    AdvisorQuirk, 12 {
        Restructurer       => "The Restructurer",
        Nepotist           => "Nepotist",
        Micromanager       => "Micromanager",
        GoldenParachute    => "Golden Parachute",
        LeaksToThePress    => "Leaks to the Press",
        CreativeAccountant => "Creative Accountant",
        Consultant         => "The Consultant",
        EmpireBuilder      => "Empire Builder",
        ConferenceCircuit  => "Conference Circuit",
        RubberStamp        => "Rubber Stamp",
        Headhunted         => "Headhunted",
        VanityMetrics      => "Vanity Metrics",
    }
}

/// The Board an Advisor sits on. Derived from the Specialization by
/// `AdvisorSpecialization::board`, never stored (ODQ-39 (d)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdvisorBoard {
    Mercenary,
    Mining,
    Research,
}

/// The Board a generic Advisor roll draws from, chosen by the caller. The Research Board is
/// never rolled generically (ODQ-39 (c)): its roles arrive only through their own routes, so
/// it has no variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenericBoard {
    Mercenary,
    Mining,
}

impl AdvisorSpecialization {
    /// The Board this role belongs to (Advisor_System §3a–§3c).
    pub fn board(self) -> AdvisorBoard {
        match self {
            Self::IntelligenceBroker
            | Self::OperationsOfficer
            | Self::FinancialPatron
            | Self::CfoMercenary => AdvisorBoard::Mercenary,
            Self::CfoMining
            | Self::VpTectonicRisk
            | Self::DirectorHeavyEngineering
            | Self::DirectorAssetProtection
            | Self::VpLaborAntiUnion
            | Self::VpLaborProUnion
            | Self::DirectorOverlandLogistics => AdvisorBoard::Mining,
            Self::ChiefScienceOfficer
            | Self::HeadReverseEngineering
            | Self::DirectorAppliedInnovation => AdvisorBoard::Research,
        }
    }
}

const MERCENARY_POOL: &[AdvisorSpecialization] = &[
    AdvisorSpecialization::IntelligenceBroker,
    AdvisorSpecialization::OperationsOfficer,
    AdvisorSpecialization::FinancialPatron,
    AdvisorSpecialization::CfoMercenary,
];

const MINING_POOL: &[AdvisorSpecialization] = &[
    AdvisorSpecialization::CfoMining,
    AdvisorSpecialization::VpTectonicRisk,
    AdvisorSpecialization::DirectorHeavyEngineering,
    AdvisorSpecialization::DirectorAssetProtection,
    AdvisorSpecialization::VpLaborAntiUnion,
    AdvisorSpecialization::VpLaborProUnion,
    AdvisorSpecialization::DirectorOverlandLogistics,
];

/// The generic roll's pool for `board`, in canon order: Mercenary 4, Mining 7 (11 in all).
pub const fn generic_pool(board: GenericBoard) -> &'static [AdvisorSpecialization] {
    match board {
        GenericBoard::Mercenary => MERCENARY_POOL,
        GenericBoard::Mining => MINING_POOL,
    }
}

roster_enum! {
    /// A Bespoke Augment SKU (Bespoke_Augmentation_System §6–7, brick 7d-1): the 8 combat
    /// SKUs, then the 3 industrial ones. Mirrored by `0029`'s two `*_augment_canon` CHECKs,
    /// split by `Augment::catalogue`.
    Augment, 11 {
        IntegralGunArm              => "Integral Gun-Arm",
        ThermalLanceArm             => "Thermal Lance-Arm",
        CoherentLightOculars        => "Coherent-Light Oculars",
        CortexIntrusionJack         => "Cortex Intrusion Jack",
        MyomerBundleArms            => "Myomer-Bundle Arms",
        ArrestorCoilLegs            => "Arrestor-Coil Legs",
        DopplerCochlea              => "Doppler Cochlea",
        GloomSightOcularsTactical   => "Gloom-Sight Oculars (Tactical)",
        GeophoneCochlea             => "Geophone Cochlea",
        GloomSightOcularsIndustrial => "Gloom-Sight Oculars (Industrial)",
        LoadBearerFrame             => "Load-Bearer Frame",
    }
}

roster_enum! {
    /// Who fitted an augment (Bespoke §2): Church ×1.0, secular ×0.5 on the VC#6 ticker.
    /// Permanent; mirrored by `0029`'s `augmentee_records_fitting_source_canon`.
    FittingSource, 2 {
        Church  => "Church",
        Secular => "Secular",
    }
}

roster_enum! {
    /// A named individual's persisted fate (Casualties_Reinforcements_and_Veterancy §6;
    /// 7d-1 D1). A persistence level, not the sim's `UnitStatus`: `Deserted` is
    /// campaign-layer and never comes out of a battle. Mirrored by `0029`'s
    /// `augmentee_records_casualty_class_canon`.
    CasualtyClass, 4 {
        Kia      => "KIA",
        Wounded  => "Wounded",
        Mia      => "MIA",
        Deserted => "Deserted",
    }
}

/// Which catalogue an `Augment` belongs to (Bespoke §2, division of labour): combat SKUs
/// fit Section members, industrial ones fit Mining Crew members. Derived, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AugmentCatalogue {
    Combat,
    Industrial,
}

impl Augment {
    /// The catalogue this SKU is sold from (Bespoke §6 combat, §7 industrial).
    pub fn catalogue(self) -> AugmentCatalogue {
        match self {
            Self::IntegralGunArm
            | Self::ThermalLanceArm
            | Self::CoherentLightOculars
            | Self::CortexIntrusionJack
            | Self::MyomerBundleArms
            | Self::ArrestorCoilLegs
            | Self::DopplerCochlea
            | Self::GloomSightOcularsTactical => AugmentCatalogue::Combat,
            Self::GeophoneCochlea
            | Self::GloomSightOcularsIndustrial
            | Self::LoadBearerFrame => AugmentCatalogue::Industrial,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn check<T: Copy + PartialEq + std::fmt::Debug, const N: usize>(
        all: [T; N],
        expected_len: usize,
        as_str: fn(T) -> &'static str,
        from_db_str: fn(&str) -> Option<T>,
    ) {
        assert_eq!(N, expected_len);
        let mut seen = HashSet::new();
        for v in all {
            assert_eq!(from_db_str(as_str(v)), Some(v), "{v:?} must round-trip");
            assert!(seen.insert(as_str(v)), "duplicate display string {:?}", as_str(v));
        }
        assert_eq!(from_db_str("Not A Flaw"), None);
    }

    #[test]
    fn roster_round_trips_every_variant() {
        check(CommanderOrigin::ALL, 5, CommanderOrigin::as_str, CommanderOrigin::from_db_str);
        check(FactionBias::ALL, 12, FactionBias::as_str, FactionBias::from_db_str);
        check(CommanderSpecialization::ALL, 12, CommanderSpecialization::as_str, CommanderSpecialization::from_db_str);
        check(CommanderFlaw::ALL, 12, CommanderFlaw::as_str, CommanderFlaw::from_db_str);
        check(CommanderGrade::ALL, 5, CommanderGrade::as_str, CommanderGrade::from_db_str);
        check(ForemanSpecialization::ALL, 13, ForemanSpecialization::as_str, ForemanSpecialization::from_db_str);
        check(ForemanFlaw::ALL, 10, ForemanFlaw::as_str, ForemanFlaw::from_db_str);
        check(AdvisorSpecialization::ALL, 14, AdvisorSpecialization::as_str, AdvisorSpecialization::from_db_str);
        check(AdvisorQuirk::ALL, 12, AdvisorQuirk::as_str, AdvisorQuirk::from_db_str);

        let flattened: Vec<FactionBias> = FactionBias::GROUPS.iter().flat_map(|g| g.iter().copied()).collect();
        assert_eq!(flattened, FactionBias::ALL.to_vec(), "GROUPS must hold every bias once, in ALL order");
    }

    /// Advisor_System §3a–§3c, written out here as the oracle.
    #[test]
    fn advisor_specialization_board_maps_all_fourteen() {
        use AdvisorBoard::{Mercenary, Mining, Research};
        const CANON: [(&str, AdvisorBoard); 14] = [
            ("Intelligence Broker (Spymaster)", Mercenary),
            ("Operations Officer (COO)", Mercenary),
            ("The Financial Patron", Mercenary),
            ("CFO (Mercenary Focus)", Mercenary),
            ("CFO (Mining Focus)", Mining),
            ("VP of Tectonic Risk & Planetology", Mining),
            ("Director of Heavy Engineering", Mining),
            ("Director of Asset Protection (Corp-Sec)", Mining),
            ("VP of Labor & Personnel (Anti-Union)", Mining),
            ("VP of Labor & Personnel (Pro-Union)", Mining),
            ("Director of Overland Logistics", Mining),
            ("Chief Science Officer (CSO)", Research),
            ("Head of Reverse Engineering", Research),
            ("Director of Applied Innovation", Research),
        ];
        for (text, board) in CANON {
            let spec = AdvisorSpecialization::from_db_str(text).unwrap_or_else(|| panic!("{text:?} is not on the roster"));
            assert_eq!(spec.board(), board, "{text:?}");
        }
        let canon: HashSet<&str> = CANON.iter().map(|(t, _)| *t).collect();
        let roster: HashSet<&str> = AdvisorSpecialization::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(canon, roster, "the roster must hold exactly the 14 canon roles");
    }

    #[test]
    fn generic_pools_are_the_canon_lists() {
        let strings = |b| generic_pool(b).iter().map(|s| s.as_str()).collect::<Vec<_>>();
        let mercenary = strings(GenericBoard::Mercenary);
        let mining = strings(GenericBoard::Mining);
        assert_eq!(
            mercenary,
            ["Intelligence Broker (Spymaster)", "Operations Officer (COO)", "The Financial Patron", "CFO (Mercenary Focus)"]
        );
        assert_eq!(
            mining,
            [
                "CFO (Mining Focus)",
                "VP of Tectonic Risk & Planetology",
                "Director of Heavy Engineering",
                "Director of Asset Protection (Corp-Sec)",
                "VP of Labor & Personnel (Anti-Union)",
                "VP of Labor & Personnel (Pro-Union)",
                "Director of Overland Logistics",
            ]
        );
        for research in ["Chief Science Officer (CSO)", "Head of Reverse Engineering", "Director of Applied Innovation"] {
            assert!(!mercenary.contains(&research) && !mining.contains(&research), "{research:?} must never roll generically");
        }
    }
}
