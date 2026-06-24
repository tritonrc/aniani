//! Integration test: the cross-platform `POST /api/v1/snapshot` endpoint writes a
//! snapshot to disk and round-trips through the loader. This is the portable
//! equivalent of the Unix-only `SIGUSR1` trigger, so it must work on every OS
//! (notably Windows, which has no `SIGUSR1`).

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use parking_lot::RwLock;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Build app state whose snapshots land in `snapshot_dir`.
fn state_with_snapshot_dir(snapshot_dir: String) -> aniani::store::SharedState {
    Arc::new(aniani::store::AppState {
        log_store: RwLock::new(aniani::store::LogStore::new()),
        metric_store: RwLock::new(aniani::store::MetricStore::new()),
        trace_store: RwLock::new(aniani::store::TraceStore::new()),
        config: aniani::config::Config {
            port: 0,
            bind_address: "127.0.0.1".into(),
            snapshot_dir,
            snapshot_interval: 0,
            max_log_entries: 100_000,
            max_series: 10_000,
            max_spans: 100_000,
            retention: "2h".into(),
            restore: false,
        },
        start_time: Instant::now(),
    })
}

async fn push_one_log(app: &axum::Router) {
    let push_body = json!({
        "streams": [{
            "stream": {"service": "checkout", "level": "info"},
            "values": [["1700000000000000000", "order placed"]]
        }]
    });
    let push = Request::builder()
        .method("POST")
        .uri("/loki/api/v1/push")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&push_body).unwrap()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(push).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn test_snapshot_endpoint_writes_and_restores() {
    let dir = tempfile::tempdir().unwrap();
    let app = aniani::server::build_router(state_with_snapshot_dir(
        dir.path().to_string_lossy().into_owned(),
    ));

    // Ingest a log so the snapshot has content.
    push_one_log(&app).await;

    // Trigger an on-demand snapshot via the cross-platform HTTP endpoint.
    let snap = Request::builder()
        .method("POST")
        .uri("/api/v1/snapshot")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(snap).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert!(
        json["bytes"].as_u64().unwrap() > 0,
        "snapshot should be non-empty"
    );

    // The snapshot file exists and round-trips back through the loader.
    let snap_path = dir.path().join("aniani.snap");
    assert!(
        snap_path.exists(),
        "aniani.snap should exist after POST /api/v1/snapshot"
    );

    let (logs, _metrics, _traces) = aniani::snapshot::load_snapshot(dir.path()).unwrap();
    assert_eq!(
        logs.total_entries, 1,
        "restored snapshot should contain the ingested log"
    );
}

/// Many overlapping snapshot triggers must not race on the shared temp path:
/// every request should succeed and the published snapshot must always be a
/// complete, loadable file. Multi-threaded so the blocking writes truly contend.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_snapshot_endpoint_concurrent_requests_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    let app = aniani::server::build_router(state_with_snapshot_dir(
        dir.path().to_string_lossy().into_owned(),
    ));

    push_one_log(&app).await;

    let mut handles = Vec::new();
    for _ in 0..12 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let req = Request::builder()
                .method("POST")
                .uri("/api/v1/snapshot")
                .body(Body::empty())
                .unwrap();
            app.oneshot(req).await.unwrap().status()
        }));
    }

    for h in handles {
        assert_eq!(
            h.await.unwrap(),
            StatusCode::OK,
            "every concurrent snapshot request should succeed"
        );
    }

    // The final published snapshot is intact and loadable.
    let (logs, _metrics, _traces) = aniani::snapshot::load_snapshot(dir.path()).unwrap();
    assert_eq!(logs.total_entries, 1);
}
