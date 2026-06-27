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
