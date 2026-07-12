use serde::Serialize;

use super::MAX_LABEL_VALUES;
use crate::store::{LabelMatchOp, LabelMatcher, SharedState};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::empty_test_state as state;
    use crate::store::log_store::LogEntry;
    use smallvec::SmallVec;

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
                    trace_id: None,

                    attributes: SmallVec::new(),
                }],
            );
        }
        let cat = describe_service(&st, "api");
        assert!(cat.log_labels.iter().any(|l| l.key == "level"));
        let level = cat.log_labels.iter().find(|l| l.key == "level").unwrap();
        assert!(level.values.contains(&"error".to_string()));
    }
}
