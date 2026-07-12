use serde::Serialize;

use super::{DEFAULT_TOP, DETAILED_TOP};
use crate::store::trace_store::SpanStatus;
use crate::store::{LabelMatchOp, LabelMatcher, SharedState};

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
/// after `since` (an ingest-sequence token). `detailed` raises the per-block item
/// cap so triage can surface more of the highest-signal entries.
pub fn summarize_activity(
    state: &SharedState,
    service: &str,
    since: Option<u64>,
    detailed: bool,
) -> ServiceActivity {
    let observed_through = state.ingest_seq.load(std::sync::atomic::Ordering::Relaxed);
    let keep = |seq: u64| since.map(|s| seq >= s).unwrap_or(true);
    let top_n = if detailed { DETAILED_TOP } else { DEFAULT_TOP };

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
    let logs_truncated = error_items.len() > top_n;
    let top: Vec<LogItem> = error_items
        .into_iter()
        .take(top_n)
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
        let truncated = notable.len() > top_n;
        notable.truncate(top_n);
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
        let truncated = items.len() > top_n;
        items.truncate(top_n);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::empty_test_state as state;
    use crate::store::log_store::LogEntry;
    use smallvec::SmallVec;

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
                        trace_id: None,
                        span_id: None,
                        severity_number: 0,
                        severity_text: None,
                        attributes: SmallVec::new(),
                    },
                    LogEntry {
                        timestamp_ns: 20,
                        line: "boom2".into(),
                        ingest_seq: 5,
                        trace_id: None,
                        span_id: None,
                        severity_number: 0,
                        severity_text: None,
                        attributes: SmallVec::new(),
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
                    trace_id: None,
                    span_id: None,
                    severity_number: 0,
                    severity_text: None,
                    attributes: SmallVec::new(),
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
                    trace_id: None,
                    span_id: None,
                    severity_number: 0,
                    severity_text: None,
                    attributes: SmallVec::new(),
                }],
            );
        }
        let a = summarize_activity(&st, "api", None, false);
        assert_eq!(a.logs.error_count, 1);
        assert_eq!(a.logs.total, 2); // total across ALL levels
    }
}
