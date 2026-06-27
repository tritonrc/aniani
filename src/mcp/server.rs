//! Axum handlers and JSON-RPC dispatch for the MCP endpoint.

use axum::http::StatusCode;

pub async fn mcp_post() -> StatusCode { StatusCode::NOT_IMPLEMENTED }
pub async fn mcp_get() -> StatusCode { StatusCode::METHOD_NOT_ALLOWED }
pub async fn mcp_delete() -> StatusCode { StatusCode::METHOD_NOT_ALLOWED }
