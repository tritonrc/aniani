use super::*;
use crate::store::LabelMatchOp;

fn make_entry(ts: i64, line: &str) -> LogEntry {
    LogEntry {
        timestamp_ns: ts,
        line: line.to_string(),
        ingest_seq: 0,
        trace_id: None,
        span_id: None,
        attributes: SmallVec::new(),
    }
}

#[test]
fn test_log_entry_deserializes_legacy_shape_without_trace_id() {
    // Simulates a snapshot written before `trace_id` existed: no `trace_id` key
    // at all. `#[serde(default)]` must fill it in as `None` rather than fail.
    let legacy_json = serde_json::json!({
        "timestamp_ns": 1000,
        "line": "hello",
        "ingest_seq": 5,
    });
    let entry: LogEntry = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(entry.timestamp_ns, 1000);
    assert_eq!(entry.line, "hello");
    assert_eq!(entry.ingest_seq, 5);
    assert_eq!(entry.trace_id, None);
}

#[test]
fn test_ingest_and_query_eq() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![
            ("service".into(), "payments".into()),
            ("level".into(), "error".into()),
        ],
        vec![make_entry(1000, "timeout")],
    );
    store.ingest_stream(
        vec![
            ("service".into(), "gateway".into()),
            ("level".into(), "info".into()),
        ],
        vec![make_entry(2000, "ok")],
    );

    let results = store.query_streams(&[LabelMatcher {
        name: "service".into(),
        op: LabelMatchOp::Eq,
        value: "payments".into(),
    }]);
    assert_eq!(results.len(), 1);

    let entries = store.get_entries(results[0], 0, i64::MAX);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].line, "timeout");
}

#[test]
fn test_query_neq() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("level".into(), "error".into())],
        vec![make_entry(1000, "err")],
    );
    store.ingest_stream(
        vec![("level".into(), "info".into())],
        vec![make_entry(2000, "ok")],
    );

    let results = store.query_streams(&[LabelMatcher {
        name: "level".into(),
        op: LabelMatchOp::Neq,
        value: "error".into(),
    }]);
    assert_eq!(results.len(), 1);
    let entries = store.get_entries(results[0], 0, i64::MAX);
    assert_eq!(entries[0].line, "ok");
}

#[test]
fn test_query_regex() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "payments".into())],
        vec![make_entry(1000, "a")],
    );
    store.ingest_stream(
        vec![("service".into(), "gateway".into())],
        vec![make_entry(2000, "b")],
    );
    store.ingest_stream(
        vec![("service".into(), "worker".into())],
        vec![make_entry(3000, "c")],
    );

    let results = store.query_streams(&[LabelMatcher {
        name: "service".into(),
        op: LabelMatchOp::Regex,
        value: "pay.*|gate.*".into(),
    }]);
    assert_eq!(results.len(), 2);
}

#[test]
fn test_query_not_regex() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("level".into(), "debug".into())],
        vec![make_entry(1000, "a")],
    );
    store.ingest_stream(
        vec![("level".into(), "error".into())],
        vec![make_entry(2000, "b")],
    );

    let results = store.query_streams(&[LabelMatcher {
        name: "level".into(),
        op: LabelMatchOp::NotRegex,
        value: "debug".into(),
    }]);
    assert_eq!(results.len(), 1);
}

#[test]
fn test_eviction() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "a".into())],
        vec![make_entry(1000, "old"), make_entry(5000, "new")],
    );
    assert_eq!(store.total_entries, 2);
    store.evict_before(3000);
    assert_eq!(store.total_entries, 1);
}

#[test]
fn test_evict_to_max() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "a".into())],
        vec![
            make_entry(1000, "1"),
            make_entry(2000, "2"),
            make_entry(3000, "3"),
        ],
    );
    assert_eq!(store.total_entries, 3);
    store.evict_to_max(1);
    assert_eq!(store.total_entries, 1);
}

#[test]
fn test_label_names_and_values() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![
            ("service".into(), "payments".into()),
            ("level".into(), "error".into()),
        ],
        vec![make_entry(1000, "x")],
    );
    let names = store.label_names();
    assert!(names.contains(&"service".to_string()));
    assert!(names.contains(&"level".to_string()));

    let values = store.get_label_values("service");
    assert_eq!(values, vec!["payments".to_string()]);
}

#[test]
fn test_neq_matches_missing_label() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![
            ("service".into(), "payments".into()),
            ("level".into(), "error".into()),
        ],
        vec![make_entry(1000, "err")],
    );
    store.ingest_stream(
        vec![("service".into(), "gateway".into())], // no "level" label
        vec![make_entry(2000, "ok")],
    );
    // {level!="error"} should match gateway (missing label)
    let results = store.query_streams(&[LabelMatcher {
        name: "level".into(),
        op: LabelMatchOp::Neq,
        value: "error".into(),
    }]);
    assert_eq!(results.len(), 1);
    let entries = store.get_entries(results[0], 0, i64::MAX);
    assert_eq!(entries[0].line, "ok");
}

#[test]
fn test_not_regex_matches_missing_label() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![
            ("service".into(), "payments".into()),
            ("level".into(), "debug".into()),
        ],
        vec![make_entry(1000, "dbg")],
    );
    store.ingest_stream(
        vec![("service".into(), "gateway".into())], // no "level" label
        vec![make_entry(2000, "ok")],
    );
    // {level!~"debug"} should match gateway (missing label)
    let results = store.query_streams(&[LabelMatcher {
        name: "level".into(),
        op: LabelMatchOp::NotRegex,
        value: "debug".into(),
    }]);
    assert_eq!(results.len(), 1);
    let entries = store.get_entries(results[0], 0, i64::MAX);
    assert_eq!(entries[0].line, "ok");
}

#[test]
fn test_out_of_order_ingest() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "a".into())],
        vec![
            make_entry(3000, "third"),
            make_entry(1000, "first"),
            make_entry(2000, "second"),
        ],
    );
    let ids = store.query_streams(&[LabelMatcher {
        name: "service".into(),
        op: LabelMatchOp::Eq,
        value: "a".into(),
    }]);
    // Verify entries are sorted and range queries work correctly
    let entries = store.get_entries(ids[0], 0, i64::MAX);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].line, "first");
    assert_eq!(entries[1].line, "second");
    assert_eq!(entries[2].line, "third");

    // Verify partition_point works for range queries
    let entries = store.get_entries(ids[0], 1500, 2500);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].line, "second");
}

#[test]
fn test_get_entries_time_range() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "a".into())],
        vec![
            make_entry(1000, "1"),
            make_entry(2000, "2"),
            make_entry(3000, "3"),
        ],
    );
    let ids = store.query_streams(&[LabelMatcher {
        name: "service".into(),
        op: LabelMatchOp::Eq,
        value: "a".into(),
    }]);
    let entries = store.get_entries(ids[0], 1500, 2500);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].line, "2");
}

#[test]
fn test_eviction_prunes_label_values() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "payments".into())],
        vec![make_entry(1000, "test")],
    );
    assert!(!store.label_names().is_empty());
    store.evict_before(2000);
    assert!(store.label_names().is_empty());
    assert!(store.get_label_values("service").is_empty());
}

#[test]
fn test_internally_unsorted_batch_after_tail() {
    let mut store = LogStore::new();
    store.ingest_stream(
        vec![("service".into(), "test".into())],
        vec![LogEntry {
            timestamp_ns: 100,
            line: "a".into(),
            ingest_seq: 0,
            trace_id: None,
            span_id: None,
            attributes: SmallVec::new(),
        }],
    );
    // Append internally unsorted batch — all > 100
    store.ingest_stream(
        vec![("service".into(), "test".into())],
        vec![
            LogEntry {
                timestamp_ns: 300,
                line: "c".into(),
                ingest_seq: 0,
                trace_id: None,
                span_id: None,
                attributes: SmallVec::new(),
            },
            LogEntry {
                timestamp_ns: 200,
                line: "b".into(),
                ingest_seq: 0,
                trace_id: None,
                span_id: None,
                attributes: SmallVec::new(),
            },
        ],
    );
    // Verify sorted
    for stream in store.streams.values() {
        for w in stream.entries.windows(2) {
            assert!(
                w[0].timestamp_ns <= w[1].timestamp_ns,
                "entries must be sorted: {} > {}",
                w[0].timestamp_ns,
                w[1].timestamp_ns
            );
        }
    }
}
