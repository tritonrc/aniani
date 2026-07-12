use super::*;

#[test]
fn test_simple_attribute_selector() {
    let expr = parse_traceql(r#"{ resource.service.name = "payments" }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector {
            conditions,
            logical_ops,
        } => {
            assert_eq!(conditions.len(), 1);
            assert!(logical_ops.is_empty());
            match &conditions[0] {
                SpanCondition::Attribute {
                    scope,
                    name,
                    op,
                    value,
                } => {
                    assert_eq!(*scope, AttrScope::Resource);
                    assert_eq!(name, "service.name");
                    assert_eq!(*op, CompareOp::Eq);
                    assert_eq!(*value, SpanValue::String("payments".into()));
                }
                _ => panic!("expected Attribute"),
            }
        }
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_huge_duration_literal_returns_parse_error() {
    let huge_duration_query = format!("{{ duration > {}h }}", "9".repeat(10_000));
    assert!(parse_traceql(&huge_duration_query).is_err());
    assert!(parse_traceql("{ status = error } | avg(duration) > 9223372036854775808ms").is_err());
}

#[test]
fn test_name_selector() {
    let expr = parse_traceql(r#"{ name = "POST /api/transfer" }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => match &conditions[0] {
            SpanCondition::Name { op, value } => {
                assert_eq!(*op, CompareOp::Eq);
                assert_eq!(value, "POST /api/transfer");
            }
            _ => panic!("expected Name"),
        },
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_status_selector() {
    let expr = parse_traceql(r#"{ status = error }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => match &conditions[0] {
            SpanCondition::Status { op, value } => {
                assert_eq!(*op, CompareOp::Eq);
                assert_eq!(*value, SpanStatusValue::Error);
            }
            _ => panic!("expected Status"),
        },
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_duration_selector() {
    let expr = parse_traceql(r#"{ duration > 500ms }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => match &conditions[0] {
            SpanCondition::Duration { op, value } => {
                assert_eq!(*op, CompareOp::Gt);
                assert_eq!(*value, Duration::from_millis(500));
            }
            _ => panic!("expected Duration"),
        },
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_duration_seconds() {
    let expr = parse_traceql(r#"{ duration > 1s }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => match &conditions[0] {
            SpanCondition::Duration { op, value } => {
                assert_eq!(*op, CompareOp::Gt);
                assert_eq!(*value, Duration::from_secs(1));
            }
            _ => panic!("expected Duration"),
        },
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_and_conditions() {
    let expr = parse_traceql(r#"{ duration > 1s && resource.service.name = "payments" }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector {
            conditions,
            logical_ops,
        } => {
            assert_eq!(conditions.len(), 2);
            assert_eq!(logical_ops, vec![LogicalOp::And]);
        }
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_or_conditions() {
    let expr = parse_traceql(r#"{ status = error || span.http.status_code >= 500 }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector {
            conditions,
            logical_ops,
        } => {
            assert_eq!(conditions.len(), 2);
            assert_eq!(logical_ops, vec![LogicalOp::Or]);
        }
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_structural_descendant() {
    let expr = parse_traceql(
        r#"{ resource.service.name = "api-gateway" } >> { resource.service.name = "payments" }"#,
    )
    .unwrap();
    match expr {
        TraceQLExpr::Structural { op, .. } => {
            assert_eq!(op, StructuralOp::Descendant);
        }
        _ => panic!("expected Structural"),
    }
}

#[test]
fn test_span_attribute_int() {
    let expr = parse_traceql(r#"{ span.http.status_code = 500 }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => match &conditions[0] {
            SpanCondition::Attribute { value, .. } => {
                assert_eq!(*value, SpanValue::Int(500));
            }
            _ => panic!("expected Attribute"),
        },
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_empty_selector() {
    let expr = parse_traceql("{}").unwrap();
    match expr {
        TraceQLExpr::SpanSelector {
            conditions,
            logical_ops,
        } => {
            assert!(conditions.is_empty());
            assert!(logical_ops.is_empty());
        }
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_empty_selector_with_spaces() {
    let expr = parse_traceql("{  }").unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => {
            assert!(conditions.is_empty());
        }
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_float_duration() {
    let expr = parse_traceql(r#"{ duration > 2.5s }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => match &conditions[0] {
            SpanCondition::Duration { value, .. } => {
                assert_eq!(*value, Duration::from_secs_f64(2.5));
            }
            _ => panic!("expected Duration"),
        },
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_count_filter_gt() {
    let expr = parse_traceql(r#"{ status = error } | count() > 2"#).unwrap();
    match expr {
        TraceQLExpr::Pipeline {
            inner,
            pipeline_stages,
        } => {
            assert!(matches!(*inner, TraceQLExpr::SpanSelector { .. }));
            assert_eq!(pipeline_stages.len(), 1);
            assert_eq!(
                pipeline_stages[0],
                PipelineStage::CountFilter {
                    op: CompareOp::Gt,
                    value: 2
                }
            );
        }
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn test_count_filter_gte() {
    let expr = parse_traceql(r#"{} | count() >= 3"#).unwrap();
    match expr {
        TraceQLExpr::Pipeline {
            pipeline_stages, ..
        } => {
            assert_eq!(
                pipeline_stages[0],
                PipelineStage::CountFilter {
                    op: CompareOp::Gte,
                    value: 3
                }
            );
        }
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn test_count_filter_eq() {
    let expr = parse_traceql(r#"{ status = ok } | count() = 1"#).unwrap();
    match expr {
        TraceQLExpr::Pipeline {
            pipeline_stages, ..
        } => {
            assert_eq!(
                pipeline_stages[0],
                PipelineStage::CountFilter {
                    op: CompareOp::Eq,
                    value: 1
                }
            );
        }
        _ => panic!("expected Pipeline"),
    }
}

#[test]
fn test_trailing_and_is_parse_error() {
    let result = parse_traceql("{ status = error && }");
    assert!(result.is_err(), "trailing && should be a parse error");
}

#[test]
fn test_trailing_or_is_parse_error() {
    let result = parse_traceql("{ status = error || }");
    assert!(result.is_err(), "trailing || should be a parse error");
}

#[test]
fn test_status_with_unsupported_operator_is_parse_error() {
    // Status is an unordered enum: ordering operators are meaningless and
    // must be rejected rather than silently returning empty results.
    for q in [
        "{ status >= error }",
        "{ status < ok }",
        "{ status > unset }",
        "{ status <= error }",
    ] {
        let result = parse_traceql(q);
        assert!(result.is_err(), "expected parse error for: {}", q);
    }
}

#[test]
fn test_status_neq_still_parses() {
    let expr = parse_traceql("{ status != error }").unwrap();
    assert!(matches!(expr, TraceQLExpr::SpanSelector { .. }));
}

#[test]
fn test_kind_selector() {
    let expr = parse_traceql(r#"{ kind = server }"#).unwrap();
    match expr {
        TraceQLExpr::SpanSelector { conditions, .. } => match &conditions[0] {
            SpanCondition::Kind { op, value } => {
                assert_eq!(*op, CompareOp::Eq);
                assert_eq!(*value, SpanKindValue::Server);
            }
            _ => panic!("expected Kind"),
        },
        _ => panic!("expected SpanSelector"),
    }
}

#[test]
fn test_kind_with_unsupported_operator_is_parse_error() {
    for q in [
        "{ kind >= server }",
        "{ kind < client }",
        "{ kind > internal }",
        "{ kind <= consumer }",
    ] {
        let result = parse_traceql(q);
        assert!(result.is_err(), "expected parse error for: {}", q);
    }
}

#[test]
fn test_kind_neq_still_parses() {
    let expr = parse_traceql("{ kind != client }").unwrap();
    assert!(matches!(expr, TraceQLExpr::SpanSelector { .. }));
}
