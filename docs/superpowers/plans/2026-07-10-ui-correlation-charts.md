# Aniani UI Correlation & Charts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the ten workstreams in `docs/superpowers/specs/2026-07-10-ui-correlation-charts-design.md`: hash-router navigation, cross-signal correlation links, two-line log rows, an SVG metrics chart, a diagnose-powered Overview, time-range/auto-refresh, friendly parse errors, and polish.

**Architecture:** The UI stays a no-build Vue 3 SPA in `src/ui/assets/app.js` (single file, options API, template strings) served by `rust-embed`. Three small backend changes (log trace_id, trace errorCount, LogQL error messages) land first/parallel; all later UI tasks are sequential because they all edit `app.js`.

**Tech Stack:** Rust (axum, nom, serde_json), Vue 3 (CDN import map), hand-rolled SVG. No new dependencies anywhere.

**Read first:** the spec (path above), `AGENTS.md` (conventions: thiserror, no unwrap in lib code, FxHashMap, module-scoped errors, commit style `module: description`), and the current `src/ui/assets/app.js` + `style.css`.

**Every task ends with:** `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test` all passing, then a commit. UI-only tasks still run the full suite (embedded assets are compiled in via rust-embed, and `src/ui/mod.rs` has tests).

**Parallelization:** Tasks 1, 2, 3 touch disjoint files and may run in parallel. Task 4 follows 2 (both edit `src/query/logql/`). Tasks 5–10 each edit `app.js`/`style.css` and are strictly sequential, in order. Task 11 is the final end-to-end pass.

---

### Task 1: Hash router + keep-alive tab state

**Files:**
- Modify: `src/ui/assets/app.js` (App component area, bottom of file)

**Contract.** Add pure helpers + a reactive route, replace the tab-button nav:

```js
// Pure, testable-by-construction helpers (top of file, near apiGet):
// parseHash('#/logs?q=%7B...%7D') -> { tab: 'logs', params: { q: '{...}' } }
// Unknown/empty hash -> { tab: 'home', params: {} }. Unknown params kept verbatim.
function parseHash(hash) { /* strip '#/', split on '?', URLSearchParams -> plain object */ }
// buildHash('logs', { q: '{service="x"}', start: '123' }) -> '#/logs?q=...&start=123'
// Omits empty/null params. buildHash('home', {}) -> '#/'
function buildHash(tab, params) { ... }
const href = buildHash // alias used in templates for <a> links

const route = reactive({ tab: 'home', params: {}, seq: 0 })
// syncFromLocation(): parse location.hash into route, seq++
// setParams(params): history.replaceState hash update WITHOUT hashchange loop
//   (write location.hash only if different; guard with an internal flag), no seq bump.
window.addEventListener('hashchange', syncFromLocation)
```

- App template: nav becomes `<a v-for="t in tabs" :href="href(t.id, {})" :class="{ active: route.tab === t.id }">{{ t.label }}</a>`; main becomes `<keep-alive><component :is="current"></component></keep-alive>`; `current` derives from `route.tab` (unknown → Landing).
- Each of Logs/Metrics/Traces gets: `mixins: [routeAware('logs')]` or equivalent shared object providing `mounted`/`activated`/`watch route.seq` → calls the view's `applyRoute(params)` when `route.tab` matches. `applyRoute` sets `this.query = params.q || this.query`; if `params.q` differs from the last-executed query, call `this.run()`. After a manual Run, the view calls `setParams({ q: this.query })`.
- Metrics also maps `service`; Traces also maps `trace` (used in Task 6). Ignore `start`/`end`/`range` for now (Tasks 5/9 consume them).
- Export new helpers on `window.__aniani` (`route`, `href`, `setParams`).

**Steps:**
- [ ] Implement helpers + route + App rewrite as above.
- [ ] Manual check with a seeded instance (see Task 11 preamble for seeding): `#/logs?q={level="error"}` deep-link auto-runs; switching tabs preserves results; back button returns to prior tab; Run updates the hash without adding history entries.
- [ ] Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test` → all pass.
- [ ] Commit: `ui: add hash router, deep links, and keep-alive tab state`

### Task 2: Backend — LogEntry.trace_id + structured-metadata emission

**Files:**
- Modify: `src/store/log_store.rs` (LogEntry)
- Modify: `src/ingest/otlp_logs.rs`, `src/ingest/loki.rs` (construction sites)
- Modify: `src/query/logql/eval.rs`, `src/query/logql/handlers.rs` (threading + JSON)
- Tests: in-module `#[cfg(test)]` blocks of the files above

**Contract.**

```rust
// log_store.rs
pub struct LogEntry {
    pub timestamp_ns: i64,
    pub line: String,
    #[serde(default)]
    pub ingest_seq: u64,
    /// Lowercase hex trace id from OTLP logs; None for Loki-push entries.
    #[serde(default)]
    pub trace_id: Option<String>,
}
```

- `otlp_logs.rs`: populate from `log_record.trace_id` when `!is_empty()` — hex-encode lowercase (there is an existing hex helper in the trace path; reuse or add a small one). Loki push sets `None`.
- Eval: wherever entries flow to the streams result as `(ts, line)`, carry the option through (e.g. `(i64, String, Option<String>)` or pass `&LogEntry`).
- `handlers.rs` query/query_range streams output: entry becomes `[ts, line]` when `trace_id` is `None`, `[ts, line, {"trace_id": "…"}]` when present (Loki structured-metadata shape).

**Steps:**
- [ ] Write failing tests: (a) OTLP ingest with a non-empty 16-byte trace id → LogQL query_range response includes the third element with the hex id; (b) Loki push ingest → values remain 2-element arrays; (c) bincode round-trip of a `LogEntry` without the field (use a struct literal via serde_json or assert `serde(default)` by deserializing a legacy-shaped value).
- [ ] Run `cargo test -p aniani` → new tests FAIL.
- [ ] Implement; run `cargo test` → PASS.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings`
- [ ] Commit: `store/logql: carry OTLP trace_id on log entries into query responses`

### Task 3: Backend — TraceResult.error_count + errorCount in search

**Files:**
- Modify: `src/store/trace_store.rs` (TraceResult + assembly ~line 304)
- Modify: `src/query/traceql/handlers.rs` (both search-result json! sites, ~lines 87 and 153)
- Tests: in-module tests beside existing search tests

**Contract.** `pub error_count: usize` on `TraceResult`, = number of spans in the trace with `SpanStatus::Error`. Both search JSON assembly sites add `"errorCount": tr.error_count`.

**Steps:**
- [ ] Write failing test: store a trace with 3 spans, one Error → search returns `errorCount == 1`; a clean trace returns 0.
- [ ] Run test → FAIL. Implement → PASS.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `store/traceql: expose per-trace errorCount in search results`

### Task 4: Backend — human LogQL parse errors + hint (after Task 2)

**Files:**
- Modify: `src/query/logql/parser.rs` (error mapping), `src/query/logql/handlers.rs` (hint)
- Tests: parser + handler in-module tests

**Contract.** Parse failures must never surface nom's `Debug` output. Map the final nom error to `LogQLError::Parse { pos, msg }` where `pos` = byte offset into the original query (original length − remaining length) and `msg` is plain language (e.g. `expected a quoted value after '='`, `expected '}' to close the selector`, `expected a label name`). Coarse-grained messages keyed on parse position/context are fine; the bar is "a human knows what to fix", not perfect diagnostics. Handler error responses gain:

```rust
const LOGQL_HINT: &str = r#"Example: {service="myapp", level="error"} |= "timeout""#;
// error body: {"status":"error","error":"parse error at position 9: expected a quoted value after '='","hint":LOGQL_HINT}
```

**Steps:**
- [ ] Write failing tests: `{service=` → error contains `position` and does NOT contain `code:` or `Parsing Error`; handler test asserts the `hint` field.
- [ ] Run → FAIL. Implement → PASS.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `logql: replace nom debug output with positioned human parse errors`

### Task 5: Logs view — two-line rows, severity, clamp, load-more, error caret

**Files:**
- Modify: `src/ui/assets/app.js` (Logs component), `src/ui/assets/style.css`

**Contract.**
- Replace the results `<table>` with a row list. Row line 1: `HH:MM:SS.mmm` timestamp (keep full ISO in a `title` attr), severity badge, log line (wraps). Line 2: dim chips `[key=value]` for every stream label. Severity = `level` label lowercased; classes `sev-error` (red), `sev-warn` (amber), `sev-info` (blue-grey), `sev-debug` (dim), fallback `sev-none` showing `—`. Error rows: `border-left: 2px solid var(--error)` on the row.
- Rows keep `trace_id` when the API returns a third values element (`stream.values[i][2]?.trace_id`) — rendered as a chip in Task 6, but parse/store it now: `rows.push({ tsNs, time, labels: [...entries of stream.stream], line, traceId })` (labels become an array of `[k, v]` pairs, no longer a joined string).
- Clamp: rows whose line exceeds 6 rendered lines get `-webkit-line-clamp: 6` (class `clamped`) and a `show more`/`show less` toggle; detect overflow via a `line.length > 600 || line.split('\n').length > 6` heuristic (no DOM measuring).
- Load more: `limit` becomes component state starting at 200; button under results — `Load more (currently 200)` — doubles it (cap 5000) and re-runs.
- Error caret: when `error` matches `/position (\d+)/` show, under the message, a `<pre class="err-caret">` with the query text on one line and `' '.repeat(pos) + '^'` on the next. Render `hint` from the error response as a muted line (extend `apiGet` to attach `json.hint` to the thrown Error as `e.hint`).
- Honor `start`/`end` route params (nanosecond strings) when present, else last hour. Store on the component when `applyRoute` sees them; a manual Run with no explicit params clears them.

**Steps:**
- [ ] Implement JS + CSS.
- [ ] Manual check against seeded instance: two-line rows, error row tint, `{service=` shows caret + hint, load-more doubles, deep-link with start/end narrows results.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `ui/logs: two-line rows with severity, clamp, load-more, error caret`

### Task 6: Correlation pivots (after Tasks 1, 2, 3, 5)

**Files:**
- Modify: `src/ui/assets/app.js` (Landing, Logs, Traces, TraceView), `style.css`

**Contract.**
- Shared helper `signalLinks(service)` → array of `{ label: 'logs'|'metrics'|'traces', href }` for the signals that service reports (from `vocab.services`), with `q` prefilled: logs `{service="X"}`, traces `{ resource.service.name = "X" }`; the metrics link carries `service=X` only (pre-selects the service dropdown and loads its catalog chips, no query).
- Landing: each service row renders `signalLinks` anchors.
- TraceView span detail: Service value becomes links via `signalLinks`; add a `View logs` anchor → `href('logs', { q: '{service="X"}', start: String(spanStartNs - 30e9), end: String(spanEndNs + 30e9) })`. Span absolute times are BigInt (`startBig`/`endBig`) — compute with BigInt (`- 30_000_000_000n`) and `String()` them.
- Logs: rows with `traceId` get a chip `trace ⧉` → `href('traces', { trace: traceId })`; the `service` label chip links to `href('logs', { q: '{service="X"}' })`.
- Traces: `applyRoute` with a `trace` param and no `q` fetches `/api/traces/{id}` directly (`open(id)`) and shows the waterfall with an empty result list. Result list items show: existing name/duration/service/short-id PLUS red badge `N errors` when `errorCount > 0` and relative start time from `startTimeUnixNano` (`(Date.now()*1e6 - Number(start))/1e9` → `12s ago`, `3m ago`, `1h ago`). Sort control (`recent | slowest | errors`) client-side; default recent (by start desc).
- Selecting a trace calls `setParams({ q: this.query, trace: id })` so the selection is deep-linkable.

**Steps:**
- [ ] Implement.
- [ ] Manual check: every pivot navigates correctly on the seeded instance (span→logs lands on the payments error log; log trace chip opens the failing checkout waterfall; badges/sort behave).
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `ui: cross-signal correlation links, trace triage badges and sorting`

### Task 7: Metrics LineChart (after Task 1; independent of 5/6 but sequential on app.js)

**Files:**
- Modify: `src/ui/assets/app.js` (new LineChart component + Metrics integration), `style.css`

**Contract.**

```js
// niceTicks(min, max, count≈5) -> { ticks: [Number], niceMin, niceMax }
// classic nice-numbers: step = 10^floor(log10(range/count)) scaled to 1/2/5.
// Handles min===max by padding ±1 (or ±|min|*0.1). Pure function.
const LineChart = {
  props: { result: Array /* matrix series [{metric, values:[[unixSec, "v"], …]}] */ },
  // computed: series -> { name (via existing seriesLabel logic), color (SERVICE_COLORS[i % len]), points }
  // template: single <svg viewBox="0 0 800 240" preserveAspectRatio="none"> + absolutely
  //   positioned HTML legend + tooltip divs (easier than SVG text wrapping).
}
```

- Geometry: x = time domain across all series, y = niceTicks over min/max of visible values. Non-numeric samples (`NaN`) skipped; a series with one point renders a 3px circle.
- Legend: one entry per series (swatch + name), click toggles `hidden` set; hidden series excluded from y-domain.
- Hover: `mousemove` on the svg maps offsetX → nearest timestamp; vertical line + tooltip listing `name: value` for each visible series at that step; `mouseleave` hides.
- Metrics view: render `<line-chart v-if="matrix.length" :result="matrix">` above the table. Keep the table. `matrix` = result array only when `resultType === 'matrix'`; scalar/vector skip the chart. Store the raw result on the component (currently only last values are kept).
- Y-axis tick labels: compact (`1.2k`, `0.05`); X-axis: `HH:MM` ticks at ~5 positions.

**Steps:**
- [ ] Implement `niceTicks` + component + integration.
- [ ] Manual check: `http_request_duration_ms` shows two colored lines w/ legend, hover readout works, single-sample series doesn't produce NaN geometry (check `stock_level`), legend toggle rescales y.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `ui/metrics: dependency-free SVG multi-series line chart`

### Task 8: Overview — stat tiles + health bars (after Tasks 1, 6)

**Files:**
- Modify: `src/ui/assets/app.js` (Landing), `style.css`

**Contract.**
- Stat tiles from `/api/v1/status.data`: Services (`serviceCount`), Traces/Spans (`totalTraces`/`totalSpans`), Series/Samples, Log lines (`totalLogEntries`), Memory (`memoryBytes` → human MB/KB), Uptime (`uptimeSeconds` → `2h 13m`). Grid of `.tile` cards (label + big value). Raw JSON dump removed.
- Health section from `/api/v1/diagnose` (`{services:[{service, health_score, top_issue}]}`), sorted ascending by score: linked service name (via `signalLinks` from Task 6), a bar (`width: score%`, color: `>=90` green `--ok`, `>=70` amber `#fed330`, else `--error`), the rounded score, `top_issue` text (muted; hidden when "No issues detected").
- The old plain services `<ul>` is removed.
- Load status/diagnose via `Promise.allSettled`; failures render section-scoped error text.
- Refresh on `activated()` (keep-alive) so returning to Overview shows fresh numbers.

**Steps:**
- [ ] Implement.
- [ ] Manual check on seeded instance: payments shows a red-ish ~57 bar with "High log error rate: 100.0%"; tiles match `/api/v1/status`; links pivot correctly.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `ui/overview: stat tiles and diagnose-powered health table`

### Task 9: Time range picker + auto-refresh (after Tasks 5–8)

**Files:**
- Modify: `src/ui/assets/app.js` (App header + shared range state + views), `style.css`

**Contract.**
- Shared `const timeRange = reactive({ preset: '1h', refresh: 'off' })`; presets `5m|15m|1h|2h`, refresh `off|5s|30s`. Header control (right of tabs, hidden on Overview): two groups of small toggle buttons.
- `rangeToNs()`/`rangeToSec()` helpers replace `hourAgoMs` usage in all three views. Preset stored in hash (`range=15m`) via `setParams`; `applyRoute` restores it. Explicit `start`/`end` params (span→logs) override the preset — the range control then shows a `custom` chip; clicking any preset clears the explicit window and re-runs.
- Auto-refresh: one interval owned by App; on tick, if `!document.hidden`, calls the active view's `run()` if it has a non-empty query (expose via `window.__aniani.activeRerun = fn` registered by each view on `activated`, cleared on `deactivated`). Changing the toggle resets the interval.

**Steps:**
- [ ] Implement.
- [ ] Manual check: preset changes window (5m hides hour-old seed data; re-seed to verify fresh entries appear), refresh=5s re-runs visibly, hidden tab pauses (check via console log or network panel), hash carries `range`.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `ui: time range presets and auto-refresh`

### Task 10: Polish (after Task 9)

**Files:**
- Modify: `src/ui/assets/index.html`, `src/ui/assets/app.js`

**Contract.**
- Favicon: `<link rel="icon" href="data:image/svg+xml,...">` — simple accent-colored dot/`a` glyph, URL-encoded inline SVG.
- Query inputs get `name` + `id` (`logs-query`, `metrics-query`, `traces-query`).
- Global `keydown` listener: `/` focuses the active view's query input (skip when target is an input/textarea/select); views register their input ref the same way as `activeRerun`.
- AiAsk: when `'LanguageModel' in globalThis` but availability is `'unavailable'`, render `<p class="muted ai-hint">AI assist requires Chrome's built-in model.</p>` instead of hiding; still render nothing when the API is absent.

**Steps:**
- [ ] Implement.
- [ ] Manual check: no favicon 404 in console, no form-field a11y issue, `/` focuses, hint logic (force by stubbing `LanguageModel.availability` in console if needed).
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] Commit: `ui: favicon, input ids, slash shortcut, AI availability hint`

### Task 11: End-to-end verification (supervisor)

- [ ] Fresh boot: `cargo run -- --port 4399` + `cargo run --example seed -- http://127.0.0.1:4399`.
- [ ] Drive in browser: every §Testing bullet in the spec's E2E list — Overview health→logs pivot, span→logs window content, log trace-chip→waterfall, trace badges/sort, chart hover/legend, range presets + auto-refresh, error caret + hint, keep-alive tab switches, deep-link reload of `#/traces?trace=…`, back button.
- [ ] `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test` — full suite green.
- [ ] Fix anything found (small fixes inline; regressions go back to a subagent).
