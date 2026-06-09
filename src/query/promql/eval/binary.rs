//! PromQL binary expression evaluation.

use rustc_hash::FxHashMap;

use super::types::{SampleLookup, SeriesLabelSet, SeriesLookup};
use super::{PromQLError, PromQLResult, SeriesResult};

pub(super) fn reject_unsupported_modifiers(
    bin: &promql_parser::parser::BinaryExpr,
) -> Result<(), PromQLError> {
    if let Some(modifier) = &bin.modifier {
        if modifier.matching.is_some() {
            return Err(PromQLError::Unsupported(
                "binary expression modifiers on()/ignoring() are not yet supported".into(),
            ));
        }
        match &modifier.card {
            promql_parser::parser::VectorMatchCardinality::ManyToOne(_)
            | promql_parser::parser::VectorMatchCardinality::OneToMany(_) => {
                return Err(PromQLError::Unsupported(
                    "binary expression modifiers group_left/group_right are not yet supported"
                        .into(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn eval_binary_result(
    op: &str,
    lhs: PromQLResult,
    rhs: PromQLResult,
) -> Result<PromQLResult, PromQLError> {
    match (&lhs, &rhs) {
        (PromQLResult::Scalar(l), PromQLResult::Scalar(r)) => {
            Ok(PromQLResult::Scalar(apply_binary_op(op, *l, *r)))
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
                                apply_binary_op(op, *scalar, v)
                            } else {
                                apply_binary_op(op, v, *scalar)
                            };
                            comparison_sample(op, t, v, result)
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
            let mut rhs_by_labels: SeriesLookup = FxHashMap::default();
            for rs in rhs_series {
                rhs_by_labels
                    .entry(labels_without_metric_name(&rs.labels))
                    .or_default()
                    .push(sample_lookup(&rs.samples));
            }

            let mut results = Vec::new();
            for ls in lhs_series {
                let match_labels = labels_without_metric_name(&ls.labels);
                let Some(rhs_matches) = rhs_by_labels.get(&match_labels) else {
                    continue;
                };

                for rhs_samples in rhs_matches {
                    let samples: Vec<(i64, f64)> = ls
                        .samples
                        .iter()
                        .filter_map(|&(t, lv)| {
                            rhs_samples.get(&t).and_then(|&rv| {
                                let result = apply_binary_op(op, lv, rv);
                                comparison_sample(op, t, lv, result)
                            })
                        })
                        .collect();
                    if !samples.is_empty() {
                        results.push(SeriesResult {
                            labels: match_labels.clone(),
                            samples,
                        });
                    }
                }
            }
            Ok(PromQLResult::InstantVector(results))
        }
        _ => Err(PromQLError::Unsupported(
            "unsupported binary operand types".into(),
        )),
    }
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
) -> Option<(i64, f64)> {
    if is_comparison(op) && result == 0.0 {
        None
    } else if is_comparison(op) {
        Some((timestamp_ms, lhs_value))
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
        _ => f64::NAN,
    }
}

fn is_comparison(op: &str) -> bool {
    matches!(op, ">" | "<" | ">=" | "<=" | "==" | "!=")
}
