//! A Commander's command cap: the most Sections one Commander can take on a single
//! mission (Mercenary_Organisation_Structure §1, GAP-50). One table, read by the server's
//! mission route and, later, by the client, so the two always agree. Vehicles don't count.

/// How many Sections a Commander may take on one mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCap {
    Sections(u32),
    /// Rank 5: the Regiment band is open-ended.
    Unlimited,
}

/// The command cap for a Commander's rank. `None` for a rank outside 1..=5, which no
/// writer produces (`rank_for_xp` gives 1..=5); a caller treats it as an integrity fault.
pub fn command_cap(rank: i16) -> Option<CommandCap> {
    match rank {
        1 => Some(CommandCap::Sections(2)),
        2 => Some(CommandCap::Sections(4)),
        3 => Some(CommandCap::Sections(20)),
        4 => Some(CommandCap::Sections(64)),
        5 => Some(CommandCap::Unlimited),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The oracle is Mercenary_Organisation_Structure §1's table, as literals.
    #[test]
    fn command_cap_matches_the_canon_table() {
        assert_eq!(command_cap(1), Some(CommandCap::Sections(2)));
        assert_eq!(command_cap(2), Some(CommandCap::Sections(4)));
        assert_eq!(command_cap(3), Some(CommandCap::Sections(20)));
        assert_eq!(command_cap(4), Some(CommandCap::Sections(64)));
        assert_eq!(command_cap(5), Some(CommandCap::Unlimited));
        for rank in [0i16, 6, -1] {
            assert_eq!(command_cap(rank), None, "rank {rank} is outside 1..=5");
        }
    }
}
