//! Vector and matrix selector evaluation for PromQL.

use promql_parser::label::{MatchOp as PromMatchOp, Matchers};
use promql_parser::parser::{AtModifier, Expr, MatrixSelector, Offset, VectorSelector};

use crate::store::metric_store::{MetricStore, Sample};
use crate::store::{LabelMatchOp, LabelMatcher};

use super::{PromQLError, PromQLResult, SeriesResult};

/// Convert an optional `Offset` to a signed millisecond value.
/// `Pos` offsets shift the lookup window into the past (positive ms to subtract),
/// `Neg` offsets shift forward (negative ms to subtract).
fn offset_to_ms(offset: &Option<Offset>) -> i64 {
    match offset {
        Some(Offset::Pos(dur)) => duration_to_i64_ms(dur),
        Some(Offset::Neg(dur)) => duration_to_i64_ms(dur).saturating_neg(),
        None => 0,
    }
}

fn duration_to_i64_ms(duration: &std::time::Duration) -> i64 {
    let ms = duration.as_millis();
    if ms > i64::MAX as u128 {
        i64::MAX
    } else {
        ms as i64
    }
}

fn advance_time(current: i64, step: i64) -> Option<i64> {
    if step <= 0 {
        return None;
    }
    current.checked_add(step)
}

/// Convert an optional `AtModifier` to a fixed evaluation timestamp in milliseconds.
/// `Start` and `End` resolve to the query's start/end respectively.
/// `At(SystemTime)` resolves to an absolute timestamp.
fn at_to_ms(at: &Option<AtModifier>, start_ms: i64, end_ms: i64) -> Option<i64> {
    match at {
        Some(AtModifier::Start) => Some(start_ms),
        Some(AtModifier::End) => Some(end_ms),
        Some(AtModifier::At(time)) => {
            let duration = time
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            Some(duration_to_i64_ms(&duration))
        }
        None => None,
    }
}

pub(super) fn eval_vector_selector(
    vs: &VectorSelector,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    let mut matchers = convert_matchers(&vs.matchers);
    // If the selector has a name (e.g. `http_requests_total`), add __name__ matcher.
    if let Some(name) = &vs.name
        && !matchers.iter().any(|m| m.name == "__name__")
    {
        matchers.push(LabelMatcher {
            name: "__name__".to_string(),
            op: LabelMatchOp::Eq,
            value: name.clone(),
        });
    }
    let series_ids = store.select_series(&matchers);
    let offset_ms = offset_to_ms(&vs.offset);
    let at_ms = at_to_ms(&vs.at, start_ms, end_ms);

    let mut results = Vec::new();

    if instant || step_ms == 0 {
        let lookback_ms = 5 * 60 * 1000;
        let forward_buffer_ms = 1000;
        let eval_time = at_ms.unwrap_or(end_ms);
        let effective_end = eval_time.saturating_sub(offset_ms);
        for sid in &series_ids {
            let samples = store.get_samples(
                *sid,
                effective_end.saturating_sub(lookback_ms),
                effective_end.saturating_add(forward_buffer_ms),
            );
            if let Some(last) = samples.last() {
                let labels = store.get_series_labels(*sid).unwrap_or_default();
                results.push(SeriesResult {
                    labels,
                    samples: vec![(end_ms, last.value)],
                });
            }
        }
        Ok(PromQLResult::InstantVector(results))
    } else {
        let lookback_ms = 5 * 60 * 1000;
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let mut series_samples = Vec::new();
            let mut t = start_ms;
            while t <= end_ms {
                let eval_time = at_ms.unwrap_or(t);
                let effective_t = eval_time.saturating_sub(offset_ms);
                let samples =
                    store.get_samples(*sid, effective_t.saturating_sub(lookback_ms), effective_t);
                if let Some(last) = samples.last() {
                    series_samples.push((t, last.value));
                }
                let Some(next_t) = advance_time(t, step_ms) else {
                    break;
                };
                t = next_t;
            }
            if !series_samples.is_empty() {
                results.push(SeriesResult {
                    labels,
                    samples: series_samples,
                });
            }
        }
        Ok(PromQLResult::InstantVector(results))
    }
}

pub(super) fn eval_matrix_selector(
    ms: &MatrixSelector,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
) -> Result<PromQLResult, PromQLError> {
    let vs = &ms.vs;
    let mut matchers = convert_matchers(&vs.matchers);
    if let Some(name) = &vs.name
        && !matchers.iter().any(|m| m.name == "__name__")
    {
        matchers.push(LabelMatcher {
            name: "__name__".to_string(),
            op: LabelMatchOp::Eq,
            value: name.clone(),
        });
    }
    let series_ids = store.select_series(&matchers);
    let range_ms = duration_to_i64_ms(&ms.range);
    let offset_ms = offset_to_ms(&vs.offset);
    let eval_time = at_to_ms(&vs.at, start_ms, end_ms).unwrap_or(end_ms);
    let effective_end = eval_time.saturating_sub(offset_ms);

    let mut results = Vec::new();
    for sid in &series_ids {
        let labels = store.get_series_labels(*sid).unwrap_or_default();
        let samples =
            store.get_samples(*sid, effective_end.saturating_sub(range_ms), effective_end);
        let sample_tuples: Vec<(i64, f64)> =
            samples.iter().map(|s| (s.timestamp_ms, s.value)).collect();
        if !sample_tuples.is_empty() {
            results.push(SeriesResult {
                labels,
                samples: sample_tuples,
            });
        }
    }
    Ok(PromQLResult::RangeVector(results))
}

pub(super) fn eval_rate_like(
    func_name: &str,
    arg: &Expr,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    let (vs, range_ms) = match arg {
        Expr::MatrixSelector(ms) => (&ms.vs, duration_to_i64_ms(&ms.range)),
        _ => {
            return Err(PromQLError::Eval(
                "rate/increase requires matrix selector".into(),
            ));
        }
    };

    let mut matchers = convert_matchers(&vs.matchers);
    if let Some(name) = &vs.name
        && !matchers.iter().any(|m| m.name == "__name__")
    {
        matchers.push(LabelMatcher {
            name: "__name__".to_string(),
            op: LabelMatchOp::Eq,
            value: name.clone(),
        });
    }
    let series_ids = store.select_series(&matchers);
    let offset_ms = offset_to_ms(&vs.offset);

    let mut results = Vec::new();

    if instant || step_ms == 0 {
        let effective_end = end_ms.saturating_sub(offset_ms);
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let samples =
                store.get_samples(*sid, effective_end.saturating_sub(range_ms), effective_end);
            if let Some(v) = compute_rate_like(func_name, samples, range_ms) {
                results.push(SeriesResult {
                    labels,
                    samples: vec![(end_ms, v)],
                });
            }
        }
        Ok(PromQLResult::InstantVector(results))
    } else {
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let mut series_samples = Vec::new();
            let mut t = start_ms;
            while t <= end_ms {
                let effective_t = t.saturating_sub(offset_ms);
                let samples =
                    store.get_samples(*sid, effective_t.saturating_sub(range_ms), effective_t);
                if let Some(v) = compute_rate_like(func_name, samples, range_ms) {
                    series_samples.push((t, v));
                }
                let Some(next_t) = advance_time(t, step_ms) else {
                    break;
                };
                t = next_t;
            }
            if !series_samples.is_empty() {
                results.push(SeriesResult {
                    labels,
                    samples: series_samples,
                });
            }
        }
        Ok(PromQLResult::InstantVector(results))
    }
}

pub(super) fn compute_rate_like(func_name: &str, samples: &[Sample], range_ms: i64) -> Option<f64> {
    if samples.len() < 2 {
        return None;
    }
    let first = samples.first()?;
    let last = samples.last()?;

    match func_name {
        "rate" | "increase" => {
            let mut total_increase = 0.0;
            for i in 1..samples.len() {
                let delta = samples[i].value - samples[i - 1].value;
                if delta >= 0.0 {
                    total_increase += delta;
                } else {
                    total_increase += samples[i].value;
                }
            }

            let sample_duration_s = (last.timestamp_ms - first.timestamp_ms) as f64 / 1000.0;
            if sample_duration_s <= 0.0 {
                return None;
            }

            if func_name == "increase" {
                Some(total_increase * (range_ms as f64 / 1000.0) / sample_duration_s)
            } else {
                Some(total_increase / sample_duration_s)
            }
        }
        "delta" => Some(last.value - first.value),
        "deriv" => {
            let n = samples.len() as f64;
            let x_mean: f64 = samples
                .iter()
                .map(|s| s.timestamp_ms as f64 / 1000.0)
                .sum::<f64>()
                / n;
            let y_mean: f64 = samples.iter().map(|s| s.value).sum::<f64>() / n;
            let mut num = 0.0;
            let mut den = 0.0;
            for s in samples {
                let dx = s.timestamp_ms as f64 / 1000.0 - x_mean;
                let dy = s.value - y_mean;
                num += dx * dy;
                den += dx * dx;
            }
            if den.abs() < f64::EPSILON {
                None
            } else {
                Some(num / den)
            }
        }
        "irate" => {
            if samples.len() >= 2 {
                let prev = &samples[samples.len() - 2];
                let curr = &samples[samples.len() - 1];
                let dt = (curr.timestamp_ms - prev.timestamp_ms) as f64 / 1000.0;
                if dt > 0.0 {
                    let delta = curr.value - prev.value;
                    let increase = if delta >= 0.0 { delta } else { curr.value };
                    Some(increase / dt)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn convert_matchers(matchers: &Matchers) -> Vec<LabelMatcher> {
    matchers
        .matchers
        .iter()
        .map(|m| LabelMatcher {
            name: m.name.clone(),
            op: match m.op {
                PromMatchOp::Equal => LabelMatchOp::Eq,
                PromMatchOp::NotEqual => LabelMatchOp::Neq,
                PromMatchOp::Re(_) => LabelMatchOp::Regex,
                PromMatchOp::NotRe(_) => LabelMatchOp::NotRegex,
            },
            value: m.value.clone(),
        })
        .collect()
}

/// Extract `(VectorSelector, range_ms)` from a matrix-selector `Expr`.
fn extract_matrix<'a>(
    arg: &'a Expr,
    func_name: &str,
) -> Result<(&'a VectorSelector, i64), PromQLError> {
    let Expr::MatrixSelector(ms) = arg else {
        return Err(PromQLError::Eval(format!(
            "{func_name} requires a range vector argument",
        )));
    };
    Ok((&ms.vs, duration_to_i64_ms(&ms.range)))
}

/// Build matchers from a VectorSelector, adding `__name__` if not present.
fn vs_matchers(vs: &VectorSelector) -> Vec<LabelMatcher> {
    let mut matchers = convert_matchers(&vs.matchers);
    if let Some(name) = &vs.name
        && !matchers.iter().any(|m| m.name == "__name__")
    {
        matchers.push(LabelMatcher {
            name: "__name__".to_string(),
            op: LabelMatchOp::Eq,
            value: name.clone(),
        });
    }
    matchers
}

/// Evaluate an `_over_time` aggregation function (avg/min/max/sum/count/last/present/stddev/stdvar).
pub(super) fn eval_over_time(
    func_name: &str,
    arg: &Expr,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    let (vs, range_ms) = extract_matrix(arg, func_name)?;
    let matchers = vs_matchers(vs);
    let series_ids = store.select_series(&matchers);
    let offset_ms = offset_to_ms(&vs.offset);

    let mut results = Vec::new();

    if instant || step_ms == 0 {
        let effective_end = end_ms.saturating_sub(offset_ms);
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let samples =
                store.get_samples(*sid, effective_end.saturating_sub(range_ms), effective_end);
            if let Some(v) = compute_over_time(func_name, samples) {
                results.push(SeriesResult {
                    labels,
                    samples: vec![(end_ms, v)],
                });
            }
        }
    } else {
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let mut series_samples = Vec::new();
            let mut t = start_ms;
            while t <= end_ms {
                let effective_t = t.saturating_sub(offset_ms);
                let samples =
                    store.get_samples(*sid, effective_t.saturating_sub(range_ms), effective_t);
                if let Some(v) = compute_over_time(func_name, samples) {
                    series_samples.push((t, v));
                }
                let Some(next_t) = advance_time(t, step_ms) else {
                    break;
                };
                t = next_t;
            }
            if !series_samples.is_empty() {
                results.push(SeriesResult {
                    labels,
                    samples: series_samples,
                });
            }
        }
    }

    Ok(PromQLResult::InstantVector(results))
}

/// Evaluate `quantile_over_time(φ, range)`.
pub(super) fn eval_quantile_over_time(
    quantile: f64,
    arg: &Expr,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    let (vs, range_ms) = extract_matrix(arg, "quantile_over_time")?;
    let matchers = vs_matchers(vs);
    let series_ids = store.select_series(&matchers);
    let offset_ms = offset_to_ms(&vs.offset);

    let mut results = Vec::new();

    if instant || step_ms == 0 {
        let effective_end = end_ms.saturating_sub(offset_ms);
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let samples =
                store.get_samples(*sid, effective_end.saturating_sub(range_ms), effective_end);
            if let Some(v) = quantile_over_time(quantile, samples) {
                results.push(SeriesResult {
                    labels,
                    samples: vec![(end_ms, v)],
                });
            }
        }
    } else {
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let mut series_samples = Vec::new();
            let mut t = start_ms;
            while t <= end_ms {
                let effective_t = t.saturating_sub(offset_ms);
                let samples =
                    store.get_samples(*sid, effective_t.saturating_sub(range_ms), effective_t);
                if let Some(v) = quantile_over_time(quantile, samples) {
                    series_samples.push((t, v));
                }
                let Some(next_t) = advance_time(t, step_ms) else {
                    break;
                };
                t = next_t;
            }
            if !series_samples.is_empty() {
                results.push(SeriesResult {
                    labels,
                    samples: series_samples,
                });
            }
        }
    }

    Ok(PromQLResult::InstantVector(results))
}

/// Evaluate `absent_over_time(range)`: returns 1 if the range is empty, else nothing.
pub(super) fn eval_absent_over_time(
    arg: &Expr,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    let (vs, range_ms) = extract_matrix(arg, "absent_over_time")?;
    let matchers = vs_matchers(vs);
    let series_ids = store.select_series(&matchers);
    let offset_ms = offset_to_ms(&vs.offset);

    let mut results = Vec::new();

    if instant || step_ms == 0 {
        let effective_end = end_ms.saturating_sub(offset_ms);
        let mut any_present = false;
        for sid in &series_ids {
            let samples =
                store.get_samples(*sid, effective_end.saturating_sub(range_ms), effective_end);
            if !samples.is_empty() {
                any_present = true;
                break;
            }
        }
        if !any_present {
            results.push(SeriesResult {
                labels: Vec::new(),
                samples: vec![(end_ms, 1.0)],
            });
        }
    } else {
        let mut series_samples = Vec::new();
        let mut t = start_ms;
        while t <= end_ms {
            let effective_t = t.saturating_sub(offset_ms);
            let mut any_present = false;
            for sid in &series_ids {
                let samples =
                    store.get_samples(*sid, effective_t.saturating_sub(range_ms), effective_t);
                if !samples.is_empty() {
                    any_present = true;
                    break;
                }
            }
            if !any_present {
                series_samples.push((t, 1.0));
            }
            let Some(next_t) = advance_time(t, step_ms) else {
                break;
            };
            t = next_t;
        }
        if !series_samples.is_empty() {
            results.push(SeriesResult {
                labels: Vec::new(),
                samples: series_samples,
            });
        }
    }

    Ok(PromQLResult::InstantVector(results))
}

/// Compute an `_over_time` aggregation over a window of samples.
fn compute_over_time(func_name: &str, samples: &[Sample]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
    Some(match func_name {
        "avg_over_time" => values.iter().sum::<f64>() / values.len() as f64,
        "min_over_time" => values.iter().copied().fold(f64::INFINITY, f64::min),
        "max_over_time" => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "sum_over_time" => values.iter().sum(),
        "count_over_time" => values.len() as f64,
        "last_over_time" => *values.last().unwrap(),
        "present_over_time" => 1.0,
        "stddev_over_time" => {
            let n = values.len() as f64;
            let mean = values.iter().sum::<f64>() / n;
            let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
            variance.sqrt()
        }
        "stdvar_over_time" => {
            let n = values.len() as f64;
            let mean = values.iter().sum::<f64>() / n;
            values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n
        }
        _ => return None,
    })
}

/// Compute `quantile_over_time` using linear interpolation.
fn quantile_over_time(quantile: f64, samples: &[Sample]) -> Option<f64> {
    if samples.is_empty() || !(0.0..=1.0).contains(&quantile) {
        return None;
    }
    let mut values: Vec<f64> = samples.iter().map(|s| s.value).collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(super::functions::interpolate_quantile(&values, quantile))
}

/// Evaluate `changes()` or `resets()` over a range vector.
///
/// `changes` counts the number of times the value changes between adjacent samples.
/// `resets` counts counter resets (value decreases) between adjacent samples.
pub(super) fn eval_changes_or_resets(
    func_name: &str,
    arg: &Expr,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    let (vs, range_ms) = extract_matrix(arg, func_name)?;
    let matchers = vs_matchers(vs);
    let series_ids = store.select_series(&matchers);
    let offset_ms = offset_to_ms(&vs.offset);

    let mut results = Vec::new();

    if instant || step_ms == 0 {
        let effective_end = end_ms.saturating_sub(offset_ms);
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let samples =
                store.get_samples(*sid, effective_end.saturating_sub(range_ms), effective_end);
            if !samples.is_empty() {
                let count = count_changes_or_resets(func_name, samples);
                results.push(SeriesResult {
                    labels,
                    samples: vec![(end_ms, count)],
                });
            }
        }
    } else {
        for sid in &series_ids {
            let labels = store.get_series_labels(*sid).unwrap_or_default();
            let mut series_samples = Vec::new();
            let mut t = start_ms;
            while t <= end_ms {
                let effective_t = t.saturating_sub(offset_ms);
                let samples =
                    store.get_samples(*sid, effective_t.saturating_sub(range_ms), effective_t);
                if !samples.is_empty() {
                    let count = count_changes_or_resets(func_name, samples);
                    series_samples.push((t, count));
                }
                let Some(next_t) = advance_time(t, step_ms) else {
                    break;
                };
                t = next_t;
            }
            if !series_samples.is_empty() {
                results.push(SeriesResult {
                    labels,
                    samples: series_samples,
                });
            }
        }
    }

    Ok(PromQLResult::InstantVector(results))
}

fn count_changes_or_resets(func_name: &str, samples: &[Sample]) -> f64 {
    let mut count = 0.0;
    for i in 1..samples.len() {
        match func_name {
            "changes" => {
                if samples[i].value != samples[i - 1].value {
                    count += 1.0;
                }
            }
            "resets" if samples[i].value < samples[i - 1].value => {
                count += 1.0;
            }
            _ => {}
        }
    }
    count
}
