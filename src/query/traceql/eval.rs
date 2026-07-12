//! TraceQL evaluator against TraceStore.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::store::trace_store::{AttributeValue, Span, SpanKind, SpanStatus, TraceStore};

use super::parser::{
    AttrScope, CompareOp, LogicalOp, PipelineStage, SpanCondition, SpanKindValue, SpanStatusValue,
    SpanValue, StructuralOp, TraceQLExpr,
};

/// Result of a TraceQL evaluation.
#[derive(Debug, Clone)]
pub struct TraceResult {
    pub trace_id: [u8; 16],
    pub matched_spans: Vec<MatchedSpan>,
}

/// A span that matched the query.
#[derive(Debug, Clone)]
pub struct MatchedSpan {
    pub span_id: [u8; 8],
    pub name: String,
    pub service_name: String,
    pub start_time_ns: i64,
    pub duration_ns: i64,
    pub status: SpanStatus,
    pub attributes: Vec<(String, String)>,
}

/// Evaluate a TraceQL expression against the trace store.
pub fn evaluate_traceql(expr: &TraceQLExpr, store: &TraceStore) -> Vec<TraceResult> {
    match expr {
        TraceQLExpr::SpanSelector {
            conditions,
            logical_ops,
        } => eval_span_selector(conditions, logical_ops, store),
        TraceQLExpr::Structural { op, lhs, rhs } => eval_structural(op, lhs, rhs, store),
        TraceQLExpr::Pipeline {
            inner,
            pipeline_stages,
        } => {
            let mut results = evaluate_traceql(inner, store);
            for stage in pipeline_stages {
                results = apply_pipeline_stage(results, stage);
            }
            results
        }
    }
}

/// Compare a u64 value against a target using the given operator.
fn compare_u64(a: u64, op: &CompareOp, b: u64) -> bool {
    match op {
        CompareOp::Eq => a == b,
        CompareOp::Neq => a != b,
        CompareOp::Gt => a > b,
        CompareOp::Lt => a < b,
        CompareOp::Gte => a >= b,
        CompareOp::Lte => a <= b,
        CompareOp::Regex => false,
    }
}

/// Compare an i64 duration (nanoseconds) against a target using the given operator.
fn compare_duration_ns(a: i64, op: &CompareOp, b: i64) -> bool {
    match op {
        CompareOp::Eq => a == b,
        CompareOp::Neq => a != b,
        CompareOp::Gt => a > b,
        CompareOp::Lt => a < b,
        CompareOp::Gte => a >= b,
        CompareOp::Lte => a <= b,
        CompareOp::Regex => false,
    }
}

/// Apply a pipeline stage to filter trace results.
fn apply_pipeline_stage(results: Vec<TraceResult>, stage: &PipelineStage) -> Vec<TraceResult> {
    match stage {
        PipelineStage::CountFilter { op, value } => results
            .into_iter()
            .filter(|r| {
                let count = r.matched_spans.len() as u64;
                compare_u64(count, op, *value)
            })
            .collect(),
        PipelineStage::AvgDuration { op, value_ns } => results
            .into_iter()
            .filter(|r| {
                if r.matched_spans.is_empty() {
                    return false;
                }
                let total: i128 = r.matched_spans.iter().map(|s| s.duration_ns as i128).sum();
                let avg_i128 = total / r.matched_spans.len() as i128;
                let avg = avg_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
                compare_duration_ns(avg, op, *value_ns)
            })
            .collect(),
        PipelineStage::MaxDuration { op, value_ns } => results
            .into_iter()
            .filter(|r| {
                if r.matched_spans.is_empty() {
                    return false;
                }
                let max_dur = r
                    .matched_spans
                    .iter()
                    .map(|s| s.duration_ns)
                    .max()
                    .unwrap_or(0);
                compare_duration_ns(max_dur, op, *value_ns)
            })
            .collect(),
        PipelineStage::MinDuration { op, value_ns } => results
            .into_iter()
            .filter(|r| {
                if r.matched_spans.is_empty() {
                    return false;
                }
                let min_dur = r
                    .matched_spans
                    .iter()
                    .map(|s| s.duration_ns)
                    .min()
                    .unwrap_or(0);
                compare_duration_ns(min_dur, op, *value_ns)
            })
            .collect(),
    }
}

fn eval_span_selector(
    conditions: &[SpanCondition],
    logical_ops: &[LogicalOp],
    store: &TraceStore,
) -> Vec<TraceResult> {
    // Pre-compile regex patterns once, indexed by condition position.
    let compiled_regexes = precompile_regexes(conditions);

    // Narrow the candidate trace set using the service/name/status indexes.
    // Only valid under pure conjunction (no OR): index intersection assumes
    // every narrowed condition must hold. Neq and regex conditions are never
    // indexable, so they simply don't contribute and force a full per-span
    // check (which still runs for correctness on the narrowed traces).
    let candidate_ids = candidate_trace_ids(conditions, logical_ops, store);

    let mut results: Vec<TraceResult> = Vec::new();

    let mut scan = |spans: &Vec<Span>, trace_id: [u8; 16]| {
        let matched: Vec<MatchedSpan> = spans
            .iter()
            .filter(|span| {
                span_matches_conditions(span, conditions, logical_ops, &compiled_regexes, store)
            })
            .map(|span| span_to_matched(span, store))
            .collect();

        if !matched.is_empty() {
            results.push(TraceResult {
                trace_id,
                matched_spans: matched,
            });
        }
    };

    match candidate_ids {
        Some(ids) => {
            for trace_id in ids {
                if let Some(spans) = store.traces.get(&trace_id) {
                    scan(spans, trace_id);
                }
            }
        }
        None => {
            for (&trace_id, spans) in &store.traces {
                scan(spans, trace_id);
            }
        }
    }

    // Sort by trace_id for deterministic output
    results.sort_by_key(|a| a.trace_id);
    results
}

/// Compute a narrowed candidate trace-ID set from indexable `=` conditions.
///
/// Returns `None` when narrowing is unsafe (an OR is present) or when no
/// condition is indexable — callers then fall back to a full scan. Only `=`
/// (Eq) on name, status, or `resource.service.name` is indexable; `!=` never
/// is, because the indexes map a key to traces that *contain* a matching span,
/// not traces that *only* contain it.
fn candidate_trace_ids(
    conditions: &[SpanCondition],
    logical_ops: &[LogicalOp],
    store: &TraceStore,
) -> Option<Vec<[u8; 16]>> {
    if logical_ops.contains(&LogicalOp::Or) {
        return None;
    }

    let mut candidate: Option<FxHashSet<[u8; 16]>> = None;
    for cond in conditions {
        if let Some(set) = indexable_traces(cond, store) {
            candidate = Some(match candidate {
                None => set,
                Some(cur) => cur.intersection(&set).copied().collect(),
            });
        }
    }
    candidate.map(|s| s.into_iter().collect())
}

/// Look up the trace-ID set an indexable `=` condition resolves to, or `None`
/// when the condition is not indexable (or the key isn't present in the store).
fn indexable_traces(cond: &SpanCondition, store: &TraceStore) -> Option<FxHashSet<[u8; 16]>> {
    match cond {
        SpanCondition::Name {
            op: CompareOp::Eq,
            value,
        } => store
            .interner
            .get(value)
            .and_then(|spur| store.name_index.get(&spur).cloned()),
        SpanCondition::Status {
            op: CompareOp::Eq,
            value,
        } => {
            let status = match value {
                SpanStatusValue::Ok => SpanStatus::Ok,
                SpanStatusValue::Error => SpanStatus::Error,
                SpanStatusValue::Unset => SpanStatus::Unset,
            };
            store.status_index.get(&status).cloned()
        }
        SpanCondition::Attribute {
            scope,
            name,
            op: CompareOp::Eq,
            value: SpanValue::String(s),
        } => indexed_attribute_traces(scope, name, s, store),
        _ => None,
    }
}

/// Resolve an indexable string `=` attribute condition to its candidate trace
/// set. `resource.service.name` uses the dedicated service index; every other
/// key consults the generic string attribute-value index, trying the same
/// candidate keys the per-span evaluator matches (`span.<name>`; and
/// `resource.<name>` plus the bare `<name>` fallback for resource scope).
fn indexed_attribute_traces(
    scope: &AttrScope,
    name: &str,
    value: &str,
    store: &TraceStore,
) -> Option<FxHashSet<[u8; 16]>> {
    if *scope == AttrScope::Resource && name == "service.name" {
        return store
            .interner
            .get(value)
            .and_then(|spur| store.service_index.get(&spur).cloned());
    }
    let candidate_keys: Vec<String> = match scope {
        AttrScope::Span => vec![format!("span.{}", name)],
        AttrScope::Resource => vec![format!("resource.{}", name), name.to_string()],
    };
    let val_spur = store.interner.get(value)?;
    let mut out: FxHashSet<[u8; 16]> = FxHashSet::default();
    for key_str in &candidate_keys {
        if let Some(key_spur) = store.interner.get(key_str)
            && let Some(values) = store.attr_index.get(&key_spur)
            && let Some(set) = values.get(&val_spur)
        {
            out.extend(set.iter().copied());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Pre-compile regex patterns from conditions. Returns one Option<Regex> per condition.
fn precompile_regexes(conditions: &[SpanCondition]) -> Vec<Option<regex::Regex>> {
    conditions
        .iter()
        .map(|cond| {
            let (op, pattern) = match cond {
                SpanCondition::Attribute {
                    op,
                    value: SpanValue::String(s),
                    ..
                } => (op, s.as_str()),
                SpanCondition::Name { op, value } => (op, value.as_str()),
                _ => return None,
            };
            if *op == CompareOp::Regex {
                regex::Regex::new(pattern).ok()
            } else {
                None
            }
        })
        .collect()
}

fn eval_structural(
    op: &StructuralOp,
    lhs: &TraceQLExpr,
    rhs: &TraceQLExpr,
    store: &TraceStore,
) -> Vec<TraceResult> {
    let lhs_results = evaluate_traceql(lhs, store);
    let rhs_results = evaluate_traceql(rhs, store);

    let rhs_by_trace: FxHashMap<[u8; 16], &TraceResult> =
        rhs_results.iter().map(|r| (r.trace_id, r)).collect();

    let mut results = Vec::new();
    for lhs_result in &lhs_results {
        let Some(rhs_result) = rhs_by_trace.get(&lhs_result.trace_id) else {
            continue;
        };
        let Some(trace_spans) = store.get_trace(&lhs_result.trace_id) else {
            continue;
        };
        // Build span_map once per trace for parent lookups.
        let span_map: FxHashMap<[u8; 8], &Span> =
            trace_spans.iter().map(|s| (s.span_id, s)).collect();

        let mut matched = Vec::new();
        for lhs_span in &lhs_result.matched_spans {
            for rhs_span in &rhs_result.matched_spans {
                let related = match op {
                    StructuralOp::Descendant => {
                        is_descendant(lhs_span.span_id, rhs_span.span_id, &span_map)
                    }
                    StructuralOp::Child => is_child(lhs_span.span_id, rhs_span.span_id, &span_map),
                    StructuralOp::Sibling => {
                        is_sibling(lhs_span.span_id, rhs_span.span_id, &span_map)
                    }
                };
                if related {
                    matched.push(rhs_span.clone());
                }
            }
        }

        if !matched.is_empty() {
            matched.sort_by_key(|s| s.span_id);
            matched.dedup_by_key(|s| s.span_id);
            results.push(TraceResult {
                trace_id: lhs_result.trace_id,
                matched_spans: matched,
            });
        }
    }

    results
}

/// Check if `parent_id` is the direct parent of `child_id`.
fn is_child(parent_id: [u8; 8], child_id: [u8; 8], span_map: &FxHashMap<[u8; 8], &Span>) -> bool {
    span_map.get(&child_id).and_then(|s| s.parent_span_id) == Some(parent_id)
}

/// Check if `a_id` and `b_id` share the same parent (and are distinct spans).
fn is_sibling(a_id: [u8; 8], b_id: [u8; 8], span_map: &FxHashMap<[u8; 8], &Span>) -> bool {
    if a_id == b_id {
        return false;
    }
    let a_parent = span_map.get(&a_id).and_then(|s| s.parent_span_id);
    let b_parent = span_map.get(&b_id).and_then(|s| s.parent_span_id);
    a_parent == b_parent
}

/// Check if `ancestor_span_id` is an ancestor of `descendant_span_id`.
fn is_descendant(
    ancestor_span_id: [u8; 8],
    descendant_span_id: [u8; 8],
    span_map: &FxHashMap<[u8; 8], &Span>,
) -> bool {
    let mut current_id = descendant_span_id;
    let mut visited = FxHashSet::default();

    loop {
        if current_id == ancestor_span_id {
            return false; // same span, not descendant
        }

        if let Some(span) = span_map.get(&current_id) {
            if let Some(parent_id) = span.parent_span_id {
                if parent_id == ancestor_span_id {
                    return true;
                }
                if visited.contains(&parent_id) {
                    return false; // cycle detection
                }
                visited.insert(current_id);
                current_id = parent_id;
            } else {
                return false; // reached root
            }
        } else {
            return false;
        }
    }
}

fn span_matches_conditions(
    span: &Span,
    conditions: &[SpanCondition],
    logical_ops: &[LogicalOp],
    compiled_regexes: &[Option<regex::Regex>],
    store: &TraceStore,
) -> bool {
    if conditions.is_empty() {
        return true; // empty selector `{}` matches all spans
    }

    // Evaluate left-to-right with AND binding tighter than OR.
    // Split by OR first: result is true if ANY OR-group is true.
    // Within each OR-group (connected by AND), ALL conditions must match.
    let mut current_and_result = span_matches_condition(
        span,
        &conditions[0],
        compiled_regexes.first().and_then(|r| r.as_ref()),
        store,
    );

    for i in 0..logical_ops.len() {
        let next_match = span_matches_condition(
            span,
            &conditions[i + 1],
            compiled_regexes.get(i + 1).and_then(|r| r.as_ref()),
            store,
        );
        match logical_ops[i] {
            LogicalOp::And => {
                current_and_result = current_and_result && next_match;
            }
            LogicalOp::Or => {
                // Short-circuit: if the current AND-group matched, the whole expression is true
                if current_and_result {
                    return true;
                }
                // Start a new AND-group
                current_and_result = next_match;
            }
        }
    }

    current_and_result
}

fn span_matches_condition(
    span: &Span,
    condition: &SpanCondition,
    compiled_regex: Option<&regex::Regex>,
    store: &TraceStore,
) -> bool {
    match condition {
        SpanCondition::Duration { op, value } => {
            let span_dur = std::time::Duration::from_nanos(span.duration_ns.max(0) as u64);
            compare_duration(&span_dur, op, value)
        }
        SpanCondition::Status { op, value } => {
            let status_matches = match value {
                SpanStatusValue::Ok => span.status == SpanStatus::Ok,
                SpanStatusValue::Error => span.status == SpanStatus::Error,
                SpanStatusValue::Unset => span.status == SpanStatus::Unset,
            };
            match op {
                CompareOp::Eq => status_matches,
                CompareOp::Neq => !status_matches,
                _ => false,
            }
        }
        SpanCondition::Name { op, value } => {
            let span_name = store.resolve(&span.name);
            compare_string(span_name, op, value, compiled_regex)
        }
        SpanCondition::Kind { op, value } => {
            let kind_matches = match value {
                SpanKindValue::Unspecified => span.kind == SpanKind::Unspecified,
                SpanKindValue::Internal => span.kind == SpanKind::Internal,
                SpanKindValue::Server => span.kind == SpanKind::Server,
                SpanKindValue::Client => span.kind == SpanKind::Client,
                SpanKindValue::Producer => span.kind == SpanKind::Producer,
                SpanKindValue::Consumer => span.kind == SpanKind::Consumer,
            };
            match op {
                CompareOp::Eq => kind_matches,
                CompareOp::Neq => !kind_matches,
                _ => false,
            }
        }
        SpanCondition::EventName { op, value } => span
            .events
            .iter()
            .any(|ev| compare_string(store.resolve(&ev.name), op, value, compiled_regex)),
        SpanCondition::EventAttribute { name, op, value } => {
            let mut found_key = false;
            for ev in &span.events {
                for (k, v) in &ev.attributes {
                    if store.resolve(k) == name {
                        found_key = true;
                        if compare_attribute_value(v, op, value, compiled_regex, store) {
                            return true;
                        }
                    }
                }
            }
            // Neq matches when no event carries the key at all, consistent
            // with the absent-attribute semantics of span attribute Neq.
            !found_key && matches!(op, CompareOp::Neq)
        }
        SpanCondition::Attribute {
            scope,
            name,
            op,
            value,
        } => {
            let attr_key = match scope {
                AttrScope::Resource => format!("resource.{}", name),
                AttrScope::Span => format!("span.{}", name),
            };
            let fallback_key = match scope {
                AttrScope::Resource => Some(name.as_str()),
                AttrScope::Span => None,
            };

            for (key_spur, attr_val) in &span.attributes {
                let key = store.resolve(key_spur);
                if key == attr_key || fallback_key.is_some_and(|fallback| key == fallback) {
                    return compare_attribute_value(attr_val, op, value, compiled_regex, store);
                }
            }

            matches!(op, CompareOp::Neq)
        }
    }
}

fn compare_duration(
    span_dur: &std::time::Duration,
    op: &CompareOp,
    target: &std::time::Duration,
) -> bool {
    match op {
        CompareOp::Eq => span_dur == target,
        CompareOp::Neq => span_dur != target,
        CompareOp::Gt => span_dur > target,
        CompareOp::Lt => span_dur < target,
        CompareOp::Gte => span_dur >= target,
        CompareOp::Lte => span_dur <= target,
        CompareOp::Regex => false,
    }
}

fn compare_string(
    actual: &str,
    op: &CompareOp,
    expected: &str,
    compiled_regex: Option<&regex::Regex>,
) -> bool {
    match op {
        CompareOp::Eq => actual == expected,
        CompareOp::Neq => actual != expected,
        CompareOp::Regex => compiled_regex
            .map(|re| re.is_match(actual))
            .unwrap_or(false),
        CompareOp::Gt => actual > expected,
        CompareOp::Lt => actual < expected,
        CompareOp::Gte => actual >= expected,
        CompareOp::Lte => actual <= expected,
    }
}

fn compare_attribute_value(
    attr: &AttributeValue,
    op: &CompareOp,
    target: &SpanValue,
    compiled_regex: Option<&regex::Regex>,
    store: &TraceStore,
) -> bool {
    match (attr, target) {
        (AttributeValue::String(s), SpanValue::String(t)) => {
            compare_string(store.resolve(s), op, t, compiled_regex)
        }
        (AttributeValue::Int(i), SpanValue::Int(t)) => compare_i64(*i, op, *t),
        (AttributeValue::Int(i), SpanValue::Float(t)) => compare_f64(*i as f64, op, *t),
        (AttributeValue::Float(f), SpanValue::Float(t)) => compare_f64(*f, op, *t),
        (AttributeValue::Float(f), SpanValue::Int(t)) => compare_f64(*f, op, *t as f64),
        (AttributeValue::String(s), SpanValue::Int(t)) => {
            // Try to parse string as int
            if let Ok(i) = store.resolve(s).parse::<i64>() {
                compare_i64(i, op, *t)
            } else {
                false
            }
        }
        (AttributeValue::Bool(b), SpanValue::Bool(t)) => match op {
            CompareOp::Eq => b == t,
            CompareOp::Neq => b != t,
            _ => false,
        },
        _ => false,
    }
}

fn compare_i64(a: i64, op: &CompareOp, b: i64) -> bool {
    match op {
        CompareOp::Eq => a == b,
        CompareOp::Neq => a != b,
        CompareOp::Gt => a > b,
        CompareOp::Lt => a < b,
        CompareOp::Gte => a >= b,
        CompareOp::Lte => a <= b,
        CompareOp::Regex => false,
    }
}

fn compare_f64(a: f64, op: &CompareOp, b: f64) -> bool {
    match op {
        CompareOp::Eq => (a - b).abs() < f64::EPSILON,
        CompareOp::Neq => (a - b).abs() >= f64::EPSILON,
        CompareOp::Gt => a > b,
        CompareOp::Lt => a < b,
        CompareOp::Gte => a >= b,
        CompareOp::Lte => a <= b,
        CompareOp::Regex => false,
    }
}

fn span_to_matched(span: &Span, store: &TraceStore) -> MatchedSpan {
    let attributes: Vec<(String, String)> = span
        .attributes
        .iter()
        .map(|(k, v)| {
            (
                store.resolve(k).to_string(),
                store.resolve_attribute_value(v),
            )
        })
        .collect();

    MatchedSpan {
        span_id: span.span_id,
        name: store.resolve(&span.name).to_string(),
        service_name: store.resolve(&span.service_name).to_string(),
        start_time_ns: span.start_time_ns,
        duration_ns: span.duration_ns,
        status: span.status,
        attributes,
    }
}

#[cfg(test)]
mod tests;
