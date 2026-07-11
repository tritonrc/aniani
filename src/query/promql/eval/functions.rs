//! PromQL function helpers.

use std::collections::BTreeMap;

use promql_parser::parser::{Call, Expr, StringLiteral};

use super::{PromQLError, PromQLResult, SeriesResult};

pub(super) fn eval_histogram_quantile(
    quantile: f64,
    buckets_result: PromQLResult,
) -> Result<PromQLResult, PromQLError> {
    let series = match buckets_result {
        PromQLResult::InstantVector(s) => s,
        _ => {
            return Err(PromQLError::Eval(
                "histogram_quantile requires instant vector".into(),
            ));
        }
    };

    type BucketEntry = (f64, Vec<(i64, f64)>);
    let mut groups: BTreeMap<Vec<(String, String)>, Vec<BucketEntry>> = BTreeMap::new();

    for sr in &series {
        let le_val = sr
            .labels
            .iter()
            .find(|(k, _)| k == "le")
            .map(|(_, v)| v.as_str());

        let le = match le_val {
            Some("+Inf") => f64::INFINITY,
            Some(v) => v.parse().unwrap_or(f64::INFINITY),
            None => continue,
        };

        let group_labels: Vec<(String, String)> = sr
            .labels
            .iter()
            .filter(|(k, _)| k != "le")
            .cloned()
            .collect();

        groups
            .entry(group_labels)
            .or_default()
            .push((le, sr.samples.clone()));
    }

    let mut results = Vec::new();
    for (group_labels, mut buckets) in groups {
        buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        if let Some((_, first_samples)) = buckets.first() {
            let timestamps: Vec<i64> = first_samples.iter().map(|(t, _)| *t).collect();
            let mut samples = Vec::new();

            for &t in &timestamps {
                let mut bucket_bounds = Vec::new();
                let mut bucket_counts = Vec::new();

                for (le, s) in &buckets {
                    if let Some((_, v)) = s.iter().find(|(ts, _)| *ts == t) {
                        bucket_bounds.push(*le);
                        bucket_counts.push(*v);
                    }
                }

                if bucket_counts.is_empty() {
                    continue;
                }

                let total = *bucket_counts.last().unwrap_or(&0.0);
                if total == 0.0 {
                    continue;
                }

                let target = quantile * total;
                let mut prev_count = 0.0;
                let mut prev_bound = 0.0;

                let mut found = false;
                for (i, &count) in bucket_counts.iter().enumerate() {
                    if count >= target {
                        let bound = bucket_bounds[i];
                        if bound.is_infinite() {
                            samples.push((t, prev_bound));
                        } else if count == prev_count {
                            samples.push((t, bound));
                        } else {
                            let fraction = (target - prev_count) / (count - prev_count);
                            let value = prev_bound + fraction * (bound - prev_bound);
                            samples.push((t, value));
                        }
                        found = true;
                        break;
                    }
                    prev_count = count;
                    prev_bound = bucket_bounds[i];
                }

                if !found {
                    samples.push((t, *bucket_bounds.last().unwrap_or(&0.0)));
                }
            }

            if !samples.is_empty() {
                results.push(SeriesResult {
                    labels: group_labels,
                    samples,
                });
            }
        }
    }

    Ok(PromQLResult::InstantVector(results))
}

pub(super) fn apply_scalar_func(
    func_name: &str,
    result: PromQLResult,
) -> Result<PromQLResult, PromQLError> {
    match result {
        PromQLResult::InstantVector(series) => {
            let mapped: Vec<SeriesResult> = series
                .into_iter()
                .map(|mut sr| {
                    for sample in &mut sr.samples {
                        sample.1 = match func_name {
                            "abs" => sample.1.abs(),
                            "ceil" => sample.1.ceil(),
                            "floor" => sample.1.floor(),
                            "round" => sample.1.round(),
                            _ => sample.1,
                        };
                    }
                    sr
                })
                .collect();
            Ok(PromQLResult::InstantVector(mapped))
        }
        PromQLResult::Scalar(v) => {
            let result = match func_name {
                "abs" => v.abs(),
                "ceil" => v.ceil(),
                "floor" => v.floor(),
                "round" => v.round(),
                _ => v,
            };
            Ok(PromQLResult::Scalar(result))
        }
        other => Ok(other),
    }
}

/// Clamp each sample value in a vector by optional min/max bounds.
pub(super) fn apply_clamp(
    result: PromQLResult,
    min_val: Option<f64>,
    max_val: Option<f64>,
) -> Result<PromQLResult, PromQLError> {
    match result {
        PromQLResult::InstantVector(series) => {
            let mapped: Vec<SeriesResult> = series
                .into_iter()
                .map(|mut sr| {
                    for sample in &mut sr.samples {
                        if let Some(min) = min_val
                            && sample.1 < min
                        {
                            sample.1 = min;
                        }
                        if let Some(max) = max_val
                            && sample.1 > max
                        {
                            sample.1 = max;
                        }
                    }
                    sr
                })
                .collect();
            Ok(PromQLResult::InstantVector(mapped))
        }
        PromQLResult::Scalar(mut v) => {
            if let Some(min) = min_val
                && v < min
            {
                v = min;
            }
            if let Some(max) = max_val
                && v > max
            {
                v = max;
            }
            Ok(PromQLResult::Scalar(v))
        }
        other => Ok(other),
    }
}

pub(super) fn extract_string_arg(expr: &Expr) -> Result<&str, PromQLError> {
    match expr {
        Expr::StringLiteral(StringLiteral { val, .. }) => Ok(val.as_str()),
        _ => Err(PromQLError::Eval("expected string literal argument".into())),
    }
}

pub(super) fn eval_label_replace(
    call: &Call,
    inner: PromQLResult,
) -> Result<PromQLResult, PromQLError> {
    if call.args.args.len() < 5 {
        return Err(PromQLError::Eval(
            "label_replace requires 5 arguments".into(),
        ));
    }
    let dst_label = extract_string_arg(&call.args.args[1])?;
    let replacement = extract_string_arg(&call.args.args[2])?;
    let src_label = extract_string_arg(&call.args.args[3])?;
    let regex_str = extract_string_arg(&call.args.args[4])?;

    let re = crate::store::compiled_label_regex(regex_str)
        .ok_or_else(|| PromQLError::Eval(format!("invalid regex: {}", regex_str)))?;

    let series = match inner {
        PromQLResult::InstantVector(s) => s,
        PromQLResult::RangeVector(s) => s,
        other => return Ok(other),
    };

    let results: Vec<SeriesResult> = series
        .into_iter()
        .map(|mut sr| {
            let src_value = sr
                .labels
                .iter()
                .find(|(k, _)| k == src_label)
                .map(|(_, v)| v.clone())
                .unwrap_or_default();

            if let Some(caps) = re.captures(&src_value) {
                let mut new_value = replacement.to_string();
                for i in (1..caps.len()).rev() {
                    let group_val = caps.get(i).map(|m| m.as_str()).unwrap_or("");
                    new_value = new_value.replace(&format!("${}", i), group_val);
                }

                if new_value.is_empty() {
                    sr.labels.retain(|(k, _)| k != dst_label);
                } else if let Some(existing) = sr.labels.iter_mut().find(|(k, _)| k == dst_label) {
                    existing.1 = new_value;
                } else {
                    sr.labels.push((dst_label.to_string(), new_value));
                    sr.labels.sort_by(|a, b| a.0.cmp(&b.0));
                }
            }
            sr
        })
        .collect();

    Ok(PromQLResult::InstantVector(results))
}

pub(super) fn eval_label_join(
    call: &Call,
    inner: PromQLResult,
) -> Result<PromQLResult, PromQLError> {
    if call.args.args.len() < 4 {
        return Err(PromQLError::Eval(
            "label_join requires at least 4 arguments".into(),
        ));
    }
    let dst_label = extract_string_arg(&call.args.args[1])?;
    let separator = extract_string_arg(&call.args.args[2])?;

    let src_labels: Vec<&str> = call.args.args[3..]
        .iter()
        .map(|expr| extract_string_arg(expr))
        .collect::<Result<_, _>>()?;

    let series = match inner {
        PromQLResult::InstantVector(s) => s,
        PromQLResult::RangeVector(s) => s,
        other => return Ok(other),
    };

    let results: Vec<SeriesResult> = series
        .into_iter()
        .map(|mut sr| {
            let parts: Vec<String> = src_labels
                .iter()
                .map(|&src| {
                    sr.labels
                        .iter()
                        .find(|(k, _)| k == src)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default()
                })
                .collect();

            let joined = parts.join(separator);

            if joined.is_empty() {
                sr.labels.retain(|(k, _)| k != dst_label);
            } else if let Some(existing) = sr.labels.iter_mut().find(|(k, _)| k == dst_label) {
                existing.1 = joined;
            } else {
                sr.labels.push((dst_label.to_string(), joined));
                sr.labels.sort_by(|a, b| a.0.cmp(&b.0));
            }
            sr
        })
        .collect();

    Ok(PromQLResult::InstantVector(results))
}
