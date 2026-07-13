//! LogQL evaluator against LogStore.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::Duration;

use rustc_hash::FxHashMap;

use crate::store::log_store::LogStore;
use crate::store::{LabelMatchOp, LabelMatcher};

use super::parser::{CompareOp, LogQLExpr, LogQLMatcher, MatchOp, MetricFunc, PipelineStage};

/// Result of a LogQL evaluation.
#[derive(Debug)]
pub enum LogQLResult {
    /// Log stream results.
    Streams(Vec<StreamResult>),
    /// Metric query results (time series).
    Matrix(Vec<MetricResult>),
}

/// A resolved log entry tuple: `(timestamp_ns, line, trace_id_hex, span_id_hex, attributes)`.
/// `trace_id` / `span_id` are lowercase hex, `None` when absent. `attributes`
/// are resolved per-entry structured attributes.
pub type ResolvedEntry = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Vec<(String, String)>,
);

/// A single stream result with entries.
#[derive(Debug)]
pub struct StreamResult {
    pub labels: Vec<(String, String)>,
    pub entries: Vec<ResolvedEntry>,
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

            // Find unwrap field if present.
            let unwrap_field: Option<&str> = stages.iter().find_map(|s| {
                if let PipelineStage::Unwrap(f) = s {
                    Some(f.as_str())
                } else {
                    None
                }
            });

            for entry in entries.iter() {
                let Some(extracted) = apply_stages_with_extract(&entry.line, &stages, &labels)
                else {
                    continue;
                };
                count += 1;
                byte_sum += entry.line.len();
                let numeric_val = if let Some(field) = unwrap_field {
                    extracted
                        .get(field)
                        .and_then(|v| v.trim().parse::<f64>().ok())
                } else {
                    entry.line.trim().parse::<f64>().ok()
                };
                if let Some(value) = numeric_val {
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
        let labels = store.get_stream_labels(sid).unwrap_or_default();
        let entry_tuples: Vec<ResolvedEntry> = entries
            .iter()
            .filter(|e| apply_stages(&e.line, stages, &labels))
            .map(|e| {
                let mut attrs: Vec<(String, String)> = e
                    .attributes
                    .iter()
                    .map(|(k, v)| {
                        (
                            store.interner.resolve(k).to_string(),
                            store.resolve_attribute_value(v),
                        )
                    })
                    .collect();
                if e.severity_number != 0 {
                    attrs.push(("severity_number".to_string(), e.severity_number.to_string()));
                }
                if let Some(text) = e.severity_text {
                    attrs.push((
                        "severity_text".to_string(),
                        store.interner.resolve(&text).to_string(),
                    ));
                }
                (
                    e.timestamp_ns,
                    e.line.clone(),
                    e.trace_id.as_ref().map(hex_bytes),
                    e.span_id.as_ref().map(hex_bytes),
                    attrs,
                )
            })
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
        let labels = store.get_stream_labels(sid).unwrap_or_default();
        for entry in store.get_entries(sid, start_ns, end_ns) {
            if !apply_stages(&entry.line, stages, &labels) {
                continue;
            }

            let should_insert = newest.len() < limit
                || newest
                    .peek()
                    .map(|Reverse((oldest_ts, _, _, _, _, _, _))| entry.timestamp_ns > *oldest_ts)
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
                entry.trace_id,
                entry.span_id,
                {
                    let mut attrs: Vec<(String, String)> = entry
                        .attributes
                        .iter()
                        .map(|(k, v)| {
                            (
                                store.interner.resolve(k).to_string(),
                                store.resolve_attribute_value(v),
                            )
                        })
                        .collect();
                    if entry.severity_number != 0 {
                        attrs.push((
                            "severity_number".to_string(),
                            entry.severity_number.to_string(),
                        ));
                    }
                    if let Some(text) = entry.severity_text {
                        attrs.push((
                            "severity_text".to_string(),
                            store.interner.resolve(&text).to_string(),
                        ));
                    }
                    attrs
                },
            )));
            sequence = sequence.wrapping_add(1);
        }
    }

    let mut grouped: FxHashMap<u64, Vec<ResolvedEntry>> = FxHashMap::default();
    for Reverse((timestamp_ns, _, sid, line, trace_id, span_id, attrs)) in newest {
        grouped.entry(sid).or_default().push((
            timestamp_ns,
            line,
            trace_id.as_ref().map(hex_bytes),
            span_id.as_ref().map(hex_bytes),
            attrs,
        ));
    }

    let mut result_stream_ids: Vec<u64> = grouped.keys().copied().collect();
    result_stream_ids.sort_unstable();

    let mut results = Vec::with_capacity(result_stream_ids.len());
    for sid in result_stream_ids {
        let Some(mut entries) = grouped.remove(&sid) else {
            continue;
        };
        entries.sort_by_key(|(timestamp_ns, _, _, _, _)| *timestamp_ns);
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

fn apply_stages(line: &str, stages: &[PipelineStage], stream_labels: &[(String, String)]) -> bool {
    apply_stages_with_extract(line, stages, stream_labels).is_some()
}

/// Run pipeline stages, returning the extracted label map on success or
/// `None` if a stage filters the entry out. Used by the metric evaluator to
/// access `| unwrap` fields.
fn apply_stages_with_extract(
    line: &str,
    stages: &[PipelineStage],
    stream_labels: &[(String, String)],
) -> Option<FxHashMap<String, String>> {
    let mut extracted: FxHashMap<String, String> = FxHashMap::default();
    // Seed with the stream's own labels so that LabelFilter stages can match
    // them even without a preceding | json / | logfmt extraction. This mirrors
    // real Loki where stream labels are visible to pipeline label filters.
    for (k, v) in stream_labels {
        extracted.insert(k.clone(), v.clone());
    }
    for stage in stages {
        match stage {
            PipelineStage::LineContains(pattern) => {
                if !line.contains(pattern.as_str()) {
                    return None;
                }
            }
            PipelineStage::LineNotContains(pattern) => {
                if line.contains(pattern.as_str()) {
                    return None;
                }
            }
            PipelineStage::LineRegex(_, re) => {
                if !re.is_match(line) {
                    return None;
                }
            }
            PipelineStage::LineNotRegex(_, re) => {
                if re.is_match(line) {
                    return None;
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
            PipelineStage::Unwrap(_) => {
                // No-op during filtering — the metric evaluator reads the
                // field name and looks it up in the extracted map.
            }
            PipelineStage::RegexpExtract(_, re) => {
                if let Some(caps) = re.captures(line) {
                    for name in re.capture_names().flatten() {
                        if let Some(m) = caps.name(name) {
                            extracted.insert(name.to_string(), m.as_str().to_string());
                        }
                    }
                }
            }
            PipelineStage::CompareFilter { key, op, value } => {
                let label_val = match extracted.get(key.as_str()) {
                    Some(v) => v.as_str(),
                    None => return None,
                };
                let num: f64 = match label_val.trim().parse() {
                    Ok(n) => n,
                    Err(_) => return None,
                };
                let passes = match op {
                    CompareOp::Gt => num > *value,
                    CompareOp::Gte => num >= *value,
                    CompareOp::Lt => num < *value,
                    CompareOp::Lte => num <= *value,
                };
                if !passes {
                    return None;
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
                    None => return None, // label not found => filter fails
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
                    return None;
                }
            }
        }
    }
    Some(extracted)
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

fn hex_bytes<const N: usize>(b: &[u8; N]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(N * 2);
    for byte in b {
        let _ = write!(s, "{byte:02x}");
    }
    s
}

#[cfg(test)]
mod tests;
