//! Repeat-warning suppression for background poll loops (GAP-38).
//!
//! A persistent outage fails the same call on every tick, and logging each one buries
//! the first, most useful line under thousands of copies. `RepeatWarn` only decides
//! *which* failures to log; the caller writes the line, so each message stays at its
//! call site. Tick-counted with no clock, so it is testable without `tokio::time::pause`.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

/// Tracks consecutive failures of one recurring call.
#[derive(Debug)]
pub struct RepeatWarn {
    consecutive: u32,
    every: u32,
}

impl RepeatWarn {
    /// Log the first failure, then every `every`th consecutive one. 0 is treated as 1.
    pub fn new(every: u32) -> Self {
        Self { consecutive: 0, every: every.max(1) }
    }

    /// Records a failure. `Some(n)` means log this one; `n` is the consecutive count.
    pub fn fail(&mut self) -> Option<u32> {
        self.consecutive = self.consecutive.saturating_add(1);
        (self.consecutive == 1 || self.consecutive % self.every == 0).then_some(self.consecutive)
    }

    /// Records a success. `Some(n)` means it ended a run of `n` failures: log the recovery.
    pub fn ok(&mut self) -> Option<u32> {
        let n = std::mem::take(&mut self.consecutive);
        (n > 0).then_some(n)
    }
}

/// One `RepeatWarn` per item, for a failure site that runs once per item per tick.
/// Use one per site, never one shared across sites: a shared counter would report
/// site A's recovery when site B succeeds, just as it would across items.
#[derive(Debug)]
pub struct KeyedRepeatWarn {
    every: u32,
    map: HashMap<Uuid, RepeatWarn>,
}

impl KeyedRepeatWarn {
    pub fn new(every: u32) -> Self {
        Self { every, map: HashMap::new() }
    }

    /// Records a failure for `key`; see [`RepeatWarn::fail`].
    pub fn fail(&mut self, key: Uuid) -> Option<u32> {
        let every = self.every;
        self.map.entry(key).or_insert_with(|| RepeatWarn::new(every)).fail()
    }

    /// Records a success for `key`; see [`RepeatWarn::ok`]. An untracked key → `None`.
    pub fn ok(&mut self, key: Uuid) -> Option<u32> {
        self.map.remove(&key).and_then(|mut w| w.ok())
    }

    /// Evicts every key not in `live` — call it with the tick's full due set, so an
    /// item that left the set stops holding a counter.
    pub fn retain(&mut self, live: &HashSet<Uuid>) {
        self.map.retain(|k, _| live.contains(k));
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyedRepeatWarn, RepeatWarn};
    use std::collections::HashSet;
    use uuid::Uuid;

    #[test]
    fn keyed_repeat_warn_counts_per_key_and_evicts() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut w = KeyedRepeatWarn::new(3);
        assert_eq!(w.fail(a), Some(1));
        assert_eq!(w.fail(b), Some(1), "key B's first failure logs on its own");
        assert_eq!(w.fail(a), None);
        assert_eq!(w.ok(a), Some(2), "key A recovers after 2");
        assert_eq!(w.fail(b), None, "key A's recovery must not reset key B");
        assert_eq!(w.fail(b), Some(3));
        assert_eq!(w.ok(a), None, "recovery is logged once");
        w.fail(a);
        assert_eq!(w.len(), 2);
        w.retain(&HashSet::from([b]));
        assert_eq!(w.len(), 1, "A left the due set and is evicted");
        assert_eq!(w.ok(a), None);
        assert_eq!(w.ok(b), Some(3));
    }

    #[test]
    fn repeat_warn_logs_first_then_every_nth() {
        let mut w = RepeatWarn::new(3);
        let logged: Vec<Option<u32>> = (0..7).map(|_| w.fail()).collect();
        assert_eq!(logged, [Some(1), None, Some(3), None, None, Some(6), None]);
    }

    #[test]
    fn repeat_warn_reports_recovery_once_and_rearms() {
        let mut w = RepeatWarn::new(3);
        assert_eq!(w.ok(), None, "no failures, nothing to recover from");
        w.fail();
        w.fail();
        assert_eq!(w.ok(), Some(2));
        assert_eq!(w.ok(), None, "recovery is logged once");
        assert_eq!(w.fail(), Some(1), "a new outage logs its first failure again");
    }
}
