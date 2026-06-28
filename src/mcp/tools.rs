//! MCP tool registry, `tools/list`, and `tools/call` dispatch.

use serde_json::{Value, json};

use crate::mcp::protocol;
use crate::mcp::synth;
use crate::query::logql::eval::{LogQLResult, evaluate_logql_limited};
use crate::query::logql::parser::parse_logql;
use crate::query::promql::eval::{PromQLResult, evaluate_instant, evaluate_range};
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
            "outputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string" },
                    "service": { "type": "string" },
                    "checkpoint": { "type": "integer" }
                },
                "required": ["scope", "checkpoint"]
            },
            "annotations": { "title": "Reset Telemetry Store", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "mark_checkpoint",
            "title": "Mark Checkpoint",
            "description": "Return an opaque monotonic checkpoint token for 'now'. Pass it later as `since` to scope a summary to telemetry ingested after this point. Not a wall-clock time.",
            "inputSchema": { "type": "object", "properties": {} },
            "outputSchema": {
                "type": "object",
                "properties": { "checkpoint": { "type": "integer" } },
                "required": ["checkpoint"]
            },
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
            "outputSchema": {
                "type": "object",
                "properties": {
                    "service": { "type": "string" },
                    "since": { "type": ["integer", "null"] },
                    "observed_through": { "type": "integer" },
                    "health_score": { "type": "number" },
                    "summary": { "type": "string" },
                    "logs": { "type": "object" },
                    "traces": { "type": "object" },
                    "metrics": { "type": "object" },
                    "truncated": { "type": "object" }
                },
                "required": ["service", "health_score", "summary"]
            },
            "annotations": read_only("Summarize Service Activity")
        }),
        json!({
            "name": "check_health",
            "title": "Check Global Health",
            "description": "Rank every known service by health, worst first. Use when you don't yet know which service is in trouble.",
            "inputSchema": { "type": "object", "properties": {} },
            "outputSchema": {
                "type": "object",
                "properties": { "services": { "type": "array", "items": { "type": "object" } } },
                "required": ["services"]
            },
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
            "outputSchema": {
                "type": "object",
                "properties": {
                    "logs": { "type": "array", "items": { "type": "object" } },
                    "shown": { "type": "integer" },
                    "total_count": { "type": "integer" },
                    "truncated": { "type": "boolean" }
                },
                "required": ["logs", "shown", "total_count", "truncated"]
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
            "outputSchema": {
                "type": "object",
                "properties": {
                    "traces": { "type": "array", "items": { "type": "object" } },
                    "shown": { "type": "integer" },
                    "total_count": { "type": "integer" },
                    "truncated": { "type": "boolean" }
                },
                "required": ["traces", "shown", "total_count", "truncated"]
            },
            "annotations": read_only("Query Traces")
        }),
        json!({
            "name": "query_metrics",
            "title": "Query Metrics",
            "description": "Run a raw PromQL query (e.g. rate(http_requests_total[5m])). For a range query pass all of start/end (unix seconds, or ms/ns) and step (a duration like 15s or 1m); omit all three for an instant query at now.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "promql": { "type": "string" },
                    "start": { "type": "string" }, "end": { "type": "string" }, "step": { "type": "string" }
                },
                "required": ["promql"]
            },
            "outputSchema": {
                "type": "object",
                "properties": {
                    "series": { "type": "array", "items": { "type": "object" } },
                    "scalar": { "type": "number" }
                }
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
            "outputSchema": {
                "type": "object",
                "properties": {
                    "trace_id": { "type": "string" },
                    "roots": { "type": "array", "items": { "type": "object" } }
                },
                "required": ["trace_id", "roots"]
            },
            "annotations": read_only("Get Trace Tree")
        }),
        json!({
            "name": "list_services",
            "title": "List Services",
            "description": "List every service reporting telemetry and which signals (logs/metrics/traces) each has.",
            "inputSchema": { "type": "object", "properties": {} },
            "outputSchema": {
                "type": "object",
                "properties": { "services": { "type": "array", "items": { "type": "object" } } },
                "required": ["services"]
            },
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
            "outputSchema": {
                "type": "object",
                "properties": {
                    "service": { "type": "string" },
                    "metrics": { "type": "array" },
                    "log_labels": { "type": "array" },
                    "span_attributes": { "type": "array" }
                },
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

/// All service names reporting any signal (logs, metrics, traces), sorted unique.
fn known_services(state: &SharedState) -> Vec<String> {
    use rustc_hash::FxHashSet;
    let mut set: FxHashSet<String> = FxHashSet::default();
    set.extend(state.log_store.read().get_label_values("service"));
    set.extend(state.metric_store.read().get_label_values("service"));
    set.extend(state.trace_store.read().service_names());
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort_unstable();
    v
}

/// Require a non-empty `service` that actually reports telemetry. On failure
/// returns a model-visible `isError` result that echoes the bad value and lists
/// valid services (per the §5 self-correction contract).
fn require_known_service(
    state: &SharedState,
    id: &Option<Value>,
    args: &Value,
) -> Result<String, Value> {
    let service = match args.get("service").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err(tool_err(id.clone(), "`service` is required".into())),
    };
    let known = known_services(state);
    if known.iter().any(|s| s == &service) {
        Ok(service)
    } else {
        let valid = if known.is_empty() {
            "none yet".to_string()
        } else {
            known.join(", ")
        };
        Err(tool_err(
            id.clone(),
            format!("unknown service \"{service}\". Known services: {valid}"),
        ))
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
    let service = args.get("service").and_then(|v| v.as_str());
    let mut structured = json!({ "scope": scope, "checkpoint": checkpoint });
    if scope == Some("service") {
        structured["service"] = json!(service);
    }
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
    let service = match require_known_service(state, &id, args) {
        Ok(s) => s,
        Err(e) => return e,
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
    let text = match overview.services.first() {
        Some(worst) => format!(
            "{} service(s) ranked worst-first; worst: {} (health {:.0}, {})",
            overview.services.len(),
            worst.service,
            worst.health_score,
            worst.top_issue
        ),
        None => "no services reporting telemetry yet".to_string(),
    };
    match serde_json::to_value(&overview) {
        Ok(v) => tool_ok(id, v, text),
        Err(e) => tool_err(id, format!("serialization error: {e}")),
    }
}

fn handle_describe_service(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let service = match require_known_service(state, &id, args) {
        Ok(s) => s,
        Err(e) => return e,
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

/// Escape a value for safe interpolation inside a double-quoted LogQL/TraceQL
/// string literal, so caller-supplied filters can't break the generated query.
fn escape_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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
            sel.push(format!("service=\"{}\"", escape_quoted(s)));
        }
        if let Some(l) = args.get("level").and_then(|v| v.as_str()) {
            sel.push(format!("level=\"{}\"", escape_quoted(l)));
        }
        if sel.is_empty() {
            return tool_err(
                id,
                "provide at least `service` or `level` (optionally with `contains`), or a raw `logql`".into(),
            );
        }
        let mut q = format!("{{{}}}", sel.join(", "));
        if let Some(c) = args.get("contains").and_then(|v| v.as_str()) {
            q.push_str(&format!(" |= \"{}\"", escape_quoted(c)));
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
    // Probe one past the limit so we can report `truncated` accurately.
    let result = evaluate_logql_limited(&expr, &store, i64::MIN, i64::MAX, None, Some(limit + 1));
    let streams = match result {
        LogQLResult::Streams(streams) => streams,
        LogQLResult::Matrix(_) => {
            return tool_err(
                id,
                "LogQL metric queries (count_over_time, rate, etc.) are not supported here; \
                 use query_metrics with a PromQL expression instead."
                    .into(),
            );
        }
    };
    let mut out: Vec<Value> = Vec::new();
    for s in streams {
        for (ts, line) in s.entries {
            out.push(
                json!({ "ts": (ts / 1_000_000).to_string(), "line": line, "labels": s.labels }),
            );
        }
    }
    let truncated = out.len() > limit;
    // Exact total is only needed (and only worth a full scan) when truncated.
    let total_count = if truncated {
        match evaluate_logql_limited(&expr, &store, i64::MIN, i64::MAX, None, None) {
            LogQLResult::Streams(all) => all.iter().map(|s| s.entries.len()).sum(),
            LogQLResult::Matrix(_) => out.len(),
        }
    } else {
        out.len()
    };
    out.truncate(limit);
    let shown = out.len();
    let text = if truncated {
        format!(
            "showing {shown} of {total_count} log line(s) — narrow `contains`/`level` or raise `limit`"
        )
    } else {
        format!("{shown} log line(s)")
    };
    tool_ok(
        id,
        json!({ "logs": out, "shown": shown, "total_count": total_count, "truncated": truncated }),
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
            conds.push(format!("resource.service.name = \"{}\"", escape_quoted(s)));
        }
        if let Some(status) = args.get("status").and_then(|v| v.as_str()) {
            conds.push(format!("status = {status}"));
        }
        if let Some(d) = args.get("min_duration").and_then(|v| v.as_str()) {
            conds.push(format!("duration > {d}"));
        }
        if let Some(n) = args.get("name").and_then(|v| v.as_str()) {
            conds.push(format!("name = \"{}\"", escape_quoted(n)));
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
    // Collect matched trace IDs under the eval lock, then drop it before
    // summarizing each — synth::trace_item takes its own read lock and
    // parking_lot RwLock is not re-entrant. A concurrent reset between these two
    // phases is possible but benign: affected IDs return None and are skipped.
    let trace_ids: Vec<[u8; 16]> = {
        let store = state.trace_store.read();
        evaluate_traceql(&expr, &store)
            .into_iter()
            .map(|r| r.trace_id)
            .collect()
    };
    let total = trace_ids.len();
    let truncated = total > limit;
    let out: Vec<Value> = trace_ids
        .iter()
        .take(limit)
        .filter_map(|tid| synth::trace_item(state, tid))
        .filter_map(|t| serde_json::to_value(&t).ok())
        .collect();
    let shown = out.len();
    let text = if truncated {
        format!("showing {shown} of {total} trace(s) — narrow filters or raise `limit`")
    } else {
        format!("{shown} trace(s)")
    };
    tool_ok(
        id,
        json!({ "traces": out, "shown": shown, "total_count": total, "truncated": truncated }),
        text,
    )
}

fn handle_query_metrics(state: &SharedState, id: Option<Value>, args: &Value) -> Value {
    let promql = match args.get("promql").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return tool_err(id, "`promql` is required".into()),
    };
    let start = args.get("start").and_then(|v| v.as_str());
    let end = args.get("end").and_then(|v| v.as_str());
    let step = args.get("step").and_then(|v| v.as_str());
    let store = state.metric_store.read();
    let eval_result = if start.is_some() || end.is_some() || step.is_some() {
        // Range query: all three are required together.
        let (Some(start), Some(end), Some(step)) = (start, end, step) else {
            return tool_err(
                id,
                "a range query requires all of `start`, `end`, `step` (omit all three for an instant query)".into(),
            );
        };
        let start_ms = match crate::query::promql::handlers::parse_timestamp_ms(start) {
            Some(t) => t,
            None => return tool_err(id, format!("invalid `start`: {start}")),
        };
        let end_ms = match crate::query::promql::handlers::parse_timestamp_ms(end) {
            Some(t) => t,
            None => return tool_err(id, format!("invalid `end`: {end}")),
        };
        let step_ms = match crate::config::parse_duration(step).map(|d| {
            let ms = d.as_millis();
            if ms > i64::MAX as u128 {
                i64::MAX
            } else {
                ms as i64
            }
        }) {
            Some(s) if s > 0 => s,
            _ => return tool_err(id, format!("invalid `step`: {step}")),
        };
        // Cap total evaluation steps (mirrors the HTTP range handler) so an agent
        // can't trigger an OOM/hang with a tiny step over a huge window.
        let num_steps = end_ms.saturating_sub(start_ms).max(0) / step_ms;
        if num_steps >= crate::query::promql::handlers::MAX_QUERY_STEPS {
            return tool_err(
                id,
                format!(
                    "range query would produce {num_steps} steps (max {}); increase `step` or narrow start/end",
                    crate::query::promql::handlers::MAX_QUERY_STEPS
                ),
            );
        }
        evaluate_range(promql, &store, start_ms, end_ms, step_ms)
    } else {
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
        evaluate_instant(promql, &store, now_ms)
    };
    let result = match eval_result {
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
    let detailed = args.get("detail").and_then(|v| v.as_str()) == Some("detailed");
    match synth::build_trace_tree(state, &bytes, detailed) {
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

    #[test]
    fn get_trace_valid_but_missing_id_is_error() {
        let st = tests_state();
        // 32 valid hex chars that do not exist in the store.
        let bad = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "get_trace", "arguments": { "trace_id": "0123456789abcdef0123456789abcdef" } }),
        );
        assert_eq!(bad["result"]["isError"], json!(true));
    }

    #[test]
    fn query_logs_reports_truncated_when_over_limit() {
        let st = tests_state();
        {
            let mut logs = st.log_store.write();
            let entries: Vec<crate::store::log_store::LogEntry> = (1..=60)
                .map(|i| crate::store::log_store::LogEntry {
                    timestamp_ns: i,
                    line: format!("line {i}"),
                    ingest_seq: 0,
                })
                .collect();
            logs.ingest_stream(vec![("service".into(), "api".into())], entries);
        }
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "service": "api", "limit": 50 } }),
        );
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["shown"], json!(50));
        assert_eq!(sc["truncated"], json!(true));
    }

    #[test]
    fn query_logs_rejects_metric_logql() {
        let st = tests_state();
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "logql": "count_over_time({service=\"api\"}[5m])" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    #[test]
    fn query_logs_contains_only_is_error() {
        let st = tests_state();
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "contains": "boom" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    fn seed_api_logs(st: &crate::store::SharedState, n: i64) {
        let mut logs = st.log_store.write();
        let entries: Vec<crate::store::log_store::LogEntry> = (1..=n)
            .map(|i| crate::store::log_store::LogEntry {
                timestamp_ns: i,
                line: format!("line {i}"),
                ingest_seq: 0,
            })
            .collect();
        logs.ingest_stream(vec![("service".into(), "api".into())], entries);
    }

    #[test]
    fn query_logs_reports_total_count_and_hint() {
        let st = tests_state();
        seed_api_logs(&st, 60);
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_logs", "arguments": { "service": "api", "limit": 50 } }),
        );
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["shown"], json!(50));
        assert_eq!(sc["total_count"], json!(60));
        assert_eq!(sc["truncated"], json!(true));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("60"), "hint should mention total: {text}");
    }

    #[test]
    fn summarize_unknown_service_is_error_listing_valid() {
        let st = tests_state();
        seed_api_logs(&st, 1);
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "summarize_activity", "arguments": { "service": "ghost" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("ghost"), "echo bad value: {text}");
        assert!(text.contains("api"), "list valid services: {text}");
    }

    #[test]
    fn describe_unknown_service_is_error() {
        let st = tests_state();
        seed_api_logs(&st, 1);
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "describe_service", "arguments": { "service": "ghost" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    #[test]
    fn reset_service_scope_echoes_service() {
        let st = tests_state();
        seed_api_logs(&st, 1);
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "reset", "arguments": { "scope": "service", "service": "api" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(false));
        assert_eq!(resp["result"]["structuredContent"]["service"], json!("api"));
    }

    #[test]
    fn query_metrics_range_step_count_is_capped() {
        let st = tests_state();
        // 1.7e9 seconds at 1s steps would be ~1.7 billion evaluations.
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_metrics", "arguments": {
                "promql": "up", "start": "0", "end": "1700000000", "step": "1s"
            } }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("step"), "should mention step/cap: {text}");
    }

    #[test]
    fn query_metrics_range_invalid_time_is_error() {
        let st = tests_state();
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_metrics", "arguments": {
                "promql": "1", "start": "not-a-time", "end": "100", "step": "15s"
            } }),
        );
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    fn seed_error_trace(st: &crate::store::SharedState) -> [u8; 16] {
        use crate::store::trace_store::{Span, SpanKind, SpanStatus};
        use smallvec::smallvec;
        let tid = [7u8; 16];
        let mut traces = st.trace_store.write();
        let rname = traces.interner.get_or_intern("GET /api");
        let svc = traces.interner.get_or_intern("api");
        let akey = traces.interner.get_or_intern("http.method");
        let aval =
            crate::store::trace_store::AttributeValue::String(traces.interner.get_or_intern("GET"));
        // Real OTLP ingestion promotes resource attrs to `resource.*` span
        // attributes; TraceQL `resource.service.name` matches against those.
        let svc_key = traces.interner.get_or_intern("resource.service.name");
        let svc_val =
            crate::store::trace_store::AttributeValue::String(traces.interner.get_or_intern("api"));
        let root = Span {
            trace_id: tid,
            span_id: [1u8; 8],
            parent_span_id: None,
            name: rname,
            service_name: svc,
            start_time_ns: 0,
            duration_ns: 5_000_000,
            status: SpanStatus::Error,
            kind: SpanKind::Server,
            attributes: smallvec![(akey, aval), (svc_key, svc_val)],
            events: vec![],
            ingest_seq: 0,
        };
        traces.ingest_spans(vec![root]);
        tid
    }

    #[test]
    fn query_traces_returns_trace_level_summary() {
        let st = tests_state();
        seed_error_trace(&st);
        let resp = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "query_traces", "arguments": { "service": "api" } }),
        );
        assert_eq!(resp["result"]["isError"], json!(false));
        let traces = resp["result"]["structuredContent"]["traces"]
            .as_array()
            .unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0]["root_span_name"], json!("GET /api"));
        assert_eq!(traces[0]["error_span_count"], json!(1));
        assert!(traces[0]["trace_id"].is_string());
    }

    #[test]
    fn get_trace_detail_controls_attributes() {
        let st = tests_state();
        let tid = seed_error_trace(&st);
        let hex: String = tid.iter().map(|b| format!("{b:02x}")).collect();
        let concise = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "get_trace", "arguments": { "trace_id": hex } }),
        );
        let roots = concise["result"]["structuredContent"]["roots"]
            .as_array()
            .unwrap();
        assert!(
            roots[0]["attributes"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(true),
            "concise omits attributes"
        );
        let detailed = call(
            &st,
            Some(json!(1)),
            &json!({ "name": "get_trace", "arguments": { "trace_id": hex, "detail": "detailed" } }),
        );
        let droots = detailed["result"]["structuredContent"]["roots"]
            .as_array()
            .unwrap();
        let attrs = droots[0]["attributes"].as_array().unwrap();
        assert!(!attrs.is_empty(), "detailed includes attributes");
    }

    #[test]
    fn every_tool_has_output_schema() {
        let resp = list(Some(json!(1)));
        let tools = resp["result"]["tools"].as_array().unwrap();
        for t in tools {
            assert!(
                t["outputSchema"].is_object(),
                "{} missing outputSchema",
                t["name"]
            );
        }
    }
}
