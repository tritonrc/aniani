//! Transport-free typed cores backing the MCP tools.

use serde::Serialize;

use crate::store::trace_store::SpanStatus;
use crate::store::{LabelMatchOp, LabelMatcher, SharedState};

const DEFAULT_TOP: usize = 20;
const MAX_LABEL_VALUES: usize = 50;

// ---------- summarize_activity ----------

/// Per-service cross-signal activity summary.
#[derive(Debug, Serialize)]
pub struct ServiceActivity {
    pub service: String,
    pub since: Option<u64>,
    pub observed_through: u64,
    pub health_score: f64,
    pub summary: String,
    pub logs: LogsBlock,
    pub traces: TracesBlock,
    pub metrics: MetricsBlock,
    pub truncated: Truncated,
}

#[derive(Debug, Serialize)]
pub struct LogsBlock {
    pub error_count: usize,
    pub total: usize,
    pub top: Vec<LogItem>,
}
#[derive(Debug, Serialize)]
pub struct LogItem {
    pub ts: String,
    pub line: String,
}
#[derive(Debug, Serialize)]
pub struct TracesBlock {
    pub error_trace_count: usize,
    pub total: usize,
    pub notable: Vec<TraceItem>,
}
#[derive(Debug, Serialize)]
pub struct TraceItem {
    pub trace_id: String,
    pub root_span_name: String,
    pub duration_ms: f64,
    pub error_span_count: usize,
}
#[derive(Debug, Serialize)]
pub struct MetricsBlock {
    pub notable: Vec<MetricItem>,
}
#[derive(Debug, Serialize)]
pub struct MetricItem {
    pub name: String,
    pub value: f64,
    pub labels: Vec<(String, String)>,
}
#[derive(Debug, Serialize, Default)]
pub struct Truncated {
    pub logs: bool,
    pub traces: bool,
    pub metrics: bool,
}

fn svc_error_matchers(service: &str) -> Vec<LabelMatcher> {
    vec![
        LabelMatcher {
            name: "service".into(),
            op: LabelMatchOp::Eq,
            value: service.to_string(),
        },
        LabelMatcher {
            name: "level".into(),
            op: LabelMatchOp::Eq,
            value: "error".into(),
        },
    ]
}

/// Summarize a single service's activity, optionally only entries ingested at or
/// after `since` (an ingest-sequence token). `_detail` reserved for future use.
pub fn summarize_activity(
    state: &SharedState,
    service: &str,
    since: Option<u64>,
    _detail: bool,
) -> ServiceActivity {
    let observed_through = state.ingest_seq.load(std::sync::atomic::Ordering::Relaxed);
    let keep = |seq: u64| since.map(|s| seq >= s).unwrap_or(true);

    // --- Logs: total across all levels + error stream items ---
    let (error_count, log_total, mut error_items) = {
        let store = state.log_store.read();
        let all = vec![LabelMatcher {
            name: "service".into(),
            op: LabelMatchOp::Eq,
            value: service.to_string(),
        }];
        let mut total = 0usize;
        for sid in store.query_streams(&all) {
            for e in store.get_entries(sid, i64::MIN, i64::MAX) {
                if keep(e.ingest_seq) {
                    total += 1;
                }
            }
        }
        let mut items: Vec<(i64, String)> = Vec::new();
        for sid in store.query_streams(&svc_error_matchers(service)) {
            for e in store.get_entries(sid, i64::MIN, i64::MAX) {
                if keep(e.ingest_seq) {
                    items.push((e.timestamp_ns, e.line.clone()));
                }
            }
        }
        (items.len(), total, items)
    };
    error_items.sort_by_key(|b| std::cmp::Reverse(b.0));
    let logs_truncated = error_items.len() > DEFAULT_TOP;
    let top: Vec<LogItem> = error_items
        .into_iter()
        .take(DEFAULT_TOP)
        .map(|(ts, line)| LogItem {
            ts: (ts / 1_000_000).to_string(),
            line,
        })
        .collect();

    // --- Traces: count only in-window spans (since-accurate) ---
    let (notable, error_trace_count, trace_total, traces_truncated, span_error_ratio) = {
        let store = state.trace_store.read();
        let mut notable: Vec<TraceItem> = Vec::new();
        let mut total_spans = 0usize;
        let mut error_spans = 0usize;
        let mut trace_total = 0usize;
        for tid in store.traces_for_service(service) {
            if let Some(spans) = store.get_trace(&tid) {
                let in_window = spans.iter().filter(|s| keep(s.ingest_seq)).count();
                if in_window == 0 {
                    continue;
                }
                let errs = spans
                    .iter()
                    .filter(|s| keep(s.ingest_seq) && s.status == SpanStatus::Error)
                    .count();
                trace_total += 1;
                total_spans += in_window;
                error_spans += errs;
                if errs > 0
                    && let Some(r) = store.trace_result(&tid)
                {
                    notable.push(TraceItem {
                        trace_id: tid.iter().map(|b| format!("{b:02x}")).collect(),
                        root_span_name: r.root_span_name,
                        duration_ms: r.duration_ns as f64 / 1_000_000.0,
                        error_span_count: errs,
                    });
                }
            }
        }
        notable.sort_by(|a, b| {
            b.duration_ms
                .partial_cmp(&a.duration_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let count = notable.len();
        let truncated = notable.len() > DEFAULT_TOP;
        notable.truncate(DEFAULT_TOP);
        let ratio = if total_spans > 0 {
            error_spans as f64 / total_spans as f64
        } else {
            0.0
        };
        (notable, count, trace_total, truncated, ratio)
    };

    // --- Metrics: error-ish series for the service (last in-window sample) ---
    let (metric_items, metrics_truncated) = {
        let store = state.metric_store.read();
        let matchers = vec![LabelMatcher {
            name: "service".into(),
            op: LabelMatchOp::Eq,
            value: service.to_string(),
        }];
        let name_key = store.interner.get("__name__");
        let mut items: Vec<MetricItem> = Vec::new();
        for sid in store.select_series(&matchers) {
            if let Some(series) = store.series.get(&sid) {
                let mname = name_key.and_then(|nk| {
                    series
                        .labels
                        .iter()
                        .find(|(k, _)| *k == nk)
                        .map(|(_, v)| store.interner.resolve(v).to_string())
                });
                let Some(mname) = mname else { continue };
                let lower = mname.to_ascii_lowercase();
                if !lower.contains("error") && !lower.contains("fail") {
                    continue;
                }
                if let Some(sample) = series.samples.iter().rev().find(|s| keep(s.ingest_seq)) {
                    let labels = series
                        .labels
                        .iter()
                        .filter(|(k, _)| Some(*k) != name_key)
                        .map(|(k, v)| {
                            (
                                store.interner.resolve(k).to_string(),
                                store.interner.resolve(v).to_string(),
                            )
                        })
                        .collect();
                    items.push(MetricItem {
                        name: mname,
                        value: sample.value,
                        labels,
                    });
                }
            }
        }
        let truncated = items.len() > DEFAULT_TOP;
        items.truncate(DEFAULT_TOP);
        (items, truncated)
    };

    let mut health = 100.0_f64;
    health -= (span_error_ratio * 30.0).min(30.0);
    if error_count > 0 {
        health -= (error_count as f64).min(40.0);
    }
    let health_score = health.clamp(0.0, 100.0);

    let summary = format!(
        "{error_count} error log(s), {error_trace_count} failing trace(s), health {health_score:.0}"
    );

    ServiceActivity {
        service: service.to_string(),
        since,
        observed_through,
        health_score,
        summary,
        logs: LogsBlock {
            error_count,
            total: log_total,
            top,
        },
        traces: TracesBlock {
            error_trace_count,
            total: trace_total,
            notable,
        },
        metrics: MetricsBlock {
            notable: metric_items,
        },
        truncated: Truncated {
            logs: logs_truncated,
            traces: traces_truncated,
            metrics: metrics_truncated,
        },
    }
}

// ---------- check_health ----------

#[derive(Debug, Serialize)]
pub struct HealthOverview {
    pub services: Vec<ServiceHealth>,
}
#[derive(Debug, Serialize)]
pub struct ServiceHealth {
    pub service: String,
    pub health_score: f64,
    pub top_issue: String,
}

/// Global health overview: every known service ranked worst-first.
pub fn check_health(state: &SharedState) -> HealthOverview {
    use rustc_hash::FxHashSet;
    let mut names: FxHashSet<String> = FxHashSet::default();
    {
        let s = state.log_store.read();
        for n in s.get_label_values("service") {
            names.insert(n);
        }
    }
    {
        let s = state.metric_store.read();
        for n in s.get_label_values("service") {
            names.insert(n);
        }
    }
    {
        let s = state.trace_store.read();
        for n in s.service_names() {
            names.insert(n);
        }
    }
    let mut services: Vec<ServiceHealth> = names
        .into_iter()
        .map(|name| {
            let a = summarize_activity(state, &name, None, false);
            let top_issue = if a.logs.error_count > 0 {
                format!("{} error log(s)", a.logs.error_count)
            } else if a.traces.error_trace_count > 0 {
                format!("{} failing trace(s)", a.traces.error_trace_count)
            } else {
                "No issues detected".to_string()
            };
            ServiceHealth {
                service: name,
                health_score: a.health_score,
                top_issue,
            }
        })
        .collect();
    services.sort_by(|a, b| {
        a.health_score
            .partial_cmp(&b.health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    HealthOverview { services }
}

// ---------- describe_service ----------

#[derive(Debug, Serialize)]
pub struct ServiceCatalog {
    pub service: String,
    pub metrics: Vec<String>,
    pub log_labels: Vec<LabelInfo>,
    pub span_attributes: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct LabelInfo {
    pub key: String,
    pub values: Vec<String>,
    pub truncated: bool,
}

/// Enriched per-service catalog: metric names, log label keys WITH capped value
/// lists, and span attribute keys.
pub fn describe_service(state: &SharedState, service: &str) -> ServiceCatalog {
    use rustc_hash::FxHashSet;

    let metrics = {
        let store = state.metric_store.read();
        let matchers = vec![LabelMatcher {
            name: "service".into(),
            op: LabelMatchOp::Eq,
            value: service.to_string(),
        }];
        let name_key = store.interner.get("__name__");
        let mut names: Vec<String> = Vec::new();
        let mut seen = FxHashSet::default();
        for sid in store.select_series(&matchers) {
            if let Some(series) = store.series.get(&sid)
                && let Some(nk) = name_key
                && let Some((_, v)) = series.labels.iter().find(|(k, _)| *k == nk)
            {
                let n = store.interner.resolve(v).to_string();
                if seen.insert(n.clone()) {
                    names.push(n);
                }
            }
        }
        names.sort();
        names
    };

    let log_labels = {
        let store = state.log_store.read();
        let matchers = vec![LabelMatcher {
            name: "service".into(),
            op: LabelMatchOp::Eq,
            value: service.to_string(),
        }];
        let mut by_key: std::collections::BTreeMap<String, FxHashSet<String>> = Default::default();
        for sid in store.query_streams(&matchers) {
            if let Some(labels) = store.get_stream_labels(sid) {
                for (k, v) in labels {
                    by_key.entry(k).or_default().insert(v);
                }
            }
        }
        by_key
            .into_iter()
            .map(|(key, vals)| {
                let mut values: Vec<String> = vals.into_iter().collect();
                values.sort();
                let truncated = values.len() > MAX_LABEL_VALUES;
                values.truncate(MAX_LABEL_VALUES);
                LabelInfo {
                    key,
                    values,
                    truncated,
                }
            })
            .collect()
    };

    let span_attributes = {
        let store = state.trace_store.read();
        let mut keys: std::collections::BTreeSet<String> = Default::default();
        if let Some(spur) = store.interner.get(service)
            && let Some(trace_ids) = store.service_index.get(&spur)
        {
            for tid in trace_ids {
                if let Some(spans) = store.traces.get(tid) {
                    for span in spans {
                        if span.service_name == spur {
                            for (k, _) in &span.attributes {
                                keys.insert(store.interner.resolve(k).to_string());
                            }
                        }
                    }
                }
            }
        }
        keys.into_iter().collect()
    };

    ServiceCatalog {
        service: service.to_string(),
        metrics,
        log_labels,
        span_attributes,
    }
}

// ---------- build_trace_tree ----------

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
    pub children: Vec<SpanNode>,
}

/// Build a parent/child span tree for one trace. Spans whose parent is absent
/// (or who have no parent) become roots.
pub fn build_trace_tree(state: &SharedState, trace_id: &[u8; 16]) -> Option<TraceTree> {
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
    ) -> SpanNode {
        const MAX_DEPTH: u32 = 128;
        let s = &spans[idx];
        let mut children: Vec<SpanNode> = if depth + 1 < MAX_DEPTH {
            children_of
                .get(&s.span_id)
                .map(|kids| {
                    kids.iter()
                        .map(|&c| node(c, spans, children_of, store, depth + 1))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        children.sort_by_key(|c| c.start_time_ns);
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
            children,
        }
    }

    let mut root_nodes: Vec<SpanNode> = roots
        .iter()
        .map(|&r| node(r, spans, &children_of, &store, 0))
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
    use crate::store::log_store::LogEntry;
    use crate::store::{AppState, LogStore, MetricStore, TraceStore};
    use clap::Parser;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    fn state() -> Arc<AppState> {
        Arc::new(AppState {
            log_store: RwLock::new(LogStore::new()),
            metric_store: RwLock::new(MetricStore::new()),
            trace_store: RwLock::new(TraceStore::new()),
            config: crate::config::Config::parse_from(["aniani"]),
            start_time: Instant::now(),
            ingest_seq: AtomicU64::new(0),
        })
    }

    #[test]
    fn summarize_counts_errors_and_respects_since() {
        let st = state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![
                    ("service".into(), "api".into()),
                    ("level".into(), "error".into()),
                ],
                vec![
                    LogEntry {
                        timestamp_ns: 10,
                        line: "boom1".into(),
                        ingest_seq: 0,
                    },
                    LogEntry {
                        timestamp_ns: 20,
                        line: "boom2".into(),
                        ingest_seq: 5,
                    },
                ],
            );
        }
        let all = summarize_activity(&st, "api", None, false);
        assert_eq!(all.logs.error_count, 2);
        let since = summarize_activity(&st, "api", Some(3), false);
        assert_eq!(since.logs.error_count, 1);
        assert_eq!(since.service, "api");
    }

    #[test]
    fn check_health_lists_services_worst_first() {
        let st = state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "healthy".into())],
                vec![LogEntry {
                    timestamp_ns: 1,
                    line: "ok".into(),
                    ingest_seq: 0,
                }],
            );
            logs.ingest_stream(
                vec![
                    ("service".into(), "broken".into()),
                    ("level".into(), "error".into()),
                ],
                vec![LogEntry {
                    timestamp_ns: 2,
                    line: "err".into(),
                    ingest_seq: 1,
                }],
            );
        }
        let health = check_health(&st);
        assert!(!health.services.is_empty());
        let scores: Vec<f64> = health.services.iter().map(|s| s.health_score).collect();
        assert!(scores.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn describe_service_reports_log_labels_and_values() {
        let st = state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![
                    ("service".into(), "api".into()),
                    ("level".into(), "error".into()),
                ],
                vec![LogEntry {
                    timestamp_ns: 1,
                    line: "x".into(),
                    ingest_seq: 0,
                }],
            );
        }
        let cat = describe_service(&st, "api");
        assert!(cat.log_labels.iter().any(|l| l.key == "level"));
        let level = cat.log_labels.iter().find(|l| l.key == "level").unwrap();
        assert!(level.values.contains(&"error".to_string()));
    }

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
        let tree = build_trace_tree(&st, &tid).expect("trace exists");
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].name, "root");
        assert_eq!(tree.roots[0].children.len(), 1);
        assert_eq!(tree.roots[0].children[0].name, "child");
    }

    #[test]
    fn summarize_surfaces_error_metrics() {
        use crate::store::metric_store::Sample;
        let st = state();
        {
            let mut m = st.metric_store.write();
            m.ingest_samples(
                "http_errors_total",
                vec![("service".into(), "api".into())],
                vec![Sample {
                    timestamp_ms: 1,
                    value: 5.0,
                    ingest_seq: 0,
                }],
            );
        }
        let a = summarize_activity(&st, "api", None, false);
        assert!(
            a.metrics
                .notable
                .iter()
                .any(|m| m.name == "http_errors_total")
        );
    }

    #[test]
    fn summarize_reports_log_total_across_levels() {
        let st = state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![
                    ("service".into(), "api".into()),
                    ("level".into(), "info".into()),
                ],
                vec![LogEntry {
                    timestamp_ns: 1,
                    line: "ok".into(),
                    ingest_seq: 0,
                }],
            );
            logs.ingest_stream(
                vec![
                    ("service".into(), "api".into()),
                    ("level".into(), "error".into()),
                ],
                vec![LogEntry {
                    timestamp_ns: 2,
                    line: "bad".into(),
                    ingest_seq: 1,
                }],
            );
        }
        let a = summarize_activity(&st, "api", None, false);
        assert_eq!(a.logs.error_count, 1);
        assert_eq!(a.logs.total, 2); // total across ALL levels
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
        let tree = build_trace_tree(&st, &tid).expect("trace exists");
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
        let tree = build_trace_tree(&st, &tid).expect("trace exists");
        assert!(
            tree.roots.is_empty(),
            "mutually-cyclic spans have no valid root"
        );
    }
}
