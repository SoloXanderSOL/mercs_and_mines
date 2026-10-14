use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::repeat_warn::{KeyedRepeatWarn, RepeatWarn};
use crate::repository::campaign::{CampaignInstance, CampaignLifecycle, CampaignRepository};

/// Returns the lifecycle state the campaign should transition to, or `None` if no
/// transition is warranted right now.
///
/// Only `Active` campaigns can transition — all other states return `None`.
/// Two triggers advance an `Active` campaign to `Ending`:
///   1. The wall-clock timer has expired (`ends_at < now`).
///   2. Any victory ticker has reached or exceeded 100.0.
pub fn check_campaign_transition(
    campaign: &CampaignInstance,
    now: DateTime<Utc>,
) -> Option<CampaignLifecycle> {
    if campaign.state != CampaignLifecycle::Active {
        return None;
    }

    if let Some(ends_at) = campaign.ends_at {
        if now > ends_at {
            return Some(CampaignLifecycle::Ending);
        }
    }

    // A non-object `victory_tickers` skips the ticker check; the caller logs it.
    if let Value::Object(map) = &campaign.victory_tickers {
        for v in map.values() {
            if v.as_f64().unwrap_or(0.0) >= 100.0 {
                return Some(CampaignLifecycle::Ending);
            }
        }
    }

    None
}

pub const LIFECYCLE_POLL_INTERVAL_SECS: u64 = 60;

/// Repeat a persistent failure, per call or per campaign, about once an hour (GAP-38).
pub const LIFECYCLE_WARN_EVERY_TICKS: u32 = (3600 / LIFECYCLE_POLL_INTERVAL_SECS) as u32;

/// Repeat-warning state for the lifecycle task: the scan itself, and one keyed counter
/// per per-campaign failure site.
#[derive(Debug)]
pub struct LifecycleWarns {
    pub list: RepeatWarn,
    pub update_state: KeyedRepeatWarn,
    pub bad_tickers: KeyedRepeatWarn,
}

impl LifecycleWarns {
    pub fn new() -> Self {
        Self {
            list: RepeatWarn::new(LIFECYCLE_WARN_EVERY_TICKS),
            update_state: KeyedRepeatWarn::new(LIFECYCLE_WARN_EVERY_TICKS),
            bad_tickers: KeyedRepeatWarn::new(LIFECYCLE_WARN_EVERY_TICKS),
        }
    }
}

impl Default for LifecycleWarns {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs one scan of all Active campaigns and applies any warranted lifecycle transitions.
/// Every failure, of the scan or of one campaign, logs its first occurrence and then
/// about once an hour, and logs its recovery once. Per-campaign counters are evicted
/// when the campaign leaves the Active set. Nothing here aborts the loop.
pub async fn poll_lifecycle_once(
    repo: &(dyn CampaignRepository + Send + Sync),
    warns: &mut LifecycleWarns,
) {
    let campaigns = match repo.list_campaigns_by_state(CampaignLifecycle::Active).await {
        Ok(v) => {
            if let Some(n) = warns.list.ok() {
                tracing::info!("lifecycle poll: list_campaigns_by_state recovered after {n} failed polls");
            }
            v
        }
        Err(e) => {
            match warns.list.fail() {
                Some(1) => tracing::warn!("lifecycle poll: list_campaigns_by_state failed: {e}"),
                Some(n) => tracing::warn!("lifecycle poll: list_campaigns_by_state still failing ({n} consecutive polls): {e}"),
                None => {}
            }
            return;
        }
    };
    let live: HashSet<Uuid> = campaigns.iter().map(|c| c.campaign_id).collect();
    warns.update_state.retain(&live);
    warns.bad_tickers.retain(&live);

    let now = Utc::now();
    for campaign in campaigns {
        let id = campaign.campaign_id;
        let transition = check_campaign_transition(&campaign, now);

        // Logged only where check_campaign_transition reached the ticker check.
        if campaign.state == CampaignLifecycle::Active && transition.is_none() {
            if campaign.victory_tickers.is_object() {
                if let Some(n) = warns.bad_tickers.ok(id) {
                    tracing::info!(campaign_id = %id, "victory_tickers is a JSON object again after {n} polls");
                }
            } else {
                match warns.bad_tickers.fail(id) {
                    Some(1) => tracing::warn!(campaign_id = %id, "victory_tickers is not a JSON object — ticker check skipped"),
                    Some(n) => tracing::warn!(campaign_id = %id, "victory_tickers is still not a JSON object ({n} consecutive polls) — ticker check skipped"),
                    None => {}
                }
            }
        }

        if let Some(new_state) = transition {
            match repo.update_sector_state(id, new_state.clone()).await {
                Ok(()) => {
                    if let Some(n) = warns.update_state.ok(id) {
                        tracing::info!(campaign_id = %id, "lifecycle poll: update_sector_state recovered after {n} failed polls");
                    }
                    tracing::info!(
                        campaign_id = %id,
                        new_state = ?new_state,
                        "campaign lifecycle transition applied"
                    );
                }
                Err(e) => match warns.update_state.fail(id) {
                    Some(1) => tracing::warn!(campaign_id = %id, "lifecycle poll: update_sector_state failed: {e}"),
                    Some(n) => tracing::warn!(campaign_id = %id, "lifecycle poll: update_sector_state still failing ({n} consecutive polls): {e}"),
                    None => {}
                },
            }
        }
    }
}

pub async fn run_sector_lifecycle_task(repo: Arc<dyn CampaignRepository + Send + Sync>) {
    let mut interval = tokio::time::interval(Duration::from_secs(LIFECYCLE_POLL_INTERVAL_SECS));
    let mut warns = LifecycleWarns::new();
    loop {
        interval.tick().await;
        poll_lifecycle_once(repo.as_ref(), &mut warns).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;
    use uuid::Uuid;

    fn make_campaign(
        state: CampaignLifecycle,
        ends_at: Option<DateTime<Utc>>,
        victory_tickers: Value,
    ) -> CampaignInstance {
        let now = Utc::now();
        CampaignInstance {
            campaign_id: Uuid::new_v4(),
            sector_id: Uuid::new_v4(),
            map_seed: 0,
            state,
            victory_tickers,
            started_at: None,
            ends_at,
            created_at: now,
            updated_at: now,
        }
    }

    /// Lists a fixed set of campaigns; every `update_sector_state` fails.
    struct FailingUpdateRepo(std::sync::Mutex<Vec<CampaignInstance>>);

    #[async_trait::async_trait]
    impl CampaignRepository for FailingUpdateRepo {
        async fn create_campaign(
            &self,
            _: &crate::repository::campaign::NewCampaignInstance,
        ) -> Result<CampaignInstance, sqlx::Error> {
            unimplemented!()
        }
        async fn get_campaign(&self, _: Uuid) -> Result<Option<CampaignInstance>, sqlx::Error> {
            unimplemented!()
        }
        async fn update_sector_state(&self, _: Uuid, _: CampaignLifecycle) -> Result<(), sqlx::Error> {
            Err(sqlx::Error::PoolClosed)
        }
        async fn apply_ticker_delta(
            &self,
            _: Uuid,
            _: crate::repository::campaign::VictoryTickerType,
            _: f64,
        ) -> Result<f64, sqlx::Error> {
            unimplemented!()
        }
        async fn list_campaigns_by_state(&self, _: CampaignLifecycle) -> Result<Vec<CampaignInstance>, sqlx::Error> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn activate_campaign(&self, _: Uuid, _: DateTime<Utc>, _: Value) -> Result<bool, sqlx::Error> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn poll_lifecycle_suppresses_per_campaign() {
        let now = Utc::now();
        let due = make_campaign(CampaignLifecycle::Active, Some(now - Duration::seconds(1)), json!({}));
        let malformed = make_campaign(CampaignLifecycle::Active, None, json!([1, 2]));
        let repo = FailingUpdateRepo(std::sync::Mutex::new(vec![due, malformed]));
        let mut warns = LifecycleWarns::new();

        poll_lifecycle_once(&repo, &mut warns).await;
        poll_lifecycle_once(&repo, &mut warns).await;
        assert_eq!(warns.update_state.len(), 1, "the due campaign's update failure is tracked");
        assert_eq!(warns.bad_tickers.len(), 1, "the malformed campaign is tracked");

        repo.0.lock().unwrap().clear();
        poll_lifecycle_once(&repo, &mut warns).await;
        assert_eq!(warns.update_state.len(), 0, "a campaign that left Active is evicted");
        assert_eq!(warns.bad_tickers.len(), 0);
    }

    #[test]
    fn active_transitions_to_ending_when_timer_expires() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            Some(now - Duration::seconds(1)),
            json!({}),
        );
        assert_eq!(check_campaign_transition(&campaign, now), Some(CampaignLifecycle::Ending));
    }

    #[test]
    fn active_returns_none_when_timer_not_expired_and_no_ticker() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            Some(now + Duration::seconds(3600)),
            json!({ "MILITARY_DOMINANCE": 50.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), None);
    }

    #[test]
    fn active_transitions_to_ending_when_ticker_hits_100() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            None,
            json!({ "CAPITALIST_DOMINATION": 100.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), Some(CampaignLifecycle::Ending));
    }

    #[test]
    fn active_transitions_to_ending_when_ticker_exceeds_100() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            None,
            json!({ "MILITARY_DOMINANCE": 150.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), Some(CampaignLifecycle::Ending));
    }

    #[test]
    fn active_with_no_ends_at_and_no_ticker_at_100_returns_none() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            None,
            json!({ "MILITARY_DOMINANCE": 99.9 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), None);
    }

    #[test]
    fn pending_returns_none() {
        let now = Utc::now();
        let campaign = make_campaign(CampaignLifecycle::Pending, None, json!({}));
        assert_eq!(check_campaign_transition(&campaign, now), None);
    }

    #[test]
    fn ending_returns_none() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Ending,
            Some(now - Duration::seconds(1)),
            json!({ "MILITARY_DOMINANCE": 100.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), None);
    }

    #[test]
    fn ended_and_archived_return_none() {
        let now = Utc::now();
        let ended = make_campaign(
            CampaignLifecycle::Ended,
            Some(now - Duration::seconds(1)),
            json!({ "MILITARY_DOMINANCE": 100.0 }),
        );
        let archived = make_campaign(
            CampaignLifecycle::Archived,
            Some(now - Duration::seconds(1)),
            json!({ "MILITARY_DOMINANCE": 100.0 }),
        );
        assert_eq!(check_campaign_transition(&ended, now), None);
        assert_eq!(check_campaign_transition(&archived, now), None);
    }
}
