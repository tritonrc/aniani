//! Helper functions for building OpenAPI path operation objects.

use serde_json::{Value, json};

pub(super) fn protobuf_post(summary: &str, tag: &str) -> Value {
    json!({
        "post": {
            "summary": summary,
            "description": "Accepts protobuf (default, Content-Type: application/x-protobuf) or JSON (Content-Type: application/json) encoded OTLP payloads. Gzip compression is supported via Content-Encoding: gzip.",
            "tags": [tag],
            "requestBody": {
                "required": true,
                "content": {
                    "application/x-protobuf": {
                        "schema": { "type": "string", "format": "binary" }
                    },
                    "application/json": {
                        "schema": { "type": "object" }
                    }
                }
            },
            "responses": {
                "200": { "description": "Success" },
                "400": { "description": "Invalid payload" }
            }
        }
    })
}

pub(super) fn simple_get(summary: &str, tag: &str) -> Value {
    json!({
        "get": {
            "summary": summary,
            "tags": [tag],
            "responses": {
                "200": { "description": "Success", "content": { "application/json": {} } }
            }
        }
    })
}

pub(super) fn query_endpoint(summary: &str, tag: &str, params: Value) -> Value {
    json!({
        "get": {
            "summary": summary,
            "tags": [tag],
            "parameters": params,
            "responses": {
                "200": { "description": "Query results", "content": {
                    "application/json": { "schema": { "$ref": "#/components/schemas/QueryResponse" } }
                }},
                "400": { "description": "Invalid query" }
            }
        },
        "post": {
            "summary": summary,
            "tags": [tag],
            "requestBody": {
                "required": true,
                "content": {
                    "application/x-www-form-urlencoded": {
                        "schema": {
                            "type": "object"
                        }
                    }
                }
            },
            "responses": {
                "200": { "description": "Query results", "content": {
                    "application/json": { "schema": { "$ref": "#/components/schemas/QueryResponse" } }
                }},
                "400": { "description": "Invalid query" }
            }
        }
    })
}

pub(super) fn labels_endpoint(summary: &str, tag: &str) -> Value {
    json!({
        "get": {
            "summary": summary,
            "tags": [tag],
            "responses": {
                "200": { "description": "Label names", "content": {
                    "application/json": { "schema": { "$ref": "#/components/schemas/LabelsResponse" } }
                }}
            }
        }
    })
}
