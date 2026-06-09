//! PromQL evaluator walking the `promql-parser` AST against MetricStore.

mod aggregation;
mod binary;
mod functions;
mod selectors;
mod types;

#[cfg(test)]
mod tests;

use promql_parser::parser::{self, Call, Expr, NumberLiteral, ParenExpr, UnaryExpr};

use crate::store::metric_store::MetricStore;

pub use types::{PromQLError, PromQLResult, SeriesResult};

/// Evaluate a PromQL query at a single instant.
pub fn evaluate_instant(
    query: &str,
    store: &MetricStore,
    time_ms: i64,
) -> Result<PromQLResult, PromQLError> {
    let ast = parser::parse(query).map_err(|e| PromQLError::Parse(e.to_string()))?;
    eval_expr(&ast, store, time_ms, time_ms, 0, true)
}

/// Evaluate a PromQL query over a range with step.
pub fn evaluate_range(
    query: &str,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
) -> Result<PromQLResult, PromQLError> {
    let ast = parser::parse(query).map_err(|e| PromQLError::Parse(e.to_string()))?;
    eval_expr(&ast, store, start_ms, end_ms, step_ms, false)
}

fn eval_expr(
    expr: &Expr,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    match expr {
        Expr::NumberLiteral(NumberLiteral { val, .. }) => Ok(PromQLResult::Scalar(*val)),
        Expr::VectorSelector(vs) => {
            selectors::eval_vector_selector(vs, store, start_ms, end_ms, step_ms, instant)
        }
        Expr::MatrixSelector(ms) => selectors::eval_matrix_selector(ms, store, start_ms, end_ms),
        Expr::Call(call) => eval_call(call, store, start_ms, end_ms, step_ms, instant),
        Expr::Aggregate(agg) => {
            let inner = eval_expr(&agg.expr, store, start_ms, end_ms, step_ms, instant)?;
            let param = agg
                .param
                .as_ref()
                .map(|param| eval_expr(param, store, start_ms, end_ms, step_ms, instant))
                .transpose()?;
            aggregation::eval_aggregation(agg, inner, param)
        }
        Expr::Binary(bin) => {
            binary::reject_unsupported_modifiers(bin)?;
            let lhs = eval_expr(&bin.lhs, store, start_ms, end_ms, step_ms, instant)?;
            let rhs = eval_expr(&bin.rhs, store, start_ms, end_ms, step_ms, instant)?;
            binary::eval_binary_result(&bin.op.to_string(), lhs, rhs)
        }
        Expr::Paren(ParenExpr { expr, .. }) => {
            eval_expr(expr, store, start_ms, end_ms, step_ms, instant)
        }
        Expr::Unary(UnaryExpr { expr, .. }) => {
            let result = eval_expr(expr, store, start_ms, end_ms, step_ms, instant)?;
            apply_unary_minus(result)
        }
        _ => Err(PromQLError::Unsupported(format!("{:?}", expr))),
    }
}

fn apply_unary_minus(result: PromQLResult) -> Result<PromQLResult, PromQLError> {
    match result {
        PromQLResult::Scalar(v) => Ok(PromQLResult::Scalar(-v)),
        PromQLResult::InstantVector(series) => Ok(PromQLResult::InstantVector(
            series
                .into_iter()
                .map(|mut s| {
                    for sample in &mut s.samples {
                        sample.1 = -sample.1;
                    }
                    s
                })
                .collect(),
        )),
        other => Ok(other),
    }
}

fn eval_call(
    call: &Call,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
) -> Result<PromQLResult, PromQLError> {
    let func_name = call.func.name;

    match func_name {
        "rate" | "increase" | "irate" | "delta" | "deriv" => {
            let Some(arg) = call.args.args.first() else {
                return Err(PromQLError::Eval(format!(
                    "{} requires a range vector argument",
                    func_name
                )));
            };
            selectors::eval_rate_like(func_name, arg, store, start_ms, end_ms, step_ms, instant)
        }
        "histogram_quantile" => {
            if call.args.args.len() < 2 {
                return Err(PromQLError::Eval(
                    "histogram_quantile requires 2 arguments".into(),
                ));
            }
            let quantile = match eval_expr(
                &call.args.args[0],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
            )? {
                PromQLResult::Scalar(v) => v,
                _ => {
                    return Err(PromQLError::Eval(
                        "first arg to histogram_quantile must be scalar".into(),
                    ));
                }
            };
            let buckets_result = eval_expr(
                &call.args.args[1],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
            )?;
            functions::eval_histogram_quantile(quantile, buckets_result)
        }
        "abs" | "ceil" | "floor" | "round" => {
            let inner = eval_one_arg(call, store, start_ms, end_ms, step_ms, instant, func_name)?;
            functions::apply_scalar_func(func_name, inner)
        }
        "label_replace" => {
            let inner = eval_one_arg(call, store, start_ms, end_ms, step_ms, instant, func_name)?;
            functions::eval_label_replace(call, inner)
        }
        "label_join" => {
            let inner = eval_one_arg(call, store, start_ms, end_ms, step_ms, instant, func_name)?;
            functions::eval_label_join(call, inner)
        }
        "absent" => {
            let inner = eval_one_arg(call, store, start_ms, end_ms, step_ms, instant, "absent")?;
            match inner {
                PromQLResult::InstantVector(series) | PromQLResult::RangeVector(series)
                    if series.is_empty() =>
                {
                    Ok(PromQLResult::InstantVector(vec![SeriesResult {
                        labels: Vec::new(),
                        samples: vec![(end_ms, 1.0)],
                    }]))
                }
                PromQLResult::InstantVector(_)
                | PromQLResult::RangeVector(_)
                | PromQLResult::Scalar(_) => Ok(PromQLResult::InstantVector(Vec::new())),
            }
        }
        "sort" | "sort_desc" => {
            let inner = eval_one_arg(call, store, start_ms, end_ms, step_ms, instant, func_name)?;
            Ok(sort_vector(inner, func_name == "sort_desc"))
        }
        "clamp" => {
            if call.args.args.len() < 3 {
                return Err(PromQLError::Eval(
                    "clamp requires 3 arguments: vector, min, max".into(),
                ));
            }
            let inner = eval_expr(
                &call.args.args[0],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
            )?;
            let min_val = eval_scalar_arg(
                &call.args.args[1],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
                "clamp min argument must be a scalar",
            )?;
            let max_val = eval_scalar_arg(
                &call.args.args[2],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
                "clamp max argument must be a scalar",
            )?;
            functions::apply_clamp(inner, Some(min_val), Some(max_val))
        }
        "clamp_min" => {
            if call.args.args.len() < 2 {
                return Err(PromQLError::Eval(
                    "clamp_min requires 2 arguments: vector, min".into(),
                ));
            }
            let inner = eval_expr(
                &call.args.args[0],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
            )?;
            let min_val = eval_scalar_arg(
                &call.args.args[1],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
                "clamp_min argument must be a scalar",
            )?;
            functions::apply_clamp(inner, Some(min_val), None)
        }
        "clamp_max" => {
            if call.args.args.len() < 2 {
                return Err(PromQLError::Eval(
                    "clamp_max requires 2 arguments: vector, max".into(),
                ));
            }
            let inner = eval_expr(
                &call.args.args[0],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
            )?;
            let max_val = eval_scalar_arg(
                &call.args.args[1],
                store,
                start_ms,
                end_ms,
                step_ms,
                instant,
                "clamp_max argument must be a scalar",
            )?;
            functions::apply_clamp(inner, None, Some(max_val))
        }
        "time" => {
            let time_s = end_ms as f64 / 1000.0;
            Ok(PromQLResult::InstantVector(vec![SeriesResult {
                labels: Vec::new(),
                samples: vec![(end_ms, time_s)],
            }]))
        }
        "vector" => {
            let inner = eval_one_arg(call, store, start_ms, end_ms, step_ms, instant, "vector")?;
            match inner {
                PromQLResult::Scalar(v) => Ok(PromQLResult::InstantVector(vec![SeriesResult {
                    labels: Vec::new(),
                    samples: vec![(end_ms, v)],
                }])),
                other => Ok(other),
            }
        }
        "scalar" => {
            let inner = eval_one_arg(call, store, start_ms, end_ms, step_ms, instant, "scalar")?;
            match inner {
                PromQLResult::InstantVector(series) if series.len() == 1 => {
                    let val = series[0]
                        .samples
                        .first()
                        .map(|(_, v)| *v)
                        .unwrap_or(f64::NAN);
                    Ok(PromQLResult::Scalar(val))
                }
                PromQLResult::InstantVector(_) => Ok(PromQLResult::Scalar(f64::NAN)),
                PromQLResult::Scalar(v) => Ok(PromQLResult::Scalar(v)),
                _ => Ok(PromQLResult::Scalar(f64::NAN)),
            }
        }
        _ => Err(PromQLError::Unsupported(format!("function: {}", func_name))),
    }
}

fn eval_one_arg(
    call: &Call,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
    func_name: &str,
) -> Result<PromQLResult, PromQLError> {
    let Some(arg) = call.args.args.first() else {
        return Err(PromQLError::Eval(format!(
            "{} requires an argument",
            func_name
        )));
    };
    eval_expr(arg, store, start_ms, end_ms, step_ms, instant)
}

fn eval_scalar_arg(
    expr: &Expr,
    store: &MetricStore,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    instant: bool,
    error: &str,
) -> Result<f64, PromQLError> {
    match eval_expr(expr, store, start_ms, end_ms, step_ms, instant)? {
        PromQLResult::Scalar(v) => Ok(v),
        _ => Err(PromQLError::Eval(error.into())),
    }
}

fn sort_vector(result: PromQLResult, descending: bool) -> PromQLResult {
    match result {
        PromQLResult::InstantVector(mut series) => {
            series.sort_by(|a, b| {
                let a_val = a.samples.last().map(|(_, v)| *v).unwrap_or(f64::NAN);
                let b_val = b.samples.last().map(|(_, v)| *v).unwrap_or(f64::NAN);
                if descending {
                    b_val
                        .partial_cmp(&a_val)
                        .unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    a_val
                        .partial_cmp(&b_val)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            });
            PromQLResult::InstantVector(series)
        }
        other => other,
    }
}
