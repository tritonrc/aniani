//! Metric storage engine for PromQL queries.
//!
//! `MetricStore` stores time-series data indexed by metric name and label pairs.

use lasso::{Rodeo, Spur};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use super::posting_list::PostingList;
use super::{LabelMatcher, LabelPairs};

/// A single metric sample.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sample {
    pub timestamp_ms: i64,
    pub value: f64,
    /// Global monotonic ingest sequence; assigned on store insert.
    #[serde(default)]
    pub ingest_seq: u64,
}

/// A metric time-series identified by labels (including __name__).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSeries {
    pub labels: SmallVec<[(Spur, Spur); 8]>,
    pub samples: Vec<Sample>,
}

/// Errors that can occur while registering metric identities.
#[derive(Debug, thiserror::Error)]
pub enum MetricStoreError {
    #[error(
        "metric name collision: normalized name `{normalized}` maps to both `{existing}` and `{incoming}`"
    )]
    MetricNameCollision {
        normalized: String,
        existing: String,
        incoming: String,
    },
}

/// In-memory metric storage with inverted index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricStore {
    /// Series ID -> series data.
    pub series: FxHashMap<u64, MetricSeries>,
    /// Label pair (name, value) -> set of series IDs.
    pub label_index: FxHashMap<(Spur, Spur), PostingList>,
    /// Label name -> set of known values.
    pub label_values: FxHashMap<Spur, FxHashSet<Spur>>,
    /// String interner.
    pub interner: Rodeo,
    /// Total series count for eviction.
    pub total_samples: usize,
    /// Exact label-set to series ID mapping to avoid hash-collision merges.
    #[serde(default)]
    pub series_ids: FxHashMap<LabelPairs, u64>,
    /// Next series ID for newly observed label sets.
    #[serde(default)]
    pub next_series_id: u64,
    /// Normalized metric name -> original source metric name.
    #[serde(default)]
    pub normalized_name_sources: FxHashMap<Spur, Spur>,
}

impl MetricStore {
    /// Create a new empty metric store.
    pub fn new() -> Self {
        Self {
            series: FxHashMap::default(),
            label_index: FxHashMap::default(),
            label_values: FxHashMap::default(),
            interner: Rodeo::default(),
            total_samples: 0,
            series_ids: FxHashMap::default(),
            next_series_id: 0,
            normalized_name_sources: FxHashMap::default(),
        }
    }

    /// Rebuild exact series identity maps after loading older snapshots.
    pub fn rebuild_series_ids(&mut self) {
        self.series_ids.clear();
        self.next_series_id = 0;

        let mut series_ids: Vec<u64> = self.series.keys().copied().collect();
        series_ids.sort_unstable();
        for series_id in series_ids {
            if let Some(series) = self.series.get(&series_id) {
                self.series_ids.insert(series.labels.clone(), series_id);
                self.next_series_id = self.next_series_id.max(series_id.saturating_add(1));
            }
        }
    }

    /// Check for a metric-name collision without mutating store state.
    pub fn check_metric_name_collision(
        &self,
        normalized_name: &str,
        source_name: &str,
    ) -> Result<(), MetricStoreError> {
        let Some(normalized_spur) = self.interner.get(normalized_name) else {
            return Ok(());
        };

        match self.normalized_name_sources.get(&normalized_spur).copied() {
            Some(existing) if self.interner.resolve(&existing) != source_name => {
                Err(MetricStoreError::MetricNameCollision {
                    normalized: normalized_name.to_string(),
                    existing: self.interner.resolve(&existing).to_string(),
                    incoming: source_name.to_string(),
                })
            }
            _ => Ok(()),
        }
    }

    /// Register a visible metric name against its original source metric name.
    pub fn register_metric_name(
        &mut self,
        normalized_name: &str,
        source_name: &str,
    ) -> Result<(), MetricStoreError> {
        let normalized_spur = self.interner.get_or_intern(normalized_name);
        let source_spur = self.interner.get_or_intern(source_name);

        match self.normalized_name_sources.get(&normalized_spur).copied() {
            Some(existing) if existing != source_spur => {
                Err(MetricStoreError::MetricNameCollision {
                    normalized: normalized_name.to_string(),
                    existing: self.interner.resolve(&existing).to_string(),
                    incoming: source_name.to_string(),
                })
            }
            Some(_) => Ok(()),
            None => {
                self.normalized_name_sources
                    .insert(normalized_spur, source_spur);
                Ok(())
            }
        }
    }

    /// Ingest samples for a metric with given name and labels.
    pub fn ingest_samples(
        &mut self,
        name: &str,
        labels: Vec<(String, String)>,
        samples: Vec<Sample>,
    ) {
        let mut all_labels = Vec::with_capacity(labels.len() + 1);
        all_labels.push(("__name__".to_string(), name.to_string()));
        all_labels.extend(labels);

        let interned_labels = super::intern_label_pairs(&mut self.interner, &all_labels);
        super::track_label_values(&mut self.label_values, &interned_labels);

        let series_id = match self.series_ids.get(&interned_labels).copied() {
            Some(series_id) => series_id,
            None => {
                let series_id = self.next_series_id;
                self.next_series_id = self.next_series_id.saturating_add(1);
                self.series_ids.insert(interned_labels.clone(), series_id);
                self.series.insert(
                    series_id,
                    MetricSeries {
                        labels: interned_labels.clone(),
                        samples: Vec::new(),
                    },
                );
                for &(k, v) in &interned_labels {
                    self.label_index
                        .entry((k, v))
                        .or_default()
                        .insert(series_id);
                }
                series_id
            }
        };

        let Some(series) = self.series.get_mut(&series_id) else {
            tracing::warn!(
                series_id,
                "series identity index referenced missing series; skipping metric ingest"
            );
            return;
        };
        let sample_count = samples.len();
        let prev_len = series.samples.len();
        series.samples.extend(samples);
        self.total_samples += sample_count;

        // Maintain sorted order for partition_point correctness. Only inspect
        // the new batch and its boundary — O(batch), not O(total).
        let needs_sort = sample_count > 0
            && super::batch_needs_sort(&series.samples, prev_len, |s| s.timestamp_ms);
        if needs_sort {
            series.samples.sort_by_key(|s| s.timestamp_ms);
        }
    }

    /// Select series matching all the given label matchers.
    /// Matchers work the same as LogStore's label matchers.
    pub fn select_series(&self, matchers: &[LabelMatcher]) -> Vec<u64> {
        super::select_indexed_ids(
            &self.interner,
            &self.label_index,
            &self.label_values,
            || self.series.keys().copied().collect(),
            matchers,
        )
    }

    /// Get samples for a series within a time range (milliseconds).
    pub fn get_samples(&self, series_id: u64, start_ms: i64, end_ms: i64) -> &[Sample] {
        match self.series.get(&series_id) {
            Some(series) => {
                let lo = series
                    .samples
                    .partition_point(|s| s.timestamp_ms < start_ms);
                let hi = series.samples.partition_point(|s| s.timestamp_ms <= end_ms);
                &series.samples[lo..hi]
            }
            None => &[],
        }
    }

    /// Return the newest sample timestamp across all metric series.
    pub fn latest_sample_timestamp_ms(&self) -> Option<i64> {
        self.series
            .values()
            .filter_map(|series| series.samples.last().map(|sample| sample.timestamp_ms))
            .max()
    }

    /// Get labels for a series as resolved strings.
    pub fn get_series_labels(&self, series_id: u64) -> Option<Vec<(String, String)>> {
        self.series.get(&series_id).map(|series| {
            series
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

    /// Evict oldest series until series count <= max_series.
    pub fn evict_to_max(&mut self, max_series: usize) {
        if self.series.len() <= max_series {
            return;
        }

        // Sort series by oldest sample timestamp (oldest first)
        let mut series_ids: Vec<u64> = self.series.keys().copied().collect();
        series_ids.sort_by_key(|id| {
            self.series[id]
                .samples
                .first()
                .map(|s| s.timestamp_ms)
                .unwrap_or(i64::MAX)
        });

        let to_remove = self.series.len() - max_series;
        for sid in series_ids.into_iter().take(to_remove) {
            if let Some(series) = self.series.remove(&sid) {
                self.total_samples = self.total_samples.saturating_sub(series.samples.len());
                self.series_ids.remove(&series.labels);
                super::remove_from_label_indexes(
                    &mut self.label_index,
                    &mut self.label_values,
                    &series.labels,
                    sid,
                );
            }
        }
    }

    /// Evict samples older than the given timestamp.
    pub fn evict_before(&mut self, cutoff_ms: i64) {
        let mut empty_series = Vec::new();
        for (&series_id, series) in &mut self.series {
            let before = series.samples.len();
            let drain_count = series
                .samples
                .partition_point(|s| s.timestamp_ms < cutoff_ms);
            if drain_count > 0 {
                series.samples.drain(..drain_count);
            }
            let removed = before - series.samples.len();
            self.total_samples = self.total_samples.saturating_sub(removed);
            if series.samples.is_empty() {
                empty_series.push(series_id);
            }
        }

        for series_id in empty_series {
            if let Some(series) = self.series.remove(&series_id) {
                self.series_ids.remove(&series.labels);
                super::remove_from_label_indexes(
                    &mut self.label_index,
                    &mut self.label_values,
                    &series.labels,
                    series_id,
                );
            }
        }
    }

    /// Clear all data from the store.
    pub fn clear(&mut self) {
        self.series.clear();
        self.label_index.clear();
        self.label_values.clear();
        self.interner = Rodeo::default();
        self.total_samples = 0;
        self.series_ids.clear();
        self.next_series_id = 0;
        self.normalized_name_sources.clear();
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

        // Find series that have service=<value>
        let series_ids: Vec<u64> = match self.label_index.get(&(service_key, service_spur)) {
            Some(pl) => pl.ids().to_vec(),
            None => return,
        };

        for series_id in series_ids {
            if let Some(series) = self.series.remove(&series_id) {
                self.total_samples = self.total_samples.saturating_sub(series.samples.len());
                self.series_ids.remove(&series.labels);
                super::remove_from_label_indexes(
                    &mut self.label_index,
                    &mut self.label_values,
                    &series.labels,
                    series_id,
                );
            }
        }

        // Drop normalized-name registrations no longer referenced by any
        // surviving series, so a later metric with a different source name that
        // normalizes to the same key is not falsely rejected as a collision.
        if let Some(name_key) = self.interner.get("__name__") {
            let surviving: FxHashSet<Spur> = self
                .series
                .values()
                .filter_map(|s| {
                    s.labels
                        .iter()
                        .find(|(k, _)| *k == name_key)
                        .map(|(_, v)| *v)
                })
                .collect();
            self.normalized_name_sources
                .retain(|normalized, _| surviving.contains(normalized));
        }
    }
}

impl MetricStore {
    /// Estimate the memory usage of this store in bytes.
    ///
    /// Accounts for series labels, sample data, index overhead, and interner memory.
    pub fn memory_estimate_bytes(&self) -> usize {
        let mut bytes = 0usize;

        // Per-series overhead
        for series in self.series.values() {
            // Labels: SmallVec of (Spur, Spur) pairs
            bytes += series.labels.len() * std::mem::size_of::<(Spur, Spur)>();
            // Samples: Vec<Sample> capacity
            bytes += series.samples.capacity() * std::mem::size_of::<Sample>();
        }

        // HashMap overhead for series
        bytes += self.series.len()
            * (std::mem::size_of::<u64>() + std::mem::size_of::<MetricSeries>() + 8);

        // Label index: FxHashMap<(Spur, Spur), PostingList>
        for pl in self.label_index.values() {
            bytes += pl.len() * std::mem::size_of::<u64>();
        }
        bytes += self.label_index.len()
            * (std::mem::size_of::<(Spur, Spur)>() + std::mem::size_of::<PostingList>() + 8);

        // Label values index
        for vals in self.label_values.values() {
            bytes += vals.len() * (std::mem::size_of::<Spur>() + 8);
        }
        bytes += self.label_values.len() * (std::mem::size_of::<Spur>() + 8);

        // Interner memory
        bytes += self.interner.current_memory_usage();

        // Exact identity and normalized-name tracking
        bytes += self.series_ids.len()
            * (std::mem::size_of::<LabelPairs>() + std::mem::size_of::<u64>() + 8);
        bytes += self.normalized_name_sources.len() * (std::mem::size_of::<Spur>() * 2 + 8);

        bytes
    }
}

impl Default for MetricStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
