mod activity;
mod common;
mod logs;
mod metrics;
mod trace_detail;
mod trace_query;

pub(super) use activity::{
    handle_check_health, handle_describe_service, handle_list_services, handle_mark_checkpoint,
    handle_reset, handle_summarize_activity,
};
pub(super) use logs::handle_query_logs;
pub(super) use metrics::handle_query_metrics;
pub(super) use trace_detail::handle_get_trace;
pub(super) use trace_query::handle_query_traces;

#[cfg(test)]
pub(super) mod test_support {
    pub(super) fn seed_error_trace(st: &crate::store::SharedState) -> [u8; 16] {
        use crate::store::trace_store::{Span, SpanKind, SpanStatus};
        use smallvec::smallvec;

        let tid = [7u8; 16];
        let mut traces = st.trace_store.write();
        let rname = traces.interner.get_or_intern("GET /api");
        let svc = traces.interner.get_or_intern("api");
        let akey = traces.interner.get_or_intern("http.method");
        let aval =
            crate::store::trace_store::AttributeValue::String(traces.interner.get_or_intern("GET"));
        // Real OTLP ingestion promotes resource attrs to `resource.*` span
        // attributes; TraceQL `resource.service.name` matches against those.
        let svc_key = traces.interner.get_or_intern("resource.service.name");
        let svc_val =
            crate::store::trace_store::AttributeValue::String(traces.interner.get_or_intern("api"));
        let root = Span {
            trace_id: tid,
            span_id: [1u8; 8],
            parent_span_id: None,
            name: rname,
            service_name: svc,
            start_time_ns: 0,
            duration_ns: 5_000_000,
            status: SpanStatus::Error,
            status_message: None,
            kind: SpanKind::Server,
            attributes: smallvec![(akey, aval), (svc_key, svc_val)],
            events: vec![],
            links: Vec::new(),
            ingest_seq: 0,
        };
        traces.ingest_spans(vec![root]);
        tid
    }
}
