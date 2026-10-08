//! Repeat-warning suppression for background poll loops (GAP-38).
//!
//! A persistent outage fails the same call on every tick, and logging each one buries
//! the first, most useful line under thousands of copies. `RepeatWarn` only decides
//! *which* failures to log; the caller writes the line, so each message stays at its
//! call site. Tick-counted with no clock, so it is testable without `tokio::time::pause`.

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

#[cfg(test)]
mod tests {
    use super::RepeatWarn;

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
