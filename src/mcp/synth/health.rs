use serde::Serialize;

use crate::store::SharedState;

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
///
/// Aggregates per-service log error counts and span error ratios in a single
/// pass per store, rather than calling `summarize_activity` once per service
/// (which would re-scan all data N times). The health score formula mirrors
/// `summarize_activity` exactly.
pub fn check_health(state: &SharedState) -> HealthOverview {
    use crate::store::trace_store::SpanStatus;
    use lasso::Spur;
    use rustc_hash::{FxHashMap, FxHashSet};

    let mut names: FxHashSet<String> = FxHashSet::default();
    // service -> error log entry count
    let mut log_errors: FxHashMap<String, usize> = FxHashMap::default();
    // service -> (total_spans, error_spans, error_trace_count)
    let mut trace_stats: FxHashMap<String, (usize, usize, usize)> = FxHashMap::default();

    {
        let store = state.log_store.read();
        for n in store.get_label_values("service") {
            names.insert(n);
        }
        let service_key = store.interner.get("service");
        let level_key = store.interner.get("level");
        let error_val = store.interner.get("error");
        if let Some(service_key) = service_key {
            for stream in store.streams.values() {
                let Some(svc_spur) = stream
                    .labels
                    .iter()
                    .find(|(k, _)| *k == service_key)
                    .map(|(_, v)| *v)
                else {
                    continue;
                };
                let is_error = level_key.is_some_and(|lk| {
                    stream
                        .labels
                        .iter()
                        .any(|(k, v)| *k == lk && Some(*v) == error_val)
                });
                if is_error {
                    *log_errors
                        .entry(store.interner.resolve(&svc_spur).to_string())
                        .or_insert(0) += stream.entries.len();
                }
            }
        }
    }

    {
        let store = state.trace_store.read();
        for n in store.service_names() {
            names.insert(n);
        }
        for spans in store.traces.values() {
            let total = spans.len();
            let errs = spans
                .iter()
                .filter(|s| s.status == SpanStatus::Error)
                .count();
            let has_error = errs > 0;
            // A service is credited with every span of any trace it appears in,
            // matching summarize_activity's traces_for_service + whole-trace scan.
            let mut svcs: FxHashSet<Spur> = FxHashSet::default();
            for s in spans {
                svcs.insert(s.service_name);
            }
            for spur in svcs {
                let name = store.interner.resolve(&spur).to_string();
                let e = trace_stats.entry(name).or_insert((0, 0, 0));
                e.0 += total;
                e.1 += errs;
                if has_error {
                    e.2 += 1;
                }
            }
        }
    }

    {
        let store = state.metric_store.read();
        for n in store.get_label_values("service") {
            names.insert(n);
        }
    }

    let mut services: Vec<ServiceHealth> = names
        .into_iter()
        .map(|name| {
            let error_count = *log_errors.get(&name).unwrap_or(&0);
            let &(total_spans, error_spans, error_trace_count) =
                trace_stats.get(&name).unwrap_or(&(0, 0, 0));
            let span_error_ratio = if total_spans > 0 {
                error_spans as f64 / total_spans as f64
            } else {
                0.0
            };
            let mut health = 100.0_f64;
            health -= (span_error_ratio * 30.0).min(30.0);
            if error_count > 0 {
                health -= (error_count as f64).min(40.0);
            }
            let health_score = health.clamp(0.0, 100.0);
            let top_issue = if error_count > 0 {
                format!("{} error log(s)", error_count)
            } else if error_trace_count > 0 {
                format!("{} failing trace(s)", error_trace_count)
            } else {
                "No issues detected".to_string()
            };
            ServiceHealth {
                service: name,
                health_score,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::empty_test_state as state;
    use crate::store::log_store::LogEntry;
    use smallvec::SmallVec;

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
                    trace_id: None,

                    attributes: SmallVec::new(),
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
                    trace_id: None,

                    attributes: SmallVec::new(),
                }],
            );
        }
        let health = check_health(&st);
        assert!(!health.services.is_empty());
        let scores: Vec<f64> = health.services.iter().map(|s| s.health_score).collect();
        assert!(scores.windows(2).all(|w| w[0] <= w[1]));
    }
}
