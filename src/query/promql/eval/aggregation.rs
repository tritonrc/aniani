//! PromQL aggregation operators.

use std::collections::BTreeMap;

use promql_parser::parser::{AggregateExpr, LabelModifier};
use rustc_hash::{FxHashMap, FxHashSet};

use super::{PromQLError, PromQLResult, SeriesResult};

pub(super) fn eval_aggregation(
    agg: &AggregateExpr,
    inner: PromQLResult,
    param: Option<PromQLResult>,
) -> Result<PromQLResult, PromQLError> {
    let series = match inner {
        PromQLResult::InstantVector(s) => s,
        PromQLResult::RangeVector(s) => s,
        PromQLResult::Scalar(v) => {
            return Ok(PromQLResult::Scalar(v));
        }
    };

    let op_name = agg.op.to_string();

    match op_name.as_str() {
        "topk" | "bottomk" => {
            let k = match param {
                Some(PromQLResult::Scalar(v)) => scalar_to_count(v),
                Some(_) => {
                    return Err(PromQLError::Eval(
                        "topk/bottomk parameter must be a scalar".into(),
                    ));
                }
                None => {
                    return Err(PromQLError::Eval(
                        "topk/bottomk requires a parameter k".into(),
                    ));
                }
            };

            let mut sorted_series = series;
            sorted_series.sort_by(|a, b| {
                let a_val = a.samples.last().map(|(_, v)| *v).unwrap_or(f64::NAN);
                let b_val = b.samples.last().map(|(_, v)| *v).unwrap_or(f64::NAN);
                if op_name == "topk" {
                    b_val
                        .partial_cmp(&a_val)
                        .unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    a_val
                        .partial_cmp(&b_val)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            });
            sorted_series.truncate(k);
            return Ok(PromQLResult::InstantVector(sorted_series));
        }
        "sum" | "avg" | "max" | "min" | "count" => {}
        other => {
            return Err(PromQLError::Unsupported(format!("aggregation: {}", other)));
        }
    }

    let mut groups: BTreeMap<Vec<(String, String)>, Vec<SeriesResult>> = BTreeMap::new();

    for sr in series {
        let group_labels = compute_group_labels(&sr.labels, &agg.modifier);
        groups.entry(group_labels).or_default().push(sr);
    }

    let mut results = Vec::new();
    for (group_labels, group_series) in groups {
        let aggregated = aggregate_group(&op_name, &group_series);
        results.push(SeriesResult {
            labels: group_labels,
            samples: aggregated,
        });
    }

    Ok(PromQLResult::InstantVector(results))
}

fn compute_group_labels(
    labels: &[(String, String)],
    modifier: &Option<LabelModifier>,
) -> Vec<(String, String)> {
    match modifier {
        Some(LabelModifier::Include(label_names)) => {
            let names: FxHashSet<&str> = label_names.labels.iter().map(String::as_str).collect();
            labels
                .iter()
                .filter(|(k, _)| names.contains(k.as_str()))
                .cloned()
                .collect()
        }
        Some(LabelModifier::Exclude(label_names)) => {
            let names: FxHashSet<&str> = label_names.labels.iter().map(String::as_str).collect();
            labels
                .iter()
                .filter(|(k, _)| !names.contains(k.as_str()))
                .cloned()
                .collect()
        }
        None => Vec::new(),
    }
}

pub(super) fn scalar_to_count(value: f64) -> usize {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= usize::MAX as f64 {
        usize::MAX
    } else {
        value as usize
    }
}

fn aggregate_group(op: &str, series: &[SeriesResult]) -> Vec<(i64, f64)> {
    if series.is_empty() {
        return Vec::new();
    }

    let mut timestamps: Vec<i64> = series
        .iter()
        .flat_map(|s| s.samples.iter().map(|(t, _)| *t))
        .collect();
    timestamps.sort_unstable();
    timestamps.dedup();

    let lookups: Vec<FxHashMap<i64, f64>> = series
        .iter()
        .map(|s| s.samples.iter().copied().collect())
        .collect();

    timestamps
        .iter()
        .filter_map(|&t| {
            let values: Vec<f64> = lookups.iter().filter_map(|m| m.get(&t).copied()).collect();
            if values.is_empty() {
                return None;
            }
            let result = match op {
                "sum" => values.iter().sum(),
                "avg" => values.iter().sum::<f64>() / values.len() as f64,
                "max" => values.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                "min" => values.iter().cloned().fold(f64::INFINITY, f64::min),
                "count" => values.len() as f64,
                _ => unreachable!("unsupported aggregations filtered before reaching here"),
            };
            Some((t, result))
        })
        .collect()
}
