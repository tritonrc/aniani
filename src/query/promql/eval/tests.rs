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
                ingest_seq: 0,
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
                ingest_seq: 0,
            }],
        );
    }
    store.ingest_samples(
        "memory_usage_bytes",
        vec![("service".into(), "api".into())],
        vec![Sample {
            timestamp_ms: 5000,
            value: 1_000_000.0,
            ingest_seq: 0,
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
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "metric_b",
        vec![("service".into(), "api".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 2.0,
            ingest_seq: 0,
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
            ingest_seq: 0,
        },
        Sample {
            timestamp_ms: 2000,
            value: 10.0,
            ingest_seq: 0,
        },
        Sample {
            timestamp_ms: 3000,
            value: 20.0,
            ingest_seq: 0,
        },
        Sample {
            timestamp_ms: 4000,
            value: 5.0,
            ingest_seq: 0,
        },
        Sample {
            timestamp_ms: 5000,
            value: 15.0,
            ingest_seq: 0,
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
            ingest_seq: 0,
        },
        Sample {
            timestamp_ms: 5000,
            value: 100.0,
            ingest_seq: 0,
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
            ingest_seq: 0,
        },
        Sample {
            timestamp_ms: 2000,
            value: 5.0,
            ingest_seq: 0,
        },
    ];
    let result = compute_rate_like("irate", &samples, 2000);
    assert!(result.is_some());
    let val = result.unwrap();
    assert!((val - 5.0).abs() < 0.01, "irate should be 5, got {}", val);
}

// ---------------------------------------------------------------------------
// Tests for metrics-improvements: binary modifiers, aggregations, over_time,
// math functions, utility functions, comparison bool modifier.
// ---------------------------------------------------------------------------

#[test]
fn test_on_modifier_matches_on_specified_labels() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "metric_a",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "prod".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 10.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "metric_b",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "staging".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 3.0,
            ingest_seq: 0,
        }],
    );
    // on(service) should match despite different env labels.
    let result = evaluate_instant(r#"metric_a * on(service) metric_b"#, &store, 1000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1, "should match 1 series via on(service)");
            assert!((series[0].samples[0].1 - 30.0).abs() < 0.01);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_ignoring_modifier() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "metric_a",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "prod".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 10.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "metric_b",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "staging".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 3.0,
            ingest_seq: 0,
        }],
    );
    // ignoring(env) matches on all labels except __name__ and env.
    let result = evaluate_instant(r#"metric_a * ignoring(env) metric_b"#, &store, 1000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert!((series[0].samples[0].1 - 30.0).abs() < 0.01);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_comparison_bool_modifier() {
    let store = make_store();
    // With bool, returns 1.0 or 0.0 instead of filtering.
    let result = evaluate_instant(r#"http_requests_total > bool 50"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            // Both series present (GET=90 → 1.0, POST=45 → 0.0).
            assert_eq!(series.len(), 2);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_stddev_aggregation() {
    let store = make_store();
    let result = evaluate_instant(r#"stddev(http_requests_total)"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let val = series[0].samples[0].1;
            // GET=90, POST=45. mean=67.5, stddev = sqrt(((22.5)^2 + (22.5)^2)/2) = 22.5
            assert!((val - 22.5).abs() < 0.1, "stddev was {val}");
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_group_aggregation() {
    let store = make_store();
    let result = evaluate_instant(r#"group(http_requests_total)"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 1.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_quantile_aggregation() {
    let store = make_store();
    // quantile(0.5, ...) = median of [45, 90] = 67.5 (linear interpolation)
    let result = evaluate_instant(r#"quantile(0.5, http_requests_total)"#, &store, 9000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let val = series[0].samples[0].1;
            assert!(
                (val - 67.5).abs() < 0.1,
                "quantile(0.5) should be 67.5, got {val}"
            );
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_avg_over_time() {
    let store = make_store();
    let result = evaluate_instant(
        r#"avg_over_time(http_requests_total{method="GET"}[10s])"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            let val = series[0].samples[0].1;
            // values 0..90, avg = 45.0
            assert!((val - 45.0).abs() < 0.1, "avg_over_time was {val}");
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_max_over_time() {
    let store = make_store();
    let result = evaluate_instant(
        r#"max_over_time(http_requests_total{method="GET"}[10s])"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 90.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_count_over_time() {
    let store = make_store();
    let result = evaluate_instant(
        r#"count_over_time(http_requests_total{method="GET"}[10s])"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 10.0); // 10 samples at t=0..9s
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_last_over_time() {
    let store = make_store();
    let result = evaluate_instant(
        r#"last_over_time(http_requests_total{method="GET"}[10s])"#,
        &store,
        9000,
    )
    .unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].samples[0].1, 90.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_math_functions() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "v",
        vec![],
        vec![Sample {
            timestamp_ms: 1000,
            value: 16.0,
            ingest_seq: 0,
        }],
    );
    let check = |q: &str, expected: f64| {
        let result = evaluate_instant(q, &store, 1000).unwrap();
        match result {
            PromQLResult::InstantVector(s) => {
                assert!(
                    (s[0].samples[0].1 - expected).abs() < 0.01,
                    "{q} expected {expected}, got {}",
                    s[0].samples[0].1
                );
            }
            _ => panic!("{q} expected InstantVector"),
        }
    };
    check("sqrt(v)", 4.0);
    check("ln(v)", 16.0f64.ln());
    check("log2(v)", 4.0);
    check("log10(v)", 16.0f64.log10());
    check("exp(v)", 16.0f64.exp());
    check("sgn(v)", 1.0);
}

#[test]
fn test_timestamp_function() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "v",
        vec![],
        vec![Sample {
            timestamp_ms: 5000,
            value: 42.0,
            ingest_seq: 0,
        }],
    );
    let result = evaluate_instant("timestamp(v)", &store, 5000).unwrap();
    match result {
        PromQLResult::InstantVector(s) => {
            assert!(
                (s[0].samples[0].1 - 5.0).abs() < 0.01,
                "timestamp should be 5.0"
            );
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_changes_function() {
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
                timestamp_ms: 2000,
                value: 1.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 3000,
                value: 2.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 4000,
                value: 2.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 5000,
                value: 3.0,
                ingest_seq: 0,
            },
        ],
    );
    let result = evaluate_instant("changes(m[10s])", &store, 5000).unwrap();
    match result {
        PromQLResult::InstantVector(s) => {
            // Changes: 1→1 (no), 1→2 (yes), 2→2 (no), 2→3 (yes) = 2
            assert_eq!(s[0].samples[0].1, 2.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_resets_function() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "m",
        vec![],
        vec![
            Sample {
                timestamp_ms: 1000,
                value: 5.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 2000,
                value: 3.0,
                ingest_seq: 0,
            }, // reset (decrease)
            Sample {
                timestamp_ms: 3000,
                value: 8.0,
                ingest_seq: 0,
            },
            Sample {
                timestamp_ms: 4000,
                value: 2.0,
                ingest_seq: 0,
            }, // reset
        ],
    );
    let result = evaluate_instant("resets(m[10s])", &store, 4000).unwrap();
    match result {
        PromQLResult::InstantVector(s) => {
            assert_eq!(s[0].samples[0].1, 2.0);
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_and_set_operator() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "m",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "prod".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 10.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "m",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "staging".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 20.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "m",
        vec![
            ("service".into(), "web".into()),
            ("env".into(), "prod".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 30.0,
            ingest_seq: 0,
        }],
    );
    let result = evaluate_instant(r#"m{env="prod"} and m{service="api"}"#, &store, 1000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1, "and: should match 1 series");
            assert_eq!(series[0].samples[0].1, 10.0, "and: should keep LHS value");
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_or_set_operator() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "a",
        vec![("service".into(), "api".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 10.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "b",
        vec![("service".into(), "web".into())],
        vec![Sample {
            timestamp_ms: 1000,
            value: 20.0,
            ingest_seq: 0,
        }],
    );
    let result = evaluate_instant(r#"a or b"#, &store, 1000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 2, "or: should return 2 series (union)");
        }
        _ => panic!("expected InstantVector"),
    }
}

#[test]
fn test_unless_set_operator() {
    let mut store = MetricStore::new();
    store.ingest_samples(
        "a",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "prod".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 10.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "a",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "staging".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 20.0,
            ingest_seq: 0,
        }],
    );
    store.ingest_samples(
        "b",
        vec![
            ("service".into(), "api".into()),
            ("env".into(), "staging".into()),
        ],
        vec![Sample {
            timestamp_ms: 1000,
            value: 30.0,
            ingest_seq: 0,
        }],
    );
    let result = evaluate_instant(r#"a unless on(service, env) b"#, &store, 1000).unwrap();
    match result {
        PromQLResult::InstantVector(series) => {
            assert_eq!(series.len(), 1, "unless: should return 1 unmatched series");
            assert_eq!(series[0].samples[0].1, 10.0);
        }
        _ => panic!("expected InstantVector"),
    }
}
