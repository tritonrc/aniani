//! Aniani: Lightweight ephemeral observability engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use parking_lot::RwLock;
use tokio::net::TcpListener;

use aniani::config::Config;
use aniani::snapshot;
use aniani::store::{self, AppState, LogStore, MetricStore, SharedState, TraceStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse();

    if config.bind_address != "127.0.0.1"
        && config.bind_address != "::1"
        && config.bind_address != "localhost"
    {
        tracing::warn!(
            bind_address = %config.bind_address,
            "binding beyond loopback exposes an unauthenticated observability service; use trusted network controls"
        );
    }

    // Initialize stores
    let (log_store, metric_store, trace_store) = if config.restore {
        let snap_dir = PathBuf::from(&config.snapshot_dir);
        match snapshot::load_snapshot(&snap_dir) {
            Ok((ls, ms, ts)) => {
                tracing::info!("restored from snapshot");
                (ls, ms, ts)
            }
            Err(e) => {
                tracing::warn!("failed to restore snapshot: {}, starting fresh", e);
                (LogStore::new(), MetricStore::new(), TraceStore::new())
            }
        }
    } else {
        (LogStore::new(), MetricStore::new(), TraceStore::new())
    };

    let state: SharedState = Arc::new(AppState {
        log_store: RwLock::new(log_store),
        metric_store: RwLock::new(metric_store),
        trace_store: RwLock::new(trace_store),
        config: config.clone(),
        start_time: Instant::now(),
    });

    // Start eviction background task
    let eviction_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let state = eviction_state.clone();
            tokio::task::spawn_blocking(move || {
                store::run_eviction(&state);
            })
            .await
            .ok();
        }
    });

    // Start snapshot timer if configured
    if config.snapshot_interval > 0 {
        let snap_state = state.clone();
        let snap_dir = PathBuf::from(&config.snapshot_dir);
        let snap_interval = config.snapshot_interval;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(snap_interval));
            loop {
                interval.tick().await;
                snapshot::save_from_state(&snap_state, &snap_dir);
            }
        });
    }

    // Register SIGUSR1 handler for on-demand snapshots (Unix only)
    #[cfg(unix)]
    {
        let sig_state = state.clone();
        let sig_dir = PathBuf::from(&config.snapshot_dir);
        tokio::spawn(async move {
            let mut signal =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
                    .expect("failed to register SIGUSR1 handler");
            loop {
                signal.recv().await;
                tracing::info!("SIGUSR1 received, saving snapshot");
                snapshot::save_from_state(&sig_state, &sig_dir);
            }
        });
    }

    // Build router and start server
    let app = aniani::server::build_router(state.clone());
    let addr = format!("{}:{}", config.bind_address, config.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("aniani listening on {}", addr);

    // Graceful shutdown: wait for a termination signal, save snapshot, then exit.
    let shutdown_state = state.clone();
    let shutdown_snap_dir = PathBuf::from(&config.snapshot_dir);
    let shutdown_signal = async move {
        #[cfg(unix)]
        {
            let ctrl_c = tokio::signal::ctrl_c();
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {
                    tracing::info!("SIGINT received, shutting down");
                }
                _ = sigterm.recv() => {
                    tracing::info!("SIGTERM received, shutting down");
                }
            }
        }

        // On Windows, Ctrl-C alone misses the most common termination paths
        // (closing the console window, logoff, OS shutdown), so select over the
        // full set of console control events to give the shutdown snapshot a
        // chance to run. Note: Windows grants close/logoff/shutdown handlers only
        // a brief grace window, so for large stores prefer --snapshot-interval.
        #[cfg(windows)]
        {
            use tokio::signal::windows;
            let mut ctrl_c = windows::ctrl_c().expect("failed to register Ctrl-C handler");
            let mut ctrl_break =
                windows::ctrl_break().expect("failed to register Ctrl-Break handler");
            let mut ctrl_close =
                windows::ctrl_close().expect("failed to register console-close handler");
            let mut ctrl_logoff =
                windows::ctrl_logoff().expect("failed to register logoff handler");
            let mut ctrl_shutdown =
                windows::ctrl_shutdown().expect("failed to register shutdown handler");
            tokio::select! {
                _ = ctrl_c.recv() => tracing::info!("Ctrl-C received, shutting down"),
                _ = ctrl_break.recv() => tracing::info!("Ctrl-Break received, shutting down"),
                _ = ctrl_close.recv() => tracing::info!("console close received, shutting down"),
                _ = ctrl_logoff.recv() => tracing::info!("logoff received, shutting down"),
                _ = ctrl_shutdown.recv() => tracing::info!("system shutdown received, shutting down"),
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("ctrl-c received, shutting down");
        }

        tracing::info!("saving shutdown snapshot");
        snapshot::save_from_state(&shutdown_state, &shutdown_snap_dir);
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;

    Ok(())
}
