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
