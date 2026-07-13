//! Shared PromQL evaluator result and error types.

use rustc_hash::FxHashMap;
use thiserror::Error;

/// PromQL evaluation errors.
#[derive(Debug, Error)]
pub enum PromQLError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unsupported expression: {0}")]
    Unsupported(String),
    #[error("evaluation error: {0}")]
    Eval(String),
}

/// Result of a PromQL evaluation.
#[derive(Debug, Clone)]
pub enum PromQLResult {
    /// Instant vector: each series has a single `(timestamp_ms, value)` sample.
    InstantVector(Vec<SeriesResult>),
    /// Range vector: each series has multiple samples.
    RangeVector(Vec<SeriesResult>),
    /// Scalar value.
    Scalar(f64),
}

/// A single series in the result.
#[derive(Debug, Clone)]
pub struct SeriesResult {
    pub labels: Vec<(String, String)>,
    pub samples: Vec<(i64, f64)>,
}

pub(super) type SeriesLabelSet = Vec<(String, String)>;
pub(super) type SampleLookup = FxHashMap<i64, f64>;
