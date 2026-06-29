use serde_json::Value;

use crate::mcp::tools::dispatch::tool_err;
use crate::store::SharedState;

/// All service names reporting any signal (logs, metrics, traces), sorted unique.
pub(super) fn known_services(state: &SharedState) -> Vec<String> {
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
pub(super) fn require_known_service(
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

/// Escape a value for safe interpolation inside a double-quoted LogQL/TraceQL
/// string literal, so caller-supplied filters can't break the generated query.
pub(super) fn escape_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
