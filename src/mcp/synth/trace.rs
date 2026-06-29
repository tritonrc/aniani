use serde::Serialize;

use super::TraceItem;
use crate::store::SharedState;
use crate::store::trace_store::SpanStatus;

#[derive(Debug, Serialize)]
pub struct TraceTree {
    pub trace_id: String,
    pub roots: Vec<SpanNode>,
}
#[derive(Debug, Serialize)]
pub struct SpanNode {
    pub span_id: String,
    pub name: String,
    pub service: String,
    pub start_time_ns: i64,
    pub duration_ms: f64,
    pub status: String,
    /// Span attributes, populated only in `detailed` mode (empty in `concise`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<(String, String)>,
    pub children: Vec<SpanNode>,
}

/// Summarize one trace for `query_traces`: root span, total duration, and the
/// number of error spans. Returns `None` if the trace has no spans.
pub fn trace_item(state: &SharedState, trace_id: &[u8; 16]) -> Option<TraceItem> {
    let store = state.trace_store.read();
    let spans = store.get_trace(trace_id)?;
    let error_span_count = spans
        .iter()
        .filter(|s| s.status == SpanStatus::Error)
        .count();
    let r = store.trace_result(trace_id)?;
    Some(TraceItem {
        trace_id: trace_id.iter().map(|b| format!("{b:02x}")).collect(),
        root_span_name: r.root_span_name,
        duration_ms: r.duration_ns as f64 / 1_000_000.0,
        error_span_count,
    })
}

/// Build a parent/child span tree for one trace. Spans whose parent is absent
/// (or who have no parent) become roots.
pub fn build_trace_tree(
    state: &SharedState,
    trace_id: &[u8; 16],
    detailed: bool,
) -> Option<TraceTree> {
    use std::collections::{HashMap, HashSet};

    let store = state.trace_store.read();
    let spans = store.get_trace(trace_id)?;

    let present: HashSet<[u8; 8]> = spans.iter().map(|s| s.span_id).collect();
    let mut children_of: HashMap<[u8; 8], Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        match s.parent_span_id {
            Some(p) if present.contains(&p) => children_of.entry(p).or_default().push(i),
            _ => roots.push(i),
        }
    }

    fn node(
        idx: usize,
        spans: &[crate::store::trace_store::Span],
        children_of: &std::collections::HashMap<[u8; 8], Vec<usize>>,
        store: &crate::store::TraceStore,
        depth: u32,
        detailed: bool,
    ) -> SpanNode {
        const MAX_DEPTH: u32 = 128;
        let s = &spans[idx];
        let mut children: Vec<SpanNode> = if depth + 1 < MAX_DEPTH {
            children_of
                .get(&s.span_id)
                .map(|kids| {
                    kids.iter()
                        .map(|&c| node(c, spans, children_of, store, depth + 1, detailed))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        children.sort_by_key(|c| c.start_time_ns);
        let attributes = if detailed {
            s.attributes
                .iter()
                .map(|(k, v)| {
                    (
                        store.resolve(k).to_string(),
                        store.resolve_attribute_value(v),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        SpanNode {
            span_id: s.span_id.iter().map(|b| format!("{b:02x}")).collect(),
            name: store.resolve(&s.name).to_string(),
            service: store.resolve(&s.service_name).to_string(),
            start_time_ns: s.start_time_ns,
            duration_ms: s.duration_ns as f64 / 1_000_000.0,
            status: match s.status {
                crate::store::trace_store::SpanStatus::Error => "error",
                crate::store::trace_store::SpanStatus::Ok => "ok",
                crate::store::trace_store::SpanStatus::Unset => "unset",
            }
            .to_string(),
            attributes,
            children,
        }
    }

    let mut root_nodes: Vec<SpanNode> = roots
        .iter()
        .map(|&r| node(r, spans, &children_of, &store, 0, detailed))
        .collect();
    root_nodes.sort_by_key(|n| n.start_time_ns);
    Some(TraceTree {
        trace_id: trace_id.iter().map(|b| format!("{b:02x}")).collect(),
        roots: root_nodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::empty_test_state as state;

    #[test]
    fn build_trace_tree_nests_children_under_parents() {
        use crate::store::trace_store::{Span, SpanKind, SpanStatus};

        let st = state();
        let tid = [1u8; 16];
        let root_id = [10u8; 8];
        let child_id = [11u8; 8];
        {
            let mut traces = st.trace_store.write();
            let rname = traces.interner.get_or_intern("root");
            let svc = traces.interner.get_or_intern("api");
            let cname = traces.interner.get_or_intern("child");
            let root = Span {
                trace_id: tid,
                span_id: root_id,
                parent_span_id: None,
                name: rname,
                service_name: svc,
                start_time_ns: 0,
                duration_ns: 100,
                status: SpanStatus::Ok,
                kind: SpanKind::Server,
                attributes: Default::default(),
                events: vec![],
                ingest_seq: 0,
            };
            let child = Span {
                trace_id: tid,
                span_id: child_id,
                parent_span_id: Some(root_id),
                name: cname,
                service_name: svc,
                start_time_ns: 10,
                duration_ns: 30,
                status: SpanStatus::Ok,
                kind: SpanKind::Internal,
                attributes: Default::default(),
                events: vec![],
                ingest_seq: 1,
            };
            traces.ingest_spans(vec![root, child]);
        }
        let tree = build_trace_tree(&st, &tid, false).expect("trace exists");
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].name, "root");
        assert_eq!(tree.roots[0].children.len(), 1);
        assert_eq!(tree.roots[0].children[0].name, "child");
    }

    #[test]
    fn build_trace_tree_caps_depth() {
        use crate::store::trace_store::{Span, SpanKind, SpanStatus};

        let st = state();
        let tid = [2u8; 16];
        {
            let mut traces = st.trace_store.write();
            let svc = traces.interner.get_or_intern("api");
            let nm = traces.interner.get_or_intern("s");
            let id_of = |i: usize| {
                let mut b = [0u8; 8];
                b[0] = (i & 0xff) as u8;
                b[1] = ((i >> 8) & 0xff) as u8;
                b
            };
            let n = 300usize;
            let mut spans = Vec::new();
            for i in 0..n {
                let parent = if i == 0 { None } else { Some(id_of(i - 1)) };
                spans.push(Span {
                    trace_id: tid,
                    span_id: id_of(i),
                    parent_span_id: parent,
                    name: nm,
                    service_name: svc,
                    start_time_ns: i as i64,
                    duration_ns: 1,
                    status: SpanStatus::Ok,
                    kind: SpanKind::Internal,
                    attributes: Default::default(),
                    events: vec![],
                    ingest_seq: i as u64,
                });
            }
            traces.ingest_spans(spans);
        }
        let tree = build_trace_tree(&st, &tid, false).expect("trace exists");
        fn depth(n: &SpanNode) -> usize {
            1 + n.children.iter().map(depth).max().unwrap_or(0)
        }
        let d = tree.roots.iter().map(depth).max().unwrap_or(0);
        assert!(d <= 128, "tree depth {d} exceeds the cap of 128");
        assert!(d >= 2, "tree should still nest");
    }

    #[test]
    fn build_trace_tree_cycle_does_not_hang() {
        use crate::store::trace_store::{Span, SpanKind, SpanStatus};

        let st = state();
        let tid = [3u8; 16];
        let a = [1u8; 8];
        let b = [2u8; 8];
        {
            let mut traces = st.trace_store.write();
            let svc = traces.interner.get_or_intern("api");
            let nm = traces.interner.get_or_intern("s");
            let mk = |span_id: [u8; 8], parent: [u8; 8]| Span {
                trace_id: tid,
                span_id,
                parent_span_id: Some(parent),
                name: nm,
                service_name: svc,
                start_time_ns: 0,
                duration_ns: 1,
                status: SpanStatus::Ok,
                kind: SpanKind::Internal,
                attributes: Default::default(),
                events: vec![],
                ingest_seq: 0,
            };
            traces.ingest_spans(vec![mk(a, b), mk(b, a)]);
        }
        let tree = build_trace_tree(&st, &tid, false).expect("trace exists");
        assert!(
            tree.roots.is_empty(),
            "mutually-cyclic spans have no valid root"
        );
    }
}
