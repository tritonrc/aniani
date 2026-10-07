use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use serde_json::Value;
use smallvec::SmallVec;

use super::{SearchParams, search};
use crate::store::trace_store::{AttributeValue, Span, SpanKind, SpanStatus, TraceStore};
use crate::store::{SharedState, empty_test_state};

enum Attr<'a> {
    Str(&'a str),
    Int(i64),
    Bool(bool),
}

/// One single-span trace in namespace `ns`. The trace id is `[trace; 16]`, so
/// trace-id order is numeric order of `trace`.
fn ingest(
    state: &SharedState,
    trace: u8,
    start_ns: i64,
    span_name: &str,
    service: &str,
    attrs: &[(&str, Attr)],
) {
    let mut store = state.trace_store.write();
    let name = store.interner.get_or_intern(span_name);
    let service_name = store.interner.get_or_intern(service);
    let attributes = base_attributes(&mut store, service, attrs);
    store.ingest_spans(vec![Span {
        trace_id: [trace; 16],
        span_id: [trace; 8],
        parent_span_id: None,
        name,
        service_name,
        start_time_ns: start_ns,
        duration_ns: 300_000_000,
        status: SpanStatus::Ok,
        status_message: None,
        kind: SpanKind::Unspecified,
        attributes,
        events: Vec::new(),
        links: Vec::new(),
        ingest_seq: 0,
    }]);
}

/// Resource attributes every test span carries, followed by `attrs`.
fn base_attributes(
    store: &mut TraceStore,
    service: &str,
    attrs: &[(&str, Attr)],
) -> SmallVec<[(lasso::Spur, AttributeValue); 8]> {
    let mut attributes: SmallVec<[(lasso::Spur, AttributeValue); 8]> = SmallVec::new();
    for (key, value) in [
        ("resource.service.namespace", Attr::Str("ns")),
        ("resource.service.name", Attr::Str(service)),
    ]
    .iter()
    .chain(attrs.iter())
    {
        let key = store.interner.get_or_intern(*key);
        let value = match value {
            Attr::Str(s) => AttributeValue::String(store.interner.get_or_intern(*s)),
            Attr::Int(i) => AttributeValue::Int(*i),
            Attr::Bool(b) => AttributeValue::Bool(*b),
        };
        attributes.push((key, value));
    }
    attributes
}

/// One multi-span trace in namespace `ns`; each tuple is
/// `(span_name, start_ns, duration_ns)`. The first span is the root and the
/// others are its children.
fn ingest_trace(state: &SharedState, trace: u8, spans: &[(&str, i64, i64)]) {
    let mut store = state.trace_store.write();
    let service_name = store.interner.get_or_intern("svc");
    let built: Vec<Span> = spans
        .iter()
        .enumerate()
        .map(|(i, (span_name, start_ns, duration_ns))| Span {
            trace_id: [trace; 16],
            span_id: [trace, i as u8, 0, 0, 0, 0, 0, 0],
            parent_span_id: (i > 0).then_some([trace, 0, 0, 0, 0, 0, 0, 0]),
            name: store.interner.get_or_intern(*span_name),
            service_name,
            start_time_ns: *start_ns,
            duration_ns: *duration_ns,
            status: SpanStatus::Ok,
            status_message: None,
            kind: SpanKind::Unspecified,
            attributes: base_attributes(&mut store, "svc", &[]),
            events: Vec::new(),
            links: Vec::new(),
            ingest_seq: 0,
        })
        .collect();
    store.ingest_spans(built);
}

async fn search_ids(state: SharedState, q: Option<&str>, limit: Option<usize>) -> Vec<String> {
    search_json(state, q, None, None, limit)
        .await
        .iter()
        .map(|t| t["traceID"].as_str().unwrap().to_owned())
        .collect()
}

/// Full trace objects, with optional `start`/`end` window parameters.
async fn search_json(
    state: SharedState,
    q: Option<&str>,
    start: Option<u64>,
    end: Option<u64>,
    limit: Option<usize>,
) -> Vec<Value> {
    let params = SearchParams {
        q: q.map(str::to_owned),
        start,
        end,
        limit,
    };
    let response = search(State(state), Query(params)).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["traces"].as_array().unwrap().clone()
}

fn id(trace: u8) -> String {
    format!("{trace:02x}").repeat(16)
}

const NS_QUERY: &str = r#"{resource.service.namespace="ns"}"#;

#[tokio::test]
async fn query_search_returns_the_newest_traces_before_the_limit() {
    let state = empty_test_state();
    // Trace-id order (1, 2, 3) is the opposite of start-time order.
    ingest(&state, 1, 1_000, "op", "svc", &[]);
    ingest(&state, 2, 2_000, "op", "svc", &[]);
    ingest(&state, 3, 3_000, "op", "svc", &[]);

    assert_eq!(
        search_ids(state, Some(NS_QUERY), Some(2)).await,
        vec![id(3), id(2)]
    );
}

#[tokio::test]
async fn equal_start_times_are_ordered_by_trace_id() {
    let state = empty_test_state();
    ingest(&state, 9, 5_000, "op", "svc", &[]);
    ingest(&state, 4, 5_000, "op", "svc", &[]);
    ingest(&state, 7, 1_000, "op", "svc", &[]);

    let first = search_ids(state.clone(), Some(NS_QUERY), None).await;
    assert_eq!(first, vec![id(4), id(9), id(7)]);
    assert_eq!(search_ids(state, Some(NS_QUERY), None).await, first);
}

#[tokio::test]
async fn recent_traces_without_a_query_stay_newest_first() {
    let state = empty_test_state();
    ingest(&state, 1, 1_000, "op", "svc", &[]);
    ingest(&state, 2, 2_000, "op", "svc", &[]);
    ingest(&state, 3, 3_000, "op", "svc", &[]);

    assert_eq!(search_ids(state, None, Some(2)).await, vec![id(3), id(2)]);
}

#[tokio::test]
async fn escaped_string_literals_match_verbatim_values() {
    let state = empty_test_state();
    ingest(&state, 1, 1_000, "op", r#"a"b\c || x"#, &[]);
    ingest(&state, 2, 2_000, "op", "a", &[]);

    let q = r#"{resource.service.namespace="ns" && resource.service.name="a\"b\\c || x"}"#;
    assert_eq!(search_ids(state, Some(q), None).await, vec![id(1)]);
}

#[tokio::test]
async fn span_scoped_keyword_keys_match_attributes_not_intrinsics() {
    let state = empty_test_state();
    ingest(
        &state,
        1,
        1_000,
        "other",
        "svc",
        &[("span.name", Attr::Str("checkout"))],
    );
    ingest(&state, 2, 2_000, "checkout", "svc", &[]);
    ingest(
        &state,
        3,
        3_000,
        "op",
        "svc",
        &[("span.status", Attr::Str("error"))],
    );

    let by_name = r#"{resource.service.namespace="ns" && span.name="checkout"}"#;
    assert_eq!(
        search_ids(state.clone(), Some(by_name), None).await,
        vec![id(1)]
    );
    let by_status = r#"{resource.service.namespace="ns" && span.status="error"}"#;
    assert_eq!(search_ids(state, Some(by_status), None).await, vec![id(3)]);
}

#[tokio::test]
async fn unquoted_integers_match_integer_and_numeric_string_attributes() {
    let state = empty_test_state();
    ingest(
        &state,
        1,
        1_000,
        "op",
        "svc",
        &[("span.http.status_code", Attr::Int(500))],
    );
    ingest(
        &state,
        2,
        2_000,
        "op",
        "svc",
        &[("span.http.status_code", Attr::Str("500"))],
    );
    ingest(
        &state,
        3,
        3_000,
        "op",
        "svc",
        &[("span.http.status_code", Attr::Int(200))],
    );

    let q = r#"{resource.service.namespace="ns" && span.http.status_code=500}"#;
    assert_eq!(search_ids(state, Some(q), None).await, vec![id(2), id(1)]);
}

#[tokio::test]
async fn booleans_are_unquoted_for_span_keys_and_quoted_for_resource_keys() {
    let state = empty_test_state();
    ingest(
        &state,
        1,
        1_000,
        "op",
        "svc",
        &[("span.flag", Attr::Bool(true))],
    );
    ingest(
        &state,
        2,
        2_000,
        "op",
        "svc",
        &[("resource.feature.enabled", Attr::Str("true"))],
    );

    let span_q = r#"{resource.service.namespace="ns" && span.flag=true}"#;
    assert_eq!(
        search_ids(state.clone(), Some(span_q), None).await,
        vec![id(1)]
    );
    let resource_q = r#"{resource.service.namespace="ns" && resource.feature.enabled="true"}"#;
    assert_eq!(search_ids(state, Some(resource_q), None).await, vec![id(2)]);
}

#[tokio::test]
async fn long_digit_runs_are_searched_as_strings() {
    let state = empty_test_state();
    ingest(
        &state,
        1,
        1_000,
        "op",
        "svc",
        &[("span.n", Attr::Str("99999999999999999999"))],
    );

    let q = r#"{resource.service.namespace="ns" && span.n="99999999999999999999"}"#;
    assert_eq!(search_ids(state, Some(q), None).await, vec![id(1)]);
}

#[tokio::test]
async fn operation_and_min_duration_conditions_hold_on_one_span() {
    let state = empty_test_state();
    // Each span lasts 300ms (see `ingest`).
    ingest(&state, 1, 1_000, r#"GET "x""#, "svc", &[]);
    ingest(&state, 2, 2_000, "other", "svc", &[]);

    let q = r#"{resource.service.namespace="ns" && name="GET \"x\"" && duration>=250ms}"#;
    assert_eq!(search_ids(state.clone(), Some(q), None).await, vec![id(1)]);
    let too_long = r#"{resource.service.namespace="ns" && name="GET \"x\"" && duration>=1s}"#;
    assert!(search_ids(state, Some(too_long), None).await.is_empty());
}

#[tokio::test]
async fn ordering_uses_trace_start_not_matched_span_start() {
    let state = empty_test_state();
    // Trace 1 starts at 500 (its "early" span) though its match is at 3_000.
    ingest_trace(&state, 1, &[("early", 500, 10), ("hit", 3_000, 10)]);
    ingest_trace(&state, 2, &[("hit", 2_000, 10)]);

    let q = r#"{resource.service.namespace="ns" && name="hit"}"#;
    let traces = search_json(state, Some(q), None, None, None).await;
    let ids: Vec<&str> = traces
        .iter()
        .map(|t| t["traceID"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![id(2), id(1)]);
    assert_eq!(traces[1]["startTimeUnixNano"], "500");
}

#[tokio::test]
async fn a_limited_search_inside_a_window_keeps_the_newest() {
    const BASE: i64 = 1_790_000_000_000_000_000;
    const SEC: i64 = 1_000_000_000;
    let state = empty_test_state();
    for (trace, offset) in [(1u8, 1), (2, 2), (3, 3)] {
        ingest_trace(&state, trace, &[("hit", BASE + offset * SEC, 10_000_000)]);
    }

    let q = r#"{resource.service.namespace="ns" && name="hit"}"#;
    let start = Some((BASE + 3 * SEC / 2) as u64);
    let end = Some((BASE + 10 * SEC) as u64);
    let ids = |traces: Vec<Value>| -> Vec<String> {
        traces
            .iter()
            .map(|t| t["traceID"].as_str().unwrap().to_owned())
            .collect()
    };
    let one = search_json(state.clone(), Some(q), start, end, Some(1)).await;
    assert_eq!(ids(one), vec![id(3)]);
    let five = search_json(state, Some(q), start, end, Some(5)).await;
    assert_eq!(ids(five), vec![id(3), id(2)]);
}

#[tokio::test]
async fn operation_and_duration_must_hold_on_the_same_span() {
    let state = empty_test_state();
    const MS: i64 = 1_000_000;
    // Trace 1 has the name on one span and the duration on another.
    ingest_trace(
        &state,
        1,
        &[("GET", 1_000, 10 * MS), ("other", 2_000, 900 * MS)],
    );
    ingest_trace(&state, 2, &[("GET", 3_000, 300 * MS)]);

    let q = r#"{resource.service.namespace="ns" && name="GET" && duration>=250ms}"#;
    assert_eq!(search_ids(state, Some(q), None).await, vec![id(2)]);
}
