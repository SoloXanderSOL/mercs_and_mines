use std::sync::Arc;
use axum::body::Body;
use http_body_util::BodyExt;
use tower::util::ServiceExt;
use mercs_server::{routes::router, state::AppState};

fn alpha_vs_scout_payload() -> serde_json::Value {
    serde_json::json!({
        "section": {
            "id": "alpha",
            "name": "Alpha Section",
            "max_strength": 5,
            "current_strength": 5,
            "individual_hp": 10,
            "accuracy": 60,
            "evasion": 20,
            "weapon": {
                "name": "Laser Carbine",
                "ap": 4,
                "base_damage": 8,
                "tag": "Laser",
                "accuracy": 0
            },
            "armor_at": 3,
            "armor_tag": "LightArmor"
        },
        "vehicle": {
            "id": "scout-1",
            "name": "Armored Scout",
            "hp": 40,
            "max_hp": 40,
            "at": 6,
            "armor_tag": "HeavyArmor",
            "evasion": 10,
            "weapons": [{
                "name": "Scout Cannon",
                "ap": 6,
                "base_damage": 12,
                "tag": "Slug",
                "accuracy": 0
            }]
        },
        "seed_override": 42,
        "max_ticks": 25,
        "combat_initiation_type": "SPOTTED"
    })
}

#[tokio::test]
async fn aar_replays_deterministically() {
    let tmp = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new(tmp.path().to_path_buf()));
    let app = router(state);

    // ── Step 1: resolve combat, get session_id ─────────────────────────
    let resolve_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/combat/resolve")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&alpha_vs_scout_payload()).unwrap()
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resolve_resp.status(), 200, "POST /api/combat/resolve failed");

    let bytes = resolve_resp.into_body().collect().await.unwrap().to_bytes();
    let resolve_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let session_id = resolve_json["session_id"]
        .as_str()
        .expect("POST /api/combat/resolve must include session_id in response");
    let original = &resolve_json["report"];

    // ── Step 2: replay via AAR endpoint ───────────────────────────────
    let aar_resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri(&format!("/api/combat/aar/{}", session_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(aar_resp.status(), 200, "GET /api/combat/aar/:id failed");

    let aar_bytes = aar_resp.into_body().collect().await.unwrap().to_bytes();
    let aar_json: serde_json::Value = serde_json::from_slice(&aar_bytes).unwrap();
    let replayed = &aar_json["report"];

    // ── Determinism assertions ────────────────────────────────────────
    assert_eq!(original["report_id"],              replayed["report_id"],
               "report_id mismatch — RNG seed drift");
    assert_eq!(original["outcome"],                replayed["outcome"],
               "outcome mismatch");
    assert_eq!(original["section_final_strength"], replayed["section_final_strength"],
               "section_final_strength mismatch");
    assert_eq!(original["vehicle_final_hp"],       replayed["vehicle_final_hp"],
               "vehicle_final_hp mismatch");
    assert_eq!(
        original["ticks"].as_array().map(|a| a.len()),
        replayed["ticks"].as_array().map(|a| a.len()),
        "tick count mismatch"
    );
    assert_eq!(
        original["ticks"],
        replayed["ticks"],
        "tick-by-tick content mismatch — RNG stream diverged"
    );
}
