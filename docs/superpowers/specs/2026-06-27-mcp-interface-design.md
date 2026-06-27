# Aniani MCP Interface — Design Spec

**Date:** 2026-06-27
**Status:** Approved design (pre-implementation)
**Reviewers folded in:** Codex (adversarial design review) + three deep-research streams (MCP Streamable-HTTP spec, MCP tool-design best practice, observability-MCP prior art).

---

## 1. Goal & positioning

Add a Model Context Protocol (MCP) server to Aniani so coding agents use it as a
**single-source observability instrument inside an agentic development workflow**.
Aniani is agent-first and easy local o11y: ephemeral, in-memory, booted per git
worktree. That lifecycle ownership is the differentiator — it enables a loop no
hosted o11y vendor can offer (they observe production telemetry they don't own
and can't reset).

**The agent loop the MCP surface is built around:**

```
Primary (clean-slate, recommended):
  reset(all)            → empty the store
  … agent runs code / tests …
  summarize_activity(service)   → everything present IS this run (no cursor needed)
  … drill down → fix → loop …

Soft (compare run N vs N+1, no wipe):
  mark_checkpoint()     → opaque ingest token
  … agent runs code …
  summarize_activity(service, since=<token>)   → only what was ingested after the mark
```

The primary path is correct by construction (after a reset the store is empty,
so nothing can be misattributed). The soft path needs the ingest cursor in §6.

## 2. Settled decisions

- **Dominant pattern:** iterative feedback loop (the agent observes what *its own*
  run produced), not post-hoc investigation.
- **Scope:** read-only tools + a single write (`reset`).
- **Granularity:** intent-level tools, not a 1:1 REST mirror. 10 tools, 1 write.
- **Transport:** hand-rolled JSON-RPC 2.0 over MCP Streamable HTTP on axum (no
  SDK), merged onto the existing shared listener like `grpc::routes`.
- **Always on** at `POST /mcp`, single port (4320), no feature flag.
- **No tool namespacing** (`aniani_*` not used): Aniani is normally the sole o11y
  MCP server in a worktree; terse names win. Revisit only if collisions appear.
- **Target protocol revision `2025-11-25`** (current stable), with back-compat
  for `2025-06-18` and `2025-03-26`.

## 3. Tool surface (10 tools, 1 write)

All tools carry annotations and an `outputSchema`; results return both a text
content block (concise, agent-readable) and `structuredContent` (the full typed
object). For large arrays the text block is a *summary*, never a verbatim JSON
re-dump, to avoid doubling token cost.

### Loop primitives

**`reset`** — the only write.
- Input: `{ scope: "all" | "service", service?: string }`. `scope` is **required
  and explicit** — there is no "omit service to wipe everything" default (that
  footgun is removed for autonomous use). `scope:"service"` requires `service`.
- Output: `{ scope, service?, checkpoint }` (post-reset ingest token).
- Annotations: `readOnlyHint:false, destructiveHint:true, idempotentHint:true,
  openWorldHint:false`, `title:"Reset Telemetry Store"`.
- Wraps the `reset` core (`clear` / `clear_service`).

**`mark_checkpoint`** — non-destructive read.
- Input: `{}`. Output: `{ checkpoint }` = current global ingest sequence (§6).
- A checkpoint is an **opaque monotonic token**, NOT wall-clock time and NOT an
  MCP pagination cursor. Document this explicitly.
- Annotations: `readOnlyHint:true, openWorldHint:false`, `title:"Mark Checkpoint"`.

**`summarize_activity`** — per-service triage; the loop payoff.
- Input: `{ service: string (required), since?: checkpoint, detail?: "concise"|"detailed" }`.
  No `since` → summarize all current data for the service (correct after reset).
  With `since` → include only entries with `ingest_seq >= since` (§6).
- Output (`structuredContent`): `{ service, since, observed_through, health_score,
  summary, logs:{error_count,total,top[]}, traces:{notable[],total},
  metrics:{notable[]}, truncated:{logs,traces,metrics} }`. Text block = the
  one-line `summary` + the few highest-signal items.
- Annotations: read-only, `openWorldHint:false`.
- Wraps a **new typed core** fusing `diagnose_service` + `summary`, ingest-filtered.

**`check_health`** — global ranked health (split out of the old overloaded `observe`).
- Input: `{}`. Output: `{ services: [{ service, health_score, top_issue }] }`,
  worst-first. Text = a ranked one-liner.
- Annotations: read-only. Wraps the `diagnose_global` typed core.

### Drill-down (read)

**`query_logs`** — `{ service?, level?, contains?, since?, limit?, detail?, logql? }`.
Structured params build LogQL server-side. `logql` is a raw escape hatch; **if
`logql` is present, structured filters are ignored** (precedence stated in the
description and echoed in the response). `since` = checkpoint token. Output:
`{ logs:[{ts,line,labels?}], shown, total_count, truncated }`.

**`query_traces`** — `{ service?, name?, status?, min_duration?, since?, limit?,
detail?, traceql? }`. Same structured+raw pattern and precedence. Output:
`{ traces:[{traceID,rootSpanName,durationMs,errorSpanCount,...}], shown,
total_count, truncated }`.

**`query_metrics`** — `{ promql: string (required), start?, end?, step? }`. Raw
PromQL passthrough (metric queries are inherently expressions; PromQL is
well-represented in training data, so no structured builder). Output: a compacted
Prometheus-style result.

> **Asymmetry, validated by review:** structured builders matter most for
> **LogQL/TraceQL** (niche DSLs LLMs mis-write — the documented Honeycomb failure
> mode); PromQL stays raw. Keep raw escape hatches on all three.

**`get_trace`** — `{ trace_id: string, detail?: "concise"|"detailed" }`. Builds a
**real parent/child span tree** (the REST handler emits OTLP-shaped batches and
leaves tree-building to the UI — MCP needs an actual tree core). Depth/branch
capping; `concise` drops verbose attributes (names/durations/status only).

### Orient (read)

**`list_services`** — `{}` → `{ services:[{ name, signals:[...] }] }`.

**`describe_service`** — `{ service: string }`. **Enriched** beyond today's
catalog: metric names, log label keys **and capped label values**, span attribute
keys, latest signal timestamp per signal, and counts — the "discover before you
query" grounding that makes first-try LogQL/TraceQL correct. Values are capped
with truncation indicators.

## 4. Result shaping & token efficiency

- Default `limit` ≈ 25–50, hard cap ≈ 100. The in-memory backend is cheap; the
  only real budget is the agent's context.
- Detect truncation by fetching `limit+1`; when truncated, return `total_count`
  and an actionable hint ("showing 50 of 4,213 — narrow `contains`/`since` or
  raise `limit`"), not a bare boolean.
- `summarize_activity` / `check_health` return **synthesis**, never raw rows.
- Keep every response well under the 25k-token client cap.

## 5. Error model

- **Protocol errors** (JSON-RPC error object): malformed JSON `-32700`; invalid
  JSON-RPC shape `-32600`; unknown method `-32601`; **unknown tool name** and
  **schema-invalid arguments** (wrong type / missing required field) `-32602`;
  internal `-32603`.
- **Tool execution errors** (`tools/call` result with `isError:true` + text):
  **semantically** bad input that is schema-valid — unknown service, malformed
  `logql`/`promql`/`traceql` string, out-of-range values. These are injected back
  into the model's context so it self-corrects.
- Self-correcting text is mandatory: unknown service → echo the bad value and
  list valid services; query parse error → parser message + position + a minimal
  valid example for that language.

This reconciles the reviewers: schema validation failures are protocol errors
(`-32602`); value-level/business validation failures are `isError` results
(aligns with MCP 2025-11-25 SEP-1303).

## 6. Ingest-sequence cursor (the correctness fix)

**Problem (found in review):** the stores index by **event time** (`timestamp_ns`,
`timestamp_ms`, `start_time_ns`) — stamped by the client SDK. A wall-clock
`checkpoint` + event-time `since` filter silently drops telemetry that arrives
*after* the checkpoint but carries an *earlier* event timestamp (batched OTLP/Loki
exports, clock skew) — exactly the loop's telemetry.

**Fix:** a global monotonic ingest sequence.
- Add `ingest_seq: AtomicU64` to `AppState`.
- Add an `ingest_seq: u64` field to `LogEntry`, `Sample`, and `Span`. On every
  append, stamp it with `state.ingest_seq.fetch_add(1, Relaxed)`. (~8 bytes/entry;
  at 100k entries ≈ 0.8 MB — negligible.)
- `mark_checkpoint` / `reset` return the current counter value as `checkpoint`.
- `since` filters `ingest_seq >= checkpoint`, independent of event time — robust
  to late arrival and skew.
- Snapshot: serialize the counter; on restore set it to `max(seen)+1` to preserve
  monotonicity.
- Eviction is unchanged (still age/event-time based); the two are orthogonal.

The **primary reset-based loop never needs this** (empty store ⇒ no
misattribution); it exists to make the soft-checkpoint path correct.

> Threading the stamp through every ingest path (`loki`, `otlp_logs`,
> `otlp_metrics`, `otlp_traces`, `remote_write`) is the one non-trivial store
> change in this work. It is mechanical and well-contained.

## 7. Transport & protocol (hand-rolled, MCP Streamable HTTP)

Endpoint: `POST /mcp` (plus `GET`/`DELETE` handling below). Stateless: the server
issues no `MCP-Session-Id`.

**Methods:** `initialize`, `notifications/initialized`, `tools/list`,
`tools/call`, `ping`. Tools are invoked **only** via `tools/call {name,arguments}`
— never as bespoke JSON-RPC methods.

**POST body handling:**
- JSON-RPC **request** (has `id`) → respond `200` with a single
  `Content-Type: application/json` JSON-RPC response (no SSE).
- JSON-RPC **notification/response** (e.g. `notifications/initialized`) → **HTTP
  202 Accepted, empty body**. (Most-missed compliance point.)
- Top-level JSON **array** (batching) → reject `-32600`. Batching is removed from
  the spec; one message per POST.

**Headers:**
- Be lenient on inbound `Accept` (we always reply `application/json`); do not
  reject clients lacking `text/event-stream`.
- `MCP-Protocol-Version` (case-insensitive) on post-initialize requests: absent →
  default `2025-03-26` and proceed (never 400 on absence); present but unsupported
  → **400**.
- **`Origin` validation (security MUST):** if `Origin` is present and not in the
  localhost/web-UI allowlist → **403 Forbidden**. Absent `Origin` (native clients
  like Claude Code) is allowed. A localhost bind alone does NOT satisfy this
  (DNS-rebinding), and it matters more here because `reset` mutates state.

**`GET /mcp` → 405**, **`DELETE /mcp` → 405** (no SSE stream, no sessions to
terminate — both spec-blessed for a stateless tools-only server).

**`initialize`:**
- Response: `protocolVersion`, `capabilities:{ tools:{} }` (omit `listChanged`;
  we send no tool-list-change notifications), `serverInfo:{ name:"aniani",
  version }`, and an `instructions` string (§8).
- Version negotiation: echo the client's `protocolVersion` if in
  `{2025-11-25, 2025-06-18, 2025-03-26}`, otherwise return `2025-11-25` (our
  latest). Do not hard-error on an unfamiliar version.

**`ping` → `{}`** (interop with Inspector / health checks).

**Accepted limitation:** stateless means we cannot enforce "initialized before
tools." Tools work regardless of handshake ordering. Acceptable for a localhost
dev tool; documented.

## 8. Server `instructions` (teaching the loop)

The `initialize` result includes an `instructions` string — the single
highest-leverage lever for correct tool sequencing. It states the loop as a short
numbered protocol:

1. `reset(all)` for a clean baseline before a run.
2. Run your code / tests; telemetry export may lag a moment — if a summary looks
   empty, wait briefly and retry.
3. `summarize_activity(service)` to see what the run produced.
4. Drill in with `query_logs` / `query_traces` / `get_trace` / `query_metrics`;
   call `describe_service` first to learn queryable labels/metrics.
5. To compare iterations without wiping, `mark_checkpoint()` before a run and pass
   the token as `since`.

Each tool `description` additionally states purpose + "use when / not when" so the
ladder (`summarize_activity`=triage, `query_*`/`get_trace`=drill,
`describe_service`=schema, `list_services`=inventory, `check_health`=global) is
unambiguous, with an inline raw-query example on each escape-hatch tool.

## 9. Architecture

Mirrors the gRPC refactor: extract **transport-free, strongly-typed cores** (not
`serde_json::Value`) from the `api/*.rs` handlers so REST and MCP share one logic
layer; MCP never calls handlers.

```
src/mcp/
├── mod.rs      # routes() -> axum::Router (POST/GET/DELETE /mcp); merged in server.rs
├── server.rs   # JSON-RPC dispatch: parse, header/Origin checks, method routing,
│               #   notification->202, version negotiation, error mapping
├── tools.rs    # tool registry: name -> { input schema, output schema, annotations, handler }
└── synth.rs    # typed cores: summarize_activity, check_health, describe_service,
                #   trace-tree builder (or extend api/ cores in place)
```

Wiring & conventions:
- `/mcp` is a normal axum route added in `build_router` **before** the
  `grpc::routes` merge and the `.fallback(handle_not_found)` reassertion, so the
  existing 404 fallback still covers unknown paths.
- `/mcp` gets a **small JSON body limit** (e.g. 1 MiB) via a route-scoped
  `DefaultBodyLimit`, NOT the 64 MiB OTLP limit.
- Module-scoped error enums via `thiserror`; no `.unwrap()`/`.expect()` in
  library code; `FxHashMap`; derive `serde` on response/snapshot types.
- **Lock discipline:** `summarize_activity` and `check_health` must batch reads
  per store (acquire, copy minimal data, release), then compute outside the lock —
  do **not** copy `diagnose`'s many-short-locks-per-service pattern. No `.await`
  held across any store lock.

## 10. Testing (TDD)

- **Unit** — each typed core against constructed stores: `summarize_activity`
  (incl. `since` ingest-filtering), `check_health`, enriched `describe_service`,
  trace-tree builder.
- **Dispatch** — `initialize` (capabilities + `instructions` + version
  negotiation incl. unknown-version fallback), `tools/list` (10 schemas with
  annotations + outputSchema), `tools/call` roundtrip per tool, `ping` → `{}`.
- **Transport/compliance** — notification → **202 empty body**; `GET /mcp` →
  405; `DELETE /mcp` → 405; JSON array → `-32600`; missing
  `MCP-Protocol-Version` → defaulted (not 400); unsupported version → 400;
  **`Origin` present+disallowed → 403**, absent Origin → allowed.
- **Error boundary** — unknown tool → `-32601`/`-32602` protocol error;
  bad query string / unknown service → `isError:true` with self-correcting text.
- **`tests/e2e_mcp.rs`** (mirrors `tests/e2e_otlp_grpc.rs`): spawn server → drive
  the real loop over HTTP — `reset(all)` → ingest via helpers →
  `summarize_activity` reflects it → `query_logs`/`get_trace` → `reset` clears.
- **Late-arriving telemetry case (the headline bug):** `mark_checkpoint` → ingest
  an entry whose **event timestamp predates** the checkpoint →
  `summarize_activity(since=mark)` **still includes it** (proves ingest-seq, not
  event-time, drives `since`).

## 11. Config & startup

- MCP always on at `/mcp`; no flag.
- Startup log prints the MCP URL (`http://127.0.0.1:<port>/mcp`) alongside the
  existing listen line.

## 12. Non-goals

- No auth/TLS (localhost; documented SHOULD-violation, mitigated by Origin
  validation which is the spec's named defense for the cross-origin threat).
- No sessions, no SSE/server-initiated messages, no JSON-RPC batching.
- No `resources/*` or `prompts/*` capabilities (tools-only server).
- No toolset-toggle / `search_tools`+`execute_tool` machinery — the 10-tool
  surface is intentionally lean.

## 13. References

- MCP transport (2025-11-25): https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
- MCP lifecycle / initialize: https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle
- MCP tools + annotations + structured content: https://modelcontextprotocol.io/specification/2025-11-25/server/tools
- Tool annotations semantics/defaults: https://blog.modelcontextprotocol.io/posts/2026-03-16-tool-annotations/
- Pagination (reserved `cursor`): https://modelcontextprotocol.io/specification/2025-11-25/server/utilities/pagination
- Security best practices: https://modelcontextprotocol.io/specification/2025-11-25/basic/security_best_practices
- Anthropic, Writing effective tools for agents: https://www.anthropic.com/engineering/writing-tools-for-agents
- Prior art tool surfaces: Grafana https://grafana.com/docs/grafana/latest/developer-resources/mcp/reference/mcp-tools-table/ · Honeycomb https://docs.honeycomb.io/integrations/mcp/tools/ · Datadog https://docs.datadoghq.com/mcp_server/tools/ · Coroot https://github.com/jamesbrink/mcp-coroot · Last9 https://github.com/last9/last9-mcp-server
