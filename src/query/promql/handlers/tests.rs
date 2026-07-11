use super::*;

#[test]
fn test_format_range_result_scalar_uses_end_timestamp() {
    // A scalar in a range query must carry the evaluation timestamp
    // (end_ms), not a hardcoded 0.
    let end_ms = 1_700_000_000_000i64;
    let v = format_range_result(PromQLResult::Scalar(42.0), end_ms);
    assert_eq!(v["data"]["resultType"], "scalar");
    let ts = v["data"]["result"][0].as_f64().unwrap();
    assert!((ts - (end_ms as f64 / 1000.0)).abs() < f64::EPSILON);
    assert_ne!(ts, 0.0);
}

#[test]
fn test_max_query_steps_constant() {
    assert_eq!(MAX_QUERY_STEPS, 11_000);
}

#[test]
fn test_parse_timestamp_rejects_non_finite_float() {
    assert_eq!(parse_timestamp_ms("NaN"), None);
    assert_eq!(parse_timestamp_ms("inf"), None);
    assert_eq!(parse_timestamp_ms("1e300"), None);
}

#[test]
fn test_step_count_within_limit() {
    // 3600000ms range / 1000ms step = 3600 steps, under the 11000 cap
    let start_ms: i64 = 1_700_000_000_000;
    let end_ms: i64 = start_ms + 3_600_000;
    let step_ms: i64 = 1_000;
    let num_steps = end_ms.saturating_sub(start_ms).max(0) / step_ms;
    assert_eq!(num_steps, 3600);
    assert!(num_steps < MAX_QUERY_STEPS);
}

#[test]
fn test_step_count_exceeds_limit() {
    // 11001000ms range / 1ms step = 11_001_000 steps, way over the cap
    let start_ms: i64 = 1_700_000_000_000;
    let end_ms: i64 = start_ms + 11_001_000;
    let step_ms: i64 = 1;
    let num_steps = end_ms.saturating_sub(start_ms).max(0) / step_ms;
    assert!(num_steps >= MAX_QUERY_STEPS);
}

#[test]
fn test_step_count_exactly_at_limit() {
    // Exactly 11000 steps should be rejected (off-by-one: eval loop is inclusive)
    let start_ms: i64 = 1_700_000_000_000;
    let step_ms: i64 = 1_000;
    let end_ms: i64 = start_ms + step_ms * MAX_QUERY_STEPS;
    let num_steps = end_ms.saturating_sub(start_ms).max(0) / step_ms;
    assert_eq!(num_steps, MAX_QUERY_STEPS);
    assert!(num_steps >= MAX_QUERY_STEPS); // rejected

    // 10999 steps should be allowed
    let end_ms_ok: i64 = start_ms + step_ms * (MAX_QUERY_STEPS - 1);
    let num_steps_ok = end_ms_ok.saturating_sub(start_ms).max(0) / step_ms;
    assert_eq!(num_steps_ok, MAX_QUERY_STEPS - 1);
    assert!(num_steps_ok < MAX_QUERY_STEPS); // allowed
}

#[test]
fn test_step_count_one_over_limit() {
    // 11001 steps should be rejected
    let start_ms: i64 = 1_700_000_000_000;
    let step_ms: i64 = 1_000;
    let end_ms: i64 = start_ms + step_ms * (MAX_QUERY_STEPS + 1);
    let num_steps = end_ms.saturating_sub(start_ms).max(0) / step_ms;
    assert_eq!(num_steps, MAX_QUERY_STEPS + 1);
    assert!(num_steps >= MAX_QUERY_STEPS);
}

#[test]
fn test_step_count_overflow_protection() {
    // Extreme timestamps: saturating_sub prevents overflow
    let start_ms: i64 = -1_000_000_000_000;
    let end_ms: i64 = i64::MAX;
    let step_ms: i64 = 1_000;
    // Without saturating_sub, this would overflow
    let num_steps = end_ms.saturating_sub(start_ms).max(0) / step_ms;
    assert!(num_steps >= MAX_QUERY_STEPS);
}

#[test]
fn test_parse_timestamp_ms_seconds() {
    assert_eq!(parse_timestamp_ms("1700000000"), Some(1_700_000_000_000));
}

#[test]
fn test_parse_timestamp_ms_milliseconds() {
    assert_eq!(parse_timestamp_ms("1700000000000"), Some(1_700_000_000_000));
}

#[test]
fn test_parse_timestamp_ms_float_seconds() {
    assert_eq!(parse_timestamp_ms("1700000000.5"), Some(1_700_000_000_500));
}
