//! Pipeline-stage parsing for TraceQL: `| count() > N`, `| avg(duration) > 1s`, etc.

use std::time::Duration;

use nom::{
    IResult, Parser,
    branch::alt,
    bytes::complete::{tag, take_while1},
    character::complete::multispace0,
};

use super::{PipelineStage, parse_compare_op, parse_duration_value};

/// Parse zero or more pipeline stages from the remaining input after the base expression.
/// Each stage is `| count() op value` or `| avg/max/min(duration) op duration_value`.
pub(super) fn parse_pipeline_stages(mut input: &str) -> (&str, Vec<PipelineStage>) {
    let mut stages = Vec::new();
    loop {
        let trimmed = input.trim_start();
        if !trimmed.starts_with('|') {
            break;
        }
        let rest = trimmed[1..].trim_start();
        if let Ok((remaining, stage)) = parse_count_filter(rest) {
            stages.push(stage);
            input = remaining;
        } else if let Ok((remaining, stage)) = parse_duration_agg_filter(rest) {
            stages.push(stage);
            input = remaining;
        } else {
            break;
        }
    }
    (input, stages)
}

/// Parse `count() op value` where op is a comparison and value is a u64.
fn parse_count_filter(input: &str) -> IResult<&str, PipelineStage> {
    let (input, _) = tag("count()")(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    let (input, num_str) = take_while1(|c: char| c.is_ascii_digit())(input)?;
    let value: u64 = num_str.parse().map_err(|_| {
        nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Digit))
    })?;
    Ok((input, PipelineStage::CountFilter { op, value }))
}

/// Parse `avg(duration) op duration`, `max(duration) op duration`, or `min(duration) op duration`.
fn parse_duration_agg_filter(input: &str) -> IResult<&str, PipelineStage> {
    let (input, agg_fn) = alt((tag("avg"), tag("max"), tag("min"))).parse_complete(input)?;
    let (input, _) = tag("(duration)")(input)?;
    let (input, _) = multispace0(input)?;
    let (input, op) = parse_compare_op(input)?;
    let (input, _) = multispace0(input)?;
    let (input, dur) = parse_duration_value(input)?;
    let value_ns = duration_to_i64_ns(dur).ok_or_else(|| {
        nom::Err::Failure(nom::error::Error::new(
            input,
            nom::error::ErrorKind::TooLarge,
        ))
    })?;
    let stage = match agg_fn {
        "avg" => PipelineStage::AvgDuration { op, value_ns },
        "max" => PipelineStage::MaxDuration { op, value_ns },
        "min" => PipelineStage::MinDuration { op, value_ns },
        _ => unreachable!(),
    };
    Ok((input, stage))
}

fn duration_to_i64_ns(duration: Duration) -> Option<i64> {
    let ns = duration.as_nanos();
    if ns > i64::MAX as u128 {
        None
    } else {
        Some(ns as i64)
    }
}
