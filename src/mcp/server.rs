//! Axum handlers and JSON-RPC dispatch for the MCP endpoint.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::mcp::protocol::{self, INVALID_REQUEST, JsonRpcRequest, METHOD_NOT_FOUND, PARSE_ERROR};
use crate::mcp::tools;
use crate::store::SharedState;

/// Dispatch one parsed JSON-RPC message. Returns `Some(response)` for requests,
/// `None` for notifications (which the caller answers with HTTP 202).
pub fn dispatch(state: &SharedState, req: JsonRpcRequest) -> Option<Value> {
    if !req.is_request() {
        return None;
    }
    let id = req.id.clone();
    let resp = match req.method.as_str() {
        "initialize" => initialize(id, &req.params),
        "ping" => protocol::success(id, json!({})),
        "tools/list" => tools::list(id),
        "tools/call" => tools::call(state, id, &req.params),
        _ => protocol::error(id, METHOD_NOT_FOUND, "method not found"),
    };
    Some(resp)
}

/// Build the `initialize` result with version negotiation.
fn initialize(id: Option<Value>, params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(|v| v.as_str());
    let version = match requested {
        Some(v) if protocol::SUPPORTED_VERSIONS.contains(&v) => v.to_string(),
        _ => protocol::LATEST_VERSION.to_string(),
    };
    protocol::success(
        id,
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "aniani", "version": env!("CARGO_PKG_VERSION") },
            "instructions": tools::INSTRUCTIONS,
        }),
    )
}

/// `POST /mcp` — single JSON-RPC message in, single JSON response (or 202).
pub async fn mcp_post(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = check_origin(&headers) {
        return *resp;
    }
    if let Err(resp) = check_protocol_version(&headers) {
        return *resp;
    }
    let req: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(_) => {
            // Valid JSON that isn't a single JSON-RPC message (e.g. an array/batch)
            // is an invalid request; anything else is a parse error.
            let code = match serde_json::from_slice::<Value>(&body) {
                Ok(_) => INVALID_REQUEST,
                Err(_) => PARSE_ERROR,
            };
            let msg = if code == INVALID_REQUEST {
                "batching/invalid request is not supported; send a single JSON-RPC message"
            } else {
                "parse error"
            };
            return json_response(protocol::error(None, code, msg));
        }
    };
    match dispatch(&state, req) {
        Some(resp) => json_response(resp),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// `GET /mcp` — no server-initiated SSE stream offered.
pub async fn mcp_get() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

/// `DELETE /mcp` — stateless, no sessions to terminate.
pub async fn mcp_delete() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

fn json_response(value: Value) -> Response {
    axum::Json(value).into_response()
}

/// Validate `Origin` to defend against DNS-rebinding. Absent Origin (native
/// clients) is allowed; a present Origin must be a localhost variant.
fn check_origin(headers: &HeaderMap) -> Result<(), Box<Response>> {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok())
        && !is_allowed_origin(origin)
    {
        return Err(Box::new(
            (StatusCode::FORBIDDEN, "origin not allowed").into_response(),
        ));
    }
    Ok(())
}

/// True if the Origin's host is a loopback address.
fn is_allowed_origin(origin: &str) -> bool {
    let after_scheme = origin.split("://").nth(1).unwrap_or(origin);
    let hostport = after_scheme.split('/').next().unwrap_or("");
    let host = if let Some(end) = hostport.strip_prefix('[').and_then(|s| s.split(']').next()) {
        end.to_string()
    } else {
        hostport.split(':').next().unwrap_or("").to_string()
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// Honor `MCP-Protocol-Version`: absent → proceed (default); present but
/// unsupported → 400.
fn check_protocol_version(headers: &HeaderMap) -> Result<(), Box<Response>> {
    match headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok()) {
        None => Ok(()),
        Some(v) if protocol::SUPPORTED_VERSIONS.contains(&v) => Ok(()),
        Some(_) => Err(Box::new(
            (StatusCode::BAD_REQUEST, "unsupported MCP-Protocol-Version").into_response(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dispatch_ping_returns_empty_result() {
        let req = serde_json::from_value(json!({
            "jsonrpc":"2.0","id":1,"method":"ping","params":{}
        })).unwrap();
        let resp = dispatch(&test_state(), req).expect("ping is a request");
        assert_eq!(resp["result"], json!({}));
    }

    #[test]
    fn dispatch_unknown_method_is_method_not_found() {
        let req = serde_json::from_value(json!({
            "jsonrpc":"2.0","id":1,"method":"nope","params":{}
        })).unwrap();
        let resp = dispatch(&test_state(), req).expect("request");
        assert_eq!(resp["error"]["code"], json!(crate::mcp::protocol::METHOD_NOT_FOUND));
    }

    #[test]
    fn dispatch_notification_returns_none() {
        let req = serde_json::from_value(json!({
            "jsonrpc":"2.0","method":"notifications/initialized"
        })).unwrap();
        assert!(dispatch(&test_state(), req).is_none());
    }

    #[test]
    fn origin_localhost_allowed_foreign_rejected() {
        assert!(is_allowed_origin("http://localhost:3000"));
        assert!(is_allowed_origin("http://127.0.0.1:4320"));
        assert!(is_allowed_origin("https://[::1]:9999"));
        assert!(!is_allowed_origin("https://evil.example.com"));
    }

    #[test]
    fn protocol_version_absent_ok_unsupported_rejected() {
        use axum::http::HeaderMap;
        let empty = HeaderMap::new();
        assert!(check_protocol_version(&empty).is_ok());

        let mut bad = HeaderMap::new();
        bad.insert("mcp-protocol-version", "1999-01-01".parse().unwrap());
        assert!(check_protocol_version(&bad).is_err());

        let mut good = HeaderMap::new();
        good.insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
        assert!(check_protocol_version(&good).is_ok());
    }

    fn test_state() -> crate::store::SharedState {
        use crate::store::{AppState, LogStore, MetricStore, TraceStore};
        use clap::Parser;
        use parking_lot::RwLock;
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;
        use std::time::Instant;
        Arc::new(AppState {
            log_store: RwLock::new(LogStore::new()),
            metric_store: RwLock::new(MetricStore::new()),
            trace_store: RwLock::new(TraceStore::new()),
            config: crate::config::Config::parse_from(["aniani"]),
            start_time: Instant::now(),
            ingest_seq: AtomicU64::new(0),
        })
    }
}
