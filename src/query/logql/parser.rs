//! LogQL parser using nom combinators.
//!
//! Supports stream selectors, pipeline stages, and metric queries.

use nom::{
    IResult, Parser,
    branch::alt,
    bytes::tag,
    character::{char, multispace0},
    combinator::{cut, map},
    multi::separated_list0,
};
use regex::Regex;
use std::time::Duration;
use thiserror::Error;

use crate::config::parse_duration;

/// LogQL parse errors.
#[derive(Debug, Error)]
pub enum LogQLParseError {
    #[error("parse error at position {pos}: {msg}")]
    Parse { pos: usize, msg: String },
}

/// Maps a nom failure on `trimmed` into a `LogQLParseError` with a human message,
/// offsetting the reported position by `leading_ws` so it points into the
/// original (untrimmed) query string the caller typed.
fn nom_err_to_human(
    trimmed: &str,
    err: nom::Err<nom::error::Error<&str>>,
    leading_ws: usize,
) -> LogQLParseError {
    let remaining = match &err {
        nom::Err::Error(e) | nom::Err::Failure(e) => e.input,
        nom::Err::Incomplete(_) => "",
    };
    let pos_in_trimmed = trimmed.len().saturating_sub(remaining.len());
    let msg = classify_parse_failure(trimmed, pos_in_trimmed);
    LogQLParseError::Parse {
        pos: leading_ws + pos_in_trimmed,
        msg,
    }
}

/// Turns a failure position into a plain-language hint by looking at what
/// immediately precedes it. Coarse-grained on purpose: covers the handful of
/// common mistakes (unquoted value, missing label name, unclosed selector)
/// and falls back to a generic message for everything else.
fn classify_parse_failure(input: &str, pos: usize) -> String {
    let pos = pos.min(input.len());
    let before = input[..pos].trim_end();

    if before.ends_with('=') {
        return "expected a quoted value after '='".to_string();
    }
    if before.ends_with('{') || before.ends_with(',') {
        return "expected a label name".to_string();
    }
    if pos >= input.len() {
        if input.contains('{') && !input.contains('}') {
            return "expected '}' to close the selector".to_string();
        }
        return "unexpected end of query".to_string();
    }
    "unexpected input here".to_string()
}

/// Top-level LogQL expression.
#[derive(Debug, Clone)]
pub enum LogQLExpr {
    /// Stream selector: `{ label_matchers }`.
    StreamSelector { matchers: Vec<LogQLMatcher> },
    /// Pipeline: selector followed by filter stages.
    Pipeline {
        selector: Box<LogQLExpr>,
        stages: Vec<PipelineStage>,
    },
    /// Metric query: `func(selector [range])`.
    MetricQuery {
        function: MetricFunc,
        inner: Box<LogQLExpr>,
        range: Duration,
    },
}

/// A label matcher in a stream selector.
#[derive(Debug, Clone, PartialEq)]
pub struct LogQLMatcher {
    pub name: String,
    pub op: MatchOp,
    pub value: String,
}

/// Match operator for labels.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchOp {
    Eq,       // =
    Neq,      // !=
    Regex,    // =~
    NotRegex, // !~
}

/// Pipeline filter stage.
#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum PipelineStage {
    LineContains(String),        // |= "text"
    LineNotContains(String),     // != "text"
    LineRegex(String, Regex),    // |~ "regex"
    LineNotRegex(String, Regex), // !~ "regex"
    JsonExtract,                 // | json
    LogfmtExtract,               // | logfmt
    LabelFilter {
        // | key="value"
        key: String,
        op: MatchOp,
        value: String,
        /// Pre-compiled regex for Regex/NotRegex match ops, None for Eq/Neq.
        compiled_regex: Option<Regex>,
    },
}

/// Metric functions over log streams.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricFunc {
    CountOverTime,
    Rate,
    BytesOverTime,
    SumOverTime,
    AvgOverTime,
    MinOverTime,
    MaxOverTime,
}

/// Parse a LogQL expression.
pub fn parse_logql(input: &str) -> Result<LogQLExpr, LogQLParseError> {
    // Position offset from any trimmed leading whitespace, so reported
    // positions point into the original (untrimmed) query the caller typed.
    let leading_ws = input.len() - input.trim_start().len();
    let trimmed = input.trim();

    // Try metric query first
    if let Ok((remaining, expr)) = parse_metric_query(trimmed) {
        let remaining_trimmed = remaining.trim_start();
        if remaining_trimmed.is_empty() {
            return Ok(expr);
        }
        let pos = leading_ws + (trimmed.len() - remaining_trimmed.len());
        return Err(LogQLParseError::Parse {
            pos,
            msg: "unexpected trailing input".to_string(),
        });
    }

    // Try pipeline or stream selector
    match parse_pipeline_or_selector(trimmed) {
        Ok((remaining, expr)) => {
            let remaining_trimmed = remaining.trim_start();
            if remaining_trimmed.is_empty() {
                Ok(expr)
            } else {
                let pos = leading_ws + (trimmed.len() - remaining_trimmed.len());
                Err(LogQLParseError::Parse {
                    pos,
                    msg: "unexpected trailing input".to_string(),
                })
            }
        }
        Err(e) => Err(nom_err_to_human(trimmed, e, leading_ws)),
    }
}

fn parse_metric_query(input: &str) -> IResult<&str, LogQLExpr> {
    let (input, func) = alt((
        map(tag("count_over_time"), |_| MetricFunc::CountOverTime),
        map(tag("bytes_over_time"), |_| MetricFunc::BytesOverTime),
        map(tag("sum_over_time"), |_| MetricFunc::SumOverTime),
        map(tag("avg_over_time"), |_| MetricFunc::AvgOverTime),
        map(tag("min_over_time"), |_| MetricFunc::MinOverTime),
        map(tag("max_over_time"), |_| MetricFunc::MaxOverTime),
        map(tag("rate"), |_| MetricFunc::Rate),
    ))
    .parse_complete(input)?;

    let (input, _) = char('(').parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, inner) = parse_pipeline_or_selector(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, range) = parse_range(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, _) = char(')').parse_complete(input)?;

    Ok((
        input,
        LogQLExpr::MetricQuery {
            function: func,
            inner: Box::new(inner),
            range,
        },
    ))
}

fn parse_range(input: &str) -> IResult<&str, Duration> {
    let (input, _) = char('[').parse_complete(input)?;
    let (input, dur_str) =
        nom::bytes::take_while1(|c: char| c.is_alphanumeric()).parse_complete(input)?;
    let (input, _) = char(']').parse_complete(input)?;

    let duration = parse_duration(dur_str).ok_or_else(|| {
        nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
    })?;

    Ok((input, duration))
}

fn parse_pipeline_or_selector(input: &str) -> IResult<&str, LogQLExpr> {
    let (input, selector) = parse_stream_selector(input)?;
    let (input, _) = multispace0().parse_complete(input)?;

    // Try to parse pipeline stages
    let mut stages = Vec::new();
    let mut remaining = input;
    loop {
        let trimmed = remaining.trim_start();
        if let Ok((rest, stage)) = parse_pipeline_stage(trimmed) {
            stages.push(stage);
            remaining = rest;
        } else {
            break;
        }
    }

    if stages.is_empty() {
        Ok((remaining, selector))
    } else {
        Ok((
            remaining,
            LogQLExpr::Pipeline {
                selector: Box::new(selector),
                stages,
            },
        ))
    }
}

fn parse_stream_selector(input: &str) -> IResult<&str, LogQLExpr> {
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, _) = char('{').parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, matchers) = separated_list0(char(','), parse_matcher).parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, _) = char('}').parse_complete(input)?;

    Ok((input, LogQLExpr::StreamSelector { matchers }))
}

fn parse_matcher(input: &str) -> IResult<&str, LogQLMatcher> {
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, name) =
        nom::bytes::take_while1(|c: char| c.is_alphanumeric() || c == '_' || c == '.')
            .parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, op) = alt((
        map(tag("=~"), |_| MatchOp::Regex),
        map(tag("!~"), |_| MatchOp::NotRegex),
        map(tag("!="), |_| MatchOp::Neq),
        map(tag("="), |_| MatchOp::Eq),
    ))
    .parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    // Once name+op have matched, a missing quoted value is a hard error, not a
    // reason for the caller (e.g. separated_list0) to silently backtrack.
    let (input, value) = cut(parse_quoted_string).parse_complete(input)?;

    Ok((
        input,
        LogQLMatcher {
            name: name.to_string(),
            op,
            value,
        },
    ))
}

fn parse_quoted_string(input: &str) -> IResult<&str, String> {
    let (input, _) = char('"').parse_complete(input)?;
    let mut result = String::new();
    let mut chars = input.char_indices();
    loop {
        match chars.next() {
            Some((i, '"')) => {
                return Ok((&input[i + 1..], result));
            }
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

fn parse_pipeline_stage(input: &str) -> IResult<&str, PipelineStage> {
    alt((
        parse_line_contains,
        parse_line_not_contains,
        parse_line_regex,
        parse_line_not_regex,
        parse_json_or_label_filter,
    ))
    .parse(input)
}

fn parse_json_or_label_filter(input: &str) -> IResult<&str, PipelineStage> {
    let (input, _) = char('|').parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;

    // Try "json" keyword
    if let Ok((rest, _)) = tag::<&str, &str, nom::error::Error<&str>>("json").parse_complete(input)
        && is_keyword_boundary(rest)
    {
        // Make sure "json" is not part of a longer identifier
        return Ok((rest, PipelineStage::JsonExtract));
    }

    // Try "logfmt" keyword
    if let Ok((rest, _)) =
        tag::<&str, &str, nom::error::Error<&str>>("logfmt").parse_complete(input)
        && is_keyword_boundary(rest)
    {
        return Ok((rest, PipelineStage::LogfmtExtract));
    }

    // Otherwise parse label filter: key op "value"
    let (input, key) =
        nom::bytes::take_while1(|c: char| c.is_alphanumeric() || c == '_').parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, op) = alt((
        map(tag("=~"), |_| MatchOp::Regex),
        map(tag("!~"), |_| MatchOp::NotRegex),
        map(tag("!="), |_| MatchOp::Neq),
        map(tag("="), |_| MatchOp::Eq),
    ))
    .parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, value) = cut(parse_quoted_string).parse_complete(input)?;

    let compiled_regex = match op {
        MatchOp::Regex | MatchOp::NotRegex => {
            let re = Regex::new(&value).map_err(|_| {
                nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify))
            })?;
            Some(re)
        }
        _ => None,
    };

    Ok((
        input,
        PipelineStage::LabelFilter {
            key: key.to_string(),
            op,
            value,
            compiled_regex,
        },
    ))
}

fn is_keyword_boundary(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_none_or(|next| !next.is_alphanumeric() && next != '_')
}

fn parse_line_contains(input: &str) -> IResult<&str, PipelineStage> {
    let (input, _) = tag("|=").parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, pattern) = parse_quoted_string(input)?;
    Ok((input, PipelineStage::LineContains(pattern)))
}

fn parse_line_not_contains(input: &str) -> IResult<&str, PipelineStage> {
    let (input, _) = tag("!=").parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, pattern) = parse_quoted_string(input)?;
    Ok((input, PipelineStage::LineNotContains(pattern)))
}

fn parse_line_regex(input: &str) -> IResult<&str, PipelineStage> {
    let (input, _) = tag("|~").parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, pattern) = parse_quoted_string(input)?;
    let re = Regex::new(&pattern).map_err(|_| {
        nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify))
    })?;
    Ok((input, PipelineStage::LineRegex(pattern, re)))
}

fn parse_line_not_regex(input: &str) -> IResult<&str, PipelineStage> {
    let (input, _) = tag("!~").parse_complete(input)?;
    let (input, _) = multispace0().parse_complete(input)?;
    let (input, pattern) = parse_quoted_string(input)?;
    let re = Regex::new(&pattern).map_err(|_| {
        nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify))
    })?;
    Ok((input, PipelineStage::LineNotRegex(pattern, re)))
}

#[cfg(test)]
mod tests;
