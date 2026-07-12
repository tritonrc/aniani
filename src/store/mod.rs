//! Storage engine: in-memory stores for logs, metrics, and traces.
//!
//! All stores are behind `parking_lot::RwLock` within a shared `AppState`.

pub mod log_store;
pub mod metric_store;
pub mod posting_list;
pub mod trace_store;

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use lasso::{Rodeo, Spur};
use parking_lot::Mutex;
use parking_lot::RwLock;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use crate::config::Config;
pub use log_store::LogStore;
pub use metric_store::MetricStore;
use posting_list::{PostingList, difference, intersect, union};
pub use trace_store::TraceStore;

/// Shared application state accessible by all handlers.
pub struct AppState {
    pub log_store: RwLock<LogStore>,
    pub metric_store: RwLock<MetricStore>,
    pub trace_store: RwLock<TraceStore>,
    pub config: Config,
    pub start_time: Instant,
    /// Monotonic counter stamped onto every ingested entry/sample/span.
    pub ingest_seq: AtomicU64,
}

/// Type alias for the shared state handle.
pub type SharedState = Arc<AppState>;

/// Shared compact label set representation used by indexed stores.
pub type LabelPairs = SmallVec<[(Spur, Spur); 8]>;

/// Types of label matchers for queries.
#[derive(Debug, Clone)]
pub enum LabelMatchOp {
    Eq,
    Neq,
    Regex,
    NotRegex,
}

/// A label matcher used in stream and series selectors.
#[derive(Debug, Clone)]
pub struct LabelMatcher {
    pub name: String,
    pub op: LabelMatchOp,
    pub value: String,
}

pub(crate) fn intern_label_pairs(interner: &mut Rodeo, labels: &[(String, String)]) -> LabelPairs {
    let mut interned: LabelPairs = labels
        .iter()
        .map(|(k, v)| (interner.get_or_intern(k), interner.get_or_intern(v)))
        .collect();
    interned.sort_by_key(|(k, _)| *k);
    interned
}

pub(crate) fn track_label_values(
    label_values: &mut FxHashMap<Spur, FxHashSet<Spur>>,
    labels: &LabelPairs,
) {
    for &(k, v) in labels {
        label_values.entry(k).or_default().insert(v);
    }
}

pub(crate) fn remove_from_label_indexes(
    label_index: &mut FxHashMap<(Spur, Spur), PostingList>,
    label_values: &mut FxHashMap<Spur, FxHashSet<Spur>>,
    labels: &LabelPairs,
    id: u64,
) {
    for &(k, v) in labels {
        if let Some(pl) = label_index.get_mut(&(k, v)) {
            pl.remove(id);
            if pl.is_empty() {
                label_index.remove(&(k, v));
                if let Some(vals) = label_values.get_mut(&k) {
                    vals.remove(&v);
                    if vals.is_empty() {
                        label_values.remove(&k);
                    }
                }
            }
        }
    }
}

pub(crate) fn select_indexed_ids<F>(
    interner: &Rodeo,
    label_index: &FxHashMap<(Spur, Spur), PostingList>,
    label_values: &FxHashMap<Spur, FxHashSet<Spur>>,
    all_ids: F,
    matchers: &[LabelMatcher],
) -> Vec<u64>
where
    F: Fn() -> Vec<u64>,
{
    if matchers.is_empty() {
        return all_ids();
    }

    let mut positive_lists: Vec<PostingList> = Vec::new();
    // Full ID set, computed at most once per query and reused by every
    // negative matcher instead of re-collecting all stream/series IDs each.
    let mut all_posting: Option<PostingList> = None;
    let all_or_init = |all_posting: &mut Option<PostingList>| -> PostingList {
        all_posting
            .get_or_insert_with(|| all_ids_posting_list(all_ids()))
            .clone()
    };

    for matcher in matchers {
        let name_spur = match interner.get(&matcher.name) {
            Some(s) => s,
            None => match matcher.op {
                LabelMatchOp::Neq | LabelMatchOp::NotRegex => {
                    positive_lists.push(all_or_init(&mut all_posting));
                    continue;
                }
                _ => return Vec::new(),
            },
        };

        match matcher.op {
            LabelMatchOp::Eq => {
                let value_spur = match interner.get(&matcher.value) {
                    Some(s) => s,
                    None => return Vec::new(),
                };
                match label_index.get(&(name_spur, value_spur)) {
                    Some(pl) => positive_lists.push(pl.clone()),
                    None => return Vec::new(),
                }
            }
            LabelMatchOp::Neq => {
                let value_spur = interner.get(&matcher.value);
                let all = all_or_init(&mut all_posting);
                let result = value_spur
                    .and_then(|vs| label_index.get(&(name_spur, vs)))
                    .map(|exclude| difference(&all, exclude))
                    .unwrap_or(all);
                positive_lists.push(result);
            }
            LabelMatchOp::Regex => {
                let Some(re) = compiled_label_regex(&matcher.value) else {
                    return Vec::new();
                };
                let mut result = PostingList::new();
                if let Some(values) = label_values.get(&name_spur) {
                    for &vs in values {
                        let val_str = interner.resolve(&vs);
                        if re.is_match(val_str)
                            && let Some(pl) = label_index.get(&(name_spur, vs))
                        {
                            result = union(&result, pl);
                        }
                    }
                }
                positive_lists.push(result);
            }
            LabelMatchOp::NotRegex => {
                let Some(re) = compiled_label_regex(&matcher.value) else {
                    return Vec::new();
                };
                let all = all_or_init(&mut all_posting);
                let mut excluded = PostingList::new();
                if let Some(values) = label_values.get(&name_spur) {
                    for &vs in values {
                        let val_str = interner.resolve(&vs);
                        if re.is_match(val_str)
                            && let Some(exclude) = label_index.get(&(name_spur, vs))
                        {
                            excluded = union(&excluded, exclude);
                        }
                    }
                }
                positive_lists.push(difference(&all, &excluded));
            }
        }
    }

    let refs: Vec<&PostingList> = positive_lists.iter().collect();
    intersect(&refs)
}

fn all_ids_posting_list(ids: Vec<u64>) -> PostingList {
    PostingList::from_ids(ids)
}

/// Process-wide cache of anchored regexes. Repeated queries/evaluations with
/// the same pattern (common in the agent dev loop) reuse the compiled `Regex`
/// instead of recompiling. Shared by label matchers (`select_indexed_ids`) and
/// PromQL `label_replace`. `Regex` clones cheaply (internally `Arc`-backed).
/// The cache is unbounded but, for an ephemeral per-worktree instance, the set
/// of distinct patterns is small.
pub(crate) static REGEX_CACHE: OnceLock<Mutex<FxHashMap<String, Regex>>> = OnceLock::new();

/// Compile (or fetch a cached) anchored regex for `pattern`: `^(?:pattern)$`.
/// Returns `None` if the pattern is invalid.
pub(crate) fn compiled_label_regex(pattern: &str) -> Option<Regex> {
    let cache = REGEX_CACHE.get_or_init(|| Mutex::new(FxHashMap::default()));
    if let Some(re) = cache.lock().get(pattern) {
        return Some(re.clone());
    }
    let anchored = format!("^(?:{})$", pattern);
    let re = Regex::new(&anchored).ok()?;
    cache.lock().insert(pattern.to_string(), re.clone());
    Some(re)
}

/// Highest `ingest_seq` present across all three stores (0 if empty). Used to
/// re-seed the global counter after restoring a snapshot so new ingests stay
/// monotonic.
pub fn max_ingest_seq(logs: &LogStore, metrics: &MetricStore, traces: &TraceStore) -> u64 {
    let l = logs
        .streams
        .values()
        .flat_map(|s| s.entries.iter().map(|e| e.ingest_seq))
        .max()
        .unwrap_or(0);
    let m = metrics
        .series
        .values()
        .flat_map(|s| s.samples.iter().map(|s| s.ingest_seq))
        .max()
        .unwrap_or(0);
    let t = traces
        .traces
        .values()
        .flat_map(|v| v.iter().map(|s| s.ingest_seq))
        .max()
        .unwrap_or(0);
    l.max(m).max(t)
}

/// Build an empty `SharedState` with default config — for unit tests only.
#[cfg(test)]
pub(crate) fn empty_test_state() -> SharedState {
    use clap::Parser;
    Arc::new(AppState {
        log_store: RwLock::new(LogStore::new()),
        metric_store: RwLock::new(MetricStore::new()),
        trace_store: RwLock::new(TraceStore::new()),
        config: Config::parse_from(["aniani"]),
        start_time: Instant::now(),
        ingest_seq: AtomicU64::new(0),
    })
}

/// Run eviction on all stores based on config.
pub fn run_eviction(state: &AppState) {
    let retention = state.config.retention_duration();
    let now_ns = system_time_ns();
    let retention_ns = duration_to_i64_ns(retention);
    let cutoff_ns = now_ns.saturating_sub(retention_ns);
    let cutoff_ms = cutoff_ns / 1_000_000;

    // Evict by time and max count
    {
        let mut logs = state.log_store.write();
        logs.evict_before(cutoff_ns);
        logs.evict_to_max(state.config.max_log_entries);
    }
    {
        let mut metrics = state.metric_store.write();
        metrics.evict_before(cutoff_ms);
        metrics.evict_to_max(state.config.max_series);
    }
    {
        let mut traces = state.trace_store.write();
        traces.evict_before(cutoff_ns);
        traces.evict_to_max(state.config.max_spans);
    }
}

fn system_time_ns() -> i64 {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    if ns > i64::MAX as u128 {
        i64::MAX
    } else {
        ns as i64
    }
}

fn duration_to_i64_ns(duration: std::time::Duration) -> i64 {
    let ns = duration.as_nanos();
    if ns > i64::MAX as u128 {
        i64::MAX
    } else {
        ns as i64
    }
}

/// After appending a new batch to an already-sorted vec, decide whether the
/// whole vec needs re-sorting. `prev_len` is the length before the batch was
/// appended. Only the batch and its boundary with the old tail are inspected,
/// so this is O(batch) rather than O(total), avoiding quadratic behavior when
/// many small batches append to the same stream/series.
pub(crate) fn batch_needs_sort<T, F>(items: &[T], prev_len: usize, key: F) -> bool
where
    F: Fn(&T) -> i64,
{
    // No new batch appended.
    if prev_len >= items.len() {
        return false;
    }
    if prev_len > 0 && key(&items[prev_len - 1]) > key(&items[prev_len]) {
        return true;
    }
    items[prev_len..]
        .windows(2)
        .any(|w| key(&w[0]) > key(&w[1]))
}

#[cfg(test)]
mod batch_needs_sort_tests {
    use super::*;

    #[test]
    fn empty_batch_needs_no_sort() {
        let v: Vec<i64> = vec![1, 2, 3];
        assert!(!batch_needs_sort(&v, 3, |x| *x));
    }

    #[test]
    fn sorted_batch_appended_to_sorted_keeps_order() {
        let v = vec![1, 2, 5, 6]; // prev_len=2, batch [5,6]
        assert!(!batch_needs_sort(&v, 2, |x| *x));
    }

    #[test]
    fn boundary_violation_needs_sort() {
        let v = vec![1, 2, 0]; // prev_len=2, 2 > 0 at boundary
        assert!(batch_needs_sort(&v, 2, |x| *x));
    }

    #[test]
    fn internally_unsorted_batch_needs_sort() {
        let v = vec![1, 2, 6, 5]; // prev_len=2, boundary ok, batch [6,5] unsorted
        assert!(batch_needs_sort(&v, 2, |x| *x));
    }

    #[test]
    fn first_batch_unsorted_needs_sort() {
        let v = vec![3, 1, 2]; // prev_len=0
        assert!(batch_needs_sort(&v, 0, |x| *x));
    }

    #[test]
    fn single_element_first_batch_needs_no_sort() {
        let v = vec![5]; // prev_len=0
        assert!(!batch_needs_sort(&v, 0, |x| *x));
    }
}

#[cfg(test)]
mod ingest_seq_restore_tests {
    use super::*;

    #[test]
    fn max_ingest_seq_finds_highest_across_stores() {
        let mut logs = LogStore::new();
        logs.ingest_stream(
            vec![("service".into(), "a".into())],
            vec![crate::store::log_store::LogEntry {
                timestamp_ns: 1,
                line: "x".into(),
                ingest_seq: 41,
                trace_id: None,
            }],
        );
        let metrics = MetricStore::new();
        let traces = TraceStore::new();
        assert_eq!(max_ingest_seq(&logs, &metrics, &traces), 41);
    }

    #[test]
    fn max_ingest_seq_finds_highest_on_metric_sample() {
        let logs = LogStore::new();
        let mut metrics = MetricStore::new();
        metrics.ingest_samples(
            "cpu",
            vec![("host".into(), "a".into())],
            vec![crate::store::metric_store::Sample {
                timestamp_ms: 1,
                value: 0.5,
                ingest_seq: 57,
            }],
        );
        let traces = TraceStore::new();
        assert_eq!(max_ingest_seq(&logs, &metrics, &traces), 57);
    }

    #[test]
    fn max_ingest_seq_finds_highest_on_span() {
        use crate::store::trace_store::{Span, SpanKind, SpanStatus};

        let logs = LogStore::new();
        let metrics = MetricStore::new();
        let mut traces = TraceStore::new();
        let name = traces.interner.get_or_intern("span-a");
        let service = traces.interner.get_or_intern("svc-a");
        traces.ingest_spans(vec![Span {
            trace_id: [1u8; 16],
            span_id: [1u8; 8],
            parent_span_id: None,
            name,
            service_name: service,
            start_time_ns: 1000,
            duration_ns: 100,
            status: SpanStatus::Ok,
            status_message: None,
            kind: SpanKind::Unspecified,
            attributes: SmallVec::new(),
            events: Vec::new(),
            ingest_seq: 63,
        }]);
        assert_eq!(max_ingest_seq(&logs, &metrics, &traces), 63);
    }
}
