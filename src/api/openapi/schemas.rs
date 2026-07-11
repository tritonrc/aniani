//! Component schemas for the OpenAPI specification.

use serde_json::{Value, json};

/// Build the `components` section of the OpenAPI document.
pub(super) fn build_components() -> Value {
    // Components / schemas
    json!({
        "schemas": {
            "LokiPushRequest": {
                "type": "object",
                "properties": {
                    "streams": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "stream": { "type": "object", "additionalProperties": { "type": "string" } },
                                "values": { "type": "array", "items": { "type": "array", "items": { "type": "string" } } }
                            }
                        }
                    }
                }
            },
            "QueryResponse": {
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "data": {
                        "type": "object",
                        "properties": {
                            "resultType": { "type": "string" },
                            "result": {}
                        }
                    }
                }
            },
            "LabelsResponse": {
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "data": { "type": "array", "items": { "type": "string" } }
                }
            },
            "SeriesResponse": {
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "data": { "type": "array", "items": { "type": "object", "additionalProperties": { "type": "string" } } }
                }
            },
            "TraceSearchResponse": {
                "type": "object",
                "properties": {
                    "traces": { "type": "array", "items": { "type": "object" } }
                }
            },
            "TraceResponse": {
                "type": "object",
                "properties": {
                    "batches": { "type": "array", "items": { "type": "object" } }
                }
            },
            "ServicesResponse": {
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "data": { "type": "object", "properties": {
                        "services": { "type": "array", "items": { "type": "object" } }
                    }}
                }
            },
            "StatusResponse": {
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "data": { "type": "object", "properties": {
                        "totalLogEntries": { "type": "integer" },
                        "totalMetricSeries": { "type": "integer" },
                        "totalMetricSamples": { "type": "integer" },
                        "totalSpans": { "type": "integer" },
                        "totalTraces": { "type": "integer" },
                        "uptimeSeconds": { "type": "integer" },
                        "serviceCount": { "type": "integer" },
                        "memoryBytes": { "type": "integer" }
                    }}
                }
            }
        }
    })
}
