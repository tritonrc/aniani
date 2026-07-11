//! Transport-free typed cores backing the MCP tools.

mod activity;
mod catalog;
mod health;
mod trace;

pub use activity::{
    LogItem, LogsBlock, MetricItem, MetricsBlock, ServiceActivity, TraceItem, TracesBlock,
    Truncated, summarize_activity,
};
pub use catalog::{LabelInfo, ServiceCatalog, describe_service};
pub use health::{HealthOverview, ServiceHealth, check_health};
pub use trace::{SpanNode, TraceTree, build_trace_tree, trace_item, trace_items};

pub(super) const DEFAULT_TOP: usize = 20;
pub(super) const DETAILED_TOP: usize = 100;
pub(super) const MAX_LABEL_VALUES: usize = 50;
