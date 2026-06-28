//! MCP tool registry, `tools/list`, and `tools/call` dispatch.

use serde_json::{Value, json};

use crate::mcp::protocol;
use crate::mcp::synth;
use crate::query::logql::eval::{LogQLResult, evaluate_logql_limited};
use crate::query::logql::parser::parse_logql;
use crate::query::promql::eval::{PromQLResult, evaluate_instant};
use crate::query::traceql::eval::evaluate_traceql;
use crate::query::traceql::parser::parse_traceql;
use crate::store::SharedState;

/// Server-level instructions injected at `initialize` (the agent loop).
pub const INSTRUCTIONS: &str = "Aniani MCP — local observability for your dev loop.";

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
            "description": "Triage one service: error logs, failing/slow traces, error metrics, health score, one-line summary. Use after a run to see what it produced. Pass `since` (a checkpoint token) to scope to the latest run. Use this BEFORE drilling in with query_* tools.",
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
            "description": "Fetch log lines. Provide structured filters (service, level, contains) OR a raw `logql` string (e.g. {service=\"api\"} |= \"error\"). If `logql` is set, structured filters are ignored. To scope to a run, reset first, then query; summarize_activity is the checkpoint-accurate run summary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "service": { "type": "string" }, "level": { "type": "string" },
                    "contains": { "type": "string" },
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
                    "limit": { "type": "integer" },
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

/// Build a successful `tools/call` result carrying both text and structured content.
fn tool_ok(id: Option<Value>, structured: Value, text: String) -> Value {
    protocol::success(
        id,
        json!({
            "content": [ { "type": "text", "text": text } ],
            "structuredContent": structured,
            "isError": false
        }),
    )
}

/// Build a `tools/call` execution-error result (model-visible, self-correcting).
fn tool_err(id: Option<Value>, message: String) -> Value {
    protocol::success(
        id,
        json!({
            "content": [ { "type": "text", "text": message } ],
            "isError": true
        }),
    )
}

/// Dispatch a `tools/call` request to the named tool handler.
pub fn call(state: &SharedState, id: Option<Value>, params: &Value) -> Value {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return protocol::error(id, protocol::INVALID_PARAMS, "missing tool name"),
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
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
    tool_ok(
        id,
        structured,
        format!("reset complete; checkpoint={checkpoint}"),
    )
}

fn handle_mark_checkpoint(state: &SharedState, id: Option<Value>) -> Value {
    let checkpoint = state.ingest_seq.load(std::sync::atomic::Ordering::Relaxed);
    tool_ok(
        id,
        json!({ "checkpoint": checkpoint }),
        format!("checkpoint={checkpoint} — pass as `since` to scope a later summary"),
    )
}

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

fn handle_check_health(state: &SharedState, id: Option<Value>) -> Value {
    let overview = synth::check_health(state);
    let text = format!("{} service(s) ranked worst-first", overview.services.len());
    match serde_json::to_value(&overview) {
        Ok(v) => tool_ok(id, v, text),
        Err(e) => tool_err(id, format!("serialization error: {e}")),
    }
}

fn handle_describe_service(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let service = match args.get("service").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return tool_err(id, "`service` is required".into()),
    };
    let cat = synth::describe_service(state, &service);
    let text = format!(
        "{} metric(s), {} log label key(s), {} span attr key(s)",
        cat.metrics.len(),
        cat.log_labels.len(),
        cat.span_attributes.len()
    );
    match serde_json::to_value(&cat) {
        Ok(v) => tool_ok(id, v, text),
        Err(e) => tool_err(id, format!("serialization error: {e}")),
    }
}

fn handle_list_services(state: &SharedState, id: Option<Value>) -> Value {
    use rustc_hash::{FxHashMap, FxHashSet};
    let mut sig: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();
    for n in state.log_store.read().get_label_values("service") {
        sig.entry(n).or_default().insert("logs");
    }
    for n in state.metric_store.read().get_label_values("service") {
        sig.entry(n).or_default().insert("metrics");
    }
    for n in state.trace_store.read().service_names() {
        sig.entry(n).or_default().insert("traces");
    }
    let mut services: Vec<Value> = sig
        .into_iter()
        .map(|(name, s)| {
            let mut sigs: Vec<&str> = s.into_iter().collect();
            sigs.sort_unstable();
            json!({ "name": name, "signals": sigs })
        })
        .collect();
    services.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    let text = format!("{} service(s)", services.len());
    tool_ok(id, json!({ "services": services }), text)
}

fn handle_query_logs(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .min(100) as usize;
    // Raw LogQL escape hatch wins over structured filters.
    let query = if let Some(raw) = args.get("logql").and_then(|v| v.as_str()) {
        raw.to_string()
    } else {
        let mut sel = Vec::new();
        if let Some(s) = args.get("service").and_then(|v| v.as_str()) {
            sel.push(format!("service=\"{s}\""));
        }
        if let Some(l) = args.get("level").and_then(|v| v.as_str()) {
            sel.push(format!("level=\"{l}\""));
        }
        if sel.is_empty() {
            return tool_err(
                id,
                "provide at least one of service/level/contains, or a raw `logql`".into(),
            );
        }
        let mut q = format!("{{{}}}", sel.join(", "));
        if let Some(c) = args.get("contains").and_then(|v| v.as_str()) {
            q.push_str(&format!(" |= \"{c}\""));
        }
        q
    };
    let expr = match parse_logql(&query) {
        Ok(e) => e,
        Err(e) => {
            return tool_err(
                id,
                format!("LogQL parse error: {e}. Example: {{service=\"api\"}} |= \"error\""),
            );
        }
    };
    let store = state.log_store.read();
    let result = evaluate_logql_limited(&expr, &store, i64::MIN, i64::MAX, None, Some(limit));
    let mut out: Vec<Value> = Vec::new();
    if let LogQLResult::Streams(streams) = result {
        for s in streams {
            for (ts, line) in s.entries {
                out.push(
                    json!({ "ts": (ts / 1_000_000).to_string(), "line": line, "labels": s.labels }),
                );
            }
        }
    }
    let total = out.len();
    let truncated = total > limit;
    out.truncate(limit);
    let text = format!(
        "{} log line(s){}",
        out.len(),
        if truncated { " (truncated)" } else { "" }
    );
    tool_ok(
        id,
        json!({ "logs": out, "shown": out.len(), "total_count": total, "truncated": truncated }),
        text,
    )
}

fn handle_query_traces(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .min(100) as usize;
    let query = if let Some(raw) = args.get("traceql").and_then(|v| v.as_str()) {
        raw.to_string()
    } else {
        let mut conds = Vec::new();
        if let Some(s) = args.get("service").and_then(|v| v.as_str()) {
            conds.push(format!("resource.service.name = \"{s}\""));
        }
        if let Some(status) = args.get("status").and_then(|v| v.as_str()) {
            conds.push(format!("status = {status}"));
        }
        if let Some(d) = args.get("min_duration").and_then(|v| v.as_str()) {
            conds.push(format!("duration > {d}"));
        }
        if let Some(n) = args.get("name").and_then(|v| v.as_str()) {
            conds.push(format!("name = \"{n}\""));
        }
        if conds.is_empty() {
            return tool_err(
                id,
                "provide service/name/status/min_duration, or a raw `traceql`".into(),
            );
        }
        format!("{{ {} }}", conds.join(" && "))
    };
    let expr = match parse_traceql(&query) {
        Ok(e) => e,
        Err(e) => {
            return tool_err(
                id,
                format!(
                    "TraceQL parse error: {e}. Example: {{ resource.service.name = \"api\" && status = error }}"
                ),
            );
        }
    };
    let store = state.trace_store.read();
    let mut results = evaluate_traceql(&expr, &store);
    let total = results.len();
    let truncated = total > limit;
    results.truncate(limit);
    let out: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "trace_id": r.trace_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "matched_spans": r.matched_spans.iter().map(|m| json!({
                    "span_id": m.span_id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    "name": m.name,
                    "service": m.service_name,
                    "duration_ms": m.duration_ns as f64 / 1_000_000.0,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let text = format!(
        "{} trace(s){}",
        out.len(),
        if truncated { " (truncated)" } else { "" }
    );
    tool_ok(
        id,
        json!({ "traces": out, "shown": out.len(), "total_count": total, "truncated": truncated }),
        text,
    )
}

fn handle_query_metrics(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let promql = match args.get("promql").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return tool_err(id, "`promql` is required".into()),
    };
    let now_ms = {
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        if ns > i64::MAX as u128 {
            i64::MAX
        } else {
            ns as i64
        }
    };
    let store = state.metric_store.read();
    let result = match evaluate_instant(promql, &store, now_ms) {
        Ok(r) => r,
        Err(e) => {
            return tool_err(
                id,
                format!("PromQL error: {e}. Example: rate(http_requests_total[5m])"),
            );
        }
    };
    let series = match result {
        PromQLResult::InstantVector(s) | PromQLResult::RangeVector(s) => s,
        PromQLResult::Scalar(v) => {
            return tool_ok(id, json!({ "scalar": v }), format!("scalar {v}"));
        }
    };
    let out: Vec<Value> = series
        .iter()
        .map(|s| {
            json!({
                "labels": s.labels,
                "samples": s.samples.iter().map(|(t, v)| json!([t.to_string(), v])).collect::<Vec<_>>(),
            })
        })
        .collect();
    let text = format!("{} series", out.len());
    tool_ok(id, json!({ "series": out }), text)
}

fn handle_get_trace(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let hex = match args.get("trace_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_err(id, "`trace_id` is required (32 hex chars)".into()),
    };
    if hex.len() != 32 {
        return tool_err(id, "trace_id must be exactly 32 hex characters".into());
    }
    let mut bytes = [0u8; 16];
    for (i, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(chunk).unwrap_or("");
        match u8::from_str_radix(pair, 16) {
            Ok(b) => bytes[i] = b,
            Err(_) => return tool_err(id, "trace_id is not valid hex".into()),
        }
    }
    match synth::build_trace_tree(state, &bytes) {
        Some(tree) => {
            let text = format!(
                "trace {} with {} root span(s)",
                tree.trace_id,
                tree.roots.len()
            );
            match serde_json::to_value(&tree) {
                Ok(v) => tool_ok(id, v, text),
                Err(e) => tool_err(id, format!("serialization error: {e}")),
            }
        }
        None => tool_err(id, format!("trace {hex} not found")),
    }
}

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
            "reset",
            "mark_checkpoint",
            "summarize_activity",
            "check_health",
            "query_logs",
            "query_traces",
            "query_metrics",
            "get_trace",
            "list_services",
            "describe_service",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        let reset = tools.iter().find(|t| t["name"] == "reset").unwrap();
        assert_eq!(reset["annotations"]["destructiveHint"], json!(true));
        assert_eq!(reset["annotations"]["readOnlyHint"], json!(false));
        let logs = tools.iter().find(|t| t["name"] == "query_logs").unwrap();
        assert_eq!(logs["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(logs["annotations"]["openWorldHint"], json!(false));
    }

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

    #[test]
    fn call_unknown_tool_is_invalid_params() {
        let st = tests_state();
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "nope", "arguments": {} }),
        );
        assert_eq!(
            resp["error"]["code"],
            json!(crate::mcp::protocol::INVALID_PARAMS)
        );
    }

    #[test]
    fn reset_all_clears_and_returns_checkpoint() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "api".into())],
                vec![crate::store::log_store::LogEntry {
                    timestamp_ns: 1,
                    line: "x".into(),
                    ingest_seq: 0,
                }],
            );
        }
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "reset", "arguments": { "scope": "all" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(false));
        assert!(resp["result"]["structuredContent"]["checkpoint"].is_u64());
        assert_eq!(st.log_store.read().streams.len(), 0);
    }

    #[test]
    fn reset_service_scope_requires_service() {
        let st = tests_state();
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "reset", "arguments": { "scope": "service" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    #[test]
    fn mark_checkpoint_returns_current_seq() {
        let st = tests_state();
        st.ingest_seq.store(7, std::sync::atomic::Ordering::Relaxed);
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "mark_checkpoint", "arguments": {} }),
        );
        assert_eq!(resp["result"]["structuredContent"]["checkpoint"], json!(7));
    }

    #[test]
    fn summarize_activity_tool_reports_errors() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![
                    ("service".into(), "api".into()),
                    ("level".into(), "error".into()),
                ],
                vec![crate::store::log_store::LogEntry {
                    timestamp_ns: 1,
                    line: "boom".into(),
                    ingest_seq: 0,
                }],
            );
        }
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "summarize_activity", "arguments": { "service": "api" } }),
        );
        assert_eq!(
            resp["result"]["structuredContent"]["logs"]["error_count"],
            json!(1)
        );
    }

    #[test]
    fn summarize_activity_requires_service() {
        let st = tests_state();
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "summarize_activity", "arguments": {} }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    #[test]
    fn check_health_tool_returns_services_array() {
        let st = tests_state();
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "check_health", "arguments": {} }),
        );
        assert!(resp["result"]["structuredContent"]["services"].is_array());
    }

    #[test]
    fn list_services_tool_includes_signals() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "api".into())],
                vec![crate::store::log_store::LogEntry {
                    timestamp_ns: 1,
                    line: "x".into(),
                    ingest_seq: 0,
                }],
            );
        }
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "list_services", "arguments": {} }),
        );
        let services = resp["result"]["structuredContent"]["services"]
            .as_array()
            .unwrap();
        assert!(
            services.iter().any(|s| s["name"] == "api"
                && s["signals"].as_array().unwrap().iter().any(|x| x == "logs"))
        );
    }

    #[test]
    fn describe_service_tool_requires_service_and_returns_catalog() {
        let st = tests_state();
        let missing = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "describe_service", "arguments": {} }),
        );
        assert_eq!(missing["result"]["isError"], json!(true));
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![
                    ("service".into(), "api".into()),
                    ("level".into(), "error".into()),
                ],
                vec![crate::store::log_store::LogEntry {
                    timestamp_ns: 1,
                    line: "x".into(),
                    ingest_seq: 0,
                }],
            );
        }
        let ok = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "describe_service", "arguments": { "service": "api" } }),
        );
        assert!(ok["result"]["structuredContent"]["log_labels"].is_array());
    }

    #[test]
    fn query_logs_structured_and_bad_logql() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            logs.ingest_stream(
                vec![("service".into(), "api".into())],
                vec![crate::store::log_store::LogEntry {
                    timestamp_ns: 5,
                    line: "hello".into(),
                    ingest_seq: 0,
                }],
            );
        }
        let ok = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "service": "api" } }),
        );
        assert_eq!(ok["result"]["isError"], json!(false));
        assert!(ok["result"]["structuredContent"]["logs"].is_array());

        let bad = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "logql": "{unterminated" } }),
        );
        assert_eq!(bad["result"]["isError"], json!(true));
    }

    #[test]
    fn query_traces_bad_traceql_is_error() {
        let st = tests_state();
        let bad = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_traces", "arguments": { "traceql": "{{{" } }),
        );
        assert_eq!(bad["result"]["isError"], json!(true));
    }

    #[test]
    fn query_metrics_bad_promql_is_error() {
        let st = tests_state();
        let bad = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_metrics", "arguments": { "promql": "rate(" } }),
        );
        assert_eq!(bad["result"]["isError"], json!(true));
    }

    #[test]
    fn get_trace_unknown_id_is_error() {
        let st = tests_state();
        let bad = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "get_trace", "arguments": { "trace_id": "zz" } }),
        );
        assert_eq!(bad["result"]["isError"], json!(true));
    }
}
