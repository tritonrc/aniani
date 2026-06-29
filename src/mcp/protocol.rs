//! JSON-RPC protocol types for the MCP server.

use serde::Deserialize;
use serde_json::{Value, json};

/// JSON-RPC 2.0 standard error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// Supported MCP protocol revisions, newest first.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
/// Version assumed when a client omits the `MCP-Protocol-Version` header.
pub const DEFAULT_VERSION: &str = "2025-03-26";
/// Version advertised when negotiation fails.
pub const LATEST_VERSION: &str = "2025-11-25";

/// A parsed JSON-RPC 2.0 message (request or notification).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl JsonRpcRequest {
    /// True if this message expects a response (has an `id`).
    pub fn is_request(&self) -> bool {
        self.id.is_some()
    }
}

/// Build a JSON-RPC success response value.
pub fn success(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
}

/// Build a JSON-RPC error response value.
pub fn error(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null),
            "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_request_and_builds_success_response() {
        let raw = json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{}});
        let req: JsonRpcRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.method, "ping");
        assert!(req.is_request());

        let resp = success(req.id.clone(), json!({}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], json!(1));
        assert_eq!(resp["result"], json!({}));
    }

    #[test]
    fn notification_has_no_id() {
        let raw = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        let req: JsonRpcRequest = serde_json::from_value(raw).unwrap();
        assert!(!req.is_request());
    }

    #[test]
    fn builds_error_response_with_code() {
        let resp = error(Some(json!(7)), METHOD_NOT_FOUND, "no such method");
        assert_eq!(resp["error"]["code"], json!(METHOD_NOT_FOUND));
        assert_eq!(resp["id"], json!(7));
    }
}
