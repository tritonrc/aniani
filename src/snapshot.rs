//! Snapshot manager: serialize/deserialize stores to bincode.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::{LogStore, MetricStore, TraceStore};

/// Serializes snapshot writes so concurrent triggers — the `POST /api/v1/snapshot`
/// endpoint, the periodic timer, the Unix `SIGUSR1` handler, and graceful
/// shutdown — never race on the shared `aniani.snap.tmp` path or briefly publish a
/// partially written `aniani.snap`. The guard is held only across synchronous disk
/// I/O (never across an `.await`), and `parking_lot::Mutex` is not poisoned on panic.
static SNAPSHOT_WRITE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Snapshot data containing all three stores.
#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub log_store: LogStore,
    pub metric_store: MetricStore,
    pub trace_store: TraceStore,
}

/// Save a snapshot of all stores to disk (clones each store internally).
/// Returns the number of bytes written.
pub fn save_snapshot(
    log_store: &LogStore,
    metric_store: &MetricStore,
    trace_store: &TraceStore,
    dir: &Path,
) -> Result<usize> {
    save_snapshot_owned(
        log_store.clone(),
        metric_store.clone(),
        trace_store.clone(),
        dir,
    )
}

/// Save a snapshot from already-owned (cloned) stores.
/// Returns the number of bytes written.
pub fn save_snapshot_owned(
    log_store: LogStore,
    metric_store: MetricStore,
    trace_store: TraceStore,
    dir: &Path,
) -> Result<usize> {
    // Serialize all writers: the remove/create_new/write/rename sequence below is
    // not safe to run concurrently against a shared temp path.
    let _write_guard = SNAPSHOT_WRITE_LOCK.lock();

    fs::create_dir_all(dir)?;

    let snapshot = Snapshot {
        log_store,
        metric_store,
        trace_store,
    };

    let bytes = bincode::serialize(&snapshot)?;
    let tmp_path = dir.join("aniani.snap.tmp");
    let final_path = dir.join("aniani.snap");

    match fs::remove_file(&tmp_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut tmp_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)?;
    tmp_file.write_all(&bytes)?;
    tmp_file.sync_all()?;
    fs::rename(&tmp_path, &final_path)?;

    tracing::info!(
        "snapshot saved ({} bytes) to {}",
        bytes.len(),
        final_path.display()
    );
    Ok(bytes.len())
}

/// Load a snapshot from disk.
pub fn load_snapshot(dir: &Path) -> Result<(LogStore, MetricStore, TraceStore)> {
    let path = dir.join("aniani.snap");
    let bytes = fs::read(&path)?;
    let mut snapshot: Snapshot = bincode::deserialize(&bytes).map_err(|e| {
        anyhow::anyhow!(
            "snapshot deserialization failed (snapshot format may be incompatible with this build): {}",
            e
        )
    })?;
    snapshot.log_store.rebuild_stream_ids();
    snapshot.metric_store.rebuild_series_ids();
    tracing::info!("snapshot restored from {}", path.display());
    Ok((
        snapshot.log_store,
        snapshot.metric_store,
        snapshot.trace_store,
    ))
}

/// Clone all stores and save a snapshot, returning the number of bytes written.
/// Acquires read locks briefly to clone, then writes to disk without holding any
/// locks. Used by the cross-platform `POST /api/v1/snapshot` endpoint, which needs
/// to surface success/failure to the caller.
pub fn save_from_state_reported(
    state: &crate::store::AppState,
    dir: &std::path::Path,
) -> Result<usize> {
    let log_store = state.log_store.read().clone();
    let metric_store = state.metric_store.read().clone();
    let trace_store = state.trace_store.read().clone();
    save_snapshot_owned(log_store, metric_store, trace_store, dir)
}

/// Fire-and-forget snapshot used by the periodic timer, the Unix SIGUSR1 handler,
/// and graceful shutdown. Errors are logged rather than propagated.
pub fn save_from_state(state: &crate::store::AppState, dir: &std::path::Path) {
    if let Err(e) = save_from_state_reported(state, dir) {
        tracing::error!("snapshot failed: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::log_store::LogEntry;
    use crate::store::metric_store::Sample;
    use smallvec::SmallVec;
    use tempfile::tempdir;

    #[test]
    fn test_snapshot_roundtrip() {
        let dir = tempdir().unwrap();

        let mut log_store = LogStore::new();
        log_store.ingest_stream(
            vec![("service".into(), "test".into())],
            vec![LogEntry {
                timestamp_ns: 1000,
                line: "hello".into(),
                ingest_seq: 0,
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
            }],
        );

        let mut metric_store = MetricStore::new();
        metric_store.ingest_samples(
            "cpu",
            vec![("host".into(), "a".into())],
            vec![Sample {
                timestamp_ms: 1000,
                value: 0.5,
                ingest_seq: 0,
            }],
        );

        let trace_store = TraceStore::new();

        save_snapshot(&log_store, &metric_store, &trace_store, dir.path()).unwrap();
        let (restored_logs, restored_metrics, _restored_traces) =
            load_snapshot(dir.path()).unwrap();

        assert_eq!(restored_logs.total_entries, 1);
        assert_eq!(restored_metrics.total_samples, 1);
    }

    #[test]
    fn test_snapshot_preserves_ingest_seq() {
        let dir = tempdir().unwrap();

        let mut log_store = LogStore::new();
        log_store.ingest_stream(
            vec![("service".into(), "test".into())],
            vec![LogEntry {
                timestamp_ns: 1000,
                line: "hello".into(),
                ingest_seq: 99,
                trace_id: None,
                span_id: None,
                severity_number: 0,
                severity_text: None,
                attributes: SmallVec::new(),
            }],
        );

        let metric_store = MetricStore::new();
        let trace_store = TraceStore::new();

        save_snapshot(&log_store, &metric_store, &trace_store, dir.path()).unwrap();
        let (restored_logs, restored_metrics, restored_traces) = load_snapshot(dir.path()).unwrap();

        let restored_entry = restored_logs
            .streams
            .values()
            .flat_map(|s| s.entries.iter())
            .next()
            .expect("restored log entry");
        assert_eq!(restored_entry.ingest_seq, 99);

        assert_eq!(
            crate::store::max_ingest_seq(&restored_logs, &restored_metrics, &restored_traces),
            99
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_snapshot_temp_symlink_does_not_overwrite_target() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target_path = dir.path().join("target");
        fs::write(&target_path, b"do not overwrite").unwrap();
        symlink(&target_path, dir.path().join("aniani.snap.tmp")).unwrap();

        let log_store = LogStore::new();
        let metric_store = MetricStore::new();
        let trace_store = TraceStore::new();

        save_snapshot(&log_store, &metric_store, &trace_store, dir.path()).unwrap();

        assert_eq!(fs::read(&target_path).unwrap(), b"do not overwrite");
        assert!(dir.path().join("aniani.snap").exists());
    }
}
