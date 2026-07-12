use super::*;
use crate::store::log_store::LogEntry;
use smallvec::SmallVec;

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
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
            },
            LogEntry {
                timestamp_ns: 2_000_000_000,
                line: "retry 1/3 failed".into(),
                ingest_seq: 0,
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
            },
            LogEntry {
                timestamp_ns: 3_000_000_000,
                line: "healthcheck ok".into(),
                ingest_seq: 0,
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
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
            trace_id: None,
            span_id: None,
            severity_number: 0,
            severity_text: None,
            attributes: SmallVec::new(),
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
                .flat_map(|stream| {
                    stream
                        .entries
                        .iter()
                        .map(|(timestamp, _, _, _, _)| *timestamp)
                })
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
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
            },
            LogEntry {
                timestamp_ns: 2_000_000_000,
                line: "plain text not json".into(),
                ingest_seq: 0,
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
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

#[test]
fn test_stream_query_carries_trace_id_through_to_entries() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "checkout".into())],
        vec![
            LogEntry {
                timestamp_ns: 1_000_000_000,
                line: "with trace".into(),
                ingest_seq: 0,
                trace_id: Some([
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                    0x0e, 0x0f, 0x10,
                ]),
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
            },
            LogEntry {
                timestamp_ns: 2_000_000_000,
                line: "without trace".into(),
                ingest_seq: 0,
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
            },
        ],
    );
    let expr = crate::query::logql::parser::parse_logql(r#"{service="checkout"}"#).unwrap();
    let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
    match result {
        LogQLResult::Streams(streams) => {
            assert_eq!(streams.len(), 1);
            let entries = &streams[0].entries;
            assert_eq!(entries.len(), 2);
            let with_trace = entries
                .iter()
                .find(|(_, line, _, _, _)| line == "with trace");
            assert_eq!(
                with_trace.and_then(|(_, _, tid, _, _)| tid.as_deref()),
                Some("0102030405060708090a0b0c0d0e0f10")
            );
            let without_trace = entries
                .iter()
                .find(|(_, line, _, _, _)| line == "without trace");
            assert_eq!(
                without_trace.and_then(|(_, _, tid, _, _)| tid.clone()),
                None
            );
        }
        _ => panic!("expected Streams"),
    }
}

#[test]
fn test_limited_stream_query_carries_trace_id_through_the_heap() {
    // The limited (top-N) path routes entries through a BinaryHeap before
    // regrouping them; verify trace_id survives that path too.
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "checkout".into())],
        vec![LogEntry {
            timestamp_ns: 1_000_000_000,
            line: "with trace".into(),
            ingest_seq: 0,
            trace_id: Some([
                0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
                0x88, 0x99,
            ]),
            span_id: None,
            severity_number: 0,
            severity_text: None,
            attributes: SmallVec::new(),
        }],
    );
    let expr = crate::query::logql::parser::parse_logql(r#"{service="checkout"}"#).unwrap();
    let result = evaluate_logql_limited(&expr, &store, 0, i64::MAX, None, Some(10));
    match result {
        LogQLResult::Streams(streams) => {
            assert_eq!(streams.len(), 1);
            assert_eq!(
                streams[0].entries[0].2.as_deref(),
                Some("aabbccddeeff00112233445566778899")
            );
        }
        _ => panic!("expected Streams"),
    }
}
