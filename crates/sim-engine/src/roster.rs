// Canonical Commander roster (Brick B; GAP-6, ruling R3).
// Ref: Commander_and_Advisor_System.md (species roster, Faction Bias table, Sect Layer);
//      Mercenary_Commander_System.md (Specializations, Fatal Flaws).
//
// The single source of truth for Commander roster strings. Migration `0018`'s CHECK
// constraints on `commander_records.origin` and `faction_bias` mirror `CommanderOrigin`
// and `FactionBias`; a drift-guard test writes every value through them. The Foreman
// pools are a separate roster and do not live here.
//
// No serde derive: the database boundary uses `as_str()`, and these types must never
// become part of a logged payload by accident.

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

        let flattened: Vec<FactionBias> = FactionBias::GROUPS.iter().flat_map(|g| g.iter().copied()).collect();
        assert_eq!(flattened, FactionBias::ALL.to_vec(), "GROUPS must hold every bias once, in ALL order");
    }
}
