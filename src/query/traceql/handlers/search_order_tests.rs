use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use serde_json::Value;
use smallvec::SmallVec;

use super::{SearchParams, search};
use crate::store::trace_store::{AttributeValue, Span, SpanKind, SpanStatus};
use crate::store::{SharedState, empty_test_state};

#[allow(dead_code)]
pub(super) enum Attr<'a> {
    Str(&'a str),
    Int(i64),
    Bool(bool),
}

/// One single-span trace in namespace `ns`. The trace id is `[trace; 16]`, so
/// trace-id order is numeric order of `trace`.
pub(super) fn ingest(
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

pub(super) async fn search_ids(
    state: SharedState,
    q: Option<&str>,
    limit: Option<usize>,
) -> Vec<String> {
    let params = SearchParams {
        q: q.map(str::to_owned),
        start: None,
        end: None,
        limit,
    };
    let response = search(State(state), Query(params)).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["traces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["traceID"].as_str().unwrap().to_owned())
        .collect()
}

pub(super) fn id(trace: u8) -> String {
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
