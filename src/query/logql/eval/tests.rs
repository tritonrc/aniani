
use super::*;
use crate::store::log_store::LogEntry;

fn make_store() -> LogStore {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![
            ("service".into(), "payments".into()),
            ("level".into(), "error".into()),
        ],
        vec![
            LogEntry {
                timestamp_ns: 1_000_000_000,
                line: "connection timeout to bank API".into(),
                ingest_seq: 0,
            },
            LogEntry {
                timestamp_ns: 2_000_000_000,
                line: "retry 1/3 failed".into(),
                ingest_seq: 0,
            },
            LogEntry {
                timestamp_ns: 3_000_000_000,
                line: "healthcheck ok".into(),
                ingest_seq: 0,
            },
        ],
    );
    store.ingest_stream(
        vec![
            ("service".into(), "gateway".into()),
            ("level".into(), "info".into()),
        ],
        vec![LogEntry {
            timestamp_ns: 1_500_000_000,
            line: "request received".into(),
            ingest_seq: 0,
        }],
    );
    store
}

#[test]
fn test_eval_stream_selector() {
    let store = make_store();
    let expr = crate::query::logql::parser::parse_logql(r#"{service="payments"}"#).unwrap();
    let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
    match result {
        LogQLResult::Streams(streams) => {
            assert_eq!(streams.len(), 1);
            assert_eq!(streams[0].entries.len(), 3);
        }
        _ => panic!("expected Streams"),
    }
}

#[test]
fn test_eval_pipeline_filter() {
    let store = make_store();
    let expr =
        crate::query::logql::parser::parse_logql(r#"{service="payments"} |= "timeout""#).unwrap();
    let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
    match result {
        LogQLResult::Streams(streams) => {
            assert_eq!(streams.len(), 1);
            assert_eq!(streams[0].entries.len(), 1);
            assert!(streams[0].entries[0].1.contains("timeout"));
        }
        _ => panic!("expected Streams"),
    }
}

#[test]
fn test_eval_pipeline_not_contains() {
    let store = make_store();
    let expr = crate::query::logql::parser::parse_logql(r#"{service="payments"} != "healthcheck""#)
        .unwrap();
    let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
    match result {
        LogQLResult::Streams(streams) => {
            assert_eq!(streams[0].entries.len(), 2);
        }
        _ => panic!("expected Streams"),
    }
}

#[test]
fn test_limited_eval_applies_pipeline_before_limit() {
    let store = make_store();
    let expr =
        crate::query::logql::parser::parse_logql(r#"{service="payments"} |= "timeout""#).unwrap();
    let result = evaluate_logql_limited(&expr, &store, 0, i64::MAX, None, Some(1));
    match result {
        LogQLResult::Streams(streams) => {
            assert_eq!(streams.len(), 1);
            assert_eq!(streams[0].entries.len(), 1);
            assert!(streams[0].entries[0].1.contains("timeout"));
        }
        _ => panic!("expected Streams"),
    }
}

#[test]
fn test_limited_eval_keeps_newest_entries_globally() {
    let store = make_store();
    let expr = crate::query::logql::parser::parse_logql(r#"{service=~".*"}"#).unwrap();
    let result = evaluate_logql_limited(&expr, &store, 0, i64::MAX, None, Some(2));
    match result {
        LogQLResult::Streams(streams) => {
            let mut timestamps: Vec<i64> = streams
                .iter()
                .flat_map(|stream| stream.entries.iter().map(|(timestamp, _)| *timestamp))
                .collect();
            timestamps.sort_unstable();
            assert_eq!(timestamps, vec![2_000_000_000, 3_000_000_000]);
        }
        _ => panic!("expected Streams"),
    }
}

#[test]
fn test_eval_count_over_time() {
    let store = make_store();
    let expr =
        crate::query::logql::parser::parse_logql(r#"count_over_time({service="payments"}[10s])"#)
            .unwrap();
    let result = evaluate_logql(
        &expr,
        &store,
        3_000_000_000,
        3_000_000_000,
        Some(10_000_000_000),
    );
    match result {
        LogQLResult::Matrix(results) => {
            assert_eq!(results.len(), 1);
            // At t=3s, window [3s-10s, 3s] = [-7s, 3s], should capture all 3 entries
            assert!(results[0].samples[0].1 >= 3.0);
        }
        _ => panic!("expected Matrix"),
    }
}

#[test]
fn test_json_pipeline_passes_non_json_lines_through() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "mix".into())],
        vec![
            LogEntry {
                timestamp_ns: 1_000_000_000,
                line: r#"{"level":"error","msg":"boom"}"#.into(),
                ingest_seq: 0,
            },
            LogEntry {
                timestamp_ns: 2_000_000_000,
                line: "plain text not json".into(),
                ingest_seq: 0,
            },
        ],
    );
    // | json should NOT drop the non-JSON line.
    let expr = crate::query::logql::parser::parse_logql(r#"{service="mix"} | json"#).unwrap();
    let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
    match result {
        LogQLResult::Streams(streams) => {
            assert_eq!(streams.len(), 1, "one stream");
            assert_eq!(
                streams[0].entries.len(),
                2,
                "non-JSON line must pass through | json"
            );
        }
        _ => panic!("expected Streams"),
    }
}
