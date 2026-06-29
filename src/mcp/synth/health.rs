use serde::Serialize;

use super::summarize_activity;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::empty_test_state as state;
    use crate::store::log_store::LogEntry;

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
}
