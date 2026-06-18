# Aniani Web UI — Design Spec

**Date:** 2026-06-18
**Status:** Approved (pending spec review)

## Summary

Add an optional, compile-time-gated web UI to Aniani for reviewing logs, metrics,
and traces. The UI is a no-build-step, ESM + Vue.js single-page app served by the
existing Axum server. It ships in default builds and can be excluded with
`--no-default-features`.

## Goals

- Browse the three signal types through ad-hoc queries:
  - **Logs** via LogQL (`/loki/api/v1/query_range`)
  - **Metrics** via PromQL (`/api/v1/query_range`)
  - **Traces** via TraceQL (`/api/search` + `/api/traces/{id}`)
- A landing/overview page showing services and store status.
- No JavaScript build/compilation step. All front-end code is hand-authored ESM.
- Self-contained binary: UI assets embedded at compile time (Vue itself loads from
  a CDN).

## Non-Goals

- No Vue Router (deferred). View switching is a reactive tab.
- No metric charting/sparklines in v1 — a text/value table is sufficient.
- No service drill-down / catalog-prefilled queries (deferred).
- No JS test harness (would require a build step or browser runner).
- No authentication — consistent with Aniani's localhost-only posture.

## Decisions (from brainstorming)

| Topic | Decision |
|-------|----------|
| Asset delivery | Embed our HTML/JS/CSS in the binary; load Vue from a CDN. |
| UI scope | Three explorers (Logs/Metrics/Traces) + a landing/overview page. |
| Cargo feature | Feature `ui`, **included in `default`**. Disable via `--no-default-features`. |
| Mount path | Under `/ui` (assets under `/ui/assets/*`). Root and APIs untouched. |
| View routing | No Vue Router — a reactive `activeTab` ref switches views. |
| Embedding | `rust-embed` (optional dep, gated by the `ui` feature). Not `include_str!`. |
| Metrics view | Text/value table in v1. |

## Architecture

### Cargo

```toml
[dependencies]
rust-embed = { version = "8", optional = true }

[features]
default = ["ui"]
ui = ["dep:rust-embed"]
```

No other new dependencies — Vue is fetched from a CDN at runtime.

### New module `src/ui/`

The whole module is gated behind `#[cfg(feature = "ui")]`.

```
src/ui/
├── mod.rs              # #![cfg(feature = "ui")] — RustEmbed asset struct + handlers + routes()
└── assets/             # embedded by rust-embed
    ├── index.html
    ├── app.js
    └── style.css
```

`mod.rs`:

```rust
#[derive(rust_embed::RustEmbed)]
#[folder = "src/ui/assets/"]
struct Asset;

pub fn routes() -> axum::Router<crate::store::SharedState> { ... }
```

Handlers:

- `GET /ui` → serves `index.html` with `Content-Type: text/html`.
- `GET /ui/assets/{file}` → `Asset::get(file)`, served with a content type derived
  from the file extension (`.js` → `text/javascript`, `.css` → `text/css`,
  `.html` → `text/html`, else `application/octet-stream`). Missing file → 404.

A small `content_type_for(path: &str) -> &'static str` helper maps extensions.

### Wiring in `src/server.rs`

At the end of `build_router`, before `.layer(...).with_state(state)`:

```rust
#[cfg(feature = "ui")]
let router = router.merge(crate::ui::routes());
```

`src/lib.rs` gains `#[cfg(feature = "ui")] pub mod ui;`.

When the feature is off, the module, dependency, and routes all disappear — zero
impact on the API surface.

## Front-end

### `index.html`

- An **import map** pinning the Vue ESM build, so `app.js` can `import { createApp } from 'vue'`:

  ```html
  <script type="importmap">
  { "imports": { "vue": "https://esm.sh/vue@3.5/es2022/vue.mjs" } }
  </script>
  ```

  (Exact CDN URL/version pinned during implementation; the `vue.esm-browser`
  variant — which bundles the template compiler — is used so templates can be
  authored as plain JS strings with no build step.)
- `<div id="app"></div>`
- `<link rel="stylesheet" href="/ui/assets/style.css">`
- `<script type="module" src="/ui/assets/app.js"></script>`

### `app.js`

`createApp` mounting a root component that holds `activeTab` (`'home' | 'logs' |
'metrics' | 'traces'`) and renders a tab bar plus the active child component. Four
components, each defined inline with a string `template`:

- **Landing** — on mount, fetches `/api/v1/services` and `/api/v1/status`; renders
  a service list and store counts/uptime.
- **Logs** — query input → `GET /loki/api/v1/query_range?query=...`; renders log
  lines (timestamp, labels, line).
- **Metrics** — query input → `GET /api/v1/query_range?query=...`; renders a table,
  one row per series (labels + latest/aggregate value).
- **Traces** — query input → `GET /api/search?q=...`; renders a trace list; clicking
  a trace fetches `/api/traces/{id}` and renders its span list.

Query params (time range/step/limit) get sensible defaults in v1 (e.g. last 1h,
auto step); inputs can be added later.

### `style.css`

Minimal hand-written CSS: a tab bar, monospace results areas, an inline error
banner style. No CSS framework.

## Error Handling

- **Front-end:** every fetch checks `res.ok`; non-2xx or network failures render the
  error text in an inline per-tab banner. Never a blank screen.
- **Rust:** unknown asset path → 404. Asset handlers are otherwise infallible string/
  byte serves (no `Result` propagation needed beyond the 404).

## Testing

Gated Rust tests in `src/ui/mod.rs` (`#[cfg(all(test, feature = "ui"))]`), using the
existing `tower::ServiceExt::oneshot` pattern seen elsewhere in the codebase:

- `GET /ui` → 200, `Content-Type: text/html`.
- `GET /ui/assets/app.js` → 200, JS content type, non-empty body.
- `GET /ui/assets/style.css` → 200, CSS content type.
- `GET /ui/assets/nope.js` → 404.

No JS tests (no build step). Manual smoke test: build with the feature, open
`http://127.0.0.1:4320/ui`, run a query against each tab.

## Documentation

- README: a short "Web UI" section — how to reach it (`/ui`), that it ships by
  default, how to disable (`--no-default-features`), and the **first-load needs
  internet** caveat (Vue CDN).
- DESIGN.md: note the optional UI surface and its feature gate.

## Open Questions / Deferred

- CDN choice and exact Vue version pin — settle during implementation; `esm.sh`
  with a pinned minor is the leading candidate.
- Time-range / step / limit inputs on the explorers — deferred to a later pass.
- Sparklines, service drill-down, Vue Router — deferred.
