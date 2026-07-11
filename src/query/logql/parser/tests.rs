use super::*;

#[test]
fn test_stream_selector_simple() {
    let expr = parse_logql(r#"{service="payments"}"#).unwrap();
    match expr {
        LogQLExpr::StreamSelector { matchers } => {
            assert_eq!(matchers.len(), 1);
            assert_eq!(matchers[0].name, "service");
            assert_eq!(matchers[0].op, MatchOp::Eq);
            assert_eq!(matchers[0].value, "payments");
        }
        _ => panic!("expected StreamSelector"),
    }
}

#[test]
fn test_stream_selector_multiple_matchers() {
    let expr = parse_logql(r#"{service="payments", level="error"}"#).unwrap();
    match expr {
        LogQLExpr::StreamSelector { matchers } => {
            assert_eq!(matchers.len(), 2);
        }
        _ => panic!("expected StreamSelector"),
    }
}

#[test]
fn test_stream_selector_regex() {
    let expr = parse_logql(r#"{service=~"pay.*"}"#).unwrap();
    match expr {
        LogQLExpr::StreamSelector { matchers } => {
            assert_eq!(matchers[0].op, MatchOp::Regex);
        }
        _ => panic!("expected StreamSelector"),
    }
}

#[test]
fn test_stream_selector_neq() {
    let expr = parse_logql(r#"{level!="debug"}"#).unwrap();
    match expr {
        LogQLExpr::StreamSelector { matchers } => {
            assert_eq!(matchers[0].op, MatchOp::Neq);
        }
        _ => panic!("expected StreamSelector"),
    }
}

#[test]
fn test_stream_selector_not_regex() {
    let expr = parse_logql(r#"{level!~"debug|trace"}"#).unwrap();
    match expr {
        LogQLExpr::StreamSelector { matchers } => {
            assert_eq!(matchers[0].op, MatchOp::NotRegex);
        }
        _ => panic!("expected StreamSelector"),
    }
}

#[test]
fn test_pipeline_line_contains() {
    let expr = parse_logql(r#"{service="payments"} |= "timeout""#).unwrap();
    match expr {
        LogQLExpr::Pipeline { stages, .. } => {
            assert_eq!(stages.len(), 1);
            assert!(matches!(&stages[0], PipelineStage::LineContains(s) if s == "timeout"));
        }
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn test_pipeline_line_not_contains() {
    let expr = parse_logql(r#"{service="payments"} != "healthcheck""#).unwrap();
    match expr {
        LogQLExpr::Pipeline { stages, .. } => {
            assert!(matches!(&stages[0], PipelineStage::LineNotContains(s) if s == "healthcheck"));
        }
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn test_pipeline_line_regex() {
    let expr = parse_logql(r#"{service="payments"} |~ "error|warn""#).unwrap();
    match expr {
        LogQLExpr::Pipeline { stages, .. } => {
            assert!(matches!(&stages[0], PipelineStage::LineRegex(s, _) if s == "error|warn"));
        }
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn test_pipeline_line_not_regex() {
    let expr = parse_logql(r#"{service="payments"} !~ "debug|trace""#).unwrap();
    match expr {
        LogQLExpr::Pipeline { stages, .. } => {
            assert!(matches!(&stages[0], PipelineStage::LineNotRegex(s, _) if s == "debug|trace"));
        }
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn test_metric_count_over_time() {
    let expr = parse_logql(r#"count_over_time({service="payments"}[5m])"#).unwrap();
    match expr {
        LogQLExpr::MetricQuery {
            function, range, ..
        } => {
            assert_eq!(function, MetricFunc::CountOverTime);
            assert_eq!(range, Duration::from_secs(300));
        }
        _ => panic!("expected MetricQuery"),
    }
}

#[test]
fn test_metric_rate() {
    let expr = parse_logql(r#"rate({service="payments"} |= "error" [1m])"#).unwrap();
    match expr {
        LogQLExpr::MetricQuery {
            function,
            range,
            inner,
        } => {
            assert_eq!(function, MetricFunc::Rate);
            assert_eq!(range, Duration::from_secs(60));
            match *inner {
                LogQLExpr::Pipeline { stages, .. } => {
                    assert_eq!(stages.len(), 1);
                }
                _ => panic!("expected Pipeline inner"),
            }
        }
        _ => panic!("expected MetricQuery"),
    }
}

#[test]
fn test_metric_bytes_over_time() {
    let expr = parse_logql(r#"bytes_over_time({service="payments"}[5m])"#).unwrap();
    match expr {
        LogQLExpr::MetricQuery { function, .. } => {
            assert_eq!(function, MetricFunc::BytesOverTime);
        }
        _ => panic!("expected MetricQuery"),
    }
}

#[test]
fn test_metric_query_rejects_trailing_garbage() {
    let result = parse_logql(r#"count_over_time({service="payments"}[5m]) extra junk"#);
    assert!(
        result.is_err(),
        "trailing input after metric query must error"
    );
}

#[test]
fn test_parse_error_never_leaks_nom_debug_output() {
    let msg = parse_logql(r#"{service="#).unwrap_err().to_string();
    assert!(
        msg.contains("position"),
        "expected a human message with a byte position, got: {msg}"
    );
    assert!(
        !msg.contains("code:") && !msg.contains("Parsing Error") && !msg.contains("Error {"),
        "nom debug output leaked into the error message: {msg}"
    );
}

#[test]
fn test_parse_error_missing_quoted_value_after_eq() {
    let input = r#"{service="#;
    let msg = parse_logql(input).unwrap_err().to_string();
    assert!(
        msg.contains("expected a quoted value after '='"),
        "got: {msg}"
    );
    // Failure occurs right after the `=`, i.e. at the end of the input here.
    assert!(
        msg.contains(&format!("position {}", input.len())),
        "got: {msg}"
    );
}

#[test]
fn test_parse_error_unclosed_selector() {
    let msg = parse_logql(r#"{service="payments""#)
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("expected '}' to close the selector"),
        "got: {msg}"
    );
}

#[test]
fn test_parse_error_bad_label_name() {
    let msg = parse_logql(r#"{="payments"}"#).unwrap_err().to_string();
    assert!(msg.contains("expected a label name"), "got: {msg}");
    assert!(msg.contains("position 1"), "got: {msg}");
}

#[test]
fn test_parse_error_position_mid_query() {
    // Failure is mid-query (not at the start, not at EOF): a second matcher
    // with an unquoted value, followed by more input.
    let input = r#"{service="payments", level=, foo="bar"}"#;
    let msg = parse_logql(input).unwrap_err().to_string();
    assert!(
        msg.contains("expected a quoted value after '='"),
        "got: {msg}"
    );
    let fail_pos = input.find("=, foo").unwrap() + 1; // position right after the second `=`
    assert!(fail_pos > 0 && fail_pos < input.len() - 1, "sanity check");
    assert!(msg.contains(&format!("position {fail_pos}")), "got: {msg}");
}
