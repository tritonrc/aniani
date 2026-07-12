use super::*;
use crate::store::trace_store::{AttributeValue, Span, SpanKind, SpanStatus};
use smallvec::SmallVec;

fn make_store() -> TraceStore {
    let mut store = TraceStore::new();
    let tid1 = [1u8; 16];
    let tid2 = [2u8; 16];

    // Trace 1: gateway -> payments (parent-child)
    let gateway_svc = store.interner.get_or_intern("gateway");
    let payments_svc = store.interner.get_or_intern("payments");
    let get_op = store.interner.get_or_intern("GET /api");
    let process_op = store.interner.get_or_intern("process_payment");

    let res_svc_key = store.interner.get_or_intern("resource.service.name");
    let gateway_val = store.interner.get_or_intern("gateway");
    let payments_val = store.interner.get_or_intern("payments");

    let span1 = Span {
        trace_id: tid1,
        span_id: [1, 0, 0, 0, 0, 0, 0, 0],
        parent_span_id: None,
        name: get_op,
        service_name: gateway_svc,
        start_time_ns: 1_000_000_000,
        duration_ns: 500_000_000, // 500ms
        status: SpanStatus::Ok,
        status_message: None,
        attributes: SmallVec::from_vec(vec![(res_svc_key, AttributeValue::String(gateway_val))]),
        kind: SpanKind::Unspecified,
        events: Vec::new(),
        links: Vec::new(),
        ingest_seq: 0,
    };
    let span2 = Span {
        trace_id: tid1,
        span_id: [2, 0, 0, 0, 0, 0, 0, 0],
        parent_span_id: Some([1, 0, 0, 0, 0, 0, 0, 0]),
        name: process_op,
        service_name: payments_svc,
        start_time_ns: 1_100_000_000,
        duration_ns: 300_000_000, // 300ms
        status: SpanStatus::Error,
        status_message: None,
        attributes: SmallVec::from_vec(vec![(res_svc_key, AttributeValue::String(payments_val))]),
        kind: SpanKind::Unspecified,
        events: Vec::new(),
        links: Vec::new(),
        ingest_seq: 0,
    };

    // Trace 2: slow span
    let slow_op = store.interner.get_or_intern("slow_query");
    let span3 = Span {
        trace_id: tid2,
        span_id: [3, 0, 0, 0, 0, 0, 0, 0],
        parent_span_id: None,
        name: slow_op,
        service_name: payments_svc,
        start_time_ns: 2_000_000_000,
        duration_ns: 2_000_000_000, // 2s
        status: SpanStatus::Ok,
        status_message: None,
        attributes: SmallVec::from_vec(vec![(res_svc_key, AttributeValue::String(payments_val))]),
        kind: SpanKind::Unspecified,
        events: Vec::new(),
        links: Vec::new(),
        ingest_seq: 0,
    };

    store.ingest_spans(vec![span1, span2, span3]);
    store
}

#[test]
fn test_eval_by_service() {
    let store = make_store();
    let expr =
        crate::query::traceql::parser::parse_traceql(r#"{ resource.service.name = "gateway" }"#)
            .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans[0].service_name, "gateway");
}

#[test]
fn test_eval_by_duration() {
    let store = make_store();
    let expr = crate::query::traceql::parser::parse_traceql(r#"{ duration > 1s }"#).unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans[0].name, "slow_query");
}

#[test]
fn test_eval_by_status() {
    let store = make_store();
    let expr = crate::query::traceql::parser::parse_traceql(r#"{ status = error }"#).unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans[0].name, "process_payment");
}

#[test]
fn test_eval_or_query_uses_full_scan_correctly() {
    // OR forces the non-indexed full-scan path; ensure it still returns the
    // right spans. Trace 1 has gateway (span1) and an error (span2); trace 2
    // matches neither side.
    let store = make_store();
    let expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "gateway" || status = error }"#,
    )
    .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    // Both gateway and error spans of trace 1 match.
    assert_eq!(results[0].matched_spans.len(), 2);
}

#[test]
fn test_eval_indexed_conjunction_narrows_correctly() {
    // A service + status conjunction is indexable on both; verify the
    // narrowed candidate set still produces exact results.
    let store = make_store();
    let expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "payments" && status = error }"#,
    )
    .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans[0].name, "process_payment");
}

#[test]
fn test_eval_by_string_attribute_uses_attr_index() {
    let mut store = TraceStore::new();
    let tid1 = [10u8; 16];
    let tid2 = [11u8; 16];
    let svc = store.interner.get_or_intern("svc");
    let op = store.interner.get_or_intern("handler");
    let status_key = store.interner.get_or_intern("span.http.status_code");
    let v500 = store.interner.get_or_intern("500");
    let v200 = store.interner.get_or_intern("200");
    let mk = |tid, span_id, val| Span {
        trace_id: tid,
        span_id,
        parent_span_id: None,
        name: op,
        service_name: svc,
        start_time_ns: 0,
        duration_ns: 10,
        status: SpanStatus::Ok,
        status_message: None,
        attributes: SmallVec::from_vec(vec![(status_key, AttributeValue::String(val))]),
        events: Vec::new(),
        links: Vec::new(),
        kind: SpanKind::Server,
        ingest_seq: 0,
    };
    store.ingest_spans(vec![
        mk(tid1, [1, 0, 0, 0, 0, 0, 0, 0], v500),
        mk(tid2, [2, 0, 0, 0, 0, 0, 0, 0], v200),
    ]);

    let expr = crate::query::traceql::parser::parse_traceql(r#"{ span.http.status_code = "500" }"#)
        .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].trace_id, tid1);
}

#[test]
fn test_eval_structural_descendant() {
    let store = make_store();
    let expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "gateway" } >> { resource.service.name = "payments" }"#,
    )
    .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    // The matched span should be the payments span (descendant)
    assert_eq!(results[0].matched_spans[0].service_name, "payments");
}

#[test]
fn test_eval_structural_child() {
    let store = make_store();
    // span1 (gateway) is the direct parent of span2 (payments).
    let expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "gateway" } > { resource.service.name = "payments" }"#,
    )
    .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans[0].service_name, "payments");
}

#[test]
fn test_eval_structural_sibling() {
    let mut store = TraceStore::new();
    let tid = [6u8; 16];
    let svc = store.interner.get_or_intern("svc");
    let res_key = store.interner.get_or_intern("resource.service.name");
    let parent_op = store.interner.get_or_intern("parent");
    let a_op = store.interner.get_or_intern("child-a");
    let b_op = store.interner.get_or_intern("child-b");
    let unrelated_op = store.interner.get_or_intern("unrelated");
    let root_val = store.interner.get_or_intern("root");
    let a_val = store.interner.get_or_intern("a");
    let b_val = store.interner.get_or_intern("b");
    let c_val = store.interner.get_or_intern("c");
    // Two siblings under the same parent, plus an unrelated root span.
    store.ingest_spans(vec![
        Span {
            trace_id: tid,
            span_id: [0, 0, 0, 0, 0, 0, 0, 0],
            parent_span_id: None,
            name: parent_op,
            service_name: svc,
            start_time_ns: 0,
            duration_ns: 100,
            status: SpanStatus::Ok,
            status_message: None,
            attributes: SmallVec::from_vec(vec![(res_key, AttributeValue::String(root_val))]),
            events: Vec::new(),
            links: Vec::new(),
            kind: SpanKind::Server,
            ingest_seq: 0,
        },
        Span {
            trace_id: tid,
            span_id: [1, 0, 0, 0, 0, 0, 0, 0],
            parent_span_id: Some([0, 0, 0, 0, 0, 0, 0, 0]),
            name: a_op,
            service_name: svc,
            start_time_ns: 1,
            duration_ns: 10,
            status: SpanStatus::Ok,
            status_message: None,
            attributes: SmallVec::from_vec(vec![(res_key, AttributeValue::String(a_val))]),
            events: Vec::new(),
            links: Vec::new(),
            kind: SpanKind::Internal,
            ingest_seq: 0,
        },
        Span {
            trace_id: tid,
            span_id: [2, 0, 0, 0, 0, 0, 0, 0],
            parent_span_id: Some([0, 0, 0, 0, 0, 0, 0, 0]),
            name: b_op,
            service_name: svc,
            start_time_ns: 2,
            duration_ns: 10,
            status: SpanStatus::Ok,
            status_message: None,
            attributes: SmallVec::from_vec(vec![(res_key, AttributeValue::String(b_val))]),
            events: Vec::new(),
            links: Vec::new(),
            kind: SpanKind::Internal,
            ingest_seq: 0,
        },
        Span {
            trace_id: tid,
            span_id: [3, 0, 0, 0, 0, 0, 0, 0],
            parent_span_id: None,
            name: unrelated_op,
            service_name: svc,
            start_time_ns: 3,
            duration_ns: 10,
            status: SpanStatus::Ok,
            status_message: None,
            attributes: SmallVec::from_vec(vec![(res_key, AttributeValue::String(c_val))]),
            events: Vec::new(),
            links: Vec::new(),
            kind: SpanKind::Internal,
            ingest_seq: 0,
        },
    ]);

    // a ~ b: child-a and child-b are siblings (same parent). Matches child-b.
    let expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "a" } ~ { resource.service.name = "b" }"#,
    )
    .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans.len(), 1);
    assert_eq!(results[0].matched_spans[0].service_name, "svc");
    assert_eq!(results[0].matched_spans[0].name, "child-b");

    // root ~ c share parent=None, so they are siblings too.
    let root_expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "root" } ~ { resource.service.name = "c" }"#,
    )
    .unwrap();
    let root_results = evaluate_traceql(&root_expr, &store);
    assert_eq!(root_results.len(), 1);
}

#[test]
fn test_eval_combined_conditions() {
    let store = make_store();
    let expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "payments" && duration > 1s }"#,
    )
    .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans[0].name, "slow_query");
}

#[test]
fn test_eval_empty_selector_matches_all() {
    let store = make_store();
    let expr = crate::query::traceql::parser::parse_traceql("{}").unwrap();
    let results = evaluate_traceql(&expr, &store);
    // Should match both traces (3 total spans across 2 traces)
    assert_eq!(results.len(), 2);
    let total_spans: usize = results.iter().map(|r| r.matched_spans.len()).sum();
    assert_eq!(total_spans, 3);
}

#[test]
fn test_eval_count_filter_gt() {
    let store = make_store();
    // {} matches all spans: trace1 has 2 spans, trace2 has 1 span
    // count() > 1 should only keep trace1
    let expr = crate::query::traceql::parser::parse_traceql("{} | count() > 1").unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans.len(), 2);
}

#[test]
fn test_eval_count_filter_eq() {
    let store = make_store();
    // count() = 1 should only keep trace2 (1 span)
    let expr = crate::query::traceql::parser::parse_traceql("{} | count() = 1").unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans.len(), 1);
    assert_eq!(results[0].matched_spans[0].name, "slow_query");
}

#[test]
fn test_eval_count_filter_gte() {
    let store = make_store();
    // count() >= 2 should only keep trace1
    let expr = crate::query::traceql::parser::parse_traceql("{} | count() >= 2").unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matched_spans.len(), 2);
}

#[test]
fn test_eval_count_filter_with_conditions() {
    let store = make_store();
    // Match payments spans, then filter by count
    // Trace1 has 1 payments span, trace2 has 1 payments span
    // count() >= 1 keeps both
    let expr = crate::query::traceql::parser::parse_traceql(
        r#"{ resource.service.name = "payments" } | count() >= 1"#,
    )
    .unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert_eq!(results.len(), 2);
}

#[test]
fn test_eval_count_filter_excludes_all() {
    let store = make_store();
    // No trace has more than 2 matching spans
    let expr = crate::query::traceql::parser::parse_traceql("{} | count() > 10").unwrap();
    let results = evaluate_traceql(&expr, &store);
    assert!(results.is_empty());
}

#[test]
fn test_eval_by_kind() {
    let mut store = TraceStore::new();
    let tid = [9u8; 16];
    let svc = store.interner.get_or_intern("svc");
    let server_op = store.interner.get_or_intern("GET /");
    let db_op = store.interner.get_or_intern("db.query");
    store.ingest_spans(vec![
        Span {
            trace_id: tid,
            span_id: [1, 0, 0, 0, 0, 0, 0, 0],
            parent_span_id: None,
            name: server_op,
            service_name: svc,
            start_time_ns: 0,
            duration_ns: 100,
            status: SpanStatus::Ok,
            status_message: None,
            attributes: SmallVec::new(),
            events: Vec::new(),
            links: Vec::new(),
            kind: SpanKind::Server,
            ingest_seq: 0,
        },
        Span {
            trace_id: tid,
            span_id: [2, 0, 0, 0, 0, 0, 0, 0],
            parent_span_id: Some([1, 0, 0, 0, 0, 0, 0, 0]),
            name: db_op,
            service_name: svc,
            start_time_ns: 10,
            duration_ns: 5,
            status: SpanStatus::Ok,
            status_message: None,
            attributes: SmallVec::new(),
            events: Vec::new(),
            links: Vec::new(),
            kind: SpanKind::Client,
            ingest_seq: 0,
        },
    ]);

    let eq_expr = crate::query::traceql::parser::parse_traceql("{ kind = server }").unwrap();
    let eq_results = evaluate_traceql(&eq_expr, &store);
    assert_eq!(eq_results.len(), 1);
    assert_eq!(eq_results[0].matched_spans.len(), 1);
    assert_eq!(eq_results[0].matched_spans[0].name, "GET /");

    let neq_expr = crate::query::traceql::parser::parse_traceql("{ kind != client }").unwrap();
    let neq_results = evaluate_traceql(&neq_expr, &store);
    assert_eq!(neq_results.len(), 1);
    assert_eq!(neq_results[0].matched_spans.len(), 1);
    assert_eq!(neq_results[0].matched_spans[0].name, "GET /");
}

#[test]
fn test_eval_by_event_name_and_attribute() {
    use crate::store::trace_store::SpanEvent;

    let mut store = TraceStore::new();
    let tid = [7u8; 16];
    let svc = store.interner.get_or_intern("svc");
    let op = store.interner.get_or_intern("handler");
    let exc_name = store.interner.get_or_intern("exception");
    let exc_type_key = store.interner.get_or_intern("exception.type");
    let exc_type_val = store.interner.get_or_intern("ConnectionRefused");
    store.ingest_spans(vec![Span {
        trace_id: tid,
        span_id: [1, 0, 0, 0, 0, 0, 0, 0],
        parent_span_id: None,
        name: op,
        service_name: svc,
        start_time_ns: 0,
        duration_ns: 10,
        status: SpanStatus::Error,
        status_message: None,
        attributes: SmallVec::new(),
        events: vec![SpanEvent {
            name: exc_name,
            time_ns: 5,
            attributes: SmallVec::from_vec(vec![(
                exc_type_key,
                AttributeValue::String(exc_type_val),
            )]),
        }],
        links: Vec::new(),
        kind: SpanKind::Internal,
        ingest_seq: 0,
    }]);

    let name_expr =
        crate::query::traceql::parser::parse_traceql(r#"{ event.name = "exception" }"#).unwrap();
    let name_results = evaluate_traceql(&name_expr, &store);
    assert_eq!(name_results.len(), 1);
    assert_eq!(name_results[0].matched_spans.len(), 1);

    let attr_expr = crate::query::traceql::parser::parse_traceql(
        r#"{ event.exception.type = "ConnectionRefused" }"#,
    )
    .unwrap();
    let attr_results = evaluate_traceql(&attr_expr, &store);
    assert_eq!(attr_results.len(), 1);

    // Neq on an absent event attribute should match (no event carries it).
    let neq_expr =
        crate::query::traceql::parser::parse_traceql(r#"{ event.missing.attr != "x" }"#).unwrap();
    let neq_results = evaluate_traceql(&neq_expr, &store);
    assert_eq!(neq_results.len(), 1);
}

#[test]
fn test_eval_by_boolean_attribute() {
    let mut store = TraceStore::new();
    let tid = [8u8; 16];
    let svc = store.interner.get_or_intern("svc");
    let op = store.interner.get_or_intern("job");
    let key = store.interner.get_or_intern("span.retry");
    store.ingest_spans(vec![Span {
        trace_id: tid,
        span_id: [1, 0, 0, 0, 0, 0, 0, 0],
        parent_span_id: None,
        name: op,
        service_name: svc,
        start_time_ns: 0,
        duration_ns: 10,
        status: SpanStatus::Ok,
        status_message: None,
        attributes: SmallVec::from_vec(vec![(key, AttributeValue::Bool(true))]),
        events: Vec::new(),
        links: Vec::new(),
        kind: SpanKind::Internal,
        ingest_seq: 0,
    }]);

    let eq_expr = crate::query::traceql::parser::parse_traceql("{ span.retry = true }").unwrap();
    let eq_results = evaluate_traceql(&eq_expr, &store);
    assert_eq!(eq_results.len(), 1);

    let false_expr =
        crate::query::traceql::parser::parse_traceql("{ span.retry = false }").unwrap();
    let false_results = evaluate_traceql(&false_expr, &store);
    assert!(false_results.is_empty());
}
