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
