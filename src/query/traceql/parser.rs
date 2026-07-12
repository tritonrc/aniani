//! TraceQL parser using nom combinators.
//!
//! Supports span selectors with attribute matching, duration filters,
//! logical operators, and structural operators.

mod pipeline;

#[cfg(test)]
mod tests;

use nom::{
    IResult, Parser,
    branch::alt,
    bytes::complete::{tag, take_while1},
    character::complete::{char, multispace0},
    combinator::map,
};
use std::time::Duration;
use thiserror::Error;

/// TraceQL parse errors.
#[derive(Debug, Error)]
pub enum TraceQLParseError {
    #[error("parse error: {0}")]
    Parse(String),
}

/// Top-level TraceQL expression.
#[derive(Debug, Clone, PartialEq)]
pub enum TraceQLExpr {
    /// A span selector: `{ conditions }`.
    /// `logical_ops[i]` is the operator between `conditions[i]` and `conditions[i+1]`.
    SpanSelector {
        conditions: Vec<SpanCondition>,
        logical_ops: Vec<LogicalOp>,
    },
    /// Structural operator between two selectors.
    Structural {
        op: StructuralOp,
        lhs: Box<TraceQLExpr>,
        rhs: Box<TraceQLExpr>,
    },
    /// Pipeline with aggregate filter: `{...} | count() > N`.
    Pipeline {
        inner: Box<TraceQLExpr>,
        pipeline_stages: Vec<PipelineStage>,
    },
}

/// A pipeline stage applied after span matching.
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineStage {
    /// `count() op value` — filter traces by the number of matched spans.
    CountFilter { op: CompareOp, value: u64 },
    /// `avg(duration) op value` — filter traces by average span duration.
    AvgDuration { op: CompareOp, value_ns: i64 },
    /// `max(duration) op value` — filter traces by maximum span duration.
    MaxDuration { op: CompareOp, value_ns: i64 },
    /// `min(duration) op value` — filter traces by minimum span duration.
    MinDuration { op: CompareOp, value_ns: i64 },
}

/// Structural operators between span selectors.
#[derive(Debug, Clone, PartialEq)]
pub enum StructuralOp {
    /// `>>` descendant
    Descendant,
}

/// A condition within a span selector.
#[derive(Debug, Clone, PartialEq)]
pub enum SpanCondition {
    /// Attribute comparison: `scope.name op value`.
    Attribute {
        scope: AttrScope,
        name: String,
        op: CompareOp,
        value: SpanValue,
    },
    /// Duration comparison: `duration op value`.
    Duration { op: CompareOp, value: Duration },
    /// Status comparison: `status = error|ok|unset`.
    Status {
        op: CompareOp,
        value: SpanStatusValue,
    },
    /// Span kind comparison: `kind = server|client|...`.
    Kind { op: CompareOp, value: SpanKindValue },
    /// Span name comparison: `name op "value"`.
    Name { op: CompareOp, value: String },
    /// Event name comparison: `event.name op "value"` — matches if any event
    /// on the span has a name satisfying the operator.
    EventName { op: CompareOp, value: String },
    /// Event attribute comparison: `event.<key> op value` — matches if any
    /// event on the span carries an attribute with the given key satisfying
    /// the operator.
    EventAttribute {
        name: String,
        op: CompareOp,
        value: SpanValue,
    },
}

/// Attribute scope.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrScope {
    Resource,
    Span,
}

/// Comparison operators.
#[derive(Debug, Clone, PartialEq)]
pub enum CompareOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    Regex,
}

/// Values in span conditions.
#[derive(Debug, Clone, PartialEq)]
pub enum SpanValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Logical operators between conditions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogicalOp {
    And,
    Or,
}

/// Span status values.
#[derive(Debug, Clone, PartialEq)]
pub enum SpanStatusValue {
    Ok,
    Error,
    Unset,
}

/// Span kind values, mirroring OTLP `SpanKind`.
#[derive(Debug, Clone, PartialEq)]
pub enum SpanKindValue {
    Unspecified,
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

/// Parse a TraceQL expression.
pub fn parse_traceql(input: &str) -> Result<TraceQLExpr, TraceQLParseError> {
    let input = input.trim();
    match parse_top_level(input) {
        Ok((remaining, expr)) => {
            // Try to parse pipeline stages from remaining input
            let remaining = remaining.trim();
            let (remaining, stages) = pipeline::parse_pipeline_stages(remaining);
            let remaining = remaining.trim();
            if remaining.is_empty() {
                if stages.is_empty() {
                    Ok(expr)
                } else {
                    Ok(TraceQLExpr::Pipeline {
                        inner: Box::new(expr),
                        pipeline_stages: stages,
                    })
                }
            } else {
                Err(TraceQLParseError::Parse(format!(
                    "unexpected trailing input: {}",
                    remaining
                )))
            }
        }
        Err(e) => Err(TraceQLParseError::Parse(format!("{}", e))),
    }
}

fn parse_top_level(input: &str) -> IResult<&str, TraceQLExpr> {
    let (input, lhs) = parse_span_selector(input)?;
    let (input, _) = multispace0(input)?;

    // Check for structural operator
    if let Ok((input, _)) = tag::<&str, &str, nom::error::Error<&str>>(">>")(input) {
        let (input, _) = multispace0(input)?;
        let (input, rhs) = parse_span_selector(input)?;
        Ok((
            input,
            TraceQLExpr::Structural {
                op: StructuralOp::Descendant,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        ))
    } else {
        Ok((input, lhs))
    }
}

fn parse_span_selector(input: &str) -> IResult<&str, TraceQLExpr> {
    let (input, _) = multispace0(input)?;
    let (input, _) = char('{')(input)?;
    let (input, _) = multispace0(input)?;
    let (input, (conditions, logical_ops)) = parse_conditions(input)?;
    let (input, _) = multispace0(input)?;
    let (input, _) = char('}')(input)?;
    Ok((
        input,
        TraceQLExpr::SpanSelector {
            conditions,
            logical_ops,
        },
    ))
}

fn parse_conditions(input: &str) -> IResult<&str, (Vec<SpanCondition>, Vec<LogicalOp>)> {
    // Handle empty selector: `{}`
    let (mut input, first) = match parse_condition(input) {
        Ok((rest, cond)) => (rest, cond),
        Err(_) => return Ok((input, (Vec::new(), Vec::new()))),
    };
    let mut conditions = vec![first];
    let mut logical_ops = Vec::new();

    #[allow(clippy::while_let_loop)]
    loop {
        let trimmed = match multispace0::<&str, nom::error::Error<&str>>(input) {
            Ok((rest, _)) => rest,
            Err(_) => break,
        };
        if let Ok((rest, op_str)) =
            alt((tag::<_, _, nom::error::Error<&str>>("&&"), tag("||"))).parse_complete(trimmed)
        {
            let op = if op_str == "&&" {
                LogicalOp::And
            } else {
                LogicalOp::Or
            };
            let rest = match multispace0::<&str, nom::error::Error<&str>>(rest) {
                Ok((r, _)) => r,
                Err(_) => break,
            };
            match parse_condition(rest) {
                Ok((rest, cond)) => {
                    logical_ops.push(op);
                    conditions.push(cond);
                    input = rest;
                }
                Err(_) => {
                    // Trailing logical operator with no RHS condition is a parse error
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        rest,
                        nom::error::ErrorKind::Tag,
                    )));
                }
            }
        } else {
            break;
        }
    }

    Ok((input, (conditions, logical_ops)))
}

fn parse_condition(input: &str) -> IResult<&str, SpanCondition> {
    let (input, _) = multispace0(input)?;

    // Keyword-dispatched conditions parse definitively: when the keyword
    // matches, a malformed condition fails the whole parse rather than
    // silently falling through to attribute parsing (which would otherwise
    // misinterpret e.g. "status >= 5" and return misleading empty results).
    if peek_keyword(input, "duration") {
        return parse_duration_condition(input);
    }
    if peek_keyword(input, "status") {
        return parse_status_condition(input);
    }
    if peek_keyword(input, "kind") {
        return parse_kind_condition(input);
    }
    if peek_keyword(input, "name") {
        return parse_name_condition(input);
    }
    if input.starts_with("event.") {
        return parse_event_condition(input);
    }
    parse_attribute_condition(input)
}

/// True if `input` starts with `keyword` followed by a non-identifier boundary
/// (whitespace/operator/end), so "status" matches but "statuscode" does not.
fn peek_keyword(input: &str, keyword: &str) -> bool {
    if !input.starts_with(keyword) {
        return false;
    }
    match input[keyword.len()..].chars().next() {
        None => true,
        Some(c) => !(c.is_alphanumeric() || c == '_' || c == '.'),
    }
}

fn parse_duration_condition(input: &str) -> IResult<&str, SpanCondition> {
    let (input, _) = tag("duration")(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    let (input, dur) = parse_duration_value(input)?;
    Ok((input, SpanCondition::Duration { op, value: dur }))
}

fn parse_status_condition(input: &str) -> IResult<&str, SpanCondition> {
    let (input, _) = tag("status")(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    let (input, status) = alt((
        map(tag("error"), |_| SpanStatusValue::Error),
        map(tag("ok"), |_| SpanStatusValue::Ok),
        map(tag("unset"), |_| SpanStatusValue::Unset),
    ))
    .parse_complete(input)?;
    // Status is an unordered enum: only = and != are meaningful. Any other
    // operator is rejected so the query fails to parse instead of evaluating
    // to silently-empty results.
    if !matches!(op, CompareOp::Eq | CompareOp::Neq) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }
    Ok((input, SpanCondition::Status { op, value: status }))
}

fn parse_kind_condition(input: &str) -> IResult<&str, SpanCondition> {
    let (input, _) = tag("kind")(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    // Order matters: longer keywords first so "consumer" isn't truncated to "c".
    let (input, kind) = alt((
        map(tag("unspecified"), |_| SpanKindValue::Unspecified),
        map(tag("internal"), |_| SpanKindValue::Internal),
        map(tag("server"), |_| SpanKindValue::Server),
        map(tag("client"), |_| SpanKindValue::Client),
        map(tag("producer"), |_| SpanKindValue::Producer),
        map(tag("consumer"), |_| SpanKindValue::Consumer),
    ))
    .parse_complete(input)?;
    if !matches!(op, CompareOp::Eq | CompareOp::Neq) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }
    Ok((input, SpanCondition::Kind { op, value: kind }))
}

fn parse_name_condition(input: &str) -> IResult<&str, SpanCondition> {
    let (input, _) = tag("name")(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    let (input, value) = parse_quoted_string(input)?;
    Ok((input, SpanCondition::Name { op, value }))
}

/// Parse an event-scoped condition: `event.name op "str"` or
/// `event.<key> op value`. The `event.` prefix is consumed here; the
/// remainder determines whether we match the event's name field or one of
/// its attributes.
fn parse_event_condition(input: &str) -> IResult<&str, SpanCondition> {
    let (input, _) = tag("event.")(input)?;
    let (input, key) =
        take_while1(|c: char| c.is_alphanumeric() || c == '.' || c == '_' || c == '-')(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    if key == "name" {
        let (input, value) = parse_quoted_string(input)?;
        Ok((input, SpanCondition::EventName { op, value }))
    } else {
        let (input, value) = parse_span_value(input)?;
        Ok((
            input,
            SpanCondition::EventAttribute {
                name: key.to_string(),
                op,
                value,
            },
        ))
    }
}

fn parse_attribute_condition(input: &str) -> IResult<&str, SpanCondition> {
    let (input, scope_and_name) =
        take_while1(|c: char| c.is_alphanumeric() || c == '.' || c == '_' || c == '-')(input)?;

    let (scope, attr_name) = if let Some(rest) = scope_and_name.strip_prefix("resource.") {
        (AttrScope::Resource, rest.to_string())
    } else if let Some(rest) = scope_and_name.strip_prefix("span.") {
        (AttrScope::Span, rest.to_string())
    } else {
        // Default to resource scope for dotted names, span otherwise
        if scope_and_name.contains('.') {
            (AttrScope::Resource, scope_and_name.to_string())
        } else {
            (AttrScope::Span, scope_and_name.to_string())
        }
    };

    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    let (input, value) = parse_span_value(input)?;

    Ok((
        input,
        SpanCondition::Attribute {
            scope,
            name: attr_name,
            op,
            value,
        },
    ))
}

pub(super) fn parse_compare_op(input: &str) -> IResult<&str, CompareOp> {
    alt((
        map(tag(">="), |_| CompareOp::Gte),
        map(tag("<="), |_| CompareOp::Lte),
        map(tag("!="), |_| CompareOp::Neq),
        map(tag("=~"), |_| CompareOp::Regex),
        map(tag("="), |_| CompareOp::Eq),
        map(tag(">"), |_| CompareOp::Gt),
        map(tag("<"), |_| CompareOp::Lt),
    ))
    .parse_complete(input)
}

fn parse_span_value(input: &str) -> IResult<&str, SpanValue> {
    alt((
        map(parse_quoted_string, SpanValue::String),
        parse_bool_value,
        parse_numeric_value,
    ))
    .parse_complete(input)
}

/// Parse a bare `true` / `false` literal, rejecting identifiers that merely
/// start with those words (e.g. `trueish`).
fn parse_bool_value(input: &str) -> IResult<&str, SpanValue> {
    let (rest, word) = alt((
        tag::<&str, &str, nom::error::Error<&str>>("true"),
        tag("false"),
    ))
    .parse_complete(input)?;
    match rest.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' || c == '.' => Err(nom::Err::Error(
            nom::error::Error::new(input, nom::error::ErrorKind::Tag),
        )),
        _ => Ok((rest, SpanValue::Bool(word == "true"))),
    }
}

fn parse_numeric_value(input: &str) -> IResult<&str, SpanValue> {
    let (input, num_str) =
        take_while1(|c: char| c.is_ascii_digit() || c == '.' || c == '-')(input)?;

    if num_str.contains('.') {
        let val: f64 = num_str.parse().map_err(|_| {
            nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Float))
        })?;
        Ok((input, SpanValue::Float(val)))
    } else {
        let val: i64 = num_str.parse().map_err(|_| {
            nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
        })?;
        Ok((input, SpanValue::Int(val)))
    }
}

fn parse_quoted_string(input: &str) -> IResult<&str, String> {
    let (input, _) = char('"')(input)?;
    let mut result = String::new();
    let mut chars = input.char_indices();
    loop {
        match chars.next() {
            Some((i, '"')) => return Ok((&input[i + 1..], result)),
            Some((_, '\\')) => {
                if let Some((_, c)) = chars.next() {
                    match c {
                        'n' => result.push('\n'),
                        't' => result.push('\t'),
                        '\\' => result.push('\\'),
                        '"' => result.push('"'),
                        other => {
                            result.push('\\');
                            result.push(other);
                        }
                    }
                }
            }
            Some((_, c)) => result.push(c),
            None => {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Char,
                )));
            }
        }
    }
}

pub(super) fn parse_duration_value(input: &str) -> IResult<&str, Duration> {
    let (input, num_str) = take_while1(|c: char| c.is_ascii_digit() || c == '.')(input)?;
    let (input, unit) = alt((tag("ms"), tag("s"), tag("m"), tag("h"))).parse_complete(input)?;

    let num: f64 = num_str.parse().map_err(|_| {
        nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Float))
    })?;

    let secs = match unit {
        "ms" => num / 1000.0,
        "s" => num,
        "m" => num * 60.0,
        "h" => num * 3600.0,
        _ => unreachable!(),
    };
    if !secs.is_finite() || secs < 0.0 || secs > Duration::MAX.as_secs_f64() {
        return Err(nom::Err::Failure(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Float,
        )));
    }

    let duration = Duration::from_secs_f64(secs);

    Ok((input, duration))
}
