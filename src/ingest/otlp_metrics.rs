//! OTLP/HTTP metrics ingestion handler.
//!
//! Decodes `ExportMetricsServiceRequest` protobuf and stores metrics.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::metrics::v1::metric::Data;
use prost::Message;
use std::sync::atomic::Ordering;

use super::label::{extract_resource_labels, promote_service_name};
use super::{decode_body, is_json_content_type, u64_to_i64_saturating};
use crate::store::metric_store::{MetricStoreError, Sample};
use crate::store::{AppState, SharedState};

/// Accepted-count summary returned by [`ingest_metrics`].
#[derive(Debug, Clone, Copy)]
pub struct MetricsAccepted {
    pub series: usize,
    pub samples: usize,
}

/// Handler for POST /v1/metrics.
///
/// Accepts both protobuf (`application/x-protobuf`, default) and JSON
/// (`application/json`) encoded `ExportMetricsServiceRequest` bodies.
pub async fn metrics_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let body = match decode_body(&headers, &body) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("failed to decode OTLP metrics body: {}", e);
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    let request = if is_json_content_type(&headers) {
        match serde_json::from_slice::<ExportMetricsServiceRequest>(body.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to decode OTLP metrics JSON: {}", e);
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
    } else {
        match ExportMetricsServiceRequest::decode(body.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to decode OTLP metrics: {}", e);
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
    };

    match ingest_metrics(&state, request) {
        Ok(accepted) => Json(serde_json::json!({
            "accepted": {
                "series": accepted.series,
                "samples": accepted.samples,
            }
        }))
        .into_response(),
        Err(e) => {
            tracing::warn!("rejecting OTLP metric ingest: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// Ingest a decoded `ExportMetricsServiceRequest` into the metric store.
///
/// Transport-agnostic: shared by the OTLP/HTTP handler and the OTLP/gRPC
/// service. Returns accepted series/sample counts, or a [`MetricStoreError`]
/// when a normalized metric name collides with a different source name.
pub fn ingest_metrics(
    state: &AppState,
    request: ExportMetricsServiceRequest,
) -> Result<MetricsAccepted, MetricStoreError> {
    type MetricData = (String, String, Vec<(String, String)>, Vec<Sample>);
    let mut prepared: Vec<MetricData> = Vec::new();

    for resource_metrics in &request.resource_metrics {
        let mut resource_labels = extract_resource_labels(&resource_metrics.resource);
        promote_service_name(&mut resource_labels);

        for scope_metrics in &resource_metrics.scope_metrics {
            for metric in &scope_metrics.metrics {
                // Normalize OTLP metric names: dots to underscores for PromQL compatibility.
                // OTLP uses dots (e.g. http.server.duration), PromQL grammar rejects dots.
                let metric_name = metric.name.replace('.', "_");

                match &metric.data {
                    Some(Data::Gauge(gauge)) => {
                        for dp in &gauge.data_points {
                            let labels = build_dp_labels(&resource_labels, &dp.attributes);
                            let value = extract_number_value(dp);
                            let ts_ms = u64_to_i64_saturating(dp.time_unix_nano) / 1_000_000;
                            prepared.push((
                                metric_name.clone(),
                                metric.name.clone(),
                                labels,
                                vec![Sample {
                                    timestamp_ms: ts_ms,
                                    value,
                                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                                }],
                            ));
                        }
                    }
                    Some(Data::Sum(sum)) => {
                        for dp in &sum.data_points {
                            let labels = build_dp_labels(&resource_labels, &dp.attributes);
                            let value = extract_number_value(dp);
                            let ts_ms = u64_to_i64_saturating(dp.time_unix_nano) / 1_000_000;
                            prepared.push((
                                metric_name.clone(),
                                metric.name.clone(),
                                labels,
                                vec![Sample {
                                    timestamp_ms: ts_ms,
                                    value,
                                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                                }],
                            ));
                        }
                    }
                    Some(Data::Histogram(hist)) => {
                        for dp in &hist.data_points {
                            let base_labels = build_dp_labels(&resource_labels, &dp.attributes);
                            let ts_ms = u64_to_i64_saturating(dp.time_unix_nano) / 1_000_000;

                            // Store each bucket as a separate series with `le` label
                            let mut cumulative_count: u64 = 0;
                            for (i, &count) in dp.bucket_counts.iter().enumerate() {
                                cumulative_count += count;
                                let le = if i < dp.explicit_bounds.len() {
                                    format!("{}", dp.explicit_bounds[i])
                                } else {
                                    "+Inf".to_string()
                                };
                                let mut labels = base_labels.clone();
                                labels.push(("le".to_string(), le));
                                let bucket_name = format!("{}_bucket", metric_name);
                                prepared.push((
                                    bucket_name,
                                    format!("{}_bucket", metric.name),
                                    labels,
                                    vec![Sample {
                                        timestamp_ms: ts_ms,
                                        value: cumulative_count as f64,
                                        ingest_seq: state
                                            .ingest_seq
                                            .fetch_add(1, Ordering::Relaxed),
                                    }],
                                ));
                            }

                            // Store _sum and _count
                            prepared.push((
                                format!("{}_sum", metric_name),
                                format!("{}_sum", metric.name),
                                base_labels.clone(),
                                vec![Sample {
                                    timestamp_ms: ts_ms,
                                    value: dp.sum.unwrap_or(0.0),
                                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                                }],
                            ));
                            prepared.push((
                                format!("{}_count", metric_name),
                                format!("{}_count", metric.name),
                                base_labels,
                                vec![Sample {
                                    timestamp_ms: ts_ms,
                                    value: dp.count as f64,
                                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                                }],
                            ));
                        }
                    }
                    Some(Data::ExponentialHistogram(_)) => {
                        tracing::warn!(
                            metric = metric_name.as_str(),
                            "skipping ExponentialHistogram — not supported"
                        );
                    }
                    Some(Data::Summary(summary)) => {
                        for dp in &summary.data_points {
                            let base_labels = build_dp_labels(&resource_labels, &dp.attributes);
                            let ts_ms = u64_to_i64_saturating(dp.time_unix_nano) / 1_000_000;

                            // Store each quantile as a separate series
                            for qv in &dp.quantile_values {
                                let mut labels = base_labels.clone();
                                labels.push(("quantile".to_string(), format!("{}", qv.quantile)));
                                prepared.push((
                                    metric_name.clone(),
                                    metric.name.clone(),
                                    labels,
                                    vec![Sample {
                                        timestamp_ms: ts_ms,
                                        value: qv.value,
                                        ingest_seq: state
                                            .ingest_seq
                                            .fetch_add(1, Ordering::Relaxed),
                                    }],
                                ));
                            }

                            // Store _sum and _count
                            prepared.push((
                                format!("{}_sum", metric_name),
                                format!("{}_sum", metric.name),
                                base_labels.clone(),
                                vec![Sample {
                                    timestamp_ms: ts_ms,
                                    value: dp.sum,
                                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                                }],
                            ));
                            prepared.push((
                                format!("{}_count", metric_name),
                                format!("{}_count", metric.name),
                                base_labels,
                                vec![Sample {
                                    timestamp_ms: ts_ms,
                                    value: dp.count as f64,
                                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                                }],
                            ));
                        }
                    }
                    _ => {
                        tracing::debug!("unsupported metric type for {}", metric_name);
                    }
                }
            }
        }
    }

    let series_count = prepared.len();
    let sample_count: usize = prepared
        .iter()
        .map(|(_, _, _, samples)| samples.len())
        .sum();

    let mut store = state.metric_store.write();
    for (name, source_name, _, _) in &prepared {
        store.check_metric_name_collision(name, source_name)?;
    }
    for (name, source_name, _, _) in &prepared {
        store.register_metric_name(name, source_name)?;
    }
    for (name, _, labels, samples) in prepared {
        store.ingest_samples(&name, labels, samples);
    }

    Ok(MetricsAccepted {
        series: series_count,
        samples: sample_count,
    })
}

fn build_dp_labels(
    resource_labels: &[(String, String)],
    attrs: &[opentelemetry_proto::tonic::common::v1::KeyValue],
) -> Vec<(String, String)> {
    if attrs.is_empty() {
        return resource_labels.to_vec();
    }
    let mut labels = resource_labels.to_vec();
    for attr in attrs {
        if let Some(val) = &attr.value
            && let Some(s) = super::label::any_value_to_string(val)
        {
            labels.push((attr.key.clone(), s));
        }
    }
    labels
}

fn extract_number_value(dp: &opentelemetry_proto::tonic::metrics::v1::NumberDataPoint) -> f64 {
    use opentelemetry_proto::tonic::metrics::v1::number_data_point::Value;
    match &dp.value {
        Some(Value::AsDouble(d)) => *d,
        Some(Value::AsInt(i)) => *i as f64,
        None => 0.0,
    }
}
