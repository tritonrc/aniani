//! OTLP/gRPC ingestion.
//!
//! Serves the three OTLP collector services (logs, metrics, traces) over gRPC,
//! multiplexed onto the same listener as the HTTP/REST surface (cleartext
//! HTTP/2). The gRPC handlers decode nothing extra — tonic hands them an
//! already-decoded `Export*ServiceRequest`, which is passed straight to the
//! shared, transport-free ingest functions in [`crate::ingest`].

use opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::{
    LogsService, LogsServiceServer,
};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::{
    MetricsService, MetricsServiceServer,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use tonic::codec::CompressionEncoding;
use tonic::{Request, Response, Status};

use crate::ingest::{MAX_DECOMPRESSED_SIZE, otlp_logs, otlp_metrics, otlp_traces};
use crate::store::SharedState;

/// OTLP/gRPC metrics collector.
#[derive(Clone)]
struct MetricsCollector {
    state: SharedState,
}

#[tonic::async_trait]
impl MetricsService for MetricsCollector {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        otlp_metrics::ingest_metrics(&self.state, request.into_inner()).map_err(|e| {
            tracing::warn!("rejecting OTLP/gRPC metric ingest: {}", e);
            Status::invalid_argument(e.to_string())
        })?;
        Ok(Response::new(ExportMetricsServiceResponse::default()))
    }
}

/// OTLP/gRPC logs collector.
#[derive(Clone)]
struct LogsCollector {
    state: SharedState,
}

#[tonic::async_trait]
impl LogsService for LogsCollector {
    async fn export(
        &self,
        request: Request<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        otlp_logs::ingest_logs(&self.state, request.into_inner());
        Ok(Response::new(ExportLogsServiceResponse::default()))
    }
}

/// OTLP/gRPC traces collector.
#[derive(Clone)]
struct TraceCollector {
    state: SharedState,
}

#[tonic::async_trait]
impl TraceService for TraceCollector {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        otlp_traces::ingest_traces(&self.state, request.into_inner());
        Ok(Response::new(ExportTraceServiceResponse::default()))
    }
}

/// Build an axum router serving the three OTLP/gRPC collector services.
///
/// Merged into the main HTTP router so gRPC and REST share one listener. Each
/// service accepts gzip-compressed requests and uses the same 64 MiB decode
/// limit as the OTLP/HTTP path (the HTTP body-limit layer does not apply to
/// gRPC). The services are mounted as explicit routes; the caller is
/// responsible for the router's fallback so unknown HTTP paths still 404.
pub fn routes(state: SharedState) -> axum::Router {
    let metrics = MetricsServiceServer::new(MetricsCollector {
        state: state.clone(),
    })
    .accept_compressed(CompressionEncoding::Gzip)
    .send_compressed(CompressionEncoding::Gzip)
    .max_decoding_message_size(MAX_DECOMPRESSED_SIZE);

    let logs = LogsServiceServer::new(LogsCollector {
        state: state.clone(),
    })
    .accept_compressed(CompressionEncoding::Gzip)
    .send_compressed(CompressionEncoding::Gzip)
    .max_decoding_message_size(MAX_DECOMPRESSED_SIZE);

    let traces = TraceServiceServer::new(TraceCollector { state })
        .accept_compressed(CompressionEncoding::Gzip)
        .send_compressed(CompressionEncoding::Gzip)
        .max_decoding_message_size(MAX_DECOMPRESSED_SIZE);

    tonic::service::Routes::new(metrics)
        .add_service(logs)
        .add_service(traces)
        .into_axum_router()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::{AppState, LogStore, MetricStore, TraceStore};
    use clap::Parser;
    use opentelemetry_proto::tonic::metrics::v1::{
        Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
    };
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::time::Instant;

    fn test_state() -> SharedState {
        Arc::new(AppState {
            log_store: RwLock::new(LogStore::new()),
            metric_store: RwLock::new(MetricStore::new()),
            trace_store: RwLock::new(TraceStore::new()),
            config: Config::parse_from(["aniani"]),
            start_time: Instant::now(),
        })
    }

    fn gauge_request(metric_name: &str) -> ExportMetricsServiceRequest {
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: None,
                scope_metrics: vec![ScopeMetrics {
                    scope: None,
                    metrics: vec![Metric {
                        name: metric_name.into(),
                        description: String::new(),
                        unit: String::new(),
                        metadata: vec![],
                        data: Some(metric::Data::Gauge(Gauge {
                            data_points: vec![NumberDataPoint {
                                attributes: vec![],
                                start_time_unix_nano: 0,
                                time_unix_nano: 1_000_000,
                                exemplars: vec![],
                                flags: 0,
                                value: Some(number_data_point::Value::AsDouble(1.0)),
                            }],
                        })),
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        }
    }

    /// A successful metrics export returns Ok, and a normalized-name collision
    /// (two source names mapping to the same normalized name) is surfaced as a
    /// gRPC `InvalidArgument` status rather than a panic or `Internal`.
    #[tokio::test]
    async fn test_metrics_name_collision_maps_to_invalid_argument() {
        let collector = MetricsCollector {
            state: test_state(),
        };

        // `http.requests` normalizes to `http_requests`.
        collector
            .export(Request::new(gauge_request("http.requests")))
            .await
            .expect("first export should succeed");

        // `http_requests` normalizes to the same name from a different source.
        let status = collector
            .export(Request::new(gauge_request("http_requests")))
            .await
            .expect_err("colliding source name should be rejected");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }
}
