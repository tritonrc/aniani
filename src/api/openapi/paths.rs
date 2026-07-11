//! Path definitions for the OpenAPI specification.

use serde_json::{Map, Value, json};

use super::helpers::{labels_endpoint, protobuf_post, query_endpoint, simple_get};

/// Build the `paths` section of the OpenAPI document.
pub(super) fn build_paths() -> Map<String, Value> {
    let mut paths = Map::new();

    // Ingestion endpoints
    paths.insert(
            "/loki/api/v1/push".into(),
            json!({
                "post": {
                    "summary": "Ingest logs via Loki push API",
                    "description": "Accepts Loki JSON directly, or snappy-compressed JSON using application/x-protobuf or application/x-snappy. Native Loki protobuf is not supported.",
                    "tags": ["ingestion"],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/LokiPushRequest" }
                            },
                            "application/x-protobuf": {
                                "schema": { "type": "string", "format": "binary" }
                            },
                            "application/x-snappy": {
                                "schema": { "type": "string", "format": "binary" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Logs ingested successfully" },
                        "400": { "description": "Invalid request body" }
                    }
                }
            }),
        );
    paths.insert(
        "/v1/metrics".into(),
        protobuf_post("Ingest metrics via OTLP/HTTP", "ingestion"),
    );
    paths.insert(
        "/v1/traces".into(),
        protobuf_post("Ingest traces via OTLP/HTTP", "ingestion"),
    );
    paths.insert(
        "/v1/logs".into(),
        protobuf_post("Ingest logs via OTLP/HTTP", "ingestion"),
    );
    paths.insert(
            "/api/v1/write".into(),
            json!({
                "post": {
                    "summary": "Ingest metrics via Prometheus remote write",
                    "description": "Accepts Prometheus remote write protobuf. Snappy compression is supported and expected by most clients.",
                    "tags": ["ingestion"],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/x-protobuf": {
                                "schema": { "type": "string", "format": "binary" }
                            }
                        }
                    },
                    "responses": {
                        "204": { "description": "Metrics ingested successfully" },
                        "400": { "description": "Invalid payload" }
                    }
                }
            }),
        );

    // LogQL query endpoints
    let query_params = json!([
        { "name": "query", "in": "query", "required": true, "schema": { "type": "string" }, "description": "LogQL query expression" },
        { "name": "time", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Evaluation timestamp (RFC3339 or Unix)" },
        { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" }, "description": "Maximum number of entries to return" }
    ]);
    paths.insert(
        "/loki/api/v1/query".into(),
        query_endpoint("Evaluate a LogQL instant query", "logql", query_params),
    );

    let range_params = json!([
        { "name": "query", "in": "query", "required": true, "schema": { "type": "string" }, "description": "LogQL query expression" },
        { "name": "start", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Start timestamp" },
        { "name": "end", "in": "query", "required": false, "schema": { "type": "string" }, "description": "End timestamp" },
        { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" }, "description": "Maximum number of entries to return" },
        { "name": "step", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Query step" }
    ]);
    paths.insert(
        "/loki/api/v1/query_range".into(),
        query_endpoint("Evaluate a LogQL range query", "logql", range_params),
    );

    paths.insert(
        "/loki/api/v1/labels".into(),
        labels_endpoint("List all log label names", "logql"),
    );

    paths.insert(
            "/loki/api/v1/label/{name}/values".into(),
            json!({
                "get": {
                    "summary": "List values for a log label",
                    "tags": ["logql"],
                    "parameters": [
                        { "name": "name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Label name" }
                    ],
                    "responses": {
                        "200": { "description": "Label values", "content": {
                            "application/json": { "schema": { "$ref": "#/components/schemas/LabelsResponse" } }
                        }}
                    }
                }
            }),
        );

    // PromQL query endpoints
    let promql_query_params = json!([
        { "name": "query", "in": "query", "required": true, "schema": { "type": "string" }, "description": "PromQL query expression" },
        { "name": "time", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Evaluation timestamp" }
    ]);
    paths.insert(
        "/api/v1/query".into(),
        query_endpoint(
            "Evaluate a PromQL instant query",
            "promql",
            promql_query_params,
        ),
    );

    let promql_range_params = json!([
        { "name": "query", "in": "query", "required": true, "schema": { "type": "string" }, "description": "PromQL query expression" },
        { "name": "start", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Start timestamp" },
        { "name": "end", "in": "query", "required": false, "schema": { "type": "string" }, "description": "End timestamp" },
        { "name": "step", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Query step" }
    ]);
    paths.insert(
        "/api/v1/query_range".into(),
        query_endpoint(
            "Evaluate a PromQL range query",
            "promql",
            promql_range_params,
        ),
    );

    paths.insert(
        "/api/v1/labels".into(),
        labels_endpoint("List all metric label names", "promql"),
    );

    paths.insert(
            "/api/v1/label/{name}/values".into(),
            json!({
                "get": {
                    "summary": "List values for a metric label",
                    "tags": ["promql"],
                    "parameters": [
                        { "name": "name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Label name" }
                    ],
                    "responses": {
                        "200": { "description": "Label values", "content": {
                            "application/json": { "schema": { "$ref": "#/components/schemas/LabelsResponse" } }
                        }}
                    }
                }
            }),
        );

    paths.insert(
            "/api/v1/series".into(),
            json!({
                "get": {
                    "summary": "Find metric series matching label matchers",
                    "tags": ["promql"],
                    "parameters": [
                        { "name": "match[]", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Series selector" },
                        { "name": "start", "in": "query", "required": false, "schema": { "type": "string" } },
                        { "name": "end", "in": "query", "required": false, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "description": "Matching series", "content": {
                            "application/json": { "schema": { "$ref": "#/components/schemas/SeriesResponse" } }
                        }}
                    }
                }
            }),
        );

    // TraceQL endpoints
    paths.insert(
            "/api/search".into(),
            json!({
                "get": {
                    "summary": "Search traces via TraceQL",
                    "description": "When the `q` parameter is omitted or empty, returns the most recent traces (up to `limit`, default 20). This is the behavior Grafana Tempo expects for datasource health checks.",
                    "tags": ["traceql"],
                    "parameters": [
                        { "name": "q", "in": "query", "required": false, "schema": { "type": "string" }, "description": "TraceQL query expression. Omit to list recent traces." },
                        { "name": "start", "in": "query", "required": false, "schema": { "type": "integer" }, "description": "Start of time range — epoch seconds or nanoseconds (auto-detected)" },
                        { "name": "end", "in": "query", "required": false, "schema": { "type": "integer" }, "description": "End of time range — epoch seconds or nanoseconds (auto-detected)" },
                        { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" }, "description": "Maximum number of traces to return (default 20)" }
                    ],
                    "responses": {
                        "200": { "description": "Search results", "content": {
                            "application/json": { "schema": { "$ref": "#/components/schemas/TraceSearchResponse" } }
                        }},
                        "400": { "description": "Invalid query" }
                    }
                }
            }),
        );

    paths.insert(
            "/api/traces/{traceID}".into(),
            json!({
                "get": {
                    "summary": "Get all spans for a trace by ID",
                    "tags": ["traceql"],
                    "parameters": [
                        { "name": "traceID", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Hex-encoded 128-bit trace ID" }
                    ],
                    "responses": {
                        "200": { "description": "Trace data", "content": {
                            "application/json": { "schema": { "$ref": "#/components/schemas/TraceResponse" } }
                        }},
                        "400": { "description": "Invalid trace ID" },
                        "404": { "description": "Trace not found" }
                    }
                }
            }),
        );

    // Discovery and management endpoints
    paths.insert(
        "/api/v1/services".into(),
        simple_get("List all known service names", "discovery"),
    );
    paths.insert(
        "/api/v1/status".into(),
        simple_get("Get store statistics and health info", "discovery"),
    );
    paths.insert(
            "/api/v1/diagnose".into(),
            json!({
                "get": {
                    "summary": "Health assessment: global overview or per-service diagnosis",
                    "tags": ["discovery"],
                    "parameters": [
                        { "name": "service", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Service name. Omit for global overview." }
                    ],
                    "responses": {
                        "200": { "description": "Health assessment", "content": { "application/json": {} } }
                    }
                }
            }),
        );
    paths.insert(
            "/api/v1/catalog".into(),
            json!({
                "get": {
                    "summary": "Get a catalog of metrics, log labels, and span attributes for one service",
                    "tags": ["discovery"],
                    "parameters": [
                        { "name": "service", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Service name" }
                    ],
                    "responses": {
                        "200": { "description": "Catalog", "content": { "application/json": {} } },
                        "400": { "description": "Missing required parameter" }
                    }
                }
            }),
        );
    paths.insert(
            "/api/v1/summary".into(),
            json!({
                "get": {
                    "summary": "Get a compact cross-signal error summary for one service",
                    "tags": ["discovery"],
                    "parameters": [
                        { "name": "service", "in": "query", "required": true, "schema": { "type": "string" }, "description": "Service name" }
                    ],
                    "responses": {
                        "200": { "description": "Service summary", "content": { "application/json": {} } },
                        "400": { "description": "Missing required parameter" }
                    }
                }
            }),
        );
    paths.insert(
            "/api/v1/reset".into(),
            json!({
                "delete": {
                    "summary": "Reset all in-memory stores",
                    "tags": ["management"],
                    "parameters": [
                        { "name": "service", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Optional service name for targeted reset" }
                    ],
                    "responses": {
                        "200": { "description": "Stores reset successfully" }
                    }
                }
            }),
        );
    paths.insert(
            "/api/v1/snapshot".into(),
            json!({
                "post": {
                    "summary": "Write a snapshot of all stores to the configured snapshot directory",
                    "description": "Cross-platform on-demand snapshot trigger (the portable equivalent of the Unix-only SIGUSR1 signal). Returns the number of bytes written.",
                    "tags": ["management"],
                    "responses": {
                        "200": { "description": "Snapshot written", "content": { "application/json": {} } },
                        "500": { "description": "Snapshot failed" }
                    }
                }
            }),
        );
    paths.insert(
        "/api/v1/metadata".into(),
        simple_get("Get metric metadata", "promql"),
    );
    paths.insert(
        "/api/v1/openapi.json".into(),
        simple_get("Get this OpenAPI specification", "discovery"),
    );
    paths.insert(
        "/ready".into(),
        json!({
            "get": {
                "summary": "Health check / readiness probe",
                "tags": ["health"],
                "responses": {
                    "200": { "description": "Service is ready", "content": {
                        "application/json": { "schema": {
                            "type": "object",
                            "properties": {
                                "status": { "type": "string", "example": "ready" }
                            }
                        }}
                    }}
                }
            }
        }),
    );

    paths
}
