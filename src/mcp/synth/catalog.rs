use serde::Serialize;

use super::MAX_LABEL_VALUES;
use crate::store::{LabelMatchOp, LabelMatcher, SharedState};

#[derive(Debug, Serialize)]
pub struct ServiceCatalog {
    pub service: String,
    pub metrics: Vec<MetricInfo>,
    pub log_labels: Vec<LabelInfo>,
    pub span_attributes: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct MetricInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
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
        let mut names: Vec<MetricInfo> = Vec::new();
        let mut seen = FxHashSet::default();
        for sid in store.select_series(&matchers) {
            if let Some(series) = store.series.get(&sid)
                && let Some(nk) = name_key
                && let Some((_, v)) = series.labels.iter().find(|(k, _)| *k == nk)
            {
                let n = store.interner.resolve(v).to_string();
                if seen.insert(n.clone()) {
                    let md = store.metric_metadata.get(v);
                    names.push(MetricInfo {
                        metric_type: md
                            .and_then(|m| m.metric_type)
                            .map(|t| t.as_str().to_string()),
                        help: md.and_then(|m| m.help.clone()),
                        unit: md.and_then(|m| m.unit.clone()),
                        name: n,
                    });
                }
            }
        }
        names.sort_by(|a, b| a.name.cmp(&b.name));
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
                    span_id: None,
                    severity_number: 0,
                    severity_text: None,
                    attributes: SmallVec::new(),
                }],
            );
        }
        let cat = describe_service(&st, "api");
        assert!(cat.log_labels.iter().any(|l| l.key == "level"));
        let level = cat.log_labels.iter().find(|l| l.key == "level").unwrap();
        assert!(level.values.contains(&"error".to_string()));
    }

    #[test]
    fn describe_service_reports_metric_types() {
        use crate::store::metric_store::Sample;

        let st = state();
        {
            let mut m = st.metric_store.write();
            m.ingest_samples(
                "http_requests_total",
                vec![("service".into(), "api".into())],
                vec![Sample {
                    timestamp_ms: 1,
                    value: 5.0,
                    ingest_seq: 0,
                }],
            );
            m.register_metric_metadata(
                "http_requests_total",
                Some(crate::store::metric_store::MetricType::Counter),
                Some("Total HTTP requests"),
                Some("requests"),
            );
        }
        let cat = describe_service(&st, "api");
        let metric = cat
            .metrics
            .iter()
            .find(|m| m.name == "http_requests_total")
            .expect("metric should be present");
        assert_eq!(metric.metric_type.as_deref(), Some("counter"));
        assert_eq!(metric.help.as_deref(), Some("Total HTTP requests"));
        assert_eq!(metric.unit.as_deref(), Some("requests"));
    }
}
