# Aniani UI: Cross-Signal Correlation, Charts, and Developer-Utility Improvements

**Date:** 2026-07-10
**Status:** Approved

## Goal

Make the embedded web UI (`src/ui/`) genuinely useful for a developer debugging a
system: connect the three signals to each other, show metrics as charts, give the
log line back its screen space, and remove the small frictions (fixed time window,
lost tab state, raw parser errors) found during a full UI inspection.

All UI work stays within the existing constraints: no build step, no runtime
dependencies beyond the pinned Vue CDN import, assets embedded via `rust-embed`.

## Scope

Ten workstreams. 1–6 are the core; 7–10 fold in the remaining findings from the
UI inspection. Explicitly **out of scope**: logs volume histogram (declined),
inline sparklines in the metrics table (declined), live tail/streaming (DESIGN.md
forbids push results), light theme, zoom/brush on charts.

---

## 1. Hash router and tab state

A hand-rolled router (~40 lines) in `app.js`. `location.hash` is the source of
truth:

| Route | Params |
|---|---|
| `#/` | — (Overview) |
| `#/logs` | `q`, `start`, `end`, `range` |
| `#/metrics` | `q`, `service`, `range` |
| `#/traces` | `q`, `trace`, `range` |

- The App component parses the hash into a reactive `route` object and listens to
  `hashchange`. Tabs render from the route.
- Views read their initial query/params from the route and **auto-run** when a
  query (or `trace` id) is present.
- Pressing Run writes the query back to the hash via `history.replaceState`
  (no history spam); navigating between tabs or following a correlation link is a
  normal hash navigation (back button works).
- Tab components are wrapped in `<keep-alive>` so switching tabs preserves
  in-memory results.
- A shared helper `href(tab, params)` builds `#/…` anchor URLs; every correlation
  link is a real `<a>` (middle-click, copy-link work).
- Hash parsing/building are small pure functions (params → hash → params
  round-trips), kept separate from Vue components.

## 2. Correlation pivots

Four pivots, all rendered as `href()` anchors:

**Service → signals.** Service names on Overview and in the span detail panel
link to the service's signals: small `logs · metrics · traces` anchors, shown
only for signals the service actually reports (from `/api/v1/services`).

**Span → logs.** The span detail panel gains a "View logs" link:
`#/logs?q={service="X"}&start=<span start − 30s>&end=<span end + 30s>`
(nanosecond timestamps). The Logs view honors explicit `start`/`end` params and
falls back to the selected range preset when absent.

**Log → trace.** Requires the backend change in §6a. Log rows whose entry
carries a `trace_id` render a `trace ⧉` chip linking to
`#/traces?trace=<id>`. The Traces view, when given a `trace` param, fetches
`/api/traces/{id}` directly without requiring a prior search, and shows the
waterfall (the result list may be empty).

**Trace list triage cues.** Requires §6b. Each trace search result shows:
a red `N errors` badge when `errorCount > 0`, a relative start time ("12s ago",
from `startTimeUnixNano`), and the existing duration. A sort toggle above the
list orders by **recent | slowest | errors first** (client-side sort of the
fetched results).

## 3. Logs view: two-line rows

Replace the three-column table with a row list:

- **Line 1:** timestamp + severity badge + the log line, full width, wrapping.
  Severity comes from the `level` label (`error`/`warn`/`info`/`debug` colored;
  dim `—` otherwise).
- **Line 2:** dim label chips `[key=value]`. The `service` chip links to that
  service's logs; a `trace` chip appears when the entry has a trace_id (§2).
- Error-level rows get a faint red left border.
- Lines longer than ~6 rendered rows clamp with a click-to-expand toggle (§9).

## 4. Metrics line chart

New `LineChart` Vue component (~150 lines, inline SVG, no deps):

- Input: the `query_range` matrix result. One `<path>` per series, colored from
  the shared service palette (same list as the trace view).
- Y-axis: 4–5 nice-number ticks. X-axis: time ticks across the window.
- Legend doubles as a show/hide toggle per series.
- Hover: vertical crosshair + tooltip listing each visible series' value at the
  nearest step.
- Renders above the existing latest-value table. Scalar results skip the chart.
- No zoom, no brushing.

## 5. Overview: stat tiles and health bars

- Replace the raw `/api/v1/status` JSON dump with a row of stat tiles:
  services, spans/traces, series/samples, log lines, memory, uptime.
- New **Health** section from `/api/v1/diagnose`: one row per service —
  linked service name (§2 anchors), a horizontal 0–100 bar colored
  green→amber→red by `health_score`, the numeric score, and the `top_issue`
  text. Sorted worst-first.
- The old plain services list is removed; the health table plus per-service
  signal links subsume it.

## 6. Backend changes (the only ones)

**a. `LogEntry.trace_id`.**
`#[serde(default)] pub trace_id: Option<String>` (lowercase hex) on
`store/log_store.rs::LogEntry` — `serde(default)` keeps old snapshots loading.
`ingest/otlp_logs.rs` populates it from the OTLP log record when non-empty.
Loki push ingest leaves it `None`. The LogQL eval path threads it through, and
`query/logql/handlers.rs` emits it Loki-structured-metadata style:
`"values": [[ts, line, {"trace_id": "…"}]]` — the third element present only
when the entry has one.

**b. `TraceResult.error_count`.**
`pub error_count: usize` on `store/trace_store.rs::TraceResult`, computed from
span statuses when search results are assembled. `query/traceql/handlers.rs`
adds `"errorCount"` to each search result object.

**c. Friendlier LogQL parse errors** (§8): the parse path maps nom errors to
`parse error at position N: <plain-language message>` instead of passing nom's
`Debug` output through, and the error response gains a `hint` field with a
valid example query, mirroring the existing `TRACEQL_HINT`.

## 7. Time range picker + auto-refresh

- Compact header control (right of the tabs), applying to Logs/Metrics/Traces:
  range presets **5m / 15m / 1h / 2h** (default 1h) and an auto-refresh toggle
  (**off / 5s / 30s**, default off).
- Selected range is stored in the hash (`&range=15m`) so links and bookmarks
  preserve it. Explicit `start`/`end` params (span→logs pivot) override the
  preset and display as "custom" until a preset is clicked.
- Auto-refresh re-runs the active view's current query on the interval; paused
  while `document.hidden`.

## 8. Friendlier query errors (UI half)

When an error message carries `at position N`, the Logs view echoes the query in
monospace with a caret (`^`) under position N. A `hint` field in the error
response renders as a muted suggestion line. Applies to Logs (and Traces, which
already has hints server-side).

## 9. Logs quality-of-life

- **Load more:** button below results doubling the fetch limit
  (200 → 400 → 800 → … capped at 5000), re-running the query.
- **Long-line clamp:** entries taller than ~6 rendered lines clamp with a
  click-to-expand toggle.

## 10. Polish

- Inline SVG favicon as a `data:` URI in `index.html` (removes the console 404).
- `name`/`id` attributes on the query inputs (removes the Chrome a11y issue).
- `/` keyboard shortcut focuses the query input on any tab (ignored while typing
  in an input).
- When `LanguageModel` exists but availability is `unavailable`, the AI-ask row
  shows a one-line muted note ("AI assist requires Chrome's built-in model")
  instead of hiding; still fully hidden when the API is absent.

---

## Error handling

- All new fetches go through the existing `apiGet` helper (uniform error text).
- The trace-by-id route shows the existing detail error state on a bad id.
- Malformed hash params degrade to defaults (empty query, 1h range) rather than
  erroring.
- Chart handles empty matrices (renders nothing) and single-sample series
  (renders a point/flat segment) without NaN geometry.

## Testing

- **Backend:** unit tests beside existing ones — OTLP log ingest → LogQL query
  round-trip asserting the structured-metadata element appears (and is absent
  for Loki-push entries); trace search response asserting `errorCount`; LogQL
  parse-error tests asserting position + human message + hint.
- **UI logic:** hash parse/build round-trip functions and the nice-number tick
  helper are pure; verified by construction and exercised end-to-end.
- **End-to-end:** against a seeded instance (`cargo run --example seed`),
  drive every pivot in the browser: Overview health row → service logs;
  span → logs window; log trace chip → waterfall; trace list sort/badges;
  chart hover; range preset + auto-refresh; error caret; keep-alive tab
  switches; deep-link reload of `#/traces?trace=…`.

## Implementation order

Suggested phases (each independently shippable):
1. Hash router + keep-alive (§1) — everything else hangs links off it.
2. Backend trio (§6a/b/c) with tests.
3. Logs rework (§3, §9) + error caret (§8).
4. Correlation pivots (§2).
5. Metrics chart (§4).
6. Overview (§5).
7. Time range + auto-refresh (§7), polish (§10).
