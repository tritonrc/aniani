//! Trace storage engine for TraceQL queries.
//!
//! `TraceStore` stores spans indexed by trace ID, service name, and span name.

use lasso::{Rodeo, Spur};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Status of a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpanStatus {
    Unset,
    Ok,
    Error,
}

/// The role a span plays in a trace, mirroring OTLP `SpanKind`.
///
/// Used by the UI to render Jaeger-style CLIENT/SERVER annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpanKind {
    Unspecified,
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl SpanKind {
    /// Map an OTLP `kind` integer to a `SpanKind`. Unknown values map to `Unspecified`.
    pub fn from_otlp(code: i32) -> Self {
        match code {
            1 => SpanKind::Internal,
            2 => SpanKind::Server,
            3 => SpanKind::Client,
            4 => SpanKind::Producer,
            5 => SpanKind::Consumer,
            _ => SpanKind::Unspecified,
        }
    }

    /// The OTLP integer code for this kind.
    pub fn as_otlp(self) -> i32 {
        match self {
            SpanKind::Unspecified => 0,
            SpanKind::Internal => 1,
            SpanKind::Server => 2,
            SpanKind::Client => 3,
            SpanKind::Producer => 4,
            SpanKind::Consumer => 5,
        }
    }
}

/// Attribute value types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttributeValue {
    String(Spur),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// (name, status, service) keys snapshot used during targeted index
/// reconciliation after partial span removal.
type SpanIndexKeys = Vec<(Spur, SpanStatus, Spur)>;

/// A timestamped event recorded on a span.
///
/// OTLP exceptions arrive as events named `exception` carrying
/// `exception.type` / `exception.message` / `exception.stacktrace` attributes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEvent {
    pub name: Spur,
    pub time_ns: i64,
    pub attributes: SmallVec<[(Spur, AttributeValue); 4]>,
}

/// A trace span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub name: Spur,
    pub service_name: Spur,
    pub start_time_ns: i64,
    pub duration_ns: i64,
    pub status: SpanStatus,
    /// OTLP `Status.message` — the human-readable error description carried
    /// alongside `status = error`. `None` when unset or for non-error statuses.
    #[serde(default)]
    pub status_message: Option<Spur>,
    pub kind: SpanKind,
    pub attributes: SmallVec<[(Spur, AttributeValue); 8]>,
    pub events: Vec<SpanEvent>,
    /// Global monotonic ingest sequence; assigned on store insert.
    #[serde(default)]
    pub ingest_seq: u64,
}

/// Count how many of the given span statuses are `SpanStatus::Error`. Takes
/// an iterator of `&SpanStatus` (rather than `&[Span]`) so callers can share
/// this across both `Span` (the store's type) and `MatchedSpan` (the TraceQL
/// eval layer's type) — shared by every code path that derives an error
/// count from a set of spans, so the definition of "error count" can't drift
/// between them.
pub fn count_error_spans<'a>(statuses: impl IntoIterator<Item = &'a SpanStatus>) -> usize {
    statuses
        .into_iter()
        .filter(|s| **s == SpanStatus::Error)
        .count()
}

/// Result of a trace search.
#[derive(Debug, Clone)]
pub struct TraceResult {
    pub trace_id: [u8; 16],
    pub root_service_name: String,
    pub root_span_name: String,
    pub start_time_ns: i64,
    pub duration_ns: i64,
    pub span_count: usize,
    /// Number of spans in the trace with `SpanStatus::Error`.
    pub error_count: usize,
}

/// In-memory trace storage with indexes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStore {
    /// trace_id -> list of spans.
    pub traces: FxHashMap<[u8; 16], Vec<Span>>,
    /// Service name (Spur) -> set of trace IDs.
    pub service_index: FxHashMap<Spur, FxHashSet<[u8; 16]>>,
    /// Span name (Spur) -> set of trace IDs.
    pub name_index: FxHashMap<Spur, FxHashSet<[u8; 16]>>,
    /// Span status -> set of trace IDs.
    pub status_index: FxHashMap<SpanStatus, FxHashSet<[u8; 16]>>,
    /// String interner.
    pub interner: Rodeo,
    /// Total span count for eviction.
    pub total_spans: usize,
}

impl TraceStore {
    /// Create a new empty trace store.
    pub fn new() -> Self {
        Self {
            traces: FxHashMap::default(),
            service_index: FxHashMap::default(),
            name_index: FxHashMap::default(),
            status_index: FxHashMap::default(),
            interner: Rodeo::default(),
            total_spans: 0,
        }
    }

    /// Ingest a batch of spans.
    pub fn ingest_spans(&mut self, spans: Vec<Span>) {
        for span in spans {
            let trace_id = span.trace_id;
            let service = span.service_name;
            let name = span.name;

            self.service_index
                .entry(service)
                .or_default()
                .insert(trace_id);
            self.name_index.entry(name).or_default().insert(trace_id);
            self.status_index
                .entry(span.status)
                .or_default()
                .insert(trace_id);

            self.traces.entry(trace_id).or_default().push(span);
            self.total_spans += 1;
        }
    }

    /// Get all spans for a trace.
    pub fn get_trace(&self, trace_id: &[u8; 16]) -> Option<&Vec<Span>> {
        self.traces.get(trace_id)
    }

    /// Get all known service names.
    pub fn service_names(&self) -> Vec<String> {
        self.service_index
            .keys()
            .map(|s| self.interner.resolve(s).to_string())
            .collect()
    }

    /// Get all trace IDs for a service.
    pub fn traces_for_service(&self, service: &str) -> Vec<[u8; 16]> {
        match self.interner.get(service) {
            Some(spur) => match self.service_index.get(&spur) {
                Some(set) => set.iter().copied().collect(),
                None => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    /// Resolve a Spur to a string.
    pub fn resolve(&self, spur: &Spur) -> &str {
        self.interner.resolve(spur)
    }

    /// Resolve an attribute value to a string representation.
    pub fn resolve_attribute_value(&self, val: &AttributeValue) -> String {
        match val {
            AttributeValue::String(s) => self.interner.resolve(s).to_string(),
            AttributeValue::Int(i) => i.to_string(),
            AttributeValue::Float(f) => f.to_string(),
            AttributeValue::Bool(b) => b.to_string(),
        }
    }

    /// Evict oldest traces until total_spans <= max.
    pub fn evict_to_max(&mut self, max_spans: usize) {
        if self.total_spans <= max_spans {
            return;
        }

        // Sort traces by earliest span start time
        let mut trace_ids: Vec<[u8; 16]> = self.traces.keys().copied().collect();
        trace_ids.sort_by_key(|tid| {
            self.traces[tid]
                .iter()
                .map(|s| s.start_time_ns)
                .min()
                .unwrap_or(i64::MAX)
        });

        for tid in trace_ids {
            if self.total_spans <= max_spans {
                break;
            }
            if let Some(spans) = self.traces.remove(&tid) {
                self.total_spans = self.total_spans.saturating_sub(spans.len());
                // Clean up indexes
                for span in &spans {
                    if let Some(set) = self.service_index.get_mut(&span.service_name) {
                        set.remove(&tid);
                    }
                    if let Some(set) = self.name_index.get_mut(&span.name) {
                        set.remove(&tid);
                    }
                    if let Some(set) = self.status_index.get_mut(&span.status) {
                        set.remove(&tid);
                    }
                }
            }
        }

        // Clean up empty index entries
        self.service_index.retain(|_, v| !v.is_empty());
        self.name_index.retain(|_, v| !v.is_empty());
        self.status_index.retain(|_, v| !v.is_empty());
    }

    /// Evict spans older than the given timestamp.
    pub fn evict_before(&mut self, cutoff_ns: i64) {
        let mut empty_traces: Vec<[u8; 16]> = Vec::new();
        // (trace_id, before-removal name/status/service keys) for traces that
        // lost at least one span. Used for targeted index reconciliation.
        let mut changed: Vec<([u8; 16], SpanIndexKeys)> = Vec::new();

        for (trace_id, spans) in &mut self.traces {
            // Skip the snapshot unless at least one span is old enough to evict.
            if !spans.iter().any(|s| s.start_time_ns < cutoff_ns) {
                continue;
            }
            let before_keys: Vec<(Spur, SpanStatus, Spur)> = spans
                .iter()
                .map(|s| (s.name, s.status, s.service_name))
                .collect();
            let before = spans.len();
            spans.retain(|s| s.start_time_ns >= cutoff_ns);
            let removed = before - spans.len();
            self.total_spans = self.total_spans.saturating_sub(removed);
            changed.push((*trace_id, before_keys));
            if spans.is_empty() {
                empty_traces.push(*trace_id);
            }
        }

        for trace_id in &empty_traces {
            self.traces.remove(trace_id);
        }

        // Reconcile only the affected traces' index contributions instead of
        // rebuilding all indexes from every surviving span.
        for (trace_id, before_keys) in changed {
            self.reconcile_trace_indexes(trace_id, &before_keys);
        }
    }

    /// Build a TraceResult summary for a trace.
    pub fn trace_result(&self, trace_id: &[u8; 16]) -> Option<TraceResult> {
        let spans = self.traces.get(trace_id)?;
        if spans.is_empty() {
            return None;
        }

        // Find root span (no parent) or earliest span
        let root = spans
            .iter()
            .find(|s| s.parent_span_id.is_none())
            .or_else(|| spans.iter().min_by_key(|s| s.start_time_ns))?;

        let start = spans.iter().map(|s| s.start_time_ns).min().unwrap_or(0);
        let end = spans
            .iter()
            .map(|s| s.start_time_ns.saturating_add(s.duration_ns))
            .max()
            .unwrap_or(0);

        let error_count = count_error_spans(spans.iter().map(|s| &s.status));

        Some(TraceResult {
            trace_id: *trace_id,
            root_service_name: self.interner.resolve(&root.service_name).to_string(),
            root_span_name: self.interner.resolve(&root.name).to_string(),
            start_time_ns: start,
            duration_ns: end.saturating_sub(start),
            span_count: spans.len(),
            error_count,
        })
    }

    /// Return summaries of the most recent traces, up to `limit`.
    ///
    /// Traces are sorted by start time descending (most recent first).
    pub fn recent_traces(&self, limit: usize) -> Vec<TraceResult> {
        let mut results: Vec<TraceResult> = self
            .traces
            .keys()
            .filter_map(|tid| self.trace_result(tid))
            .collect();
        results.sort_by_key(|b| std::cmp::Reverse(b.start_time_ns));
        results.truncate(limit);
        results
    }

    /// Clear all data from the store.
    pub fn clear(&mut self) {
        self.traces.clear();
        self.service_index.clear();
        self.name_index.clear();
        self.status_index.clear();
        self.interner = Rodeo::default();
        self.total_spans = 0;
    }

    /// Clear all spans belonging to a specific service, preserving other services' spans.
    pub fn clear_service(&mut self, service: &str) {
        let service_spur = match self.interner.get(service) {
            Some(s) => s,
            None => return,
        };

        let trace_ids: Vec<[u8; 16]> = match self.service_index.get(&service_spur) {
            Some(set) => set.iter().copied().collect(),
            None => return,
        };

        for trace_id in &trace_ids {
            if let Some(spans) = self.traces.get_mut(trace_id) {
                let before_keys: Vec<(Spur, SpanStatus, Spur)> = spans
                    .iter()
                    .map(|s| (s.name, s.status, s.service_name))
                    .collect();
                let before = spans.len();
                spans.retain(|s| s.service_name != service_spur);
                let removed = before - spans.len();
                self.total_spans = self.total_spans.saturating_sub(removed);

                if spans.is_empty() {
                    self.traces.remove(trace_id);
                }
                self.reconcile_trace_indexes(*trace_id, &before_keys);
            }
        }
    }

    /// Reconcile a single trace's index contributions after its span set
    /// changed. `before` holds the (name, status, service) keys the trace had
    /// before removal; the surviving spans (if any) are read from `self.traces`.
    /// Only index entries no longer represented by a surviving span are pruned,
    /// so this is O(affected spans) rather than a full rebuild over all spans.
    fn reconcile_trace_indexes(&mut self, trace_id: [u8; 16], before: &SpanIndexKeys) {
        // Current surviving membership.
        let mut sur_svc: FxHashSet<Spur> = FxHashSet::default();
        let mut sur_name: FxHashSet<Spur> = FxHashSet::default();
        let mut sur_status: FxHashSet<SpanStatus> = FxHashSet::default();
        if let Some(spans) = self.traces.get(&trace_id) {
            for span in spans {
                sur_svc.insert(span.service_name);
                sur_name.insert(span.name);
                sur_status.insert(span.status);
            }
        }

        for &(name, status, svc) in before {
            if !sur_name.contains(&name)
                && let Some(set) = self.name_index.get_mut(&name)
                && set.remove(&trace_id)
                && set.is_empty()
            {
                self.name_index.remove(&name);
            }
            if !sur_status.contains(&status)
                && let Some(set) = self.status_index.get_mut(&status)
                && set.remove(&trace_id)
                && set.is_empty()
            {
                self.status_index.remove(&status);
            }
            if !sur_svc.contains(&svc)
                && let Some(set) = self.service_index.get_mut(&svc)
                && set.remove(&trace_id)
                && set.is_empty()
            {
                self.service_index.remove(&svc);
            }
        }
    }
}

impl TraceStore {
    /// Estimate the memory usage of this store in bytes.
    ///
    /// Accounts for span data, attribute sizes, index overhead, and interner memory.
    pub fn memory_estimate_bytes(&self) -> usize {
        let mut bytes = 0usize;

        // Per-trace: Vec<Span> and each span's attributes and events
        for spans in self.traces.values() {
            bytes += spans.capacity() * std::mem::size_of::<Span>();
            for span in spans {
                // Attribute key/value pairs stored in SmallVec
                bytes += span.attributes.len() * std::mem::size_of::<(Spur, AttributeValue)>();
                // Span events: the heap Vec of SpanEvent plus each event's attributes
                bytes += span.events.capacity() * std::mem::size_of::<SpanEvent>();
                for event in &span.events {
                    bytes += event.attributes.len() * std::mem::size_of::<(Spur, AttributeValue)>();
                }
            }
        }

        // HashMap overhead for traces
        bytes += self.traces.len()
            * (std::mem::size_of::<[u8; 16]>() + std::mem::size_of::<Vec<Span>>() + 8);

        // Service index: FxHashMap<Spur, FxHashSet<[u8; 16]>>
        for set in self.service_index.values() {
            bytes += set.len() * (std::mem::size_of::<[u8; 16]>() + 8);
        }
        bytes += self.service_index.len() * (std::mem::size_of::<Spur>() + 8);

        // Name index: FxHashMap<Spur, FxHashSet<[u8; 16]>>
        for set in self.name_index.values() {
            bytes += set.len() * (std::mem::size_of::<[u8; 16]>() + 8);
        }
        bytes += self.name_index.len() * (std::mem::size_of::<Spur>() + 8);

        // Status index: FxHashMap<SpanStatus, FxHashSet<[u8; 16]>>
        for set in self.status_index.values() {
            bytes += set.len() * (std::mem::size_of::<[u8; 16]>() + 8);
        }
        bytes += self.status_index.len() * (std::mem::size_of::<SpanStatus>() + 8);

        // Interner memory
        bytes += self.interner.current_memory_usage();

        bytes
    }
}

impl Default for TraceStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
