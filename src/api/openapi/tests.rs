use super::*;

#[test]
fn test_spec_has_openapi_version() {
    let s = spec();
    assert_eq!(s["openapi"], "3.0.3");
}

#[test]
fn test_spec_has_all_paths() {
    let s = spec();
    let paths = s["paths"].as_object().unwrap();
    let expected = [
        "/loki/api/v1/query",
        "/loki/api/v1/query_range",
        "/loki/api/v1/labels",
        "/loki/api/v1/label/{name}/values",
        "/loki/api/v1/push",
        "/api/v1/write",
        "/api/v1/query",
        "/api/v1/query_range",
        "/api/v1/labels",
        "/api/v1/label/{name}/values",
        "/api/v1/series",
        "/api/v1/services",
        "/api/v1/status",
        "/api/search",
        "/api/traces/{traceID}",
        "/v1/metrics",
        "/v1/traces",
        "/v1/logs",
        "/api/v1/diagnose",
        "/api/v1/catalog",
        "/api/v1/summary",
        "/api/v1/reset",
        "/api/v1/snapshot",
        "/api/v1/metadata",
        "/api/v1/openapi.json",
        "/ready",
    ];
    for path in &expected {
        assert!(paths.contains_key(*path), "missing path: {}", path);
    }
}
