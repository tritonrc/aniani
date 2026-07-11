//! LogQL evaluator against LogStore.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::Duration;

use rustc_hash::FxHashMap;

use crate::store::log_store::LogStore;
use crate::store::{LabelMatchOp, LabelMatcher};

use super::parser::{LogQLExpr, LogQLMatcher, MatchOp, MetricFunc, PipelineStage};

/// Result of a LogQL evaluation.
#[derive(Debug)]
pub enum LogQLResult {
    /// Log stream results.
    Streams(Vec<StreamResult>),
    /// Metric query results (time series).
    Matrix(Vec<MetricResult>),
}

/// A single stream result with entries.
#[derive(Debug)]
pub struct StreamResult {
    pub labels: Vec<(String, String)>,
    pub entries: Vec<(i64, String)>,
}

/// A single metric result (from count_over_time, rate, etc.).
#[derive(Debug)]
pub struct MetricResult {
    pub labels: Vec<(String, String)>,
    pub samples: Vec<(i64, f64)>,
}

/// Evaluate a LogQL expression against the log store.
pub fn evaluate_logql(
    expr: &LogQLExpr,
    store: &LogStore,
    start_ns: i64,
    end_ns: i64,
    step_ns: Option<i64>,
) -> LogQLResult {
    evaluate_logql_limited(expr, store, start_ns, end_ns, step_ns, None)
}

/// Evaluate a LogQL expression with an optional global stream-entry limit.
pub fn evaluate_logql_limited(
    expr: &LogQLExpr,
    store: &LogStore,
    start_ns: i64,
    end_ns: i64,
    step_ns: Option<i64>,
    stream_limit: Option<usize>,
) -> LogQLResult {
    match expr {
        LogQLExpr::StreamSelector { matchers } => {
            evaluate_stream_query(matchers, &[], store, start_ns, end_ns, stream_limit)
        }
        LogQLExpr::Pipeline {
            selector, stages, ..
        } => {
            let (matchers, mut all_stages) = extract_selector_and_stages(selector);
            all_stages.extend(stages.clone());
            evaluate_stream_query(
                &matchers,
                &all_stages,
                store,
                start_ns,
                end_ns,
                stream_limit,
            )
        }
        LogQLExpr::MetricQuery {
            function,
            inner,
            range,
        } => {
            let step = step_ns.unwrap_or_else(|| duration_to_i64_ns(range));
            evaluate_metric_query(function, inner, *range, store, start_ns, end_ns, step)
        }
    }
}

fn evaluate_metric_query(
    function: &MetricFunc,
    inner: &LogQLExpr,
    range: Duration,
    store: &LogStore,
    start_ns: i64,
    end_ns: i64,
    step_ns: i64,
) -> LogQLResult {
    let range_ns = duration_to_i64_ns(&range);

    // Get the selector and optional stages
    let (matchers, stages) = extract_selector_and_stages(inner);
    let lm = convert_matchers(&matchers);
    let stream_ids = store.query_streams(&lm);

    let mut results = Vec::new();

    for sid in &stream_ids {
        let labels = store.get_stream_labels(*sid).unwrap_or_default();
        let mut samples = Vec::new();

        let mut t = start_ns;
        while t <= end_ns {
            let window_start = t.saturating_sub(range_ns);
            let window_end = t;
            let entries = store.get_entries(*sid, window_start, window_end);

            let mut count = 0usize;
            let mut byte_sum = 0usize;
            let mut numeric_count = 0usize;
            let mut numeric_sum = 0.0;
            let mut numeric_min = f64::INFINITY;
            let mut numeric_max = f64::NEG_INFINITY;

            for entry in entries.iter().filter(|e| apply_stages(&e.line, &stages)) {
                count += 1;
                byte_sum += entry.line.len();
                if let Ok(value) = entry.line.trim().parse::<f64>() {
                    numeric_count += 1;
                    numeric_sum += value;
                    numeric_min = numeric_min.min(value);
                    numeric_max = numeric_max.max(value);
                }
            }

            let value = match function {
                MetricFunc::CountOverTime => count as f64,
                MetricFunc::Rate => {
                    if range_ns > 0 {
                        count as f64 / (range_ns as f64 / 1_000_000_000.0)
                    } else {
                        0.0
                    }
                }
                MetricFunc::BytesOverTime => byte_sum as f64,
                MetricFunc::SumOverTime => numeric_sum,
                MetricFunc::AvgOverTime => {
                    if numeric_count == 0 {
                        0.0
                    } else {
                        numeric_sum / numeric_count as f64
                    }
                }
                MetricFunc::MinOverTime => {
                    if numeric_count == 0 {
                        let Some(next_t) = advance_time(t, step_ns) else {
                            break;
                        };
                        t = next_t;
                        continue;
                    }
                    numeric_min
                }
                MetricFunc::MaxOverTime => {
                    if numeric_count == 0 {
                        let Some(next_t) = advance_time(t, step_ns) else {
                            break;
                        };
                        t = next_t;
                        continue;
                    }
                    numeric_max
                }
            };

            samples.push((t, value));
            let Some(next_t) = advance_time(t, step_ns) else {
                break;
            };
            t = next_t;
        }

        if !samples.is_empty() {
            results.push(MetricResult { labels, samples });
        }
    }

    LogQLResult::Matrix(results)
}

fn duration_to_i64_ns(duration: &Duration) -> i64 {
    let ns = duration.as_nanos();
    if ns > i64::MAX as u128 {
        i64::MAX
    } else {
        ns as i64
    }
}

fn evaluate_stream_query(
    matchers: &[LogQLMatcher],
    stages: &[PipelineStage],
    store: &LogStore,
    start_ns: i64,
    end_ns: i64,
    limit: Option<usize>,
) -> LogQLResult {
    let lm = convert_matchers(matchers);
    let stream_ids = store.query_streams(&lm);

    match limit {
        Some(0) => LogQLResult::Streams(Vec::new()),
        Some(limit) => LogQLResult::Streams(evaluate_limited_stream_query(
            &stream_ids,
            stages,
            store,
            start_ns,
            end_ns,
            limit,
        )),
        None => LogQLResult::Streams(evaluate_unlimited_stream_query(
            &stream_ids,
            stages,
            store,
            start_ns,
            end_ns,
        )),
    }
}

fn evaluate_unlimited_stream_query(
    stream_ids: &[u64],
    stages: &[PipelineStage],
    store: &LogStore,
    start_ns: i64,
    end_ns: i64,
) -> Vec<StreamResult> {
    let mut results = Vec::new();
    for &sid in stream_ids {
        let entries = store.get_entries(sid, start_ns, end_ns);
        if entries.is_empty() {
            continue;
        }
        let entry_tuples: Vec<(i64, String)> = entries
            .iter()
            .filter(|e| apply_stages(&e.line, stages))
            .map(|e| (e.timestamp_ns, e.line.clone()))
            .collect();
        if entry_tuples.is_empty() {
            continue;
        }
        let labels = store.get_stream_labels(sid).unwrap_or_default();
        results.push(StreamResult {
            labels,
            entries: entry_tuples,
        });
    }
    results
}

fn evaluate_limited_stream_query(
    stream_ids: &[u64],
    stages: &[PipelineStage],
    store: &LogStore,
    start_ns: i64,
    end_ns: i64,
    limit: usize,
) -> Vec<StreamResult> {
    let mut newest = BinaryHeap::with_capacity(limit);
    let mut sequence = 0usize;

    for &sid in stream_ids {
        for entry in store.get_entries(sid, start_ns, end_ns) {
            if !apply_stages(&entry.line, stages) {
                continue;
            }

            let should_insert = newest.len() < limit
                || newest
                    .peek()
                    .map(|Reverse((oldest_ts, _, _, _))| entry.timestamp_ns > *oldest_ts)
                    .unwrap_or(false);
            if !should_insert {
                continue;
            }

            if newest.len() == limit {
                newest.pop();
            }

            newest.push(Reverse((
                entry.timestamp_ns,
                sequence,
                sid,
                entry.line.clone(),
            )));
            sequence = sequence.wrapping_add(1);
        }
    }

    let mut grouped: FxHashMap<u64, Vec<(i64, String)>> = FxHashMap::default();
    for Reverse((timestamp_ns, _, sid, line)) in newest {
        grouped.entry(sid).or_default().push((timestamp_ns, line));
    }

    let mut result_stream_ids: Vec<u64> = grouped.keys().copied().collect();
    result_stream_ids.sort_unstable();

    let mut results = Vec::with_capacity(result_stream_ids.len());
    for sid in result_stream_ids {
        let Some(mut entries) = grouped.remove(&sid) else {
            continue;
        };
        entries.sort_by_key(|(timestamp_ns, _)| *timestamp_ns);
        let labels = store.get_stream_labels(sid).unwrap_or_default();
        results.push(StreamResult { labels, entries });
    }
    results
}

fn advance_time(current: i64, step: i64) -> Option<i64> {
    if step <= 0 {
        return None;
    }
    current.checked_add(step)
}

fn extract_selector_and_stages(
    expr: &LogQLExpr,
) -> (Vec<super::parser::LogQLMatcher>, Vec<PipelineStage>) {
    match expr {
        LogQLExpr::StreamSelector { matchers } => (matchers.clone(), Vec::new()),
        LogQLExpr::Pipeline {
            selector, stages, ..
        } => {
            let (matchers, mut existing_stages) = extract_selector_and_stages(selector);
            existing_stages.extend(stages.clone());
            (matchers, existing_stages)
        }
        _ => (Vec::new(), Vec::new()),
    }
}

fn apply_stages(line: &str, stages: &[PipelineStage]) -> bool {
    let mut extracted: FxHashMap<String, String> = FxHashMap::default();
    for stage in stages {
        match stage {
            PipelineStage::LineContains(pattern) => {
                if !line.contains(pattern.as_str()) {
                    return false;
                }
            }
            PipelineStage::LineNotContains(pattern) => {
                if line.contains(pattern.as_str()) {
                    return false;
                }
            }
            PipelineStage::LineRegex(_, re) => {
                if !re.is_match(line) {
                    return false;
                }
            }
            PipelineStage::LineNotRegex(_, re) => {
                if re.is_match(line) {
                    return false;
                }
            }
            PipelineStage::LogfmtExtract => {
                for pair in line.split_whitespace() {
                    if let Some((key, val)) = pair.split_once('=')
                        && !key.is_empty()
                    {
                        // Trim surrounding quotes from value
                        let val = val
                            .strip_prefix('"')
                            .and_then(|v| v.strip_suffix('"'))
                            .unwrap_or(val);
                        extracted.insert(key.to_string(), val.to_string());
                    }
                }
            }
            PipelineStage::JsonExtract => {
                let parsed: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    // Non-JSON lines pass through with no labels extracted,
                    // matching real LogQL: | json only extracts labels, it
                    // never filters lines out.
                    Err(_) => continue,
                };
                if let serde_json::Value::Object(map) = parsed {
                    for (k, v) in map {
                        let val_str = match &v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        extracted.insert(k, val_str);
                    }
                }
            }
            PipelineStage::LabelFilter {
                key,
                op,
                value,
                compiled_regex,
            } => {
                let label_val = match extracted.get(key.as_str()) {
                    Some(v) => v.as_str(),
                    None => return false, // label not found => filter fails
                };
                let matches = match op {
                    MatchOp::Eq => label_val == value,
                    MatchOp::Neq => label_val != value,
                    MatchOp::Regex => compiled_regex
                        .as_ref()
                        .map(|re| re.is_match(label_val))
                        .unwrap_or(false),
                    MatchOp::NotRegex => compiled_regex
                        .as_ref()
                        .map(|re| !re.is_match(label_val))
                        .unwrap_or(false),
                };
                if !matches {
                    return false;
                }
            }
        }
    }
    true
}

fn convert_matchers(matchers: &[super::parser::LogQLMatcher]) -> Vec<LabelMatcher> {
    matchers
        .iter()
        .map(|m| LabelMatcher {
            name: m.name.clone(),
            op: match m.op {
                MatchOp::Eq => LabelMatchOp::Eq,
                MatchOp::Neq => LabelMatchOp::Neq,
                MatchOp::Regex => LabelMatchOp::Regex,
                MatchOp::NotRegex => LabelMatchOp::NotRegex,
            },
            value: m.value.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::log_store::LogEntry;

    fn make_store() -> LogStore {
        let mut store = LogStore::new();
        store.ingest_stream(
            vec![
                ("service".into(), "payments".into()),
                ("level".into(), "error".into()),
            ],
            vec![
                LogEntry {
                    timestamp_ns: 1_000_000_000,
                    line: "connection timeout to bank API".into(),
                    ingest_seq: 0,
                },
                LogEntry {
                    timestamp_ns: 2_000_000_000,
                    line: "retry 1/3 failed".into(),
                    ingest_seq: 0,
                },
                LogEntry {
                    timestamp_ns: 3_000_000_000,
                    line: "healthcheck ok".into(),
                    ingest_seq: 0,
                },
            ],
        );
        store.ingest_stream(
            vec![
                ("service".into(), "gateway".into()),
                ("level".into(), "info".into()),
            ],
            vec![LogEntry {
                timestamp_ns: 1_500_000_000,
                line: "request received".into(),
                ingest_seq: 0,
            }],
        );
        store
    }

    #[test]
    fn test_eval_stream_selector() {
        let store = make_store();
        let expr = crate::query::logql::parser::parse_logql(r#"{service="payments"}"#).unwrap();
        let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
        match result {
            LogQLResult::Streams(streams) => {
                assert_eq!(streams.len(), 1);
                assert_eq!(streams[0].entries.len(), 3);
            }
            _ => panic!("expected Streams"),
        }
    }

    #[test]
    fn test_eval_pipeline_filter() {
        let store = make_store();
        let expr = crate::query::logql::parser::parse_logql(r#"{service="payments"} |= "timeout""#)
            .unwrap();
        let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
        match result {
            LogQLResult::Streams(streams) => {
                assert_eq!(streams.len(), 1);
                assert_eq!(streams[0].entries.len(), 1);
                assert!(streams[0].entries[0].1.contains("timeout"));
            }
            _ => panic!("expected Streams"),
        }
    }

    #[test]
    fn test_eval_pipeline_not_contains() {
        let store = make_store();
        let expr =
            crate::query::logql::parser::parse_logql(r#"{service="payments"} != "healthcheck""#)
                .unwrap();
        let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
        match result {
            LogQLResult::Streams(streams) => {
                assert_eq!(streams[0].entries.len(), 2);
            }
            _ => panic!("expected Streams"),
        }
    }

    #[test]
    fn test_limited_eval_applies_pipeline_before_limit() {
        let store = make_store();
        let expr = crate::query::logql::parser::parse_logql(r#"{service="payments"} |= "timeout""#)
            .unwrap();
        let result = evaluate_logql_limited(&expr, &store, 0, i64::MAX, None, Some(1));
        match result {
            LogQLResult::Streams(streams) => {
                assert_eq!(streams.len(), 1);
                assert_eq!(streams[0].entries.len(), 1);
                assert!(streams[0].entries[0].1.contains("timeout"));
            }
            _ => panic!("expected Streams"),
        }
    }

    #[test]
    fn test_limited_eval_keeps_newest_entries_globally() {
        let store = make_store();
        let expr = crate::query::logql::parser::parse_logql(r#"{service=~".*"}"#).unwrap();
        let result = evaluate_logql_limited(&expr, &store, 0, i64::MAX, None, Some(2));
        match result {
            LogQLResult::Streams(streams) => {
                let mut timestamps: Vec<i64> = streams
                    .iter()
                    .flat_map(|stream| stream.entries.iter().map(|(timestamp, _)| *timestamp))
                    .collect();
                timestamps.sort_unstable();
                assert_eq!(timestamps, vec![2_000_000_000, 3_000_000_000]);
            }
            _ => panic!("expected Streams"),
        }
    }

    #[test]
    fn test_eval_count_over_time() {
        let store = make_store();
        let expr = crate::query::logql::parser::parse_logql(
            r#"count_over_time({service="payments"}[10s])"#,
        )
        .unwrap();
        let result = evaluate_logql(
            &expr,
            &store,
            3_000_000_000,
            3_000_000_000,
            Some(10_000_000_000),
        );
        match result {
            LogQLResult::Matrix(results) => {
                assert_eq!(results.len(), 1);
                // At t=3s, window [3s-10s, 3s] = [-7s, 3s], should capture all 3 entries
                assert!(results[0].samples[0].1 >= 3.0);
            }
            _ => panic!("expected Matrix"),
        }
    }

    #[test]
    fn test_json_pipeline_passes_non_json_lines_through() {
        let mut store = LogStore::new();
        store.ingest_stream(
            vec![("service".into(), "mix".into())],
            vec![
                LogEntry {
                    timestamp_ns: 1_000_000_000,
                    line: r#"{"level":"error","msg":"boom"}"#.into(),
                    ingest_seq: 0,
                },
                LogEntry {
                    timestamp_ns: 2_000_000_000,
                    line: "plain text not json".into(),
                    ingest_seq: 0,
                },
            ],
        );
        // | json should NOT drop the non-JSON line.
        let expr = crate::query::logql::parser::parse_logql(r#"{service="mix"} | json"#).unwrap();
        let result = evaluate_logql(&expr, &store, 0, i64::MAX, None);
        match result {
            LogQLResult::Streams(streams) => {
                assert_eq!(streams.len(), 1, "one stream");
                assert_eq!(
                    streams[0].entries.len(),
                    2,
                    "non-JSON line must pass through | json"
                );
            }
            _ => panic!("expected Streams"),
        }
    }
}
