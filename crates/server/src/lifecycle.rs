use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;

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

/// Runs one scan of all Active campaigns and applies any warranted lifecycle transitions.
/// DB errors are logged at WARN and skipped — a transient failure must not abort the loop.
pub async fn poll_lifecycle_once(repo: &(dyn CampaignRepository + Send + Sync)) {
    let campaigns = match repo.list_campaigns_by_state(CampaignLifecycle::Active).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("lifecycle poll: list_campaigns_by_state failed: {e}");
            return;
        }
    };
    let now = Utc::now();
    for campaign in campaigns {
        if let Some(new_state) = check_campaign_transition(&campaign, now) {
            match repo.update_sector_state(campaign.campaign_id, new_state.clone()).await {
                Ok(()) => tracing::info!(
                    campaign_id = %campaign.campaign_id,
                    new_state = ?new_state,
                    "campaign lifecycle transition applied"
                ),
                Err(e) => tracing::warn!(
                    campaign_id = %campaign.campaign_id,
                    "lifecycle poll: update_sector_state failed: {e}"
                ),
            }
        }
    }
}

pub async fn run_sector_lifecycle_task(repo: Arc<dyn CampaignRepository + Send + Sync>) {
    let mut interval = tokio::time::interval(Duration::from_secs(LIFECYCLE_POLL_INTERVAL_SECS));
    loop {
        interval.tick().await;
        poll_lifecycle_once(repo.as_ref()).await;
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
            json!({ "MilitaryDominance": 50.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), None);
    }

    #[test]
    fn active_transitions_to_ending_when_ticker_hits_100() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            None,
            json!({ "CapitalistDomination": 100.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), Some(CampaignLifecycle::Ending));
    }

    #[test]
    fn active_transitions_to_ending_when_ticker_exceeds_100() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            None,
            json!({ "MilitaryDominance": 150.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), Some(CampaignLifecycle::Ending));
    }

    #[test]
    fn active_with_no_ends_at_and_no_ticker_at_100_returns_none() {
        let now = Utc::now();
        let campaign = make_campaign(
            CampaignLifecycle::Active,
            None,
            json!({ "MilitaryDominance": 99.9 }),
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
            json!({ "MilitaryDominance": 100.0 }),
        );
        assert_eq!(check_campaign_transition(&campaign, now), None);
    }

    #[test]
    fn ended_and_archived_return_none() {
        let now = Utc::now();
        let ended = make_campaign(
            CampaignLifecycle::Ended,
            Some(now - Duration::seconds(1)),
            json!({ "MilitaryDominance": 100.0 }),
        );
        let archived = make_campaign(
            CampaignLifecycle::Archived,
            Some(now - Duration::seconds(1)),
            json!({ "MilitaryDominance": 100.0 }),
        );
        assert_eq!(check_campaign_transition(&ended, now), None);
        assert_eq!(check_campaign_transition(&archived, now), None);
    }
}
