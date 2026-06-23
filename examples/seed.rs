//! Seed a running Aniani instance with a realistic, multi-service trace set
//! (plus a little logs/metrics) so the web UI has something to explore.
//!
//! Usage:
//!   cargo run --example seed                 # targets http://127.0.0.1:4320
//!   cargo run --example seed -- http://host:port
//!
//! The traces model an e-commerce checkout fanning out across seven services
//! (frontend → gateway → auth/inventory/payments → db/cache/stripe), with deep
//! nesting, cross-service CLIENT/SERVER pairs, and one failing checkout whose
//! Stripe call records an exception event — exactly the shape the Jaeger-style
//! trace view is built to drill into.

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::metrics::v1::{
    Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::span::Event;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

/// OTLP span kinds used below.
const SERVER: i32 = 2;
const CLIENT: i32 = 3;

/// A declarative span used to assemble a trace. Times are millisecond offsets
/// from the trace's base timestamp; `parent == 0` marks the root.
struct SpanSpec {
    service: &'static str,
    name: &'static str,
    kind: i32,
    id: u8,
    parent: u8,
    start_ms: i64,
    dur_ms: i64,
    error: bool,
    attrs: &'static [(&'static str, &'static str)],
    /// (type, message, stacktrace) recorded as an `exception` event.
    exception: Option<(&'static str, &'static str, &'static str)>,
}

fn kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.into())),
        }),
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

fn otlp_span(trace_id: &[u8; 16], s: &SpanSpec, base_ns: u64) -> Span {
    let start = base_ns + s.start_ms.max(0) as u64 * 1_000_000;
    let end = start + s.dur_ms.max(0) as u64 * 1_000_000;
    let attributes: Vec<KeyValue> = s.attrs.iter().map(|(k, v)| kv(k, v)).collect();
    let events = match s.exception {
        Some((ty, msg, stack)) => vec![Event {
            time_unix_nano: start + (s.dur_ms.max(0) as u64 / 2) * 1_000_000,
            name: "exception".into(),
            attributes: vec![
                kv("exception.type", ty),
                kv("exception.message", msg),
                kv("exception.stacktrace", stack),
            ],
            dropped_attributes_count: 0,
        }],
        None => vec![],
    };
    Span {
        trace_id: trace_id.to_vec(),
        span_id: [s.id; 8].to_vec(),
        trace_state: String::new(),
        parent_span_id: if s.parent == 0 {
            vec![]
        } else {
            [s.parent; 8].to_vec()
        },
        name: s.name.into(),
        kind: s.kind,
        start_time_unix_nano: start,
        end_time_unix_nano: end,
        attributes,
        dropped_attributes_count: 0,
        events,
        dropped_events_count: 0,
        links: vec![],
        dropped_links_count: 0,
        status: Some(Status {
            message: String::new(),
            code: if s.error { 2 } else { 1 },
        }),
        flags: 0,
    }
}

/// Assemble specs into an OTLP request, grouping spans by service (one
/// `ResourceSpans` per service, preserving first-seen order).
fn build_trace(trace_id: &[u8; 16], base_ns: u64, specs: &[SpanSpec]) -> ExportTraceServiceRequest {
    let mut order: Vec<&'static str> = Vec::new();
    let mut by_service: Vec<(&'static str, Vec<Span>)> = Vec::new();
    for spec in specs {
        if !order.contains(&spec.service) {
            order.push(spec.service);
            by_service.push((spec.service, Vec::new()));
        }
        let slot = by_service
            .iter_mut()
            .find(|(s, _)| *s == spec.service)
            .unwrap();
        slot.1.push(otlp_span(trace_id, spec, base_ns));
    }

    let resource_spans = by_service
        .into_iter()
        .map(|(service, spans)| ResourceSpans {
            resource: Some(Resource {
                attributes: vec![kv("service.name", service)],
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        })
        .collect();

    ExportTraceServiceRequest { resource_spans }
}

/// The checkout fan-out. `failed` flips the payment path (and its ancestors) to
/// error and attaches an exception to the Stripe call.
fn checkout_specs(failed: bool) -> Vec<SpanSpec> {
    let stack = "Traceback (most recent call last):\n  File \"payments/stripe.py\", line 88, in charge\n    resp = client.charges.create(**payload)\n  File \"stripe/api.py\", line 142, in create\n    raise TimeoutError(\"upstream timed out after 5000ms\")\nTimeoutError: upstream timed out after 5000ms";
    vec![
        SpanSpec {
            service: "frontend",
            name: "POST /checkout",
            kind: SERVER,
            id: 1,
            parent: 0,
            start_ms: 0,
            dur_ms: 920,
            error: failed,
            attrs: &[
                ("http.method", "POST"),
                ("http.route", "/checkout"),
                ("http.status_code", "200"),
                ("client.id", "web-spa"),
            ],
            exception: None,
        },
        SpanSpec {
            service: "gateway",
            name: "POST /checkout",
            kind: SERVER,
            id: 2,
            parent: 1,
            start_ms: 6,
            dur_ms: 900,
            error: failed,
            attrs: &[("http.method", "POST"), ("http.target", "/checkout")],
            exception: None,
        },
        SpanSpec {
            service: "gateway",
            name: "auth.verify",
            kind: CLIENT,
            id: 3,
            parent: 2,
            start_ms: 12,
            dur_ms: 55,
            error: false,
            attrs: &[("rpc.system", "grpc"), ("peer.service", "auth")],
            exception: None,
        },
        SpanSpec {
            service: "auth",
            name: "verify_token",
            kind: SERVER,
            id: 4,
            parent: 3,
            start_ms: 14,
            dur_ms: 50,
            error: false,
            attrs: &[("auth.method", "jwt"), ("user.tier", "premium")],
            exception: None,
        },
        SpanSpec {
            service: "auth",
            name: "cache.get",
            kind: CLIENT,
            id: 5,
            parent: 4,
            start_ms: 16,
            dur_ms: 9,
            error: false,
            attrs: &[("db.system", "redis"), ("peer.service", "cache")],
            exception: None,
        },
        SpanSpec {
            service: "cache",
            name: "GET session",
            kind: SERVER,
            id: 6,
            parent: 5,
            start_ms: 17,
            dur_ms: 6,
            error: false,
            attrs: &[("cache.hit", "true")],
            exception: None,
        },
        SpanSpec {
            service: "gateway",
            name: "inventory.check",
            kind: CLIENT,
            id: 7,
            parent: 2,
            start_ms: 72,
            dur_ms: 185,
            error: false,
            attrs: &[("rpc.system", "grpc"), ("peer.service", "inventory")],
            exception: None,
        },
        SpanSpec {
            service: "inventory",
            name: "check_stock",
            kind: SERVER,
            id: 8,
            parent: 7,
            start_ms: 75,
            dur_ms: 178,
            error: false,
            attrs: &[("sku", "WIDGET-42"), ("qty", "3")],
            exception: None,
        },
        SpanSpec {
            service: "inventory",
            name: "SELECT stock",
            kind: CLIENT,
            id: 9,
            parent: 8,
            start_ms: 82,
            dur_ms: 160,
            error: false,
            attrs: &[("db.system", "postgresql"), ("peer.service", "db")],
            exception: None,
        },
        SpanSpec {
            service: "db",
            name: "query stock",
            kind: SERVER,
            id: 10,
            parent: 9,
            start_ms: 85,
            dur_ms: 150,
            error: false,
            attrs: &[
                ("db.system", "postgresql"),
                ("db.statement", "SELECT qty FROM stock WHERE sku = $1"),
                ("db.rows", "1"),
            ],
            exception: None,
        },
        SpanSpec {
            service: "gateway",
            name: "payments.charge",
            kind: CLIENT,
            id: 11,
            parent: 2,
            start_ms: 264,
            dur_ms: 620,
            error: failed,
            attrs: &[("rpc.system", "grpc"), ("peer.service", "payments")],
            exception: None,
        },
        SpanSpec {
            service: "payments",
            name: "charge_card",
            kind: SERVER,
            id: 12,
            parent: 11,
            start_ms: 268,
            dur_ms: 610,
            error: failed,
            attrs: &[("payment.amount", "129.99"), ("payment.currency", "USD")],
            exception: None,
        },
        SpanSpec {
            service: "payments",
            name: "INSERT payment",
            kind: CLIENT,
            id: 13,
            parent: 12,
            start_ms: 272,
            dur_ms: 55,
            error: false,
            attrs: &[("db.system", "postgresql"), ("peer.service", "db")],
            exception: None,
        },
        SpanSpec {
            service: "db",
            name: "insert payment",
            kind: SERVER,
            id: 14,
            parent: 13,
            start_ms: 275,
            dur_ms: 48,
            error: false,
            attrs: &[
                ("db.system", "postgresql"),
                ("db.statement", "INSERT INTO payments (...) VALUES (...)"),
            ],
            exception: None,
        },
        SpanSpec {
            service: "payments",
            name: "stripe.charge",
            kind: CLIENT,
            id: 15,
            parent: 12,
            start_ms: 340,
            dur_ms: if failed { 530 } else { 360 },
            error: failed,
            attrs: &[
                ("peer.service", "stripe"),
                ("http.url", "https://api.stripe.com/v1/charges"),
                ("http.method", "POST"),
            ],
            exception: if failed {
                Some(("TimeoutError", "upstream timed out after 5000ms", stack))
            } else {
                None
            },
        },
    ]
}

/// A shorter product-listing trace, for variety in the search results.
fn products_specs() -> Vec<SpanSpec> {
    vec![
        SpanSpec {
            service: "frontend",
            name: "GET /products",
            kind: SERVER,
            id: 1,
            parent: 0,
            start_ms: 0,
            dur_ms: 140,
            error: false,
            attrs: &[("http.method", "GET"), ("http.route", "/products")],
            exception: None,
        },
        SpanSpec {
            service: "gateway",
            name: "GET /products",
            kind: SERVER,
            id: 2,
            parent: 1,
            start_ms: 4,
            dur_ms: 132,
            error: false,
            attrs: &[("http.method", "GET")],
            exception: None,
        },
        SpanSpec {
            service: "inventory",
            name: "list_products",
            kind: SERVER,
            id: 3,
            parent: 2,
            start_ms: 10,
            dur_ms: 120,
            error: false,
            attrs: &[("page.size", "24")],
            exception: None,
        },
        SpanSpec {
            service: "inventory",
            name: "SELECT products",
            kind: CLIENT,
            id: 4,
            parent: 3,
            start_ms: 16,
            dur_ms: 100,
            error: false,
            attrs: &[("db.system", "postgresql"), ("peer.service", "db")],
            exception: None,
        },
        SpanSpec {
            service: "db",
            name: "query products",
            kind: SERVER,
            id: 5,
            parent: 4,
            start_ms: 20,
            dur_ms: 92,
            error: false,
            attrs: &[
                ("db.system", "postgresql"),
                ("db.statement", "SELECT * FROM products LIMIT 24"),
                ("db.rows", "24"),
            ],
            exception: None,
        },
    ]
}

/// Build an 8-byte span id from a counter.
fn id8(n: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[4..].copy_from_slice(&n.to_be_bytes());
    b
}

#[allow(clippy::too_many_arguments)]
fn owned_span(
    trace_id: &[u8; 16],
    span_id: [u8; 8],
    parent: Option<[u8; 8]>,
    name: String,
    kind: i32,
    start_ns: u64,
    dur_ns: u64,
    attrs: Vec<KeyValue>,
) -> Span {
    Span {
        trace_id: trace_id.to_vec(),
        span_id: span_id.to_vec(),
        trace_state: String::new(),
        parent_span_id: parent.map(|p| p.to_vec()).unwrap_or_default(),
        name,
        kind,
        start_time_unix_nano: start_ns,
        end_time_unix_nano: start_ns + dur_ns,
        attributes: attrs,
        dropped_attributes_count: 0,
        events: vec![],
        dropped_events_count: 0,
        links: vec![],
        dropped_links_count: 0,
        status: Some(Status {
            message: String::new(),
            code: 1,
        }),
        flags: 0,
    }
}

fn push_span(by_service: &mut Vec<(String, Vec<Span>)>, service: &str, span: Span) {
    if let Some(slot) = by_service.iter_mut().find(|(s, _)| s == service) {
        slot.1.push(span);
    } else {
        by_service.push((service.to_string(), vec![span]));
    }
}

/// A wide "dashboard" trace: the gateway fans out to many widget data sources,
/// each hitting a backend service and its datastore. Produces a tall waterfall
/// (2 + widgets*2 spans) for exercising long timelines and the sticky ruler.
fn big_dashboard_trace(
    trace_id: &[u8; 16],
    base_ns: u64,
    widgets: usize,
) -> ExportTraceServiceRequest {
    let backends = [
        "search",
        "recommendations",
        "ads",
        "profile",
        "reviews",
        "pricing",
    ];
    let ms = 1_000_000u64;
    let mut by_service: Vec<(String, Vec<Span>)> = Vec::new();

    let root_id = id8(1);
    let gw_id = id8(2);
    push_span(
        &mut by_service,
        "frontend",
        owned_span(
            trace_id,
            root_id,
            None,
            "GET /dashboard".into(),
            SERVER,
            base_ns,
            1480 * ms,
            vec![kv("http.route", "/dashboard")],
        ),
    );
    push_span(
        &mut by_service,
        "gateway",
        owned_span(
            trace_id,
            gw_id,
            Some(root_id),
            "GET /dashboard".into(),
            SERVER,
            base_ns + 5 * ms,
            1460 * ms,
            vec![],
        ),
    );

    let mut counter = 3u32;
    for i in 0..widgets {
        let backend = backends[i % backends.len()];
        let start = base_ns + (20 + i as u64 * 52) * ms;
        let dur = (180 + (i % 4) as u64 * 130) * ms;
        let svc_id = id8(counter);
        let db_id = id8(counter + 1);
        counter += 2;
        push_span(
            &mut by_service,
            backend,
            owned_span(
                trace_id,
                svc_id,
                Some(gw_id),
                format!("fetch widget {}", i + 1),
                SERVER,
                start,
                dur,
                vec![kv("widget.index", &(i + 1).to_string())],
            ),
        );
        push_span(
            &mut by_service,
            "db",
            owned_span(
                trace_id,
                db_id,
                Some(svc_id),
                format!("SELECT {}", backend),
                CLIENT,
                start + 15 * ms,
                dur.saturating_sub(40 * ms),
                vec![kv("db.system", "postgresql"), kv("peer.service", "db")],
            ),
        );
    }

    let resource_spans = by_service
        .into_iter()
        .map(|(service, spans)| ResourceSpans {
            resource: Some(Resource {
                attributes: vec![kv("service.name", &service)],
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        })
        .collect();
    ExportTraceServiceRequest { resource_spans }
}

async fn post_proto(client: &reqwest::Client, url: &str, body: Vec<u8>) -> anyhow::Result<()> {
    let resp = client
        .post(url)
        .header("content-type", "application/x-protobuf")
        .body(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("{} -> {}", url, resp.status());
    }
    Ok(())
}

fn gauge_request(
    service: &str,
    metric_name: &str,
    value: f64,
    ts_ns: u64,
) -> ExportMetricsServiceRequest {
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(Resource {
                attributes: vec![kv("service.name", service)],
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
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
                            time_unix_nano: ts_ns,
                            exemplars: vec![],
                            flags: 0,
                            value: Some(number_data_point::Value::AsDouble(value)),
                        }],
                    })),
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:4320".to_string());
    let base = base.trim_end_matches('/').to_string();
    let client = reqwest::Client::new();
    let now = now_ns();
    let ms = 1_000_000u64;

    // --- Traces: four traces spread across the last few minutes. ---
    let traces: Vec<(&str, [u8; 16], u64, Vec<SpanSpec>)> = vec![
        (
            "checkout (ok)",
            [0x11; 16],
            now - 300_000 * ms,
            checkout_specs(false),
        ),
        (
            "checkout (ok)",
            [0x22; 16],
            now - 180_000 * ms,
            checkout_specs(false),
        ),
        (
            "checkout (failed)",
            [0x33; 16],
            now - 90_000 * ms,
            checkout_specs(true),
        ),
        ("products", [0x44; 16], now - 45_000 * ms, products_specs()),
    ];
    let mut span_total = 0;
    for (label, tid, base_ns, specs) in &traces {
        span_total += specs.len();
        let req = build_trace(tid, *base_ns, specs);
        post_proto(&client, &format!("{}/v1/traces", base), req.encode_to_vec()).await?;
        println!("  trace {:>18}  {} spans", label, specs.len());
    }

    // A wide fan-out trace (tall waterfall) to exercise long timelines.
    let big = big_dashboard_trace(&[0x55; 16], now - 30_000 * ms, 26);
    let big_spans: usize = big
        .resource_spans
        .iter()
        .flat_map(|rs| rs.scope_spans.iter())
        .map(|ss| ss.spans.len())
        .sum();
    span_total += big_spans;
    post_proto(&client, &format!("{}/v1/traces", base), big.encode_to_vec()).await?;
    println!("  trace {:>18}  {} spans", "dashboard (wide)", big_spans);

    // --- Metrics: a couple of gauges per service so the Metrics tab is live. ---
    let metrics: &[(&str, &str, f64)] = &[
        ("gateway", "http_request_duration_ms", 142.0),
        ("payments", "http_request_duration_ms", 610.0),
        ("inventory", "stock_level", 318.0),
        ("db", "db_query_duration_ms", 95.0),
    ];
    for (svc, name, val) in metrics {
        let req = gauge_request(svc, name, *val, now);
        post_proto(
            &client,
            &format!("{}/v1/metrics", base),
            req.encode_to_vec(),
        )
        .await?;
    }

    // --- Logs: a few lines via the Loki push API, including one error. ---
    let logs = json!({
        "streams": [
            {
                "stream": {"service": "payments", "level": "error"},
                "values": [[ (now - 90_000 * ms).to_string(), "stripe.charge failed: upstream timed out after 5000ms" ]]
            },
            {
                "stream": {"service": "gateway", "level": "info"},
                "values": [
                    [ (now - 300_000 * ms).to_string(), "POST /checkout 200 in 906ms" ],
                    [ (now - 45_000 * ms).to_string(), "GET /products 200 in 136ms" ]
                ]
            },
            {
                "stream": {"service": "inventory", "level": "warn"},
                "values": [[ (now - 180_000 * ms).to_string(), "stock low for sku WIDGET-42" ]]
            }
        ]
    });
    let resp = client
        .post(format!("{}/loki/api/v1/push", base))
        .header("content-type", "application/json")
        .body(logs.to_string())
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("loki push -> {}", resp.status());
    }

    println!(
        "\nSeeded {} traces ({} spans), {} metrics, logs across 3 services.",
        traces.len(),
        span_total,
        metrics.len()
    );
    println!(
        "Open the Traces tab at {}/ui and run a query (e.g. a service chip).",
        base
    );
    Ok(())
}
