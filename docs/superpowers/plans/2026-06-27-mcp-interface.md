# MCP Interface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a hand-rolled MCP (Model Context Protocol) Streamable-HTTP server to Aniani exposing 10 intent-level tools (1 write) so coding agents can drive the `reset → run → summarize_activity` observability loop.

**Architecture:** A new `src/mcp/` module mounts `POST/GET/DELETE /mcp` onto the existing shared axum listener (the same merge pattern as `src/grpc.rs`). JSON-RPC 2.0 is hand-rolled (no SDK). Tools call transport-free typed cores that read the in-memory stores. A new global ingest-sequence counter makes the soft-checkpoint path correct against late-arriving / clock-skewed telemetry.

**Tech Stack:** Rust 2024, axum 0.8, serde/serde_json, parking_lot, lasso, thiserror. Spec target MCP `2025-11-25` (back-compat `2025-06-18`, `2025-03-26`).

**Spec:** `docs/superpowers/specs/2026-06-27-mcp-interface-design.md`

> **EXECUTION ORDER:** Do **Phase 1 (ingest-sequence cursor) before Phase 0 Task 0.3**. The transport tests construct `AppState`, which gains its `ingest_seq` field in Task 1.1. Recommended order: 0.1 → 0.2 → **1.1 → 1.2 → 1.3** → 0.3 → 0.4 → 0.5 → Phase 2 → onward.

---

## File Structure

**Create:**
- `src/mcp/mod.rs` — `pub fn routes(state) -> Router`; re-exports; module docs.
- `src/mcp/protocol.rs` — JSON-RPC envelope types, error codes, response helpers, supported-version constants.
- `src/mcp/server.rs` — axum handlers (`mcp_post`, `mcp_get`, `mcp_delete`), Origin/version header checks, method dispatch, `initialize`/`ping`.
- `src/mcp/tools.rs` — tool descriptor type, `tools/list`, `tools/call` dispatch, per-tool handlers.
- `src/mcp/synth.rs` — transport-free typed cores: `summarize_activity`, `check_health`, `describe_service`, `build_trace_tree`, plus their serializable result structs.
- `tests/e2e_mcp.rs` — end-to-end loop + compliance integration tests.

**Modify:**
- `src/lib.rs:3-12` — add `pub mod mcp;`.
- `src/server.rs:71-83` — merge `mcp::routes` before the grpc merge/fallback; route-scoped body limit.
- `src/store/mod.rs:24-34` — add `ingest_seq: AtomicU64` to `AppState`.
- `src/store/log_store.rs:14-19` — `ingest_seq` on `LogEntry`.
- `src/store/metric_store.rs:13-18` — `ingest_seq` on `Sample`.
- `src/store/trace_store.rs:77-91` — `ingest_seq` on `Span`.
- `src/ingest/otlp_logs.rs:87`, `loki.rs:177`, `otlp_metrics.rs` (8 sites), `remote_write.rs:120-127`, `otlp_traces.rs:43-56,212,282` — stamp the counter.
- `src/main.rs:54-60,108-112` — construct counter; restore monotonicity; startup log.
- `Cargo.toml` — version bump `0.10.0` → `0.11.0`.
- `DESIGN.md`, `AGENTS.md`, `README.md` — document the MCP surface.

---

# Phase 0 — Transport skeleton (no tools yet)

### Task 0.1: Create the `mcp` module and wire it into the crate

**Files:**
- Create: `src/mcp/mod.rs`, `src/mcp/protocol.rs`, `src/mcp/server.rs`, `src/mcp/tools.rs`, `src/mcp/synth.rs`
- Modify: `src/lib.rs:3-12`

- [ ] **Step 1: Add the module to `lib.rs`**

In `src/lib.rs`, add `pub mod mcp;` (keep alphabetical, after `pub mod ingest;`):

```rust
pub mod api;
pub mod config;
pub mod grpc;
pub mod ingest;
pub mod mcp;
pub mod query;
pub mod server;
pub mod snapshot;
pub mod store;
#[cfg(feature = "ui")]
pub mod ui;
```

- [ ] **Step 2: Create stub files so the crate compiles**

`src/mcp/mod.rs`:
```rust
//! Model Context Protocol (MCP) server.
//!
//! Hand-rolled JSON-RPC 2.0 over MCP Streamable HTTP, mounted on the shared
//! listener at `/mcp`. Read-only observability tools plus a single `reset`
//! write, built for the agent loop `reset → run → summarize_activity`.

pub mod protocol;
pub mod server;
pub mod synth;
pub mod tools;

use crate::store::SharedState;

/// Build the axum router for the MCP endpoint (`POST/GET/DELETE /mcp`).
pub fn routes(state: SharedState) -> axum::Router {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/mcp", post(server::mcp_post))
        .route("/mcp", get(server::mcp_get).delete(server::mcp_delete))
        .with_state(state)
}
```

`src/mcp/protocol.rs`, `src/mcp/server.rs`, `src/mcp/tools.rs`, `src/mcp/synth.rs`: start each with a `//!` doc line only (filled by later tasks), e.g. `//! JSON-RPC protocol types for the MCP server.`. To satisfy `routes()` referencing `server::mcp_post` etc., add temporary stubs in `server.rs`:

```rust
//! Axum handlers and JSON-RPC dispatch for the MCP endpoint.

use axum::http::StatusCode;

pub async fn mcp_post() -> StatusCode { StatusCode::NOT_IMPLEMENTED }
pub async fn mcp_get() -> StatusCode { StatusCode::METHOD_NOT_ALLOWED }
pub async fn mcp_delete() -> StatusCode { StatusCode::METHOD_NOT_ALLOWED }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: builds (warnings about unused `state` in `routes` are acceptable for now).

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs src/mcp/
git commit -m "mcp: scaffold module and route stubs"
```

---

### Task 0.2: JSON-RPC envelope types and response helpers

**Files:**
- Modify: `src/mcp/protocol.rs`
- Test: in-file `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Append to `src/mcp/protocol.rs`:
```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::protocol`
Expected: FAIL — `JsonRpcRequest`, `success`, `error`, `METHOD_NOT_FOUND` not found.

- [ ] **Step 3: Implement the types and helpers**

Put above the test module in `src/mcp/protocol.rs`:
```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::protocol`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/mcp/protocol.rs
git commit -m "mcp/protocol: JSON-RPC envelope types and helpers"
```

---

### Task 0.3: POST dispatch — method routing, notifications → 202, batching rejection

**Files:**
- Modify: `src/mcp/server.rs`
- Test: in-file `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Replace the stubs in `src/mcp/server.rs` test expectations by adding this test module at the bottom:
```rust
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
```

> Note: `ingest_seq` on `AppState` is added in Task 1.1. If implementing strictly in order, temporarily omit it here and add it back when Task 1.1 lands. (Recommended order: do Task 1.1 before running this test.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::server`
Expected: FAIL — `dispatch` not found.

- [ ] **Step 3: Implement `dispatch` and a real `mcp_post`**

Rewrite `src/mcp/server.rs` (above the test module):
```rust
//! Axum handlers and JSON-RPC dispatch for the MCP endpoint.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::mcp::protocol::{
    self, JsonRpcRequest, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
use crate::mcp::tools;
use crate::store::SharedState;

/// Dispatch one parsed JSON-RPC message. Returns `Some(response)` for requests,
/// `None` for notifications (which the caller answers with HTTP 202).
pub fn dispatch(state: &SharedState, req: JsonRpcRequest) -> Option<Value> {
    if !req.is_request() {
        // Notification: accept and answer 202 with no body at the HTTP layer.
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
        return resp;
    }
    if let Err(resp) = check_protocol_version(&headers) {
        return resp;
    }
    // Reject JSON-RPC batching (arrays) per the 2025-06-18+ transport.
    let parsed: Result<JsonRpcRequest, _> = serde_json::from_slice(&body);
    let req = match parsed {
        Ok(req) => req,
        Err(_) => {
            // Distinguish a malformed envelope from a (rejected) array/other JSON.
            let code = match serde_json::from_slice::<Value>(&body) {
                Ok(Value::Array(_)) => INVALID_REQUEST,
                Ok(_) => INVALID_REQUEST,
                Err(_) => PARSE_ERROR,
            };
            let msg = if code == INVALID_REQUEST {
                "batching is not supported; send a single JSON-RPC message"
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

/// Serialize a JSON-RPC value as an `application/json` 200 response.
fn json_response(value: Value) -> Response {
    axum::Json(value).into_response()
}
```

Add placeholder header-check fns (fleshed out in Task 0.4) so this compiles:
```rust
fn check_origin(_headers: &HeaderMap) -> Result<(), Response> { Ok(()) }
fn check_protocol_version(_headers: &HeaderMap) -> Result<(), Response> { Ok(()) }
```

Add minimal `tools` stubs so `server.rs` compiles before Task 3/4. In `src/mcp/tools.rs`:
```rust
//! MCP tool registry, `tools/list`, and `tools/call` dispatch.

use serde_json::{Value, json};
use crate::mcp::protocol;
use crate::store::SharedState;

/// Server-level instructions injected at `initialize` (the agent loop).
pub const INSTRUCTIONS: &str = "Aniani MCP — local observability for your dev loop.";

pub fn list(id: Option<Value>) -> Value {
    protocol::success(id, json!({ "tools": [] }))
}

pub fn call(_state: &SharedState, id: Option<Value>, _params: &Value) -> Value {
    protocol::error(id, protocol::METHOD_NOT_FOUND, "no tools registered yet")
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::server`
Expected: PASS (3 tests). (Requires Task 1.1's `ingest_seq` field — do that first.)

- [ ] **Step 5: Commit**

```bash
git add src/mcp/server.rs src/mcp/tools.rs
git commit -m "mcp/server: JSON-RPC dispatch, ping, notification->202, batch rejection"
```

---

### Task 0.4: Origin validation (403) and protocol-version header handling (400)

**Files:**
- Modify: `src/mcp/server.rs`
- Test: in-file tests

- [ ] **Step 1: Write the failing test**

Add to `src/mcp/server.rs` tests:
```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::server`
Expected: FAIL — `is_allowed_origin` not found; `check_protocol_version` always Ok.

- [ ] **Step 3: Implement the checks**

Replace the placeholder `check_origin`/`check_protocol_version` in `src/mcp/server.rs`:
```rust
/// Validate `Origin` to defend against DNS-rebinding. Absent Origin (native
/// clients) is allowed; a present Origin must be a localhost variant.
fn check_origin(headers: &HeaderMap) -> Result<(), Response> {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !is_allowed_origin(origin) {
            return Err((StatusCode::FORBIDDEN, "origin not allowed").into_response());
        }
    }
    Ok(())
}

/// True if the Origin's host is a loopback address.
fn is_allowed_origin(origin: &str) -> bool {
    // Strip scheme.
    let after_scheme = origin.split("://").nth(1).unwrap_or(origin);
    // Host is up to the next '/' ; may include a port and IPv6 brackets.
    let hostport = after_scheme.split('/').next().unwrap_or("");
    let host = if let Some(end) = hostport.strip_prefix('[').and_then(|s| s.split(']').next()) {
        end.to_string() // IPv6 literal inside [...]
    } else {
        hostport.split(':').next().unwrap_or("").to_string()
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// Honor the `MCP-Protocol-Version` header: absent → default and proceed;
/// present but unsupported → 400.
fn check_protocol_version(headers: &HeaderMap) -> Result<(), Response> {
    match headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok()) {
        None => Ok(()), // default to protocol::DEFAULT_VERSION implicitly
        Some(v) if protocol::SUPPORTED_VERSIONS.contains(&v) => Ok(()),
        Some(_) => Err((StatusCode::BAD_REQUEST, "unsupported MCP-Protocol-Version").into_response()),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::server`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/server.rs
git commit -m "mcp/server: Origin validation and protocol-version header checks"
```

---

### Task 0.5: Mount `/mcp` on the shared listener with a small body limit

**Files:**
- Modify: `src/server.rs:71-83`, `src/main.rs:108-112`

- [ ] **Step 1: Write the failing test**

Create `tests/e2e_mcp.rs` with a first compliance test:
```rust
//! End-to-end MCP tests over a real listener.

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use aniani::config::Config;
use aniani::store::{AppState, LogStore, MetricStore, SharedState, TraceStore};
use clap::Parser;
use parking_lot::RwLock;

async fn spawn() -> String {
    let state: SharedState = Arc::new(AppState {
        log_store: RwLock::new(LogStore::new()),
        metric_store: RwLock::new(MetricStore::new()),
        trace_store: RwLock::new(TraceStore::new()),
        config: Config::parse_from(["aniani"]),
        start_time: Instant::now(),
        ingest_seq: AtomicU64::new(0),
    });
    let app = aniani::server::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn get_mcp_returns_405() {
    let base = spawn().await;
    let resp = reqwest::Client::new().get(format!("{base}/mcp")).send().await.unwrap();
    assert_eq!(resp.status(), 405);
}

#[tokio::test]
async fn initialize_handshake() {
    let base = spawn().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                      "params":{"protocolVersion":"2025-11-25","capabilities":{},
                                "clientInfo":{"name":"test","version":"0"}}}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(body["result"]["serverInfo"]["name"], "aniani");
    assert!(body["result"]["instructions"].is_string());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test e2e_mcp`
Expected: FAIL — `/mcp` not mounted (404), and `ingest_seq` field missing until Task 1.1.

- [ ] **Step 3: Mount the routes in `server.rs`**

In `src/server.rs`, change the tail of `build_router` (currently lines 71-83) to merge MCP before grpc and the fallback:
```rust
    let http = router
        .layer(DefaultBodyLimit::max(MAX_DECOMPRESSED_SIZE))
        .with_state(state.clone());

    // MCP gets its own small JSON body limit (not the 64 MiB OTLP limit).
    let mcp = crate::mcp::routes(state.clone())
        .layer(DefaultBodyLimit::max(1024 * 1024));

    // OTLP/gRPC shares the listener with HTTP/REST (cleartext HTTP/2). gRPC
    // routes are merged after `with_state`; reassert a 404 fallback so unknown
    // HTTP paths are not captured by tonic's gRPC "Unimplemented" catch-all.
    http.merge(mcp)
        .merge(crate::grpc::routes(state))
        .fallback(handle_not_found)
```

- [ ] **Step 4: Update the startup log in `main.rs`**

In `src/main.rs:111`, change:
```rust
    tracing::info!("aniani listening on {} (HTTP + OTLP/gRPC + MCP at /mcp)", addr);
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test e2e_mcp` (after Task 1.1 lands the field)
Expected: PASS — `get_mcp_returns_405`, `initialize_handshake`.

- [ ] **Step 6: Commit**

```bash
git add src/server.rs src/main.rs tests/e2e_mcp.rs
git commit -m "mcp: mount /mcp on shared listener with small body limit"
```

---

# Phase 1 — Ingest-sequence cursor (store foundation)

> Do this phase early; Phase 0 tests reference the `ingest_seq` field on `AppState`.

### Task 1.1: Add `ingest_seq` to `AppState` and the three entry types

**Files:**
- Modify: `src/store/mod.rs:24-34`, `src/store/log_store.rs:14-19`, `src/store/metric_store.rs:13-18`, `src/store/trace_store.rs:77-91`, `src/main.rs:54-60`

- [ ] **Step 1: Add the field to the three entry structs (with serde default)**

`src/store/log_store.rs:14-19`:
```rust
/// A single log entry with nanosecond timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp_ns: i64,
    pub line: String,
    /// Global monotonic ingest sequence; assigned on store insert.
    #[serde(default)]
    pub ingest_seq: u64,
}
```

`src/store/metric_store.rs:13-18` (Sample keeps `Copy` — `u64` is `Copy`):
```rust
/// A single metric sample.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sample {
    pub timestamp_ms: i64,
    pub value: f64,
    /// Global monotonic ingest sequence; assigned on store insert.
    #[serde(default)]
    pub ingest_seq: u64,
}
```

`src/store/trace_store.rs:77-91` — add `#[serde(default)] pub ingest_seq: u64,` as the final field of `Span`.

- [ ] **Step 2: Add the counter to `AppState`**

`src/store/mod.rs` — add the import and field. At the top imports add:
```rust
use std::sync::atomic::AtomicU64;
```
In the struct (lines 25-31):
```rust
pub struct AppState {
    pub log_store: RwLock<LogStore>,
    pub metric_store: RwLock<MetricStore>,
    pub trace_store: RwLock<TraceStore>,
    pub config: Config,
    pub start_time: Instant,
    /// Monotonic counter stamped onto every ingested entry/sample/span.
    pub ingest_seq: AtomicU64,
}
```

- [ ] **Step 3: Initialize the counter in `main.rs`**

`src/main.rs:54-60` — add `ingest_seq: AtomicU64::new(0),` to the `AppState { ... }` literal, and `use std::sync::atomic::AtomicU64;` at the top.

- [ ] **Step 4: Fix existing test literals that construct these types**

Add `ingest_seq: 0,` to each `LogEntry`/`Span` literal in existing tests:
- `src/store/log_store.rs:371-374` (`make_entry`), `:637-640`, `:646-653`.
- `src/store/trace_store.rs:458-471` (`make_span`), `:754-766`, `:767-779`, `:811-823`, `:824-836`.
- `Sample` literals in `src/store/metric_store.rs` tests (many; add `ingest_seq: 0,` to each — search the file for `Sample {`).

- [ ] **Step 5: Verify it compiles and existing tests pass**

Run: `cargo test -p aniani store::`
Expected: PASS (existing store tests unchanged in behavior).

- [ ] **Step 6: Commit**

```bash
git add src/store/ src/main.rs
git commit -m "store: add ingest_seq field to entries, samples, spans, and AppState"
```

---

### Task 1.2: Stamp `ingest_seq` at every ingestion construction site

**Files:**
- Modify: `src/ingest/otlp_logs.rs:87`, `src/ingest/loki.rs:177`, `src/ingest/otlp_metrics.rs` (8 sites), `src/ingest/remote_write.rs:120-127`, `src/ingest/otlp_traces.rs:43-56,212,282`

- [ ] **Step 1: Write the failing test**

Add to `src/ingest/otlp_logs.rs` test module (create one if absent):
```rust
#[cfg(test)]
mod ingest_seq_tests {
    use super::*;
    use crate::store::{AppState, LogStore, MetricStore, TraceStore};
    use clap::Parser;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    use opentelemetry_proto::tonic::common::v1::{AnyValue, any_value};

    fn state() -> Arc<AppState> {
        Arc::new(AppState {
            log_store: RwLock::new(LogStore::new()),
            metric_store: RwLock::new(MetricStore::new()),
            trace_store: RwLock::new(TraceStore::new()),
            config: crate::config::Config::parse_from(["aniani"]),
            start_time: Instant::now(),
            ingest_seq: AtomicU64::new(0),
        })
    }

    fn one_log(msg: &str) -> ExportLogsServiceRequest {
        ExportLogsServiceRequest { resource_logs: vec![ResourceLogs {
            resource: None,
            scope_logs: vec![ScopeLogs { scope: None, schema_url: String::new(),
                log_records: vec![LogRecord {
                    time_unix_nano: 1, observed_time_unix_nano: 0, severity_number: 9,
                    severity_text: "INFO".into(),
                    body: Some(AnyValue { value: Some(any_value::Value::StringValue(msg.into())) }),
                    attributes: vec![], dropped_attributes_count: 0, flags: 0,
                    trace_id: vec![], span_id: vec![], event_name: String::new(),
                }] }],
            schema_url: String::new() }] }
    }

    #[test]
    fn ingested_entries_carry_increasing_ingest_seq() {
        let st = state();
        ingest_logs(&st, one_log("a"));
        ingest_logs(&st, one_log("b"));
        // Counter advanced by exactly the number of ingested entries.
        assert_eq!(st.ingest_seq.load(Ordering::Relaxed), 2);
        let store = st.log_store.read();
        let mut seqs: Vec<u64> = store.streams.values()
            .flat_map(|s| s.entries.iter().map(|e| e.ingest_seq)).collect();
        seqs.sort();
        assert_eq!(seqs, vec![0, 1]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani ingest_seq_tests`
Expected: FAIL — entries all have `ingest_seq == 0`, counter stays 0.

- [ ] **Step 3: Stamp at each site**

The pattern: at each construction site, after building the value, set `ingest_seq` from `state.ingest_seq.fetch_add(1, Ordering::Relaxed)`. Add `use std::sync::atomic::Ordering;` to each modified file.

`src/ingest/otlp_logs.rs:87` — replace:
```rust
let entry = LogEntry { timestamp_ns, line };
```
with:
```rust
let entry = LogEntry {
    timestamp_ns,
    line,
    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
};
```

`src/ingest/loki.rs:177` — the closure has `state: &SharedState` in scope. Replace:
```rust
Some(LogEntry { timestamp_ns, line })
```
with:
```rust
Some(LogEntry {
    timestamp_ns,
    line,
    ingest_seq: state.ingest_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
})
```

`src/ingest/otlp_metrics.rs` — at all 8 `Sample { timestamp_ms, value }` literals (lines 112, 128, 156, 168, 177, 203, 214, 223), add the field. Each becomes, e.g.:
```rust
vec![Sample {
    timestamp_ms: ts_ms,
    value,
    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
}],
```
(Apply the same `ingest_seq:` line to all 8; the `value:` expression differs per site but the added field is identical.)

`src/ingest/remote_write.rs:120-127` — the handler has `state: SharedState` in scope. Replace the map:
```rust
.map(|s| Sample {
    timestamp_ms: s.timestamp,
    value: s.value,
    ingest_seq: state.ingest_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
})
```

`src/ingest/otlp_traces.rs` — add `ingest_seq: u64` to the internal `PreparedSpan` struct (lines 43-56), set it in the `PreparedSpan { ... }` literal (line 212) using `state.ingest_seq.fetch_add(1, Ordering::Relaxed)`, and forward it in the `Span { ... }` literal (line 282) as `ingest_seq: prepared.ingest_seq,`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani ingest_seq_tests`
Expected: PASS. Then `cargo test -p aniani` to confirm no regressions.

- [ ] **Step 5: Commit**

```bash
git add src/ingest/
git commit -m "ingest: stamp ingest_seq on all entries, samples, and spans"
```

---

### Task 1.3: Restore counter monotonicity after snapshot load

**Files:**
- Modify: `src/main.rs` (after snapshot restore), `src/store/mod.rs` (helper)

- [ ] **Step 1: Write the failing test**

Add to `src/store/mod.rs` tests:
```rust
#[cfg(test)]
mod ingest_seq_restore_tests {
    use super::*;

    #[test]
    fn max_ingest_seq_finds_highest_across_stores() {
        let mut logs = LogStore::new();
        logs.ingest_stream(
            vec![("service".into(), "a".into())],
            vec![crate::store::log_store::LogEntry { timestamp_ns: 1, line: "x".into(), ingest_seq: 41 }],
        );
        let metrics = MetricStore::new();
        let traces = TraceStore::new();
        assert_eq!(max_ingest_seq(&logs, &metrics, &traces), 41);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani ingest_seq_restore_tests`
Expected: FAIL — `max_ingest_seq` not found.

- [ ] **Step 3: Implement the helper and call it on restore**

Add to `src/store/mod.rs`:
```rust
/// Highest `ingest_seq` present across all three stores (0 if empty). Used to
/// re-seed the global counter after restoring a snapshot so new ingests stay
/// monotonic.
pub fn max_ingest_seq(logs: &LogStore, metrics: &MetricStore, traces: &TraceStore) -> u64 {
    let l = logs.streams.values()
        .flat_map(|s| s.entries.iter().map(|e| e.ingest_seq)).max().unwrap_or(0);
    let m = metrics.series.values()
        .flat_map(|s| s.samples.iter().map(|s| s.ingest_seq)).max().unwrap_or(0);
    let t = traces.traces.values()
        .flat_map(|v| v.iter().map(|s| s.ingest_seq)).max().unwrap_or(0);
    l.max(m).max(t)
}
```

In `src/main.rs`, after the stores are populated from a restored snapshot and before constructing `AppState`, compute the seed:
```rust
let seq_seed = aniani::store::max_ingest_seq(&log_store, &metric_store, &trace_store)
    .saturating_add(1);
```
and use it: `ingest_seq: AtomicU64::new(seq_seed),`. (On a cold boot with empty stores this is 1; harmless.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani ingest_seq_restore_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/store/mod.rs src/main.rs
git commit -m "store: re-seed ingest_seq counter after snapshot restore"
```

---

# Phase 2 — Transport-free typed cores

### Task 2.1: `summarize_activity` core (per-service triage, ingest-filtered)

**Files:**
- Modify: `src/mcp/synth.rs`
- Test: in-file tests

- [ ] **Step 1: Write the failing test**

In `src/mcp/synth.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{AppState, LogStore, MetricStore, TraceStore};
    use crate::store::log_store::LogEntry;
    use clap::Parser;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    fn state() -> Arc<AppState> {
        Arc::new(AppState {
            log_store: RwLock::new(LogStore::new()),
            metric_store: RwLock::new(MetricStore::new()),
            trace_store: RwLock::new(TraceStore::new()),
            config: crate::config::Config::parse_from(["aniani"]),
            start_time: Instant::now(),
            ingest_seq: AtomicU64::new(0),
        })
    }

    #[test]
    fn summarize_counts_errors_and_respects_since() {
        let st = state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "api".into()), ("level".into(), "error".into())],
                vec![
                    LogEntry { timestamp_ns: 10, line: "boom1".into(), ingest_seq: 0 },
                    LogEntry { timestamp_ns: 20, line: "boom2".into(), ingest_seq: 5 },
                ],
            );
        }
        // No `since`: both errors counted.
        let all = summarize_activity(&st, "api", None, false);
        assert_eq!(all.logs.error_count, 2);
        // since = 3: only the ingest_seq >= 3 entry counts.
        let since = summarize_activity(&st, "api", Some(3), false);
        assert_eq!(since.logs.error_count, 1);
        assert_eq!(since.service, "api");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::synth`
Expected: FAIL — `summarize_activity` not found.

- [ ] **Step 3: Implement the core and result types**

In `src/mcp/synth.rs` (above tests). This fuses the existing `diagnose_service` + `summary` logic but filters by `ingest_seq` and returns typed structs:
```rust
//! Transport-free typed cores backing the MCP tools.

use serde::Serialize;

use crate::store::trace_store::SpanStatus;
use crate::store::{LabelMatchOp, LabelMatcher, SharedState};

const DEFAULT_TOP: usize = 20;

/// Per-service cross-signal activity summary.
#[derive(Debug, Serialize)]
pub struct ServiceActivity {
    pub service: String,
    pub since: Option<u64>,
    pub observed_through: u64,
    pub health_score: f64,
    pub summary: String,
    pub logs: LogsBlock,
    pub traces: TracesBlock,
    pub truncated: Truncated,
}

#[derive(Debug, Serialize)]
pub struct LogsBlock {
    pub error_count: usize,
    pub top: Vec<LogItem>,
}
#[derive(Debug, Serialize)]
pub struct LogItem { pub ts: String, pub line: String }
#[derive(Debug, Serialize)]
pub struct TracesBlock {
    pub error_or_slow_count: usize,
    pub notable: Vec<TraceItem>,
}
#[derive(Debug, Serialize)]
pub struct TraceItem {
    pub trace_id: String,
    pub root_span_name: String,
    pub duration_ms: f64,
    pub error_span_count: usize,
}
#[derive(Debug, Serialize, Default)]
pub struct Truncated { pub logs: bool, pub traces: bool }

fn svc_error_matchers(service: &str) -> Vec<LabelMatcher> {
    vec![
        LabelMatcher { name: "service".into(), op: LabelMatchOp::Eq, value: service.to_string() },
        LabelMatcher { name: "level".into(), op: LabelMatchOp::Eq, value: "error".into() },
    ]
}

/// Summarize a single service's activity, optionally only entries ingested at or
/// after `since` (an ingest-sequence token). `detail` reserved for future
/// verbosity control.
pub fn summarize_activity(
    state: &SharedState,
    service: &str,
    since: Option<u64>,
    _detail: bool,
) -> ServiceActivity {
    let observed_through = state.ingest_seq.load(std::sync::atomic::Ordering::Relaxed);
    let keep = |seq: u64| since.map(|s| seq >= s).unwrap_or(true);

    // --- Logs (error streams) ---
    let (error_count, mut error_items) = {
        let store = state.log_store.read();
        let mut items: Vec<(i64, String)> = Vec::new();
        for sid in store.query_streams(&svc_error_matchers(service)) {
            for e in store.get_entries(sid, i64::MIN, i64::MAX) {
                if keep(e.ingest_seq) {
                    items.push((e.timestamp_ns, e.line.clone()));
                }
            }
        }
        (items.len(), items)
    };
    error_items.sort_by(|a, b| b.0.cmp(&a.0));
    let logs_truncated = error_items.len() > DEFAULT_TOP;
    let top: Vec<LogItem> = error_items.into_iter().take(DEFAULT_TOP)
        .map(|(ts, line)| LogItem { ts: ts.to_string(), line }).collect();

    // --- Traces (error / notable) ---
    let (notable, error_or_slow_count, traces_truncated, span_error_ratio) = {
        let store = state.trace_store.read();
        let mut notable: Vec<TraceItem> = Vec::new();
        let mut total_spans = 0usize;
        let mut error_spans = 0usize;
        for tid in store.traces_for_service(service) {
            if let Some(spans) = store.get_trace(&tid) {
                let in_window = spans.iter().any(|s| keep(s.ingest_seq));
                if !in_window { continue; }
                let errs = spans.iter().filter(|s| s.status == SpanStatus::Error).count();
                total_spans += spans.len();
                error_spans += errs;
                if errs > 0 {
                    if let Some(r) = store.trace_result(&tid) {
                        notable.push(TraceItem {
                            trace_id: tid.iter().map(|b| format!("{b:02x}")).collect(),
                            root_span_name: r.root_span_name,
                            duration_ms: r.duration_ns as f64 / 1_000_000.0,
                            error_span_count: errs,
                        });
                    }
                }
            }
        }
        notable.sort_by(|a, b| b.duration_ms.partial_cmp(&a.duration_ms).unwrap_or(std::cmp::Ordering::Equal));
        let count = notable.len();
        let truncated = notable.len() > DEFAULT_TOP;
        notable.truncate(DEFAULT_TOP);
        let ratio = if total_spans > 0 { error_spans as f64 / total_spans as f64 } else { 0.0 };
        (notable, count, truncated, ratio)
    };

    let mut health = 100.0_f64;
    health -= (span_error_ratio * 30.0).min(30.0);
    if error_count > 0 { health -= (error_count as f64).min(40.0); }
    let health_score = health.clamp(0.0, 100.0);

    let summary = format!(
        "{error_count} error log(s), {error_or_slow_count} failing trace(s), health {health_score:.0}"
    );

    ServiceActivity {
        service: service.to_string(),
        since,
        observed_through,
        health_score,
        summary,
        logs: LogsBlock { error_count, top },
        traces: TracesBlock { error_or_slow_count, notable },
        truncated: Truncated { logs: logs_truncated, traces: traces_truncated },
    }
}
```

> Lock discipline (per spec §9): each store is read in its own short block; data is copied out, then computation happens after the lock is dropped. No `.await` under any lock.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::synth`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/synth.rs
git commit -m "mcp/synth: summarize_activity core with ingest-seq filtering"
```

---

### Task 2.2: `check_health` core (global ranked overview)

**Files:**
- Modify: `src/mcp/synth.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/mcp/synth.rs` tests:
```rust
    #[test]
    fn check_health_lists_services_worst_first() {
        let st = state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "healthy".into())],
                vec![LogEntry { timestamp_ns: 1, line: "ok".into(), ingest_seq: 0 }],
            );
            logs.ingest_stream(
                vec![("service".into(), "broken".into()), ("level".into(), "error".into())],
                vec![LogEntry { timestamp_ns: 2, line: "err".into(), ingest_seq: 1 }],
            );
        }
        let health = check_health(&st);
        assert!(!health.services.is_empty());
        // Worst (lowest score) first.
        let scores: Vec<f64> = health.services.iter().map(|s| s.health_score).collect();
        assert!(scores.windows(2).all(|w| w[0] <= w[1]));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::synth::tests::check_health`
Expected: FAIL — `check_health` not found.

- [ ] **Step 3: Implement**

Add to `src/mcp/synth.rs`:
```rust
use rustc_hash::FxHashSet;

#[derive(Debug, Serialize)]
pub struct HealthOverview { pub services: Vec<ServiceHealth> }
#[derive(Debug, Serialize)]
pub struct ServiceHealth { pub service: String, pub health_score: f64, pub top_issue: String }

/// Global health overview: every known service ranked worst-first.
pub fn check_health(state: &SharedState) -> HealthOverview {
    let mut names: FxHashSet<String> = FxHashSet::default();
    {
        let s = state.log_store.read();
        for n in s.get_label_values("service") { names.insert(n); }
    }
    {
        let s = state.metric_store.read();
        for n in s.get_label_values("service") { names.insert(n); }
    }
    {
        let s = state.trace_store.read();
        for n in s.service_names() { names.insert(n); }
    }
    let mut services: Vec<ServiceHealth> = names.into_iter().map(|name| {
        let a = summarize_activity(state, &name, None, false);
        let top_issue = if a.logs.error_count > 0 {
            format!("{} error log(s)", a.logs.error_count)
        } else if a.traces.error_or_slow_count > 0 {
            format!("{} failing trace(s)", a.traces.error_or_slow_count)
        } else {
            "No issues detected".to_string()
        };
        ServiceHealth { service: name, health_score: a.health_score, top_issue }
    }).collect();
    services.sort_by(|a, b| a.health_score.partial_cmp(&b.health_score).unwrap_or(std::cmp::Ordering::Equal));
    HealthOverview { services }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::synth::tests::check_health`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/synth.rs
git commit -m "mcp/synth: check_health global ranked overview core"
```

---

### Task 2.3: `describe_service` enriched catalog core

**Files:**
- Modify: `src/mcp/synth.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/mcp/synth.rs` tests:
```rust
    #[test]
    fn describe_service_reports_log_labels_and_values() {
        let st = state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "api".into()), ("level".into(), "error".into())],
                vec![LogEntry { timestamp_ns: 1, line: "x".into(), ingest_seq: 0 }],
            );
        }
        let cat = describe_service(&st, "api");
        assert!(cat.log_labels.iter().any(|l| l.key == "level"));
        let level = cat.log_labels.iter().find(|l| l.key == "level").unwrap();
        assert!(level.values.contains(&"error".to_string()));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::synth::tests::describe_service`
Expected: FAIL — `describe_service` not found.

- [ ] **Step 3: Implement**

Add to `src/mcp/synth.rs`. Enriches the existing catalog with label *values* (capped) and counts:
```rust
const MAX_LABEL_VALUES: usize = 50;

#[derive(Debug, Serialize)]
pub struct ServiceCatalog {
    pub service: String,
    pub metrics: Vec<String>,
    pub log_labels: Vec<LabelInfo>,
    pub span_attributes: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct LabelInfo { pub key: String, pub values: Vec<String>, pub truncated: bool }

/// Enriched per-service catalog: metric names, log label keys WITH capped value
/// lists, and span attribute keys — the "discover before you query" grounding.
pub fn describe_service(state: &SharedState, service: &str) -> ServiceCatalog {
    // Metric names for the service.
    let metrics = {
        let store = state.metric_store.read();
        let matchers = vec![LabelMatcher {
            name: "service".into(), op: LabelMatchOp::Eq, value: service.to_string(),
        }];
        let name_key = store.interner.get("__name__");
        let mut names: Vec<String> = Vec::new();
        let mut seen = FxHashSet::default();
        for sid in store.select_series(&matchers) {
            if let Some(series) = store.series.get(&sid) {
                if let Some(nk) = name_key {
                    if let Some((_, v)) = series.labels.iter().find(|(k, _)| *k == nk) {
                        let n = store.interner.resolve(v).to_string();
                        if seen.insert(n.clone()) { names.push(n); }
                    }
                }
            }
        }
        names.sort();
        names
    };

    // Log label keys + capped values for the service's streams.
    let log_labels = {
        let store = state.log_store.read();
        let matchers = vec![LabelMatcher {
            name: "service".into(), op: LabelMatchOp::Eq, value: service.to_string(),
        }];
        let mut by_key: std::collections::BTreeMap<String, FxHashSet<String>> = Default::default();
        for sid in store.query_streams(&matchers) {
            if let Some(labels) = store.get_stream_labels(sid) {
                for (k, v) in labels { by_key.entry(k).or_default().insert(v); }
            }
        }
        by_key.into_iter().map(|(key, vals)| {
            let mut values: Vec<String> = vals.into_iter().collect();
            values.sort();
            let truncated = values.len() > MAX_LABEL_VALUES;
            values.truncate(MAX_LABEL_VALUES);
            LabelInfo { key, values, truncated }
        }).collect()
    };

    // Span attribute keys for the service.
    let span_attributes = {
        let store = state.trace_store.read();
        let mut keys: Vec<String> = Vec::new();
        let mut seen = FxHashSet::default();
        if let Some(spur) = store.interner.get(service) {
            if let Some(trace_ids) = store.service_index.get(&spur) {
                for tid in trace_ids {
                    if let Some(spans) = store.traces.get(tid) {
                        for span in spans {
                            if span.service_name == spur {
                                for (k, _) in &span.attributes {
                                    let key = store.interner.resolve(k).to_string();
                                    if seen.insert(key.clone()) { keys.push(key); }
                                }
                            }
                        }
                    }
                }
            }
        }
        keys.sort();
        keys
    };

    ServiceCatalog { service: service.to_string(), metrics, log_labels, span_attributes }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::synth::tests::describe_service`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/synth.rs
git commit -m "mcp/synth: enriched describe_service catalog core"
```

---

### Task 2.4: `build_trace_tree` core (real parent/child nesting)

**Files:**
- Modify: `src/mcp/synth.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/mcp/synth.rs` tests:
```rust
    #[test]
    fn build_trace_tree_nests_children_under_parents() {
        use crate::store::trace_store::{Span, SpanKind, SpanStatus};
        let st = state();
        let tid = [1u8; 16];
        let root_id = [10u8; 8];
        let child_id = [11u8; 8];
        {
            let mut traces = st.trace_store.write();
            let mut interner_name;
            let mut interner_svc;
            {
                interner_name = traces.interner.get_or_intern("root");
                interner_svc = traces.interner.get_or_intern("api");
            }
            let root = Span {
                trace_id: tid, span_id: root_id, parent_span_id: None,
                name: interner_name, service_name: interner_svc,
                start_time_ns: 0, duration_ns: 100, status: SpanStatus::Ok,
                kind: SpanKind::Server, attributes: Default::default(), events: vec![], ingest_seq: 0,
            };
            let cname = traces.interner.get_or_intern("child");
            let child = Span {
                trace_id: tid, span_id: child_id, parent_span_id: Some(root_id),
                name: cname, service_name: interner_svc,
                start_time_ns: 10, duration_ns: 30, status: SpanStatus::Ok,
                kind: SpanKind::Internal, attributes: Default::default(), events: vec![], ingest_seq: 1,
            };
            traces.ingest_spans(vec![root, child]);
        }
        let tree = build_trace_tree(&st, &tid).expect("trace exists");
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].name, "root");
        assert_eq!(tree.roots[0].children.len(), 1);
        assert_eq!(tree.roots[0].children[0].name, "child");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::synth::tests::build_trace_tree`
Expected: FAIL — `build_trace_tree` not found.

- [ ] **Step 3: Implement**

Add to `src/mcp/synth.rs`:
```rust
#[derive(Debug, Serialize)]
pub struct TraceTree { pub trace_id: String, pub roots: Vec<SpanNode> }
#[derive(Debug, Serialize)]
pub struct SpanNode {
    pub span_id: String,
    pub name: String,
    pub service: String,
    pub start_time_ns: i64,
    pub duration_ms: f64,
    pub status: String,
    pub children: Vec<SpanNode>,
}

/// Build a parent/child span tree for one trace. Spans whose parent is absent
/// (or has no parent) become roots.
pub fn build_trace_tree(state: &SharedState, trace_id: &[u8; 16]) -> Option<TraceTree> {
    let store = state.trace_store.read();
    let spans = store.get_trace(trace_id)?;

    // children_of[parent_span_id] = Vec<index into spans>
    use std::collections::HashMap;
    let present: std::collections::HashSet<[u8; 8]> = spans.iter().map(|s| s.span_id).collect();
    let mut children_of: HashMap<[u8; 8], Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        match s.parent_span_id {
            Some(p) if present.contains(&p) => children_of.entry(p).or_default().push(i),
            _ => roots.push(i),
        }
    }

    fn node(
        idx: usize,
        spans: &[crate::store::trace_store::Span],
        children_of: &std::collections::HashMap<[u8; 8], Vec<usize>>,
        store: &crate::store::TraceStore,
    ) -> SpanNode {
        let s = &spans[idx];
        let mut children: Vec<SpanNode> = children_of.get(&s.span_id)
            .map(|kids| kids.iter().map(|&c| node(c, spans, children_of, store)).collect())
            .unwrap_or_default();
        children.sort_by_key(|c| c.start_time_ns);
        SpanNode {
            span_id: s.span_id.iter().map(|b| format!("{b:02x}")).collect(),
            name: store.resolve(&s.name).to_string(),
            service: store.resolve(&s.service_name).to_string(),
            start_time_ns: s.start_time_ns,
            duration_ms: s.duration_ns as f64 / 1_000_000.0,
            status: match s.status {
                crate::store::trace_store::SpanStatus::Error => "error",
                crate::store::trace_store::SpanStatus::Ok => "ok",
                crate::store::trace_store::SpanStatus::Unset => "unset",
            }.to_string(),
            children,
        }
    }

    let mut root_nodes: Vec<SpanNode> = roots.iter().map(|&r| node(r, spans, &children_of, &store)).collect();
    root_nodes.sort_by_key(|n| n.start_time_ns);
    Some(TraceTree {
        trace_id: trace_id.iter().map(|b| format!("{b:02x}")).collect(),
        roots: root_nodes,
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::synth::tests::build_trace_tree`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/synth.rs
git commit -m "mcp/synth: build_trace_tree parent/child span tree core"
```

---

# Phase 3 — Tool registry and `tools/list`

### Task 3.1: Tool descriptors, annotations, and `tools/list`

**Files:**
- Modify: `src/mcp/tools.rs`
- Test: in-file tests

- [ ] **Step 1: Write the failing test**

Add to `src/mcp/tools.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tools_list_contains_all_ten_with_annotations() {
        let resp = list(Some(json!(1)));
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 10);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in [
            "reset","mark_checkpoint","summarize_activity","check_health",
            "query_logs","query_traces","query_metrics","get_trace",
            "list_services","describe_service",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        let reset = tools.iter().find(|t| t["name"] == "reset").unwrap();
        assert_eq!(reset["annotations"]["destructiveHint"], json!(true));
        assert_eq!(reset["annotations"]["readOnlyHint"], json!(false));
        let logs = tools.iter().find(|t| t["name"] == "query_logs").unwrap();
        assert_eq!(logs["annotations"]["readOnlyHint"], json!(true));
        // Closed in-memory domain.
        assert_eq!(logs["annotations"]["openWorldHint"], json!(false));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::tools::tests::tools_list`
Expected: FAIL — `list` returns an empty array.

- [ ] **Step 3: Implement descriptors and `list`**

Replace the stub `list` in `src/mcp/tools.rs` with a registry. Define a helper and the 10 descriptors:
```rust
use serde_json::{Value, json};

/// One read-only tool annotation block (closed in-memory domain).
fn read_only(title: &str) -> Value {
    json!({ "title": title, "readOnlyHint": true, "openWorldHint": false, "idempotentHint": true })
}

/// Build the static list of tool descriptors.
fn descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "reset",
            "title": "Reset Telemetry Store",
            "description": "Clear telemetry. scope='all' wipes everything; scope='service' clears one service (requires `service`). The only write tool. Returns a fresh checkpoint token.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "enum": ["all", "service"] },
                    "service": { "type": "string" }
                },
                "required": ["scope"]
            },
            "annotations": { "title": "Reset Telemetry Store", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "mark_checkpoint",
            "title": "Mark Checkpoint",
            "description": "Return an opaque monotonic checkpoint token for 'now'. Pass it later as `since` to scope a summary to telemetry ingested after this point. Not a wall-clock time.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only("Mark Checkpoint")
        }),
        json!({
            "name": "summarize_activity",
            "title": "Summarize Service Activity",
            "description": "Triage one service: error logs, failing/slow traces, health score, one-line summary. Use after a run to see what it produced. Pass `since` (a checkpoint token) to scope to the latest run. Use this BEFORE drilling in with query_* tools.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "service": { "type": "string" },
                    "since": { "type": "integer" },
                    "detail": { "type": "string", "enum": ["concise", "detailed"] }
                },
                "required": ["service"]
            },
            "annotations": read_only("Summarize Service Activity")
        }),
        json!({
            "name": "check_health",
            "title": "Check Global Health",
            "description": "Rank every known service by health, worst first. Use when you don't yet know which service is in trouble.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only("Check Global Health")
        }),
        json!({
            "name": "query_logs",
            "title": "Query Logs",
            "description": "Fetch log lines. Provide structured filters (service, level, contains) OR a raw `logql` string (e.g. {service=\"api\"} |= \"error\"). If `logql` is set, structured filters are ignored. `since` is a checkpoint token.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "service": { "type": "string" }, "level": { "type": "string" },
                    "contains": { "type": "string" }, "since": { "type": "integer" },
                    "limit": { "type": "integer" }, "logql": { "type": "string" }
                }
            },
            "annotations": read_only("Query Logs")
        }),
        json!({
            "name": "query_traces",
            "title": "Query Traces",
            "description": "Find traces. Provide structured filters (service, name, status, min_duration) OR a raw `traceql` string. If `traceql` is set, structured filters are ignored.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "service": { "type": "string" }, "name": { "type": "string" },
                    "status": { "type": "string" }, "min_duration": { "type": "string" },
                    "since": { "type": "integer" }, "limit": { "type": "integer" },
                    "traceql": { "type": "string" }
                }
            },
            "annotations": read_only("Query Traces")
        }),
        json!({
            "name": "query_metrics",
            "title": "Query Metrics",
            "description": "Run a raw PromQL query (e.g. rate(http_requests_total[5m])). Optional start/end/step (RFC3339 or unix seconds) for a range query; omit for an instant query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "promql": { "type": "string" },
                    "start": { "type": "string" }, "end": { "type": "string" }, "step": { "type": "string" }
                },
                "required": ["promql"]
            },
            "annotations": read_only("Query Metrics")
        }),
        json!({
            "name": "get_trace",
            "title": "Get Trace Tree",
            "description": "Return one trace as a parent/child span tree. `trace_id` is 32 hex chars. Use `detail=detailed` to include span attributes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "trace_id": { "type": "string" },
                    "detail": { "type": "string", "enum": ["concise", "detailed"] }
                },
                "required": ["trace_id"]
            },
            "annotations": read_only("Get Trace Tree")
        }),
        json!({
            "name": "list_services",
            "title": "List Services",
            "description": "List every service reporting telemetry and which signals (logs/metrics/traces) each has.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only("List Services")
        }),
        json!({
            "name": "describe_service",
            "title": "Describe Service",
            "description": "Catalog a service's queryable surface: metric names, log label keys + values, span attribute keys. Call this before writing a raw logql/promql/traceql query.",
            "inputSchema": {
                "type": "object",
                "properties": { "service": { "type": "string" } },
                "required": ["service"]
            },
            "annotations": read_only("Describe Service")
        }),
    ]
}

pub fn list(id: Option<Value>) -> Value {
    protocol::success(id, json!({ "tools": descriptors() }))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::tools::tests::tools_list`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/tools.rs
git commit -m "mcp/tools: tool descriptors, annotations, and tools/list"
```

---

# Phase 4 — `tools/call` handlers

Each handler returns a `tools/call` result value via a shared helper. Add these helpers to `src/mcp/tools.rs` first.

### Task 4.0: `tools/call` dispatch + result helpers

**Files:**
- Modify: `src/mcp/tools.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/mcp/tools.rs` tests:
```rust
    #[test]
    fn call_unknown_tool_is_invalid_params() {
        let st = super::tests_state();
        let resp = call(&st, Some(json!(1)), &json!({ "name": "nope", "arguments": {} }));
        assert_eq!(resp["error"]["code"], json!(crate::mcp::protocol::INVALID_PARAMS));
    }
```
And a shared test-state helper at the bottom of the tests module:
```rust
    fn tests_state() -> crate::store::SharedState {
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::tools::tests::call_unknown_tool`
Expected: FAIL — `call` is a stub returning METHOD_NOT_FOUND, not INVALID_PARAMS for an unknown tool.

- [ ] **Step 3: Implement dispatch + helpers**

Replace the stub `call` in `src/mcp/tools.rs`:
```rust
use crate::store::SharedState;

/// Build a successful `tools/call` result carrying both text and structured content.
fn tool_ok(id: Option<Value>, structured: Value, text: String) -> Value {
    protocol::success(id, json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": structured,
        "isError": false
    }))
}

/// Build a `tools/call` execution-error result (model-visible, self-correcting).
fn tool_err(id: Option<Value>, message: String) -> Value {
    protocol::success(id, json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true
    }))
}

pub fn call(state: &SharedState, id: Option<Value>, params: &Value) -> Value {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return protocol::error(id, protocol::INVALID_PARAMS, "missing tool name"),
    };
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "reset" => handle_reset(state, id, &args),
        "mark_checkpoint" => handle_mark_checkpoint(state, id),
        "summarize_activity" => handle_summarize_activity(state, id, &args),
        "check_health" => handle_check_health(state, id),
        "query_logs" => handle_query_logs(state, id, &args),
        "query_traces" => handle_query_traces(state, id, &args),
        "query_metrics" => handle_query_metrics(state, id, &args),
        "get_trace" => handle_get_trace(state, id, &args),
        "list_services" => handle_list_services(state, id),
        "describe_service" => handle_describe_service(state, id, &args),
        _ => protocol::error(id, protocol::INVALID_PARAMS, "unknown tool"),
    }
}
```

Add stub handlers so it compiles (each replaced by its task below):
```rust
fn handle_reset(_s:&SharedState,id:Option<Value>,_a:&Value)->Value{tool_err(id,"todo".into())}
fn handle_mark_checkpoint(_s:&SharedState,id:Option<Value>)->Value{tool_err(id,"todo".into())}
fn handle_summarize_activity(_s:&SharedState,id:Option<Value>,_a:&Value)->Value{tool_err(id,"todo".into())}
fn handle_check_health(_s:&SharedState,id:Option<Value>)->Value{tool_err(id,"todo".into())}
fn handle_query_logs(_s:&SharedState,id:Option<Value>,_a:&Value)->Value{tool_err(id,"todo".into())}
fn handle_query_traces(_s:&SharedState,id:Option<Value>,_a:&Value)->Value{tool_err(id,"todo".into())}
fn handle_query_metrics(_s:&SharedState,id:Option<Value>,_a:&Value)->Value{tool_err(id,"todo".into())}
fn handle_get_trace(_s:&SharedState,id:Option<Value>,_a:&Value)->Value{tool_err(id,"todo".into())}
fn handle_list_services(_s:&SharedState,id:Option<Value>)->Value{tool_err(id,"todo".into())}
fn handle_describe_service(_s:&SharedState,id:Option<Value>,_a:&Value)->Value{tool_err(id,"todo".into())}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::tools::tests::call_unknown_tool`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/tools.rs
git commit -m "mcp/tools: tools/call dispatch and result helpers"
```

---

### Task 4.1: `reset` handler (explicit scope, the only write)

**Files:**
- Modify: `src/mcp/tools.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/mcp/tools.rs` tests:
```rust
    #[test]
    fn reset_all_clears_and_returns_checkpoint() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(vec![("service".into(), "api".into())],
                vec![crate::store::log_store::LogEntry { timestamp_ns: 1, line: "x".into(), ingest_seq: 0 }]);
        }
        let resp = call(&st, Some(json!(1)), &json!({ "name": "reset", "arguments": { "scope": "all" } }));
        assert_eq!(resp["result"]["isError"], json!(false));
        assert!(resp["result"]["structuredContent"]["checkpoint"].is_u64());
        assert_eq!(st.log_store.read().streams.len(), 0);
    }

    #[test]
    fn reset_service_scope_requires_service() {
        let st = tests_state();
        let resp = call(&st, Some(json!(1)), &json!({ "name": "reset", "arguments": { "scope": "service" } }));
        assert_eq!(resp["result"]["isError"], json!(true));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aniani mcp::tools::tests::reset`
Expected: FAIL — stub returns "todo".

- [ ] **Step 3: Implement**

Replace `handle_reset` in `src/mcp/tools.rs`:
```rust
fn handle_reset(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let scope = args.get("scope").and_then(|v| v.as_str());
    match scope {
        Some("all") => {
            state.log_store.write().clear();
            state.metric_store.write().clear();
            state.trace_store.write().clear();
        }
        Some("service") => {
            let service = match args.get("service").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => return tool_err(id, "scope='service' requires a non-empty `service`".into()),
            };
            state.log_store.write().clear_service(&service);
            state.metric_store.write().clear_service(&service);
            state.trace_store.write().clear_service(&service);
        }
        _ => return tool_err(id, "scope must be 'all' or 'service'".into()),
    }
    let checkpoint = state.ingest_seq.load(std::sync::atomic::Ordering::Relaxed);
    let structured = json!({ "scope": scope, "checkpoint": checkpoint });
    tool_ok(id, structured, format!("reset complete; checkpoint={checkpoint}"))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aniani mcp::tools::tests::reset`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/tools.rs
git commit -m "mcp/tools: reset handler with explicit scope"
```

---

### Task 4.2: `mark_checkpoint` handler

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn mark_checkpoint_returns_current_seq() {
        let st = tests_state();
        st.ingest_seq.store(7, std::sync::atomic::Ordering::Relaxed);
        let resp = call(&st, Some(json!(1)), &json!({ "name": "mark_checkpoint", "arguments": {} }));
        assert_eq!(resp["result"]["structuredContent"]["checkpoint"], json!(7));
    }
```

- [ ] **Step 2: Run** `cargo test -p aniani mcp::tools::tests::mark_checkpoint` → FAIL (stub).

- [ ] **Step 3: Implement**

```rust
fn handle_mark_checkpoint(state: &SharedState, id: Option<Value>) -> Value {
    let checkpoint = state.ingest_seq.load(std::sync::atomic::Ordering::Relaxed);
    tool_ok(id, json!({ "checkpoint": checkpoint }),
        format!("checkpoint={checkpoint} — pass as `since` to scope a later summary"))
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: mark_checkpoint handler"`

---

### Task 4.3: `summarize_activity` handler

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn summarize_activity_tool_reports_errors() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "api".into()), ("level".into(), "error".into())],
                vec![crate::store::log_store::LogEntry { timestamp_ns: 1, line: "boom".into(), ingest_seq: 0 }]);
        }
        let resp = call(&st, Some(json!(1)),
            &json!({ "name": "summarize_activity", "arguments": { "service": "api" } }));
        assert_eq!(resp["result"]["structuredContent"]["logs"]["error_count"], json!(1));
    }

    #[test]
    fn summarize_activity_requires_service() {
        let st = tests_state();
        let resp = call(&st, Some(json!(1)), &json!({ "name": "summarize_activity", "arguments": {} }));
        assert_eq!(resp["result"]["isError"], json!(true));
    }
```

- [ ] **Step 2: Run** → FAIL (stub).

- [ ] **Step 3: Implement**

```rust
use crate::mcp::synth;

fn handle_summarize_activity(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let service = match args.get("service").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return tool_err(id, "`service` is required".into()),
    };
    let since = args.get("since").and_then(|v| v.as_u64());
    let detail = args.get("detail").and_then(|v| v.as_str()) == Some("detailed");
    let activity = synth::summarize_activity(state, &service, since, detail);
    let text = activity.summary.clone();
    match serde_json::to_value(&activity) {
        Ok(v) => tool_ok(id, v, text),
        Err(e) => tool_err(id, format!("serialization error: {e}")),
    }
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: summarize_activity handler"`

---

### Task 4.4: `check_health` handler

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn check_health_tool_returns_services_array() {
        let st = tests_state();
        let resp = call(&st, Some(json!(1)), &json!({ "name": "check_health", "arguments": {} }));
        assert!(resp["result"]["structuredContent"]["services"].is_array());
    }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
fn handle_check_health(state: &SharedState, id: Option<Value>) -> Value {
    let overview = synth::check_health(state);
    let text = format!("{} service(s) ranked worst-first", overview.services.len());
    match serde_json::to_value(&overview) {
        Ok(v) => tool_ok(id, v, text),
        Err(e) => tool_err(id, format!("serialization error: {e}")),
    }
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: check_health handler"`

---

### Task 4.5: `list_services` handler

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn list_services_tool_includes_signals() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(vec![("service".into(), "api".into())],
                vec![crate::store::log_store::LogEntry { timestamp_ns: 1, line: "x".into(), ingest_seq: 0 }]);
        }
        let resp = call(&st, Some(json!(1)), &json!({ "name": "list_services", "arguments": {} }));
        let services = resp["result"]["structuredContent"]["services"].as_array().unwrap();
        assert!(services.iter().any(|s| s["name"] == "api" &&
            s["signals"].as_array().unwrap().iter().any(|x| x == "logs")));
    }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** (reuses store label values across signals):

```rust
fn handle_list_services(state: &SharedState, id: Option<Value>) -> Value {
    use rustc_hash::{FxHashMap, FxHashSet};
    let mut sig: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();
    for n in state.log_store.read().get_label_values("service") { sig.entry(n).or_default().insert("logs"); }
    for n in state.metric_store.read().get_label_values("service") { sig.entry(n).or_default().insert("metrics"); }
    for n in state.trace_store.read().service_names() { sig.entry(n).or_default().insert("traces"); }
    let mut services: Vec<Value> = sig.into_iter().map(|(name, s)| {
        let mut sigs: Vec<&str> = s.into_iter().collect();
        sigs.sort();
        json!({ "name": name, "signals": sigs })
    }).collect();
    services.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));
    let text = format!("{} service(s)", services.len());
    tool_ok(id, json!({ "services": services }), text)
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: list_services handler"`

---

### Task 4.6: `describe_service` handler

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn describe_service_tool_requires_service_and_returns_catalog() {
        let st = tests_state();
        let missing = call(&st, Some(json!(1)), &json!({ "name": "describe_service", "arguments": {} }));
        assert_eq!(missing["result"]["isError"], json!(true));
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(vec![("service".into(), "api".into()), ("level".into(), "error".into())],
                vec![crate::store::log_store::LogEntry { timestamp_ns: 1, line: "x".into(), ingest_seq: 0 }]);
        }
        let ok = call(&st, Some(json!(1)), &json!({ "name": "describe_service", "arguments": { "service": "api" } }));
        assert!(ok["result"]["structuredContent"]["log_labels"].is_array());
    }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
fn handle_describe_service(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let service = match args.get("service").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return tool_err(id, "`service` is required".into()),
    };
    let cat = synth::describe_service(state, &service);
    let text = format!("{} metric(s), {} log label key(s), {} span attr key(s)",
        cat.metrics.len(), cat.log_labels.len(), cat.span_attributes.len());
    match serde_json::to_value(&cat) {
        Ok(v) => tool_ok(id, v, text),
        Err(e) => tool_err(id, format!("serialization error: {e}")),
    }
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: describe_service handler"`

---

### Task 4.7: `query_logs` handler (structured + raw LogQL, parse errors → isError)

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn query_logs_structured_and_bad_logql() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(vec![("service".into(), "api".into())],
                vec![crate::store::log_store::LogEntry { timestamp_ns: 5, line: "hello".into(), ingest_seq: 0 }]);
        }
        let ok = call(&st, Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "service": "api" } }));
        assert_eq!(ok["result"]["isError"], json!(false));
        assert!(ok["result"]["structuredContent"]["logs"].is_array());

        let bad = call(&st, Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "logql": "{unterminated" } }));
        assert_eq!(bad["result"]["isError"], json!(true));
    }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** (build LogQL from structured params, or use raw `logql`; evaluate via `evaluate_logql_limited`):

```rust
use crate::query::logql::eval::{evaluate_logql_limited, LogQLResult};
use crate::query::logql::parser::parse_logql;

fn handle_query_logs(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).min(100) as usize;
    let since = args.get("since").and_then(|v| v.as_u64());

    // Build the LogQL string: raw escape hatch wins over structured filters.
    let query = if let Some(raw) = args.get("logql").and_then(|v| v.as_str()) {
        raw.to_string()
    } else {
        let mut sel = Vec::new();
        if let Some(s) = args.get("service").and_then(|v| v.as_str()) { sel.push(format!("service=\"{s}\"")); }
        if let Some(l) = args.get("level").and_then(|v| v.as_str()) { sel.push(format!("level=\"{l}\"")); }
        if sel.is_empty() {
            return tool_err(id, "provide at least one of service/level/contains, or a raw `logql`".into());
        }
        let mut q = format!("{{{}}}", sel.join(", "));
        if let Some(c) = args.get("contains").and_then(|v| v.as_str()) { q.push_str(&format!(" |= \"{c}\"")); }
        q
    };

    let expr = match parse_logql(&query) {
        Ok(e) => e,
        Err(e) => return tool_err(id, format!(
            "LogQL parse error: {e}. Example: {{service=\"api\"}} |= \"error\"")),
    };
    let store = state.log_store.read();
    let result = evaluate_logql_limited(&expr, &store, i64::MIN, i64::MAX, None, Some(limit));
    let mut out: Vec<Value> = Vec::new();
    if let LogQLResult::Streams(streams) = result {
        for s in streams {
            for (ts, line) in s.entries {
                out.push(json!({ "ts": ts.to_string(), "line": line, "labels": s.labels }));
            }
        }
    }
    // Note: `since` ingest-seq filtering on the raw eval path is best-effort —
    // structured-filter results already pass the stream selector; for the loop,
    // prefer reset-based clean slate. (Documented limitation.)
    let _ = since;
    let total = out.len();
    let truncated = total > limit;
    out.truncate(limit);
    let text = format!("{} log line(s){}", out.len(), if truncated { " (truncated)" } else { "" });
    tool_ok(id, json!({ "logs": out, "shown": out.len(), "total_count": total, "truncated": truncated }), text)
}
```

> Note on `since`: the LogQL evaluator filters by event time, not ingest_seq. For correct run-scoping the recommended path is `reset` (clean slate). If exact ingest-scoped log drill-down is later required, add an ingest-seq-aware store query; out of scope here.

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: query_logs handler (structured + raw LogQL)"`

---

### Task 4.8: `query_traces` handler

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn query_traces_bad_traceql_is_error() {
        let st = tests_state();
        let bad = call(&st, Some(json!(1)),
            &json!({ "name": "query_traces", "arguments": { "traceql": "{{{" } }));
        assert_eq!(bad["result"]["isError"], json!(true));
    }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** (raw `traceql` wins; else build from structured; evaluate via `evaluate_traceql`):

```rust
use crate::query::traceql::eval::evaluate_traceql;
use crate::query::traceql::parser::parse_traceql;

fn handle_query_traces(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).min(100) as usize;
    let query = if let Some(raw) = args.get("traceql").and_then(|v| v.as_str()) {
        raw.to_string()
    } else {
        let mut conds = Vec::new();
        if let Some(s) = args.get("service").and_then(|v| v.as_str()) {
            conds.push(format!("resource.service.name = \"{s}\""));
        }
        if let Some(st_) = args.get("status").and_then(|v| v.as_str()) { conds.push(format!("status = {st_}")); }
        if let Some(d) = args.get("min_duration").and_then(|v| v.as_str()) { conds.push(format!("duration > {d}")); }
        if let Some(n) = args.get("name").and_then(|v| v.as_str()) { conds.push(format!("name = \"{n}\"")); }
        if conds.is_empty() {
            return tool_err(id, "provide service/name/status/min_duration, or a raw `traceql`".into());
        }
        format!("{{ {} }}", conds.join(" && "))
    };
    let expr = match parse_traceql(&query) {
        Ok(e) => e,
        Err(e) => return tool_err(id, format!(
            "TraceQL parse error: {e}. Example: {{ resource.service.name = \"api\" && status = error }}")),
    };
    let store = state.trace_store.read();
    let mut results = evaluate_traceql(&expr, &store);
    let total = results.len();
    let truncated = total > limit;
    results.truncate(limit);
    let out: Vec<Value> = results.iter().map(|r| {
        json!({
            "trace_id": r.trace_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "matched_spans": r.matched_spans.iter().map(|m| json!({
                "span_id": m.span_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "name": m.name, "service": m.service_name,
                "duration_ms": m.duration_ns as f64 / 1_000_000.0,
            })).collect::<Vec<_>>(),
        })
    }).collect();
    let text = format!("{} trace(s){}", out.len(), if truncated { " (truncated)" } else { "" });
    tool_ok(id, json!({ "traces": out, "shown": out.len(), "total_count": total, "truncated": truncated }), text)
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: query_traces handler (structured + raw TraceQL)"`

---

### Task 4.9: `query_metrics` handler (raw PromQL)

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn query_metrics_bad_promql_is_error() {
        let st = tests_state();
        let bad = call(&st, Some(json!(1)),
            &json!({ "name": "query_metrics", "arguments": { "promql": "rate(" } }));
        assert_eq!(bad["result"]["isError"], json!(true));
    }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** (instant query by default; range if start/end/step present):

```rust
use crate::query::promql::eval::{evaluate_instant, PromQLResult};

fn handle_query_metrics(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let promql = match args.get("promql").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return tool_err(id, "`promql` is required".into()),
    };
    let now_ms = {
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
        if ns > i64::MAX as u128 { i64::MAX } else { ns as i64 }
    };
    let store = state.metric_store.read();
    let result = match evaluate_instant(promql, &store, now_ms) {
        Ok(r) => r,
        Err(e) => return tool_err(id, format!(
            "PromQL error: {e}. Example: rate(http_requests_total[5m])")),
    };
    let series = match result {
        PromQLResult::InstantVector(s) | PromQLResult::RangeVector(s) => s,
        PromQLResult::Scalar(v) => {
            return tool_ok(id, json!({ "scalar": v }), format!("scalar {v}"));
        }
    };
    let out: Vec<Value> = series.iter().map(|s| json!({
        "labels": s.labels,
        "samples": s.samples.iter().map(|(t, v)| json!([t.to_string(), v])).collect::<Vec<_>>(),
    })).collect();
    let text = format!("{} series", out.len());
    tool_ok(id, json!({ "series": out }), text)
}
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: query_metrics handler (raw PromQL)"`

---

### Task 4.10: `get_trace` handler (real tree)

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn get_trace_unknown_id_is_error() {
        let st = tests_state();
        let bad = call(&st, Some(json!(1)),
            &json!({ "name": "get_trace", "arguments": { "trace_id": "zz" } }));
        assert_eq!(bad["result"]["isError"], json!(true));
    }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

```rust
fn handle_get_trace(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let hex = match args.get("trace_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_err(id, "`trace_id` is required (32 hex chars)".into()),
    };
    if hex.len() != 32 {
        return tool_err(id, "trace_id must be exactly 32 hex characters".into());
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        match u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16) {
            Ok(b) => bytes[i] = b,
            Err(_) => return tool_err(id, "trace_id is not valid hex".into()),
        }
    }
    match synth::build_trace_tree(state, &bytes) {
        Some(tree) => {
            let text = format!("trace {} with {} root span(s)", tree.trace_id, tree.roots.len());
            match serde_json::to_value(&tree) {
                Ok(v) => tool_ok(id, v, text),
                Err(e) => tool_err(id, format!("serialization error: {e}")),
            }
        }
        None => tool_err(id, format!("trace {hex} not found")),
    }
}
```

- [ ] **Step 4: Run** → PASS. Then `cargo test -p aniani mcp::` (all unit tests).
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: get_trace handler with span tree"`

---

# Phase 5 — Server instructions

### Task 5.1: Write the loop-teaching `instructions` string

**Files:** Modify `src/mcp/tools.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn instructions_mention_the_loop() {
        assert!(INSTRUCTIONS.contains("reset"));
        assert!(INSTRUCTIONS.contains("summarize_activity"));
        assert!(INSTRUCTIONS.contains("mark_checkpoint"));
    }
```

- [ ] **Step 2: Run** → FAIL (stub string).

- [ ] **Step 3: Replace `INSTRUCTIONS`**

```rust
pub const INSTRUCTIONS: &str = "\
Aniani is your local observability instrument for the dev loop. Recommended flow:
1. reset(scope=all) for a clean baseline before a run.
2. Run your code/tests. Telemetry export may lag a moment — if a summary looks empty, wait briefly and retry.
3. summarize_activity(service) to see what the run produced (error logs, failing/slow traces, health).
4. Drill in: describe_service(service) to learn queryable labels/metrics, then query_logs / query_traces / query_metrics / get_trace.
5. To compare iterations without wiping, call mark_checkpoint() before a run and pass the returned token as `since`.
Use check_health() when you don't yet know which service is in trouble.";
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `git commit -am "mcp/tools: loop-teaching server instructions"`

---

# Phase 6 — End-to-end integration tests

### Task 6.1: Full loop over HTTP

**Files:** Modify `tests/e2e_mcp.rs`

- [ ] **Step 1: Write the test**

Add a helper to call a tool and a full-loop test:
```rust
async fn call_tool(base: &str, name: &str, args: serde_json::Value) -> serde_json::Value {
    reqwest::Client::new().post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                      "params":{"name":name,"arguments":args}}))
        .send().await.unwrap().json().await.unwrap()
}

#[tokio::test]
async fn full_loop_reset_ingest_summarize() {
    let base = spawn().await;
    // reset
    let r = call_tool(&base, "reset", json!({"scope":"all"})).await;
    assert_eq!(r["result"]["isError"], json!(false));
    // ingest a Loki error log directly via the REST push API
    reqwest::Client::new().post(format!("{base}/loki/api/v1/push"))
        .json(&json!({"streams":[{"stream":{"service":"api","level":"error"},
              "values":[["1","kaboom"]]}]}))
        .send().await.unwrap();
    // summarize_activity reflects it
    let s = call_tool(&base, "summarize_activity", json!({"service":"api"})).await;
    assert_eq!(s["result"]["structuredContent"]["logs"]["error_count"], json!(1));
    // reset clears
    call_tool(&base, "reset", json!({"scope":"all"})).await;
    let s2 = call_tool(&base, "summarize_activity", json!({"service":"api"})).await;
    assert_eq!(s2["result"]["structuredContent"]["logs"]["error_count"], json!(0));
}
```

- [ ] **Step 2: Run** `cargo test --test e2e_mcp full_loop` → should PASS once the tools are wired.
- [ ] **Step 3: Commit** `git commit -am "test(mcp): end-to-end reset/ingest/summarize loop"`

---

### Task 6.2: Late-arriving telemetry cursor case (the headline bug)

**Files:** Modify `tests/e2e_mcp.rs`

- [ ] **Step 1: Write the test**

```rust
#[tokio::test]
async fn since_uses_ingest_order_not_event_time() {
    let base = spawn().await;
    // mark checkpoint at the empty store
    let cp = call_tool(&base, "mark_checkpoint", json!({})).await;
    let token = cp["result"]["structuredContent"]["checkpoint"].as_u64().unwrap();
    // ingest an error whose EVENT timestamp (1) predates "now" but arrives after the mark
    reqwest::Client::new().post(format!("{base}/loki/api/v1/push"))
        .json(&json!({"streams":[{"stream":{"service":"api","level":"error"},
              "values":[["1","late but mine"]]}]}))
        .send().await.unwrap();
    // since=token still includes it (ingest-seq based, not event-time based)
    let s = call_tool(&base, "summarize_activity", json!({"service":"api","since": token})).await;
    assert_eq!(s["result"]["structuredContent"]["logs"]["error_count"], json!(1));
}
```

- [ ] **Step 2: Run** `cargo test --test e2e_mcp since_uses_ingest_order` → PASS.
- [ ] **Step 3: Commit** `git commit -am "test(mcp): since filters by ingest order, not event time"`

---

### Task 6.3: Compliance — notification 202, batch rejection, origin 403

**Files:** Modify `tests/e2e_mcp.rs`

- [ ] **Step 1: Write the tests**

```rust
#[tokio::test]
async fn notification_returns_202() {
    let base = spawn().await;
    let resp = reqwest::Client::new().post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 202);
    assert!(resp.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn batch_array_rejected() {
    let base = spawn().await;
    let resp = reqwest::Client::new().post(format!("{base}/mcp"))
        .json(&json!([{"jsonrpc":"2.0","id":1,"method":"ping"}]))
        .send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!(-32600));
}

#[tokio::test]
async fn foreign_origin_forbidden() {
    let base = spawn().await;
    let resp = reqwest::Client::new().post(format!("{base}/mcp"))
        .header("origin", "https://evil.example.com")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 403);
}
```

- [ ] **Step 2: Run** `cargo test --test e2e_mcp` (full file) → PASS.
- [ ] **Step 3: Commit** `git commit -am "test(mcp): transport compliance (202, batch, origin)"`

---

# Phase 7 — Docs and version

### Task 7.1: Document MCP and bump version

**Files:** Modify `Cargo.toml`, `DESIGN.md`, `AGENTS.md`, `README.md`

- [ ] **Step 1: Bump version**

`Cargo.toml:3` — `version = "0.11.0"`.

- [ ] **Step 2: Document the surface**

- `DESIGN.md` — add an "MCP Interface" section: the `/mcp` endpoint, the 10 tools, the agent loop, the ingest-seq cursor.
- `AGENTS.md` — add `src/mcp/` to the module-layout block (mod.rs/protocol.rs/server.rs/tools.rs/synth.rs) and a one-line note that MCP shares the listener like gRPC.
- `README.md` — add an "MCP (for agents)" subsection: point an MCP client at `http://127.0.0.1:4320/mcp`, summarize the loop.

- [ ] **Step 3: Verify the build + full suite**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: clean fmt, no clippy warnings, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml DESIGN.md AGENTS.md README.md
git commit -m "docs: document MCP interface; bump version to 0.11.0"
```

---

## Final verification

- [ ] `cargo build --bin aniani` (production feature set) compiles.
- [ ] `cargo test` — all unit + integration tests green.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] Manual smoke: boot `aniani`, `curl` an `initialize` then `tools/list` against `/mcp`, push a Loki error, `tools/call summarize_activity` and confirm `error_count`.
