//! Log storage engine with inverted index for LogQL queries.
//!
//! `LogStore` stores log streams indexed by label pairs using sorted posting lists.
//! Each stream contains an ordered sequence of `LogEntry` items.

use lasso::{Rodeo, Spur};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use super::posting_list::PostingList;
use super::trace_store::AttributeValue;
use super::{LabelMatcher, LabelPairs};

/// A single log entry with nanosecond timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp_ns: i64,
    pub line: String,
    /// Global monotonic ingest sequence; assigned on store insert.
    #[serde(default)]
    pub ingest_seq: u64,
    /// W3C trace id (16 bytes, all-zero absent). Hex-encoded at the
    /// presentation layer, not here.
    #[serde(default)]
    pub trace_id: Option<[u8; 16]>,
    /// W3C span id (8 bytes, all-zero absent). Hex-encoded at the
    /// presentation layer.
    #[serde(default)]
    pub span_id: Option<[u8; 8]>,
    /// OTLP severity number (0 when absent, e.g. Loki-push).
    #[serde(default)]
    pub severity_number: i32,
    /// OTLP severity text (interned, e.g. "INFO", "ERROR"). `None` when
    /// absent (Loki-push entries have no first-class severity text).
    #[serde(default)]
    pub severity_text: Option<Spur>,
    /// Per-entry structured attributes (typed, interned). OTLP
    /// `LogRecord.attributes` land here instead of being promoted to stream
    /// labels, avoiding cardinality explosion from high-cardinality keys.
    #[serde(default)]
    pub attributes: SmallVec<[(Spur, AttributeValue); 8]>,
}

/// A log stream identified by a set of labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogStream {
    pub labels: SmallVec<[(Spur, Spur); 8]>,
    pub entries: Vec<LogEntry>,
}

/// In-memory log storage with inverted index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogStore {
    /// Stream ID -> stream data.
    pub streams: FxHashMap<u64, LogStream>,
    /// Label pair (name, value) -> set of stream IDs.
    pub label_index: FxHashMap<(Spur, Spur), PostingList>,
    /// Label name -> set of known values.
    pub label_values: FxHashMap<Spur, FxHashSet<Spur>>,
    /// String interner.
    pub interner: Rodeo,
    /// Total entry count for eviction.
    pub total_entries: usize,
    /// Exact label-set to stream ID mapping to avoid hash-collision merges.
    #[serde(default)]
    pub stream_ids: FxHashMap<LabelPairs, u64>,
    /// Next stream ID for newly observed label sets.
    #[serde(default)]
    pub next_stream_id: u64,
}

impl LogStore {
    /// Create a new empty log store.
    pub fn new() -> Self {
        Self {
            streams: FxHashMap::default(),
            label_index: FxHashMap::default(),
            label_values: FxHashMap::default(),
            interner: Rodeo::default(),
            total_entries: 0,
            stream_ids: FxHashMap::default(),
            next_stream_id: 0,
        }
    }

    /// Rebuild the label-set to stream-ID map after loading older snapshots.
    pub fn rebuild_stream_ids(&mut self) {
        self.stream_ids.clear();
        self.next_stream_id = 0;

        let mut stream_ids: Vec<u64> = self.streams.keys().copied().collect();
        stream_ids.sort_unstable();
        for stream_id in stream_ids {
            if let Some(stream) = self.streams.get(&stream_id) {
                self.stream_ids.insert(stream.labels.clone(), stream_id);
                self.next_stream_id = self.next_stream_id.max(stream_id.saturating_add(1));
            }
        }
    }

    /// Ingest a stream with the given labels and entries.
    pub fn ingest_stream(&mut self, labels: Vec<(String, String)>, entries: Vec<LogEntry>) {
        let interned_labels = super::intern_label_pairs(&mut self.interner, &labels);
        super::track_label_values(&mut self.label_values, &interned_labels);

        let stream_id = match self.stream_ids.get(&interned_labels).copied() {
            Some(stream_id) => stream_id,
            None => {
                let stream_id = self.next_stream_id;
                self.next_stream_id = self.next_stream_id.saturating_add(1);
                self.stream_ids.insert(interned_labels.clone(), stream_id);
                self.streams.insert(
                    stream_id,
                    LogStream {
                        labels: interned_labels.clone(),
                        entries: Vec::new(),
                    },
                );
                for &(k, v) in &interned_labels {
                    self.label_index
                        .entry((k, v))
                        .or_default()
                        .insert(stream_id);
                }
                stream_id
            }
        };

        let Some(stream) = self.streams.get_mut(&stream_id) else {
            tracing::warn!(
                stream_id,
                "stream identity index referenced missing stream; skipping log ingest"
            );
            return;
        };
        let entry_count = entries.len();
        let prev_len = stream.entries.len();
        stream.entries.extend(entries);
        self.total_entries += entry_count;

        // Maintain sorted order for partition_point correctness. The existing
        // entries (0..prev_len) are already sorted, so we only check the newly
        // appended batch for internal order plus the single boundary between
        // the old tail and the batch head. This is O(batch) rather than
        // re-scanning the whole vec, avoiding quadratic behavior when many
        // small batches append to the same stream.
        let needs_sort = entry_count > 0
            && super::batch_needs_sort(&stream.entries, prev_len, |e| e.timestamp_ns);
        if needs_sort {
            stream.entries.sort_by_key(|e| e.timestamp_ns);
        }
    }

    /// Query streams matching all the given label matchers.
    pub fn query_streams(&self, matchers: &[LabelMatcher]) -> Vec<u64> {
        super::select_indexed_ids(
            &self.interner,
            &self.label_index,
            &self.label_values,
            || self.streams.keys().copied().collect(),
            matchers,
        )
    }

    /// Get entries for a stream within a time range.
    pub fn get_entries(&self, stream_id: u64, start_ns: i64, end_ns: i64) -> &[LogEntry] {
        match self.streams.get(&stream_id) {
            Some(stream) => {
                let lo = stream
                    .entries
                    .partition_point(|e| e.timestamp_ns < start_ns);
                let hi = stream.entries.partition_point(|e| e.timestamp_ns <= end_ns);
                &stream.entries[lo..hi]
            }
            None => &[],
        }
    }

    /// Get labels for a stream as resolved strings.
    pub fn get_stream_labels(&self, stream_id: u64) -> Option<Vec<(String, String)>> {
        self.streams.get(&stream_id).map(|stream| {
            stream
                .labels
                .iter()
                .map(|(k, v)| {
                    (
                        self.interner.resolve(k).to_string(),
                        self.interner.resolve(v).to_string(),
                    )
                })
                .collect()
        })
    }

    /// Resolve an `AttributeValue` to its display string using the interner.
    pub fn resolve_attribute_value(&self, val: &AttributeValue) -> String {
        match val {
            AttributeValue::String(s) => self.interner.resolve(s).to_string(),
            AttributeValue::Int(i) => i.to_string(),
            AttributeValue::Float(f) => f.to_string(),
            AttributeValue::Bool(b) => b.to_string(),
            AttributeValue::Array(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|v| self.resolve_attribute_value(v))
                    .collect();
                format!("[{}]", parts.join(", "))
            }
            AttributeValue::Bytes(b) => {
                use std::fmt::Write;
                let mut hex = String::with_capacity(b.len() * 2 + 2);
                let _ = write!(hex, "0x");
                for byte in b {
                    let _ = write!(hex, "{byte:02x}");
                }
                hex
            }
            AttributeValue::KeyValueList(pairs) => {
                let parts: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}={}",
                            self.interner.resolve(k),
                            self.resolve_attribute_value(v)
                        )
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }

    /// Get all label names.
    pub fn label_names(&self) -> Vec<String> {
        self.label_values
            .keys()
            .map(|k| self.interner.resolve(k).to_string())
            .collect()
    }

    /// Get all values for a given label name.
    pub fn get_label_values(&self, name: &str) -> Vec<String> {
        let spur = match self.interner.get(name) {
            Some(s) => s,
            None => return Vec::new(),
        };
        match self.label_values.get(&spur) {
            Some(values) => values
                .iter()
                .map(|v| self.interner.resolve(v).to_string())
                .collect(),
            None => Vec::new(),
        }
    }

    /// Evict entries older than the given timestamp.
    pub fn evict_before(&mut self, cutoff_ns: i64) {
        let mut empty_streams = Vec::new();
        for (&stream_id, stream) in &mut self.streams {
            let before = stream.entries.len();
            let drain_count = stream
                .entries
                .partition_point(|e| e.timestamp_ns < cutoff_ns);
            if drain_count > 0 {
                stream.entries.drain(..drain_count);
            }
            let removed = before - stream.entries.len();
            self.total_entries = self.total_entries.saturating_sub(removed);
            if stream.entries.is_empty() {
                empty_streams.push(stream_id);
            }
        }

        for stream_id in empty_streams {
            if let Some(stream) = self.streams.remove(&stream_id) {
                self.stream_ids.remove(&stream.labels);
                super::remove_from_label_indexes(
                    &mut self.label_index,
                    &mut self.label_values,
                    &stream.labels,
                    stream_id,
                );
            }
        }
    }

    /// Evict oldest entries until total_entries <= max.
    pub fn evict_to_max(&mut self, max_entries: usize) {
        if self.total_entries <= max_entries {
            return;
        }
        let to_remove = self.total_entries - max_entries;

        let mut stream_ids: Vec<u64> = self.streams.keys().copied().collect();
        stream_ids.sort_by_key(|id| {
            self.streams[id]
                .entries
                .first()
                .map(|e| e.timestamp_ns)
                .unwrap_or(i64::MAX)
        });

        let mut remaining = to_remove;
        for sid in stream_ids {
            if remaining == 0 {
                break;
            }
            if let Some(stream) = self.streams.get_mut(&sid) {
                let drain = remaining.min(stream.entries.len());
                stream.entries.drain(..drain);
                remaining -= drain;
                self.total_entries -= drain;
                if stream.entries.is_empty()
                    && let Some(stream) = self.streams.remove(&sid)
                {
                    self.stream_ids.remove(&stream.labels);
                    super::remove_from_label_indexes(
                        &mut self.label_index,
                        &mut self.label_values,
                        &stream.labels,
                        sid,
                    );
                }
            }
        }
    }

    /// Clear all data from the store.
    pub fn clear(&mut self) {
        self.streams.clear();
        self.label_index.clear();
        self.label_values.clear();
        self.interner = Rodeo::default();
        self.stream_ids.clear();
        self.next_stream_id = 0;
        self.total_entries = 0;
    }

    /// Clear all data belonging to a specific service.
    pub fn clear_service(&mut self, service: &str) {
        let service_spur = match self.interner.get(service) {
            Some(s) => s,
            None => return,
        };
        let service_key = match self.interner.get("service") {
            Some(s) => s,
            None => return,
        };

        // Find streams that have service=<value>
        let stream_ids: Vec<u64> = match self.label_index.get(&(service_key, service_spur)) {
            Some(pl) => pl.ids().to_vec(),
            None => return,
        };

        for stream_id in stream_ids {
            if let Some(stream) = self.streams.remove(&stream_id) {
                self.total_entries = self.total_entries.saturating_sub(stream.entries.len());
                self.stream_ids.remove(&stream.labels);
                super::remove_from_label_indexes(
                    &mut self.label_index,
                    &mut self.label_values,
                    &stream.labels,
                    stream_id,
                );
            }
        }
    }
}

impl LogStore {
    /// Estimate the memory usage of this store in bytes.
    ///
    /// Sums actual string lengths in log entries, Vec overhead, label index sizes,
    /// and the interner's memory footprint.
    pub fn memory_estimate_bytes(&self) -> usize {
        let mut bytes = 0usize;

        // Per-stream overhead: SmallVec labels + Vec<LogEntry> bookkeeping
        for stream in self.streams.values() {
            // Labels: SmallVec overhead is inline for <=8, each pair is (Spur, Spur) = 8 bytes
            bytes += stream.labels.len() * std::mem::size_of::<(Spur, Spur)>();
            // Vec<LogEntry> capacity overhead
            bytes += stream.entries.capacity() * std::mem::size_of::<LogEntry>();
            // Actual string data in each entry (heap allocation beyond LogEntry struct)
            for entry in &stream.entries {
                bytes += entry.line.capacity();
                // Per-entry attributes: each (Spur, AttributeValue) pair
                bytes += entry.attributes.len() * std::mem::size_of::<(Spur, AttributeValue)>();
            }
        }

        // HashMap overhead for streams: ~(key_size + value_size + 8) * capacity
        // Approximate with entries * overhead_per_bucket
        bytes += self.streams.len()
            * (std::mem::size_of::<u64>() + std::mem::size_of::<LogStream>() + 8);

        // Label index: FxHashMap<(Spur, Spur), PostingList>
        for pl in self.label_index.values() {
            bytes += pl.len() * std::mem::size_of::<u64>();
        }
        bytes += self.label_index.len()
            * (std::mem::size_of::<(Spur, Spur)>() + std::mem::size_of::<PostingList>() + 8);

        // Label values index: FxHashMap<Spur, FxHashSet<Spur>>
        for vals in self.label_values.values() {
            bytes += vals.len() * (std::mem::size_of::<Spur>() + 8);
        }
        bytes += self.label_values.len() * (std::mem::size_of::<Spur>() + 8);

        // Interner memory
        bytes += self.interner.current_memory_usage();

        bytes
    }
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
