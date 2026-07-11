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
        attributes: SmallVec::from_vec(vec![(res_svc_key, AttributeValue::String(gateway_val))]),
        kind: SpanKind::Unspecified,
        events: Vec::new(),
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
        attributes: SmallVec::from_vec(vec![(res_svc_key, AttributeValue::String(payments_val))]),
        kind: SpanKind::Unspecified,
        events: Vec::new(),
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
        attributes: SmallVec::from_vec(vec![(res_svc_key, AttributeValue::String(payments_val))]),
        kind: SpanKind::Unspecified,
        events: Vec::new(),
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
