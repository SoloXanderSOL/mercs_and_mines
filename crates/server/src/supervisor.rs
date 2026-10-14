//! Background-task supervision (GAP-41).
//!
//! A panic inside a spawned task ends that task; nothing restarts it unless something
//! awaits its `JoinHandle`. `supervise` runs the task as its own inner `tokio::spawn`,
//! awaits the handle, and restarts after `backoff` on a panic or an unexpected return.
//! Wall-clock time is used here only to tell a fast exit from a healthy run; the
//! supervisor sits outside every sim path.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::repeat_warn::RepeatWarn;

/// Log the first fast exit, then about once an hour (720 × a 5 s backoff), like the
/// tick gates (GAP-38). A deterministic panic would otherwise log every `backoff`.
const EXIT_WARN_EVERY: u32 = 720;

/// Records one exit. A run that lasted at least `healthy_after` ends the previous run of
/// fast exits, so a fresh panic after a long healthy run always logs.
/// `Some(n)` means log this exit; `n` is the consecutive fast-exit count.
fn note_exit(warn: &mut RepeatWarn, ran_for: Duration, healthy_after: Duration) -> Option<u32> {
    if ran_for >= healthy_after {
        warn.ok();
    }
    warn.fail()
}

/// Runs `make()` as a spawned task for ever, restarting it after `backoff` whenever it
/// panics or returns. Returns only if the task is cancelled (runtime shutdown).
/// A restarted task rebuilds its own state, including its `RepeatWarn`s: a panic is
/// news, so the first failure after a restart warns again.
pub async fn supervise<F, Fut>(name: &'static str, backoff: Duration, healthy_after: Duration, make: F)
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut exit_warn = RepeatWarn::new(EXIT_WARN_EVERY);
    loop {
        let started = Instant::now();
        let exit = match tokio::spawn(make()).await {
            Ok(()) => "returned".to_string(),
            Err(e) if e.is_panic() => {
                let payload = e.into_panic();
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                format!("panicked: {msg}")
            }
            Err(e) => {
                tracing::warn!("{name} task cancelled ({e}); not restarting");
                return;
            }
        };
        if let Some(n) = note_exit(&mut exit_warn, started.elapsed(), healthy_after) {
            tracing::error!(
                "{name} task {exit} ({n} consecutive fast exits); restarting in {backoff:?}"
            );
        }
        tokio::time::sleep(backoff).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    #[tokio::test]
    async fn supervise_restarts_after_panic() {
        let calls = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let (c, n) = (Arc::clone(&calls), Arc::clone(&notify));
        let handle = tokio::spawn(supervise("t", Duration::ZERO, Duration::from_secs(3600), move || {
            let (c, n) = (Arc::clone(&c), Arc::clone(&n));
            async move {
                if c.fetch_add(1, Ordering::SeqCst) < 3 {
                    panic!("deliberate supervisor test panic");
                }
                n.notify_one();
                std::future::pending::<()>().await;
            }
        }));
        tokio::time::timeout(Duration::from_secs(10), notify.notified())
            .await
            .expect("supervisor did not restart the task after its panics");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        handle.abort();
    }

    #[test]
    fn note_exit_resets_after_healthy_run() {
        let mut w = RepeatWarn::new(10);
        let (fast, healthy, after) =
            (Duration::from_secs(1), Duration::from_secs(120), Duration::from_secs(60));
        assert_eq!(note_exit(&mut w, fast, after), Some(1));
        assert_eq!(note_exit(&mut w, fast, after), None);
        assert_eq!(note_exit(&mut w, healthy, after), Some(1), "a healthy run resets the count");
    }
}
