//! PromQL binary expression evaluation.
//!
//! Supports `on()` / `ignoring()` label modifiers and `group_left` /
//! `group_right` many-to-one / one-to-many joins, plus the `bool` modifier
//! for comparison operators.

use promql_parser::parser::{BinaryExpr, LabelModifier, VectorMatchCardinality};
use rustc_hash::FxHashMap;

use super::types::{SampleLookup, SeriesLabelSet};
use super::{PromQLError, PromQLResult, SeriesResult};

/// Which labels to use for matching two vectors in a binary op.
#[derive(Debug, Clone)]
enum MatchMode {
    /// Default: match on all labels except `__name__`.
    AllLabels,
    /// `on(a, b, ...)`: match only on the listed labels.
    On(Vec<String>),
    /// `ignoring(a, b, ...)`: match on all labels except `__name__` and the listed labels.
    Ignoring(Vec<String>),
}

impl MatchMode {
    /// Extract the match mode from an optional `LabelModifier`.
    fn from_modifier(modifier: &Option<LabelModifier>) -> Self {
        match modifier {
            Some(LabelModifier::Include(labels)) => {
                MatchMode::On(labels.labels.iter().map(|l| l.to_string()).collect())
            }
            Some(LabelModifier::Exclude(labels)) => {
                MatchMode::Ignoring(labels.labels.iter().map(|l| l.to_string()).collect())
            }
            None => MatchMode::AllLabels,
        }
    }

    /// Compute the match key from a series' label set.
    fn match_key<'a>(&'a self, labels: &'a [(String, String)]) -> SeriesLabelSet {
        match self {
            MatchMode::AllLabels => labels_without_metric_name(labels),
            MatchMode::On(names) => {
                let ns: std::collections::HashSet<&str> =
                    names.iter().map(String::as_str).collect();
                labels
                    .iter()
                    .filter(|(k, _)| ns.contains(k.as_str()))
                    .cloned()
                    .collect()
            }
            MatchMode::Ignoring(names) => {
                let ns: std::collections::HashSet<&str> =
                    names.iter().map(String::as_str).collect();
                labels
                    .iter()
                    .filter(|(k, _)| k != "__name__" && !ns.contains(k.as_str()))
                    .cloned()
                    .collect()
            }
        }
    }
}

/// Many-to-one / one-to-many join info.
#[derive(Debug, Clone)]
enum JoinMode {
    /// Default one-to-one (or many-to-many for comparisons).
    Plain,
    /// `group_left(labels)`: many LHS series → one RHS series, carrying `labels` from RHS.
    GroupLeft(Vec<String>),
    /// `group_right(labels)`: one LHS series → many RHS series, carrying `labels` from LHS.
    GroupRight(Vec<String>),
}

impl JoinMode {
    fn from_cardinality(card: &VectorMatchCardinality) -> Self {
        match card {
            VectorMatchCardinality::ManyToOne(labels) => {
                JoinMode::GroupLeft(labels.labels.iter().map(|l| l.to_string()).collect())
            }
            VectorMatchCardinality::OneToMany(labels) => {
                JoinMode::GroupRight(labels.labels.iter().map(|l| l.to_string()).collect())
            }
            _ => JoinMode::Plain,
        }
    }
}

pub(super) fn eval_binary_result(
    bin: &BinaryExpr,
    lhs: PromQLResult,
    rhs: PromQLResult,
) -> Result<PromQLResult, PromQLError> {
    let op = bin.op.to_string();
    let default_mod = promql_parser::parser::BinModifier::default();
    let mod_ref = bin.modifier.as_ref().unwrap_or(&default_mod);
    let match_mode = MatchMode::from_modifier(&mod_ref.matching);
    let join_mode = JoinMode::from_cardinality(&mod_ref.card);
    let return_bool = mod_ref.return_bool;
    let is_cmp = is_comparison(&op);

    match (&lhs, &rhs) {
        (PromQLResult::Scalar(l), PromQLResult::Scalar(r)) => {
            Ok(PromQLResult::Scalar(apply_binary_op(&op, *l, *r)))
        }
        (PromQLResult::InstantVector(series), PromQLResult::Scalar(scalar))
        | (PromQLResult::Scalar(scalar), PromQLResult::InstantVector(series)) => {
            let is_lhs_scalar = matches!(lhs, PromQLResult::Scalar(_));
            let results: Vec<SeriesResult> = series
                .iter()
                .map(|sr| {
                    let samples: Vec<(i64, f64)> = sr
                        .samples
                        .iter()
                        .filter_map(|&(t, v)| {
                            let result = if is_lhs_scalar {
                                apply_binary_op(&op, *scalar, v)
                            } else {
                                apply_binary_op(&op, v, *scalar)
                            };
                            comparison_sample(&op, t, v, result, return_bool)
                        })
                        .collect();
                    SeriesResult {
                        labels: sr.labels.clone(),
                        samples,
                    }
                })
                .filter(|sr| !sr.samples.is_empty())
                .collect();
            Ok(PromQLResult::InstantVector(results))
        }
        (PromQLResult::InstantVector(lhs_series), PromQLResult::InstantVector(rhs_series)) => {
            eval_vector_vector(
                &op,
                lhs_series,
                rhs_series,
                &match_mode,
                &join_mode,
                return_bool,
                is_cmp,
            )
        }
        _ => Err(PromQLError::Unsupported(
            "unsupported binary operand types".into(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_vector_vector(
    op: &str,
    lhs_series: &[SeriesResult],
    rhs_series: &[SeriesResult],
    match_mode: &MatchMode,
    join_mode: &JoinMode,
    return_bool: bool,
    is_cmp: bool,
) -> Result<PromQLResult, PromQLError> {
    // Index RHS by match key for lookup.
    // Each entry stores the series reference (for label carry in joins) and the sample map.
    type RhsEntry<'a> = (&'a SeriesResult, SampleLookup);
    let mut rhs_by_key: FxHashMap<SeriesLabelSet, Vec<RhsEntry>> = FxHashMap::default();
    for rs in rhs_series {
        let key = match_mode.match_key(&rs.labels);
        rhs_by_key
            .entry(key)
            .or_default()
            .push((rs, sample_lookup(&rs.samples)));
    }

    let mut results = Vec::new();

    match join_mode {
        JoinMode::Plain => {
            // One-to-one (arithmetic) or many-to-many (comparison/logical).
            for ls in lhs_series {
                let key = match_mode.match_key(&ls.labels);
                let Some(rhs_matches) = rhs_by_key.get(&key) else {
                    continue;
                };
                for (_rhs_sr, rhs_samples) in rhs_matches {
                    let samples: Vec<(i64, f64)> = ls
                        .samples
                        .iter()
                        .filter_map(|&(t, lv)| {
                            rhs_samples.get(&t).and_then(|&rv| {
                                let result = apply_binary_op(op, lv, rv);
                                comparison_sample(op, t, lv, result, return_bool)
                            })
                        })
                        .collect();
                    if !samples.is_empty() {
                        let labels = if is_cmp {
                            match_mode.match_key(&ls.labels)
                        } else {
                            // For arithmetic ops, drop __name__ from result labels.
                            labels_without_metric_name(&ls.labels)
                        };
                        results.push(SeriesResult { labels, samples });
                    }
                }
            }
        }
        JoinMode::GroupLeft(carry_labels) => {
            // Many-to-one: multiple LHS series match one RHS series.
            // Result keeps LHS labels + carries specified labels from RHS.
            for ls in lhs_series {
                let key = match_mode.match_key(&ls.labels);
                let Some(rhs_matches) = rhs_by_key.get(&key) else {
                    continue;
                };
                // group_left: use the first RHS match (there should be exactly one).
                if rhs_matches.len() > 1 {
                    return Err(PromQLError::Eval(format!(
                        "group_left: multiple RHS series match labels {:?}",
                        key
                    )));
                }
                let (rhs_sr, rhs_samples) = &rhs_matches[0];
                let samples: Vec<(i64, f64)> = ls
                    .samples
                    .iter()
                    .filter_map(|&(t, lv)| {
                        rhs_samples.get(&t).and_then(|&rv| {
                            let result = apply_binary_op(op, lv, rv);
                            comparison_sample(op, t, lv, result, return_bool)
                        })
                    })
                    .collect();
                if !samples.is_empty() {
                    let mut labels = labels_without_metric_name(&ls.labels);
                    let carry_set: std::collections::HashSet<&str> =
                        carry_labels.iter().map(String::as_str).collect();
                    for (k, v) in &rhs_sr.labels {
                        if carry_set.contains(k.as_str()) {
                            if let Some(existing) = labels.iter_mut().find(|(ek, _)| ek == k) {
                                existing.1 = v.clone();
                            } else {
                                labels.push((k.clone(), v.clone()));
                            }
                        }
                    }
                    labels.sort_by(|a, b| a.0.cmp(&b.0));
                    results.push(SeriesResult { labels, samples });
                }
            }
        }
        JoinMode::GroupRight(carry_labels) => {
            // One-to-many: one LHS series matches multiple RHS series.
            // Result keeps RHS labels + carries specified labels from LHS.
            for ls in lhs_series {
                let key = match_mode.match_key(&ls.labels);
                let Some(rhs_matches) = rhs_by_key.get(&key) else {
                    continue;
                };
                if rhs_matches.len() > 1 {
                    return Err(PromQLError::Eval(format!(
                        "group_right: multiple LHS series match labels {:?}",
                        key
                    )));
                }
                let carry_set: std::collections::HashSet<&str> =
                    carry_labels.iter().map(String::as_str).collect();
                for (rhs_sr, rhs_samples) in rhs_matches {
                    let samples: Vec<(i64, f64)> = ls
                        .samples
                        .iter()
                        .filter_map(|&(t, lv)| {
                            rhs_samples.get(&t).and_then(|&rv| {
                                let result = apply_binary_op(op, lv, rv);
                                comparison_sample(op, t, lv, result, return_bool)
                            })
                        })
                        .collect();
                    if !samples.is_empty() {
                        let mut labels = labels_without_metric_name(&rhs_sr.labels);
                        for (k, v) in &ls.labels {
                            if carry_set.contains(k.as_str()) {
                                if let Some(existing) = labels.iter_mut().find(|(ek, _)| ek == k) {
                                    existing.1 = v.clone();
                                } else {
                                    labels.push((k.clone(), v.clone()));
                                }
                            }
                        }
                        labels.sort_by(|a, b| a.0.cmp(&b.0));
                        results.push(SeriesResult { labels, samples });
                    }
                }
            }
        }
    }

    Ok(PromQLResult::InstantVector(results))
}

fn labels_without_metric_name(labels: &[(String, String)]) -> SeriesLabelSet {
    labels
        .iter()
        .filter(|(k, _)| k != "__name__")
        .cloned()
        .collect()
}

fn sample_lookup(samples: &[(i64, f64)]) -> SampleLookup {
    samples.iter().copied().collect()
}

fn comparison_sample(
    op: &str,
    timestamp_ms: i64,
    lhs_value: f64,
    result: f64,
    return_bool: bool,
) -> Option<(i64, f64)> {
    if is_comparison(op) {
        if return_bool {
            // bool modifier: return 1.0/0.0 for all series (no filtering).
            Some((timestamp_ms, result))
        } else if result == 0.0 {
            // Comparison false → filter out.
            None
        } else {
            // Comparison true → keep original value.
            Some((timestamp_ms, lhs_value))
        }
    } else {
        Some((timestamp_ms, result))
    }
}

fn apply_binary_op(op: &str, l: f64, r: f64) -> f64 {
    match op {
        "+" => l + r,
        "-" => l - r,
        "*" => l * r,
        "/" => {
            if r == 0.0 {
                f64::NAN
            } else {
                l / r
            }
        }
        "%" => {
            if r == 0.0 {
                f64::NAN
            } else {
                l % r
            }
        }
        ">" => {
            if l > r {
                1.0
            } else {
                0.0
            }
        }
        "<" => {
            if l < r {
                1.0
            } else {
                0.0
            }
        }
        ">=" => {
            if l >= r {
                1.0
            } else {
                0.0
            }
        }
        "<=" => {
            if l <= r {
                1.0
            } else {
                0.0
            }
        }
        "==" => {
            if (l - r).abs() < f64::EPSILON {
                1.0
            } else {
                0.0
            }
        }
        "!=" => {
            if (l - r).abs() >= f64::EPSILON {
                1.0
            } else {
                0.0
            }
        }
        "^" => l.powf(r),
        "and" => {
            if result_truthy(l) {
                r
            } else {
                0.0
            }
        }
        "or" => {
            if result_truthy(l) {
                l
            } else {
                r
            }
        }
        "unless" => {
            if result_truthy(l) {
                0.0
            } else {
                r
            }
        }
        _ => f64::NAN,
    }
}

fn result_truthy(v: f64) -> bool {
    !v.is_nan() && v != 0.0
}

fn is_comparison(op: &str) -> bool {
    matches!(op, ">" | "<" | ">=" | "<=" | "==" | "!=")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_binary_arithmetic() {
        assert_eq!(apply_binary_op("+", 1.0, 2.0), 3.0);
        assert_eq!(apply_binary_op("-", 5.0, 3.0), 2.0);
        assert_eq!(apply_binary_op("*", 3.0, 4.0), 12.0);
        assert_eq!(apply_binary_op("/", 10.0, 4.0), 2.5);
        assert!(apply_binary_op("/", 1.0, 0.0).is_nan());
    }

    #[test]
    fn test_apply_binary_comparison() {
        assert_eq!(apply_binary_op(">", 5.0, 3.0), 1.0);
        assert_eq!(apply_binary_op(">", 3.0, 5.0), 0.0);
        assert_eq!(apply_binary_op("<=", 3.0, 3.0), 1.0);
    }

    #[test]
    fn test_comparison_sample_without_bool_filters() {
        // > 5 is false for value 3 → None
        assert_eq!(comparison_sample(">", 100, 3.0, 0.0, false), None);
        // > 5 is true for value 10 → keep original value
        assert_eq!(
            comparison_sample(">", 100, 10.0, 1.0, false),
            Some((100, 10.0))
        );
    }

    #[test]
    fn test_comparison_sample_with_bool_returns_zero_or_one() {
        // bool mode: returns the comparison result (0.0 or 1.0) without filtering
        assert_eq!(
            comparison_sample(">", 100, 3.0, 0.0, true),
            Some((100, 0.0))
        );
        assert_eq!(
            comparison_sample(">", 100, 10.0, 1.0, true),
            Some((100, 1.0))
        );
    }
}
