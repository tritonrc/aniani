use super::aggregation::scalar_to_count;
use super::selectors::compute_rate_like;
use super::*;
use crate::store::metric_store::{MetricStore, Sample};

fn make_store() -> MetricStore {
    let mut store = MetricStore::new();
    for i in 0..10 {
        store.ingest_samples(
            "http_requests_total",
            vec![
                ("method".into(), "GET".into()),
                ("service".into(), "api".into()),
            ],
            vec![Sample {
                timestamp_ms: i * 1000,
                value: (i * 10) as f64,
            }],
        );
        store.ingest_samples(
            "http_requests_total",
            vec![
                ("method".into(), "POST".into()),
                ("service".into(), "api".into()),
            ],
            vec![Sample {
                timestamp_ms: i * 1000,
                value: (i * 5) as f64,
            }],
        );
    }
    store.ingest_samples(
        "memory_usage_bytes",
        vec![("service".into(), "api".into())],
        vec![Sample {
            timestamp_ms: 5000,
            value: 1_000_000.0,
        }],
    );
    store
}

#[test]
fn test_instant_selector() {
    let store = make_store();
    let result = evaluate_instant(r#"http_requests_total{method="GET"}"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 90.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_rate() {
    let store = make_store();
    let result = evaluate_instant(
        r#"rate(http_requests_total{method="GET"}[10s])"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let rate = series[0].samples[0].1;
            assert!((rate - 10.0).abs() < 0.01, "rate was {}", rate);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_sum_by() {
    let store = make_store();
    let result =
        evaluate_instant(r#"sum(http_requests_total) by (service)"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 135.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_avg() {
    let store = make_store();
    let result = evaluate_instant(r#"avg(http_requests_total)"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 67.5);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_binary_scalar() {
    let store = make_store();
    let result =
        evaluate_instant(r#"http_requests_total{method="GET"} / 10"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series[0].samples[0].1, 9.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_binary_vector_matches_by_labels_without_metric_name() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "metric_a",
        vec![("service".into(), "api".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 7.0,
        }],
    );
    store.ingest_samples(
        "metric_b",
        vec![("service".into(), "api".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 2.0,
        }],
    );

    let result = evaluate_instant("metric_a - metric_b", &store, 1000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].labels, vec![("service".into(), "api".into())]);
            assert_eq!(series[0].samples[0].1, 5.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_scalar_to_count_rejects_negative_and_non_finite() {
    assert_eq!(scalar_to_count(-1.0), 0);
    assert_eq!(scalar_to_count(f64::NAN), 0);
    assert_eq!(scalar_to_count(f64::INFINITY), 0);
    assert_eq!(scalar_to_count(2.9), 2);
}

#[test]
fn test_comparison_filter() {
    let store = make_store();
    let result = evaluate_instant(r#"http_requests_total > 50"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 90.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_number_literal() {
    let store = make_store();
    let result = evaluate_instant("42", &store, 9000).unwrap();
    match result {
        PromQLResult::Scalar(v) => assert_eq!(v, 42.0),
        _ => panic!("expected Scalar"),
    }
}

#[test]
fn task_18_label_replace_basic() {
    let store = make_store();
    let result = evaluate_instant(
        r#"label_replace(http_requests_total{method="GET"}, "verb", "$1", "method", "(.*)")"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let verb = series[0]
                .labels
                .iter()
                .find(|(k, _)| k == "verb")
                .map(|(_, v)| v.as_str());
            assert_eq!(verb, Some("GET"));
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn task_18_label_replace_no_match() {
    let store = make_store();
    let result = evaluate_instant(
        r#"label_replace(http_requests_total{method="GET"}, "verb", "$1", "method", "NOMATCH")"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let verb = series[0].labels.iter().find(|(k, _)| k == "verb");
            assert!(verb.is_none());
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn task_18_label_replace_capture_group() {
    let store = make_store();
    let result = evaluate_instant(
        r#"label_replace(http_requests_total{method="POST"}, "short", "$1", "method", "(...).*")"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let short = series[0]
                .labels
                .iter()
                .find(|(k, _)| k == "short")
                .map(|(_, v)| v.as_str());
            assert_eq!(short, Some("POS"));
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn task_18_label_join_basic() {
    let store = make_store();
    let result = evaluate_instant(
        r#"label_join(http_requests_total{method="GET"}, "combined", "-", "method", "service")"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let combined = series[0]
                .labels
                .iter()
                .find(|(k, _)| k == "combined")
                .map(|(_, v)| v.as_str());
            assert_eq!(combined, Some("GET-api"));
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn task_18_label_join_single_source() {
    let store = make_store();
    let result = evaluate_instant(
        r#"label_join(http_requests_total{method="GET"}, "copy", "", "method")"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let copy = series[0]
                .labels
                .iter()
                .find(|(k, _)| k == "copy")
                .map(|(_, v)| v.as_str());
            assert_eq!(copy, Some("GET"));
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn task_18_label_replace_static_replacement() {
    let store = make_store();
    let result = evaluate_instant(
        r#"label_replace(http_requests_total{method="GET"}, "env", "production", "method", ".*")"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let env = series[0]
                .labels
                .iter()
                .find(|(k, _)| k == "env")
                .map(|(_, v)| v.as_str());
            assert_eq!(env, Some("production"));
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_rate_with_counter_reset() {
    let samples = vec![
        Sample {
            timestamp_ms: 1000,
            value: 0.0,
        },
        Sample {
            timestamp_ms: 2000,
            value: 10.0,
        },
        Sample {
            timestamp_ms: 3000,
            value: 20.0,
        },
        Sample {
            timestamp_ms: 4000,
            value: 5.0,
        },
        Sample {
            timestamp_ms: 5000,
            value: 15.0,
        },
    ];
    let result = compute_rate_like("rate", &samples, 5000);
    assert!(result.is_some());
    let val = result.unwrap();
    assert!(
        (val - 8.75).abs() < 0.01,
        "rate should be ~8.75, got {}",
        val
    );
}

#[test]
fn test_rate_no_reset() {
    let samples = vec![
        Sample {
            timestamp_ms: 1000,
            value: 0.0,
        },
        Sample {
            timestamp_ms: 5000,
            value: 100.0,
        },
    ];
    let result = compute_rate_like("rate", &samples, 5000);
    assert!(result.is_some());
    let val = result.unwrap();
    assert!((val - 25.0).abs() < 0.01, "rate should be 25, got {}", val);
}

#[test]
fn test_irate_with_reset() {
    let samples = vec![
        Sample {
            timestamp_ms: 1000,
            value: 100.0,
        },
        Sample {
            timestamp_ms: 2000,
            value: 5.0,
        },
    ];
    let result = compute_rate_like("irate", &samples, 2000);
    assert!(result.is_some());
    let val = result.unwrap();
    assert!((val - 5.0).abs() < 0.01, "irate should be 5, got {}", val);
}
