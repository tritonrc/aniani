
use super::*;

#[allow(clippy::too_many_arguments)]
fn make_span(
    interner: &mut Rodeo,
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent: Option<[u8; 8]>,
    name: &str,
    service: &str,
    start: i64,
    duration: i64,
    status: SpanStatus,
) -> Span {
    Span {
        trace_id,
        span_id,
        parent_span_id: parent,
        name: interner.get_or_intern(name),
        service_name: interner.get_or_intern(service),
        start_time_ns: start,
        duration_ns: duration,
        status,
        kind: SpanKind::Unspecified,
        attributes: SmallVec::new(),
        events: Vec::new(),
        ingest_seq: 0,
    }
}

#[test]
fn test_ingest_and_get_trace() {
    let mut store = TraceStore::new();
    let tid = [1u8; 16];
    let span = make_span(
        &mut store.interner,
        tid,
        [1u8; 8],
        None,
        "GET /api",
        "gateway",
        1000,
        500,
        SpanStatus::Ok,
    );
    store.ingest_spans(vec![span]);

    let trace = store.get_trace(&tid).unwrap();
    assert_eq!(trace.len(), 1);
    assert_eq!(store.resolve(&trace[0].name), "GET /api");
}

#[test]
fn test_service_index() {
    let mut store = TraceStore::new();
    let tid1 = [1u8; 16];
    let tid2 = [2u8; 16];
    let span1 = make_span(
        &mut store.interner,
        tid1,
        [1u8; 8],
        None,
        "op1",
        "payments",
        1000,
        100,
        SpanStatus::Ok,
    );
    let span2 = make_span(
        &mut store.interner,
        tid2,
        [2u8; 8],
        None,
        "op2",
        "gateway",
        2000,
        200,
        SpanStatus::Ok,
    );
    store.ingest_spans(vec![span1, span2]);

    let traces = store.traces_for_service("payments");
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0], tid1);
}

#[test]
fn test_service_names() {
    let mut store = TraceStore::new();
    let span = make_span(
        &mut store.interner,
        [1u8; 16],
        [1u8; 8],
        None,
        "op",
        "myservice",
        1000,
        100,
        SpanStatus::Ok,
    );
    store.ingest_spans(vec![span]);

    let names = store.service_names();
    assert_eq!(names, vec!["myservice".to_string()]);
}

#[test]
fn test_eviction() {
    let mut store = TraceStore::new();
    let span1 = make_span(
        &mut store.interner,
        [1u8; 16],
        [1u8; 8],
        None,
        "old",
        "svc",
        1000,
        100,
        SpanStatus::Ok,
    );
    let span2 = make_span(
        &mut store.interner,
        [2u8; 16],
        [2u8; 8],
        None,
        "new",
        "svc",
        5000,
        100,
        SpanStatus::Ok,
    );
    store.ingest_spans(vec![span1, span2]);
    assert_eq!(store.total_spans, 2);

    store.evict_before(3000);
    assert_eq!(store.total_spans, 1);
    assert!(store.get_trace(&[1u8; 16]).is_none());
    assert!(store.get_trace(&[2u8; 16]).is_some());
}

#[test]
fn test_evict_to_max() {
    let mut store = TraceStore::new();
    let span1 = make_span(
        &mut store.interner,
        [1u8; 16],
        [1u8; 8],
        None,
        "old",
        "svc",
        1000,
        100,
        SpanStatus::Ok,
    );
    let span2 = make_span(
        &mut store.interner,
        [2u8; 16],
        [2u8; 8],
        None,
        "new",
        "svc",
        5000,
        100,
        SpanStatus::Ok,
    );
    let span3 = make_span(
        &mut store.interner,
        [2u8; 16],
        [3u8; 8],
        Some([2u8; 8]),
        "child",
        "svc",
        5100,
        50,
        SpanStatus::Ok,
    );
    store.ingest_spans(vec![span1, span2, span3]);
    assert_eq!(store.total_spans, 3);
    // Evict to max 2 should remove the oldest trace (1 span)
    store.evict_to_max(2);
    assert_eq!(store.total_spans, 2);
    assert!(store.get_trace(&[1u8; 16]).is_none());
    assert!(store.get_trace(&[2u8; 16]).is_some());
}

#[test]
fn test_trace_result() {
    let mut store = TraceStore::new();
    let tid = [1u8; 16];
    let root = make_span(
        &mut store.interner,
        tid,
        [1u8; 8],
        None,
        "root",
        "svc",
        1000,
        500,
        SpanStatus::Ok,
    );
    let child = make_span(
        &mut store.interner,
        tid,
        [2u8; 8],
        Some([1u8; 8]),
        "child",
        "svc",
        1100,
        200,
        SpanStatus::Ok,
    );
    store.ingest_spans(vec![root, child]);

    let result = store.trace_result(&tid).unwrap();
    assert_eq!(result.root_span_name, "root");
    assert_eq!(result.span_count, 2);
    assert_eq!(result.start_time_ns, 1000);
    assert_eq!(result.duration_ns, 500); // 1000+500=1500 - 1000
}

#[test]
fn test_recent_traces_returns_most_recent_first() {
    let mut store = TraceStore::new();

    let old = make_span(
        &mut store.interner,
        [1u8; 16],
        [1u8; 8],
        None,
        "old-span",
        "svc",
        1000,
        100,
        SpanStatus::Ok,
    );
    let mid = make_span(
        &mut store.interner,
        [2u8; 16],
        [2u8; 8],
        None,
        "mid-span",
        "svc",
        5000,
        100,
        SpanStatus::Ok,
    );
    let new = make_span(
        &mut store.interner,
        [3u8; 16],
        [3u8; 8],
        None,
        "new-span",
        "svc",
        9000,
        100,
        SpanStatus::Ok,
    );
    store.ingest_spans(vec![old, mid, new]);

    let recent = store.recent_traces(10);
    assert_eq!(recent.len(), 3);
    // Most recent first
    assert_eq!(recent[0].root_span_name, "new-span");
    assert_eq!(recent[1].root_span_name, "mid-span");
    assert_eq!(recent[2].root_span_name, "old-span");
}

#[test]
fn test_recent_traces_respects_limit() {
    let mut store = TraceStore::new();

    for i in 0..5u8 {
        let span = make_span(
            &mut store.interner,
            [i; 16],
            [i; 8],
            None,
            &format!("span-{}", i),
            "svc",
            i as i64 * 1000,
            100,
            SpanStatus::Ok,
        );
        store.ingest_spans(vec![span]);
    }

    let recent = store.recent_traces(2);
    assert_eq!(recent.len(), 2);
    // Most recent two
    assert_eq!(recent[0].start_time_ns, 4000);
    assert_eq!(recent[1].start_time_ns, 3000);
}

#[test]
fn test_recent_traces_empty_store() {
    let store = TraceStore::new();
    let recent = store.recent_traces(10);
    assert!(recent.is_empty());
}

#[test]
fn test_clear_service_preserves_other_service_spans() {
    let mut store = TraceStore::new();
    let trace_id = [1u8; 16];

    let svc_a = store.interner.get_or_intern("service-a");
    let svc_b = store.interner.get_or_intern("service-b");
    let name_a = store.interner.get_or_intern("span-a");
    let name_b = store.interner.get_or_intern("span-b");

    store.ingest_spans(vec![
        Span {
            trace_id,
            span_id: [1u8; 8],
            parent_span_id: None,
            name: name_a,
            service_name: svc_a,
            start_time_ns: 1000,
            duration_ns: 100,
            status: SpanStatus::Ok,
            kind: SpanKind::Unspecified,
            attributes: SmallVec::new(),
            events: Vec::new(),
            ingest_seq: 0,
        },
        Span {
            trace_id,
            span_id: [2u8; 8],
            parent_span_id: Some([1u8; 8]),
            name: name_b,
            service_name: svc_b,
            start_time_ns: 1050,
            duration_ns: 50,
            status: SpanStatus::Ok,
            kind: SpanKind::Unspecified,
            attributes: SmallVec::new(),
            events: Vec::new(),
            ingest_seq: 0,
        },
    ]);

    assert_eq!(store.total_spans, 2);

    store.clear_service("service-a");

    assert_eq!(
        store.total_spans, 1,
        "only service-a span should be removed"
    );
    let spans = store.get_trace(&trace_id);
    assert!(spans.is_some(), "trace should still exist");
    assert_eq!(spans.unwrap().len(), 1);
    assert_eq!(spans.unwrap()[0].service_name, svc_b);

    // service-a should be gone from index
    assert!(!store.service_index.contains_key(&svc_a));
    // service-b should still be in index
    assert!(store.service_index.contains_key(&svc_b));
}

#[test]
fn test_evict_partial_cleans_stale_indexes() {
    let mut store = TraceStore::new();
    let trace_id = [1u8; 16];

    let svc = store.interner.get_or_intern("my-svc");
    let old_name = store.interner.get_or_intern("old-span");
    let new_name = store.interner.get_or_intern("new-span");

    store.ingest_spans(vec![
        Span {
            trace_id,
            span_id: [1u8; 8],
            parent_span_id: None,
            name: old_name,
            service_name: svc,
            start_time_ns: 100,
            duration_ns: 10,
            status: SpanStatus::Error,
            kind: SpanKind::Unspecified,
            attributes: SmallVec::new(),
            events: Vec::new(),
            ingest_seq: 0,
        },
        Span {
            trace_id,
            span_id: [2u8; 8],
            parent_span_id: Some([1u8; 8]),
            name: new_name,
            service_name: svc,
            start_time_ns: 1000,
            duration_ns: 10,
            status: SpanStatus::Ok,
            kind: SpanKind::Unspecified,
            attributes: SmallVec::new(),
            events: Vec::new(),
            ingest_seq: 0,
        },
    ]);

    // Evict spans older than 500
    store.evict_before(500);

    assert_eq!(store.total_spans, 1);
    // Error status should be gone from index
    let error_traces = store.status_index.get(&SpanStatus::Error);
    assert!(
        error_traces.is_none() || error_traces.unwrap().is_empty(),
        "error status should be removed after partial eviction"
    );
    // Old span name should be gone
    let old_name_traces = store.name_index.get(&old_name);
    assert!(
        old_name_traces.is_none() || old_name_traces.unwrap().is_empty(),
        "old span name should be removed after partial eviction"
    );
    // New span should still be indexed
    assert!(store.name_index.contains_key(&new_name));
}
