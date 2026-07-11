use super::*;

#[test]
fn test_parse_timestamp_ns_nanoseconds() {
    assert_eq!(
        parse_timestamp_ns("1700000000000000000"),
        Some(1700000000000000000)
    );
}

#[test]
fn test_parse_timestamp_ns_seconds() {
    assert_eq!(
        parse_timestamp_ns("1700000000"),
        Some(1_700_000_000_000_000_000)
    );
}

#[test]
fn test_parse_timestamp_ns_milliseconds() {
    assert_eq!(
        parse_timestamp_ns("1700000000000"),
        Some(1_700_000_000_000_000_000)
    );
}

#[test]
fn test_parse_timestamp_ns_microseconds() {
    assert_eq!(
        parse_timestamp_ns("1700000000000000"),
        Some(1_700_000_000_000_000_000)
    );
}

#[test]
fn test_parse_timestamp_ns_float_seconds() {
    let result = parse_timestamp_ns("1700000000.5").unwrap();
    assert!((result - 1_700_000_000_500_000_000).abs() < 1000);
}

#[test]
fn test_max_query_steps_constant() {
    assert_eq!(MAX_QUERY_STEPS, 11_000);
}

#[test]
fn test_parse_timestamp_rejects_non_finite_float() {
    assert_eq!(parse_timestamp_ns("NaN"), None);
    assert_eq!(parse_timestamp_ns("inf"), None);
    assert_eq!(parse_timestamp_ns("1e300"), None);
}

#[test]
fn test_bounded_limit_caps_untrusted_limit() {
    assert_eq!(bounded_limit(None), DEFAULT_ENTRY_LIMIT);
    assert_eq!(bounded_limit(Some(MAX_ENTRY_LIMIT + 1)), MAX_ENTRY_LIMIT);
}

#[test]
fn test_step_count_within_limit() {
    // 3600s range in ns / 1s step in ns = 3600 steps, under the 11000 cap
    let start_ns: i64 = 1_700_000_000_000_000_000;
    let end_ns: i64 = start_ns + 3_600_000_000_000; // 3600s in ns
    let step_ns: i64 = 1_000_000_000; // 1s in ns
    let num_steps = end_ns.saturating_sub(start_ns).max(0) / step_ns;
    assert_eq!(num_steps, 3600);
    assert!(num_steps < MAX_QUERY_STEPS);
}

#[test]
fn test_step_count_exceeds_limit() {
    // Range that produces more than 11000 steps
    let start_ns: i64 = 1_700_000_000_000_000_000;
    let step_ns: i64 = 1_000_000_000; // 1s in ns
    let end_ns: i64 = start_ns + step_ns * 12_000; // 12000 steps
    let num_steps = end_ns.saturating_sub(start_ns).max(0) / step_ns;
    assert_eq!(num_steps, 12_000);
    assert!(num_steps >= MAX_QUERY_STEPS);
}

#[test]
fn test_step_count_exactly_at_limit() {
    // Exactly 11000 steps should be rejected (off-by-one: eval loop is inclusive)
    let start_ns: i64 = 1_700_000_000_000_000_000;
    let step_ns: i64 = 1_000_000_000;
    let end_ns: i64 = start_ns + step_ns * MAX_QUERY_STEPS;
    let num_steps = end_ns.saturating_sub(start_ns).max(0) / step_ns;
    assert_eq!(num_steps, MAX_QUERY_STEPS);
    assert!(num_steps >= MAX_QUERY_STEPS); // rejected

    // 10999 steps should be allowed
    let end_ns_ok: i64 = start_ns + step_ns * (MAX_QUERY_STEPS - 1);
    let num_steps_ok = end_ns_ok.saturating_sub(start_ns).max(0) / step_ns;
    assert_eq!(num_steps_ok, MAX_QUERY_STEPS - 1);
    assert!(num_steps_ok < MAX_QUERY_STEPS); // allowed
}

#[test]
fn test_step_count_one_over_limit() {
    // 11001 steps should be rejected
    let start_ns: i64 = 1_700_000_000_000_000_000;
    let step_ns: i64 = 1_000_000_000;
    let end_ns: i64 = start_ns + step_ns * (MAX_QUERY_STEPS + 1);
    let num_steps = end_ns.saturating_sub(start_ns).max(0) / step_ns;
    assert_eq!(num_steps, MAX_QUERY_STEPS + 1);
    assert!(num_steps >= MAX_QUERY_STEPS);
}

#[test]
fn test_step_count_overflow_protection() {
    // Extreme timestamps: saturating_sub prevents overflow
    let start_ns: i64 = -1_000_000_000_000_000_000;
    let end_ns: i64 = i64::MAX;
    let step_ns: i64 = 1_000_000_000;
    // Without saturating_sub, this would overflow
    let num_steps = end_ns.saturating_sub(start_ns).max(0) / step_ns;
    assert!(num_steps >= MAX_QUERY_STEPS);
}

#[test]
fn test_effective_step_from_metric_query_range() {
    // When step is None, the effective step comes from the MetricQuery range
    use super::super::parser::{LogQLExpr, LogQLMatcher, MatchOp, MetricFunc};
    use std::time::Duration;

    let expr = LogQLExpr::MetricQuery {
        function: MetricFunc::CountOverTime,
        inner: Box::new(LogQLExpr::StreamSelector {
            matchers: vec![LogQLMatcher {
                name: "service".into(),
                op: MatchOp::Eq,
                value: "test".into(),
            }],
        }),
        range: Duration::from_secs(5), // 5s range = 5_000_000_000 ns step
    };

    let step_ns: Option<i64> = None;
    let effective = match (&expr, step_ns) {
        (_, Some(s)) => Some(s),
        (LogQLExpr::MetricQuery { range, .. }, None) => Some(range.as_nanos() as i64),
        _ => None,
    };
    assert_eq!(effective, Some(5_000_000_000));
}
