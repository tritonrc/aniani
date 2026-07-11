use super::*;
use crate::store::{LabelMatchOp, LabelMatcher};

#[test]
fn test_ingest_and_select() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "http_requests_total",
        vec![("method".into(), "GET".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 42.0,
            ingest_seq: 0,
        }],
    );

    let ids = store.select_series(&[LabelMatcher {
        name: "__name__".into(),
        op: LabelMatchOp::Eq,
        value: "http_requests_total".into(),
    }]);
    assert_eq!(ids.len(), 1);

    let samples = store.get_samples(ids[0], 0, i64::MAX);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].value, 42.0);
}

#[test]
fn test_select_by_label() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "http_requests_total",
        vec![("method".into(), "GET".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 10.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "http_requests_total",
        vec![("method".into(), "POST".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 5.0,
            ingest_seq: 0,
        }],
    );

    let ids = store.select_series(&[
        LabelMatcher {
            name: "__name__".into(),
            op: LabelMatchOp::Eq,
            value: "http_requests_total".into(),
        },
        LabelMatcher {
            name: "method".into(),
            op: LabelMatchOp::Eq,
            value: "GET".into(),
        },
    ]);
    assert_eq!(ids.len(), 1);
    let samples = store.get_samples(ids[0], 0, i64::MAX);
    assert_eq!(samples[0].value, 10.0);
}

#[test]
fn test_eviction() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "m",
        vec![],
        vec![
            Sample {
                timestamp_ms: 1000,
                value: 1.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 5000,
                value: 2.0,
                ingest_seq: 0,
            },
        ],
    );
    assert_eq!(store.total_samples, 2);
    store.evict_before(3000);
    assert_eq!(store.total_samples, 1);
}

#[test]
fn test_neq_matches_missing_label() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "cpu",
        vec![
            ("host".into(), "server1".into()),
            ("env".into(), "prod".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 1.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "cpu",
        vec![("host".into(), "server2".into())], // no "env" label
        vec![Sample {
            timestamp_ms: 2000,
            value: 2.0,
            ingest_seq: 0,
        }],
    );
    // {__name__="cpu", env!="prod"} should match server2 (missing env label)
    let ids = store.select_series(&[
        LabelMatcher {
            name: "__name__".into(),
            op: LabelMatchOp::Eq,
            value: "cpu".into(),
        },
        LabelMatcher {
            name: "env".into(),
            op: LabelMatchOp::Neq,
            value: "prod".into(),
        },
    ]);
    assert_eq!(ids.len(), 1);
    let samples = store.get_samples(ids[0], 0, i64::MAX);
    assert_eq!(samples[0].value, 2.0);
}

#[test]
fn test_not_regex_matches_missing_label() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "cpu",
        vec![
            ("host".into(), "server1".into()),
            ("env".into(), "staging".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 1.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "cpu",
        vec![("host".into(), "server2".into())], // no "env" label
        vec![Sample {
            timestamp_ms: 2000,
            value: 2.0,
            ingest_seq: 0,
        }],
    );
    // {__name__="cpu", env!~"staging"} should match server2 (missing env label)
    let ids = store.select_series(&[
        LabelMatcher {
            name: "__name__".into(),
            op: LabelMatchOp::Eq,
            value: "cpu".into(),
        },
        LabelMatcher {
            name: "env".into(),
            op: LabelMatchOp::NotRegex,
            value: "staging".into(),
        },
    ]);
    assert_eq!(ids.len(), 1);
    let samples = store.get_samples(ids[0], 0, i64::MAX);
    assert_eq!(samples[0].value, 2.0);
}

#[test]
fn test_out_of_order_ingest() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "m",
        vec![],
        vec![
            Sample {
                timestamp_ms: 3000,
                value: 3.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 1000,
                value: 1.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 2000,
                value: 2.0,
                ingest_seq: 0,
            },
        ],
    );
    let ids = store.select_series(&[LabelMatcher {
        name: "__name__".into(),
        op: LabelMatchOp::Eq,
        value: "m".into(),
    }]);
    let samples = store.get_samples(ids[0], 0, i64::MAX);
    assert_eq!(samples.len(), 3);
    assert_eq!(samples[0].value, 1.0);
    assert_eq!(samples[1].value, 2.0);
    assert_eq!(samples[2].value, 3.0);

    // Verify partition_point works for range queries
    let samples = store.get_samples(ids[0], 1500, 2500);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].value, 2.0);
}

#[test]
fn test_evict_to_max_series() {
    let mut store = MetricStore::new();
    // Create 3 series with different oldest timestamps
    store.ingest_samples(
        "m",
        vec![("host".into(), "a".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 1.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "m",
        vec![("host".into(), "b".into())],
        vec![Sample {
            timestamp_ms: 2000,
            value: 2.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "m",
        vec![("host".into(), "c".into())],
        vec![
            Sample {
                timestamp_ms: 3000,
                value: 3.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 4000,
                value: 4.0,
                ingest_seq: 0,
            },
        ],
    );
    assert_eq!(store.series.len(), 3);
    assert_eq!(store.total_samples, 4);

    // Evict to max 1 series — should remove the 2 oldest series (a, b)
    store.evict_to_max(1);
    assert_eq!(store.series.len(), 1);
    assert_eq!(store.total_samples, 2); // only series c remains with 2 samples
}

#[test]
fn test_label_names_and_values() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "cpu",
        vec![("host".into(), "server1".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 0.5,
            ingest_seq: 0,
        }],
    );
    let names = store.label_names();
    assert!(names.contains(&"__name__".to_string()));
    assert!(names.contains(&"host".to_string()));

    let values = store.get_label_values("host");
    assert_eq!(values, vec!["server1".to_string()]);
}

#[test]
fn test_eviction_prunes_label_values() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "cpu",
        vec![("host".into(), "a".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 0.5,
            ingest_seq: 0,
        }],
    );
    assert!(!store.label_names().is_empty());
    store.evict_before(2000);
    assert!(store.label_names().is_empty());
}

#[test]
fn test_internally_unsorted_batch_after_tail() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "cpu",
        vec![("host".into(), "a".into())],
        vec![Sample {
            timestamp_ms: 100,
            value: 1.0,
            ingest_seq: 0,
        }],
    );
    // Append internally unsorted batch — all > 100
    store.ingest_samples(
        "cpu",
        vec![("host".into(), "a".into())],
        vec![
            Sample {
                timestamp_ms: 300,
                value: 3.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 200,
                value: 2.0,
                ingest_seq: 0,
            },
        ],
    );
    // Verify sorted
    for series in store.series.values() {
        for w in series.samples.windows(2) {
            assert!(
                w[0].timestamp_ms <= w[1].timestamp_ms,
                "samples must be sorted: {} > {}",
                w[0].timestamp_ms,
                w[1].timestamp_ms
            );
        }
    }
}

#[test]
fn test_clear_service_prunes_normalized_name_sources() {
    let mut store = MetricStore::new();
    // A metric whose normalized name collides with a different source name.
    store.ingest_samples(
        "http.server.duration",
        vec![("service".into(), "a".into())],
        vec![Sample {
            timestamp_ms: 1,
            value: 1.0,
            ingest_seq: 0,
        }],
    );
    // Register the normalized name against its source name.
    store
        .register_metric_name("http_server_duration", "http.server.duration")
        .unwrap();

    // Clearing service "a" should drop the now-orphaned registration.
    store.clear_service("a");
    assert!(
        store.normalized_name_sources.is_empty(),
        "normalized_name_sources should be pruned after clearing the only service"
    );

    // A different source name normalizing to the same key must not be
    // falsely rejected as a collision.
    assert!(
        store
            .check_metric_name_collision("http_server_duration", "http.client.duration")
            .is_ok(),
        "no false collision after clear_service pruned the stale registration"
    );
}
