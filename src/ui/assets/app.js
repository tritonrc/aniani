import { createApp, reactive } from 'vue'

// --- shared fetch helper: parses JSON, throws a useful message on error ---
async function apiGet(url) {
  let res
  let text
  try {
    res = await fetch(url)
    text = await res.text()
  } catch (e) {
    throw new Error('network error: ' + e.message)
  }
  let json = null
  try {
    json = JSON.parse(text)
  } catch {
    // leave json null; fall through to error handling below
  }
  if (!res.ok || (json && json.status === 'error')) {
    const msg = (json && (json.error || json.message)) || text || res.statusText
    const err = new Error(msg)
    if (json && json.hint) err.hint = json.hint
    throw err
  }
  return json
}

// --- shared time-range state ------------------------------------------------
// Preset window (used by Logs/Metrics/Traces queries) and auto-refresh mode,
// shared across views via the App header control.
const RANGE_PRESETS = { '5m': 300, '15m': 900, '1h': 3600, '2h': 7200 }
const timeRange = reactive({ preset: '1h', refresh: 'off' })

function rangeToSec() {
  return RANGE_PRESETS[timeRange.preset] || 3600
}
function rangeStartMs() {
  return Date.now() - rangeToSec() * 1000
}

// Range value for the `range` URL param: the default '1h' preset is omitted
// (implicit), everything else is passed through explicitly.
function rangeParam() {
  return timeRange.preset === '1h' ? '' : timeRange.preset
}

// True when a view is showing an explicit start/end window (currently only
// Logs, via the span → logs pivot) instead of the timeRange preset; drives
// the 'custom' chip in App's range control.
const customWindow = reactive({ active: false })

// Sync timeRange.preset from a hash `range` param, when present and known.
// Called by routeAware before a view's own applyRoute runs, so the header
// control reflects the URL on every route change.
function applyRangeParam(params) {
  if (params && params.range && RANGE_PRESETS[params.range]) {
    timeRange.preset = params.range
  }
}

// HH:MM:SS.mmm in local time, from a nanosecond timestamp (as Number).
function formatLocalTime(tsNs) {
  const d = new Date(tsNs / 1_000_000)
  const pad = (n, len) => String(n).padStart(len, '0')
  return pad(d.getHours(), 2) + ':' + pad(d.getMinutes(), 2) + ':' + pad(d.getSeconds(), 2) + '.' + pad(d.getMilliseconds(), 3)
}

// Relative-time label from a nanosecond timestamp (string or number): '12s ago',
// '3m ago', '1h ago'. Rounded to the nearest unit; never negative.
function agoLabel(startNs) {
  const diffSec = (Date.now() * 1e6 - Number(startNs)) / 1e9
  const s = Math.max(0, Math.round(diffSec))
  if (s < 60) return s + 's ago'
  if (s < 3600) return Math.round(s / 60) + 'm ago'
  return Math.round(s / 3600) + 'h ago'
}

// Human byte size: '512 B', '63.1 KB', '1.2 MB'.
function formatBytes(n) {
  if (n < 1024) return n + ' B'
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB'
  return (n / (1024 * 1024)).toFixed(1) + ' MB'
}

// Human uptime from a seconds count: '42s', '12m 3s', '2h 13m'.
function formatUptime(sec) {
  const s = Math.floor(sec)
  if (s < 60) return s + 's'
  if (s < 3600) return Math.floor(s / 60) + 'm ' + (s % 60) + 's'
  return Math.floor(s / 3600) + 'h ' + Math.floor((s % 3600) / 60) + 'm'
}

// --- hash-router helpers ---------------------------------------------------

// '#/logs?q=%7B...%7D' -> { tab: 'logs', params: { q: '{...}' } }
// Empty/unknown hash -> { tab: 'home', params: {} }. Unknown params kept verbatim.
function parseHash(hash) {
  const body = (hash || '').replace(/^#\/?/, '')
  const qIdx = body.indexOf('?')
  const tab = (qIdx === -1 ? body : body.slice(0, qIdx)) || 'home'
  const qs = qIdx === -1 ? '' : body.slice(qIdx + 1)
  return { tab, params: Object.fromEntries(new URLSearchParams(qs)) }
}

// buildHash('logs', { q: '{service="x"}', start: '123' }) -> '#/logs?q=...&start=123'
// Omits empty/null params. buildHash('home', {}) -> '#/'
function buildHash(tab, params) {
  const usp = new URLSearchParams()
  for (const [k, v] of Object.entries(params || {})) {
    if (v === null || v === undefined || v === '') continue
    usp.set(k, v)
  }
  const qs = usp.toString()
  const path = tab && tab !== 'home' ? tab : ''
  return '#/' + path + (qs ? '?' + qs : '')
}
const href = buildHash // alias used in templates for <a> links

// Escape a label value for safe insertion into a double-quoted query literal.
function escLabel(v) {
  return String(v).replace(/\\/g, '\\\\').replace(/"/g, '\\"')
}

// --- trace-view helpers ---------------------------------------------------

// Stable per-trace service color palette (assigned by sorted service order).
const SERVICE_COLORS = [
  '#4ea1ff', '#ff9f43', '#26de81', '#fc5c65', '#a55eea',
  '#fed330', '#2bcbba', '#fd9644', '#778ca3', '#eb3b5a',
]

const SPAN_KIND_LABELS = ['unspecified', 'internal', 'server', 'client', 'producer', 'consumer']

// Human-readable duration. Nanoseconds in, adaptive unit out.
function formatDuration(ns) {
  const n = Number(ns) || 0
  if (n >= 1e9) return (n / 1e9).toFixed(2) + 's'
  if (n >= 1e6) return (n / 1e6).toFixed(2) + 'ms'
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'µs'
  return Math.round(n) + 'ns'
}

// Parse a stringified-nanosecond field as BigInt; never throw on bad input.
function toBigNs(v) {
  try {
    if (v === null || v === undefined || v === '') return 0n
    return BigInt(v)
  } catch {
    return 0n
  }
}

function serviceFromResource(resource) {
  const attrs = (resource && resource.attributes) || []
  const a = attrs.find((x) => x.key === 'service.name')
  return (a && a.value && a.value.stringValue) || 'unknown'
}

function mapAttrs(list) {
  return (list || []).map((a) => ({
    key: a.key,
    value: (a.value && a.value.stringValue) != null ? String(a.value.stringValue) : '',
  }))
}

// Normalize one OTLP span (from /api/traces/{id}) into a flat view model.
// Absolute nanosecond timestamps exceed Number's safe integer range, so they
// are kept as BigInt; only small intra-trace offsets get narrowed to Number.
function normalizeSpan(sp, service) {
  return {
    spanId: sp.spanId || '',
    parentSpanId: sp.parentSpanId || '',
    name: sp.name || '(unnamed)',
    service,
    startBig: toBigNs(sp.startTimeUnixNano),
    endBig: toBigNs(sp.endTimeUnixNano),
    statusCode: (sp.status && sp.status.code) || 0,
    kind: sp.kind || 0,
    attributes: mapAttrs(sp.attributes),
    events: (sp.events || []).map((ev) => ({
      name: ev.name || '',
      timeBig: toBigNs(ev.timeUnixNano),
      attributes: mapAttrs(ev.attributes),
    })),
    links: (sp.links || []).map((l) => ({
      traceId: l.traceId || '',
      spanId: l.spanId || '',
      traceState: l.traceState || '',
      flags: l.flags || 0,
      attributes: mapAttrs(l.attributes),
    })),
  }
}

// An event is an exception if it is named `exception` or carries any
// `exception.*` attribute (OTLP's recorded-exception convention).
function isException(ev) {
  return ev.name === 'exception' || ev.attributes.some((a) => a.key && a.key.indexOf('exception.') === 0)
}

function attrVal(ev, key) {
  const a = ev.attributes.find((x) => x.key === key)
  return a ? a.value : ''
}

// Parse the /api/traces/{id} payload into a render model: flat spans with
// intra-trace offsets, a parent/child tree, per-service colors, and totals.
function buildTraceModel(detail) {
  const spans = []
  for (const b of (detail && detail.batches) || []) {
    const service = serviceFromResource(b.resource)
    for (const ss of b.scopeSpans || []) {
      for (const sp of ss.spans || []) spans.push(normalizeSpan(sp, service))
    }
  }
  if (!spans.length) return null
  spans.forEach((s, i) => { s.uid = i }) // stable unique id; spanIds may collide

  // Timeline bounds: ignore spans with a missing (zero) start so one bad span
  // can't collapse minStart to 0 and skew every offset to ~now.
  const timed = spans.filter((s) => s.startBig > 0n)
  const base = timed.length ? timed : spans
  let minStart = base[0].startBig
  let maxEnd = base[0].endBig
  for (const s of base) {
    if (s.startBig < minStart) minStart = s.startBig
    if (s.endBig > maxEnd) maxEnd = s.endBig
  }
  if (maxEnd < minStart) maxEnd = minStart
  const totalNs = Number(maxEnd - minStart)
  const clamp = (n) => Math.min(totalNs, Math.max(0, n))
  for (const s of spans) {
    s.offsetNs = clamp(Number(s.startBig - minStart))
    s.durationNs = Math.max(0, Number(s.endBig - s.startBig))
    for (const ev of s.events) ev.offsetNs = clamp(Number(ev.timeBig - minStart))
  }

  // One node per span. byId resolves a parentSpanId to the FIRST span carrying
  // that id, so duplicate spanIds keep distinct nodes instead of clobbering.
  const nodes = spans.map((s) => ({ span: s, children: [] }))
  const byId = {}
  for (const n of nodes) if (!(n.span.spanId in byId)) byId[n.span.spanId] = n
  const roots = []
  for (const n of nodes) {
    const parent = n.span.parentSpanId ? byId[n.span.parentSpanId] : null
    if (parent && parent !== n) parent.children.push(n)
    else roots.push(n) // no parent, parent absent, or self-parent
  }
  // Recover spans trapped in a parent cycle (unreachable from any root) so they
  // surface as roots instead of silently vanishing from the waterfall.
  const reached = new Set()
  const rstack = [...roots]
  while (rstack.length) {
    const n = rstack.pop()
    if (reached.has(n.span.uid)) continue
    reached.add(n.span.uid)
    for (const c of n.children) rstack.push(c)
  }
  for (const n of nodes) if (!reached.has(n.span.uid)) roots.push(n)
  for (const n of nodes) n.span.hasChildren = n.children.length > 0

  // Sort siblings by start time, iteratively and cycle-safe (no recursion limit).
  const sortSiblings = (arr) =>
    arr.sort((a, b) =>
      a.span.startBig < b.span.startBig ? -1 : a.span.startBig > b.span.startBig ? 1 : a.span.name.localeCompare(b.span.name),
    )
  sortSiblings(roots)
  const sstack = [...roots]
  const sseen = new Set()
  while (sstack.length) {
    const n = sstack.pop()
    if (sseen.has(n.span.uid)) continue
    sseen.add(n.span.uid)
    sortSiblings(n.children)
    for (const c of n.children) sstack.push(c)
  }

  // Per-service color: fixed palette, then procedural hues past its length so
  // wide fan-outs (11+ services) stay visually distinct.
  const services = [...new Set(spans.map((s) => s.service))].sort()
  const serviceColor = {}
  services.forEach((svc, i) => {
    serviceColor[svc] =
      i < SERVICE_COLORS.length
        ? SERVICE_COLORS[i]
        : 'hsl(' + Math.round((i * 360) / services.length) + ' 60% 62%)'
  })

  return {
    spans,
    roots,
    totalNs,
    services,
    serviceColor,
    count: spans.length,
    errorCount: spans.filter((s) => s.statusCode === 2).length,
  }
}

// Jaeger-style trace waterfall: a service-colored timeline of spans with a
// collapsible tree and click-to-expand detail (tags, process, events/exceptions).
const TraceView = {
  props: {
    detail: { type: Object, required: true },
    traceId: { type: String, default: '' },
  },
  data() {
    return { collapsed: {}, expanded: {}, showRaw: false }
  },
  computed: {
    model() {
      return buildTraceModel(this.detail)
    },
    ticks() {
      const total = (this.model && this.model.totalNs) || 0
      // A zero-width trace (all spans share one instant) has no meaningful axis.
      if (total <= 0) return [{ pct: 0, label: '0' }]
      return [0, 25, 50, 75, 100].map((pct) => ({ pct, label: formatDuration((total * pct) / 100) }))
    },
    rows() {
      const m = this.model
      if (!m) return []
      const total = m.totalNs || 1
      const out = []
      const seen = new Set()
      // Iterative DFS: tolerates very deep chains (no recursion limit) and
      // cannot loop on a malformed cycle (seen guard).
      const stack = []
      for (let i = m.roots.length - 1; i >= 0; i--) stack.push({ node: m.roots[i], depth: 0 })
      while (stack.length) {
        const { node, depth } = stack.pop()
        const s = node.span
        if (seen.has(s.uid)) continue
        seen.add(s.uid)
        let leftPct = Math.min(100, Math.max(0, (s.offsetNs / total) * 100))
        let widthPct = (s.durationNs / total) * 100
        if (!isFinite(widthPct) || widthPct < 0) widthPct = 0
        widthPct = Math.min(100, Math.max(widthPct, 0.4))
        if (leftPct + widthPct > 100) leftPct = Math.max(0, 100 - widthPct) // keep bar in track
        out.push({
          span: s,
          depth,
          hasChildren: node.children.length > 0,
          leftPct,
          widthPct,
          color: m.serviceColor[s.service],
        })
        if (!this.collapsed[s.uid] && node.children.length) {
          for (let i = node.children.length - 1; i >= 0; i--) stack.push({ node: node.children[i], depth: depth + 1 })
        }
      }
      return out
    },
  },
  methods: {
    formatDuration(ns) {
      return formatDuration(ns)
    },
    kindLabel(k) {
      return (SPAN_KIND_LABELS[k] || 'unspecified').toUpperCase()
    },
    statusLabel(code) {
      return code === 2 ? 'ERROR' : code === 1 ? 'OK' : 'UNSET'
    },
    statusClass(code) {
      return code === 2 ? 'err' : code === 1 ? 'ok' : 'muted'
    },
    pretty(v) {
      return JSON.stringify(v, null, 2)
    },
    durStyle(row) {
      const end = row.leftPct + row.widthPct
      // Room to the right of the bar → label sits just after the bar's end.
      if (end <= 85) return { left: end + '%', paddingLeft: '5px' }
      // Wide bar (no room to the right) → tuck the label inside the bar's right
      // end with a translucent backing so it stays legible on any service color
      // and never collides with the span name/service in the left column.
      return {
        right: 100 - end + '%',
        textAlign: 'right',
        padding: '0 4px',
        color: 'var(--fg)',
        background: 'rgba(15, 20, 25, 0.55)',
        borderRadius: '3px',
      }
    },
    toggleCollapse(id) {
      this.collapsed[id] = !this.collapsed[id]
    },
    toggleDetail(id) {
      this.expanded[id] = !this.expanded[id]
    },
    expandAll() {
      this.collapsed = {}
    },
    collapseAll() {
      // Collapse every span that has children so only roots remain visible.
      const next = {}
      if (this.model) this.model.spans.forEach((s) => { if (s.hasChildren) next[s.uid] = true })
      this.collapsed = next
    },
    spanTags(span) {
      return span.attributes
        .filter((a) => a.key.indexOf('resource.') !== 0)
        .map((a) => ({ key: a.key.indexOf('span.') === 0 ? a.key.slice(5) : a.key, value: a.value }))
    },
    processTags(span) {
      return span.attributes
        .filter((a) => a.key.indexOf('resource.') === 0)
        .map((a) => ({ key: a.key.slice(9), value: a.value }))
    },
    exceptions(span) {
      return span.events.filter(isException).map((ev) => ({
        type: attrVal(ev, 'exception.type'),
        message: attrVal(ev, 'exception.message'),
        stacktrace: attrVal(ev, 'exception.stacktrace'),
        offsetNs: ev.offsetNs,
      }))
    },
    otherEvents(span) {
      return span.events.filter((ev) => !isException(ev))
    },
    href,
    signalLinks,
    sigEmoji,
    // href('logs', ...) for the span's service, windowed 30s before/after the
    // span so the correlated log lines are in view without an extra search.
    // startBig/endBig are BigInt; do the arithmetic in BigInt then String() it.
    viewLogsHref(span) {
      const start = span.startBig - 30_000_000_000n
      const end = span.endBig + 30_000_000_000n
      return href('logs', { q: '{service="' + escLabel(span.service) + '"}', start: String(start), end: String(end) })
    },
    // Hash link to open a different trace (used by span-link pivots).
    traceLinkHref(traceId) {
      return href('traces', { trace: traceId })
    },
    // Abbreviate a 16/8-byte hex id for compact display.
    shortId(id) {
      if (!id) return ''
      return id.length > 12 ? id.slice(0, 8) + '…' : id
    },
  },
  template: `
    <div class="trace-view" v-if="model">
      <div class="tv-summary">
        <span class="tv-stat"><strong>{{ formatDuration(model.totalNs) }}</strong> total</span>
        <span class="tv-stat"><strong>{{ model.count }}</strong> spans</span>
        <span class="tv-stat"><strong>{{ model.services.length }}</strong> services</span>
        <span class="tv-stat err" v-if="model.errorCount"><strong>{{ model.errorCount }}</strong> errors</span>
        <span class="tv-spacer"></span>
        <button class="tv-btn" @click="expandAll">Expand all</button>
        <button class="tv-btn" @click="collapseAll">Collapse all</button>
        <button class="tv-btn" @click="showRaw = !showRaw">{{ showRaw ? 'Hide JSON' : 'Raw JSON' }}</button>
      </div>
      <div class="tv-legend">
        <span class="tv-legend-item" v-for="svc in model.services" :key="svc">
          <span class="tv-swatch" :style="{ background: model.serviceColor[svc] }"></span>{{ svc }}
        </span>
      </div>
      <pre class="json" v-if="showRaw">{{ pretty(detail) }}</pre>
      <template v-else>
        <div class="tl-ruler">
          <div class="tl-name-col"></div>
          <div class="tl-track">
            <span class="tl-tick" v-for="t in ticks" :key="t.pct" :style="{ left: t.pct + '%' }">{{ t.label }}</span>
          </div>
        </div>
        <div class="tl-rows">
          <div class="tl-row-group" v-for="row in rows" :key="row.span.uid">
            <div
              class="tl-row"
              :class="{ open: expanded[row.span.uid], error: row.span.statusCode === 2 }"
              @click="toggleDetail(row.span.uid)"
            >
              <div class="tl-name-col" :style="{ paddingLeft: (row.depth * 14 + 4) + 'px' }">
                <span class="tl-toggle" v-if="row.hasChildren" @click.stop="toggleCollapse(row.span.uid)">{{ collapsed[row.span.uid] ? '▸' : '▾' }}</span>
                <span class="tl-toggle ghost" v-else></span>
                <span class="tl-svc-bar" :style="{ background: row.color }"></span>
                <span class="tl-span-name" :title="row.span.name">{{ row.span.name }}</span>
                <span class="tl-svc-tag">{{ row.span.service }}</span>
                <span class="tl-err-dot" v-if="row.span.statusCode === 2" title="error">●</span>
              </div>
              <div class="tl-track">
                <div class="tl-bar" :style="{ left: row.leftPct + '%', width: row.widthPct + '%', background: row.color }"></div>
                <span class="tl-dur" :style="durStyle(row)">{{ formatDuration(row.span.durationNs) }}</span>
              </div>
            </div>
            <div class="tl-detail" v-if="expanded[row.span.uid]">
              <div class="tl-meta">
                <div>
                  <span class="k">Service</span>
                  <span class="v tl-svc-cell">
                    <span class="tl-svc-name" :title="row.span.service">{{ row.span.service }}</span>
                    <span class="sig-icons">
                      <a
                        v-for="l in signalLinks(row.span.service)"
                        :key="l.label"
                        class="sig-icon"
                        :href="l.href"
                        :title="'View ' + l.label + ' for ' + row.span.service"
                        :aria-label="'View ' + l.label + ' for ' + row.span.service"
                      >{{ sigEmoji(l.label) }}</a>
                    </span>
                  </span>
                </div>
                <div><span class="k">Operation</span><span class="v">{{ row.span.name }}</span></div>
                <div><span class="k">Logs</span><span class="v"><a :href="viewLogsHref(row.span)" class="pivot-link">View logs (±30s)</a></span></div>
                <div><span class="k">Kind</span><span class="v">{{ kindLabel(row.span.kind) }}</span></div>
                <div><span class="k">Status</span><span class="v" :class="statusClass(row.span.statusCode)">{{ statusLabel(row.span.statusCode) }}</span></div>
                <div><span class="k">Duration</span><span class="v">{{ formatDuration(row.span.durationNs) }}</span></div>
                <div><span class="k">Start</span><span class="v">+{{ formatDuration(row.span.offsetNs) }}</span></div>
                <div><span class="k">Span ID</span><span class="v mono">{{ row.span.spanId }}</span></div>
                <div v-if="row.span.parentSpanId"><span class="k">Parent</span><span class="v mono">{{ row.span.parentSpanId }}</span></div>
              </div>
              <div class="tl-section tl-exc-section" v-if="exceptions(row.span).length">
                <h4>Exceptions</h4>
                <div class="tl-exc" v-for="(ex, i) in exceptions(row.span)" :key="'ex' + i">
                  <div class="tl-exc-head">{{ ex.type || 'exception' }}<span class="tl-at"> @ +{{ formatDuration(ex.offsetNs) }}</span></div>
                  <div class="tl-exc-msg" v-if="ex.message">{{ ex.message }}</div>
                  <pre class="tl-exc-stack" v-if="ex.stacktrace">{{ ex.stacktrace }}</pre>
                </div>
              </div>
              <div class="tl-cols">
                <div class="tl-section" v-if="spanTags(row.span).length">
                  <h4>Tags</h4>
                  <table class="tl-kv"><tbody>
                    <tr v-for="(a, i) in spanTags(row.span)" :key="'t' + i"><td class="k">{{ a.key }}</td><td class="v">{{ a.value }}</td></tr>
                  </tbody></table>
                </div>
                <div class="tl-section" v-if="processTags(row.span).length">
                  <h4>Process</h4>
                  <table class="tl-kv"><tbody>
                    <tr v-for="(a, i) in processTags(row.span)" :key="'p' + i"><td class="k">{{ a.key }}</td><td class="v">{{ a.value }}</td></tr>
                  </tbody></table>
                </div>
              </div>
              <div class="tl-section" v-if="otherEvents(row.span).length">
                <h4>Events</h4>
                <div class="tl-event" v-for="(ev, i) in otherEvents(row.span)" :key="'e' + i">
                  <div class="tl-event-head">{{ ev.name }}<span class="tl-at"> @ +{{ formatDuration(ev.offsetNs) }}</span></div>
                  <table class="tl-kv" v-if="ev.attributes.length"><tbody>
                    <tr v-for="(a, j) in ev.attributes" :key="j"><td class="k">{{ a.key }}</td><td class="v">{{ a.value }}</td></tr>
                  </tbody></table>
                </div>
              </div>
              <div class="tl-section" v-if="row.span.links && row.span.links.length">
                <h4>Links</h4>
                <div class="tl-link" v-for="(lk, i) in row.span.links" :key="'l' + i">
                  <div class="tl-link-head">
                    <a class="pivot-link" :href="traceLinkHref(lk.traceId)" :title="'Open linked trace ' + lk.traceId">{{ shortId(lk.traceId) }}</a>
                    <span class="tl-link-span mono">/{{ shortId(lk.spanId) }}</span>
                  </div>
                  <table class="tl-kv" v-if="lk.traceState || lk.flags || lk.attributes.length"><tbody>
                    <tr v-if="lk.traceState"><td class="k">traceState</td><td class="v mono">{{ lk.traceState }}</td></tr>
                    <tr v-if="lk.flags"><td class="k">flags</td><td class="v mono">{{ lk.flags }}</td></tr>
                    <tr v-for="(a, j) in lk.attributes" :key="j"><td class="k">{{ a.key }}</td><td class="v">{{ a.value }}</td></tr>
                  </tbody></table>
                </div>
              </div>
            </div>
          </div>
        </div>
      </template>
    </div>
    <p v-else class="muted">No spans in this trace.</p>
  `,
}

const vocab = reactive({
  services: [],     // [{ name, signals: [] }]
  metricNames: [],  // ["http_request_duration_ms", ...]
  catalog: {},      // { [service]: { metrics: [], log_labels: [] } }
})
async function loadVocab() {
  const [s, m] = await Promise.allSettled([
    apiGet('/api/v1/services'),
    apiGet('/api/v1/label/__name__/values'),
  ])
  if (s.status === 'fulfilled') vocab.services = (s.value.data && s.value.data.services) || []
  if (m.status === 'fulfilled') vocab.metricNames = m.value.data || []
}
async function loadCatalog(service) {
  if (!service) return null
  if (vocab.catalog[service]) return vocab.catalog[service]
  try {
    const c = await apiGet('/api/v1/catalog?service=' + encodeURIComponent(service))
    vocab.catalog[service] = (c && c.data) || {}
    return vocab.catalog[service]
  } catch (_) { return null }
}

// Cross-signal pivot links for a service: one entry per signal it reports
// (per vocab.services), each pre-filled so landing on the target tab needs no
// further typing. The metrics link carries only `service` (pre-selects the
// dropdown and loads its catalog chips) since there's no single query to run.
function signalLinks(service) {
  const entry = (vocab.services || []).find((s) => s.name === service)
  const signals = (entry && entry.signals) || []
  const links = []
  if (signals.includes('logs')) {
    links.push({ label: 'logs', href: href('logs', { q: '{service="' + escLabel(service) + '"}' }) })
  }
  if (signals.includes('metrics')) {
    links.push({ label: 'metrics', href: href('metrics', { service }) })
  }
  if (signals.includes('traces')) {
    links.push({ label: 'traces', href: href('traces', { q: '{ resource.service.name = "' + escLabel(service) + '" }' }) })
  }
  return links
}

// Emoji glyph for a signalLinks() entry, used as a compact icon in the
// TraceView span detail (see the tl-svc-cell / sig-icons markup above).
function sigEmoji(label) {
  if (label === 'logs') return '📄'
  if (label === 'metrics') return '📈'
  return '🔍'
}

const Landing = {
  template: `
    <section class="view">
      <h2>Overview</h2>
      <h3>Status</h3>
      <p v-if="statusError" class="error">{{ statusError }}</p>
      <div class="tile-grid" v-if="tiles.length">
        <div class="tile" v-for="t in tiles" :key="t.label" :title="t.title">
          <div class="tile-label">{{ t.label }}</div>
          <div class="tile-value">{{ t.value }}</div>
        </div>
      </div>
      <h3>Health</h3>
      <p v-if="healthError" class="error">{{ healthError }}</p>
      <div class="health-list" v-if="sortedHealth.length">
        <div class="health-row" v-for="h in sortedHealth" :key="h.service">
          <span class="health-name">
            <strong>{{ h.service }}</strong>
            <span class="signal-links">
              <a v-for="l in signalLinks(h.service)" :key="l.label" :href="l.href">{{ l.label }}</a>
            </span>
          </span>
          <span class="health-bar"><span class="health-fill" :class="barClass(h.health_score)" :style="{ width: h.health_score + '%' }"></span></span>
          <span class="health-score">{{ Math.round(h.health_score) }}</span>
          <span class="health-issue muted" v-if="h.top_issue !== 'No issues detected'">{{ h.top_issue }}</span>
        </div>
      </div>
      <p v-else-if="!healthError" class="muted">No services have reported telemetry yet.</p>
    </section>
  `,
  data() {
    return { statusData: null, health: [], statusError: '', healthError: '', loadedOnce: false }
  },
  computed: {
    tiles() {
      if (!this.statusData) return []
      const d = this.statusData
      return [
        { label: 'Services', value: String(d.serviceCount) },
        { label: 'Traces / Spans', value: d.totalTraces + ' / ' + d.totalSpans },
        { label: 'Series / Samples', value: d.totalMetricSeries + ' / ' + d.totalMetricSamples },
        { label: 'Log lines', value: String(d.totalLogEntries) },
        {
          label: 'Memory',
          value: formatBytes(d.memoryBytes),
          title: 'logs ' + formatBytes(d.logMemoryBytes) +
            ' · metrics ' + formatBytes(d.metricMemoryBytes) +
            ' · traces ' + formatBytes(d.traceMemoryBytes),
        },
        { label: 'Uptime', value: formatUptime(d.uptimeSeconds) },
      ]
    },
    sortedHealth() {
      return [...this.health].sort((a, b) => a.health_score - b.health_score)
    },
  },
  methods: {
    signalLinks,
    barClass(score) {
      if (score >= 90) return 'h-ok'
      if (score >= 70) return 'h-warn'
      return 'h-bad'
    },
    async load() {
      // Independent failures so one section's error doesn't blank the other.
      const [st, dg] = await Promise.allSettled([
        apiGet('/api/v1/status'),
        apiGet('/api/v1/diagnose'),
      ])
      loadVocab() // refresh shared service vocab so signal links include newly-seen services
      if (st.status === 'fulfilled') {
        this.statusData = st.value.data
        this.statusError = ''
      } else {
        this.statusError = st.reason.message
      }
      if (dg.status === 'fulfilled') {
        this.health = dg.value.services || []
        this.healthError = ''
      } else {
        this.healthError = dg.reason.message
      }
    },
  },
  mounted() {
    this.load()
  },
  activated() {
    // <keep-alive> fires activated() right after mounted() on first insertion;
    // skip that one so the initial load isn't fetched twice.
    if (!this.loadedOnce) {
      this.loadedOnce = true
      return
    }
    this.load()
  },
}

// English text in/out — silences Chrome's "no output language" warning and
// improves output quality. Shared by availability() and create().
const AI_TEXT_EN = {
  expectedInputs: [{ type: 'text', languages: ['en'] }],
  expectedOutputs: [{ type: 'text', languages: ['en'] }],
}

const AiAsk = {
  props: { lang: { type: String, required: true } },
  emits: ['query'],
  template: `
    <template v-if="apiPresent">
      <div class="ai-ask" v-if="state !== 'unavailable' && state !== 'unknown'">
        <input
          v-model="text"
          :placeholder="placeholder"
          spellcheck="false"
          @keydown.enter.prevent="ask"
        />
        <button @click="ask" :disabled="busy">
          {{ busy ? (downloading ? 'Downloading model…' : 'Thinking…') : 'Ask AI' }}
        </button>
        <span class="ai-status" v-if="status">{{ status }}</span>
      </div>
      <p class="muted ai-hint" v-else-if="state === 'unavailable'">AI assist requires Chrome's built-in model.</p>
    </template>
  `,
  data() {
    return {
      apiPresent: 'LanguageModel' in globalThis,
      state: 'unknown',
      text: '',
      busy: false,
      downloading: false,
      status: '',
    }
  },
  computed: {
    placeholder() {
      const ex = {
        logql: 'e.g. errors from payments in the last hour',
        promql: 'e.g. request latency for the gateway service',
        traceql: 'e.g. slow traces in payments',
      }
      return 'Ask in plain English — ' + (ex[this.lang] || '')
    },
    langName() {
      return { logql: 'LogQL', promql: 'PromQL', traceql: 'TraceQL' }[this.lang] || this.lang
    },
  },
  async mounted() {
    if (!this.apiPresent) return
    try {
      this.state = await globalThis.LanguageModel.availability(AI_TEXT_EN)
    } catch (_) {
      this.state = 'unavailable'
    }
  },
  methods: {
    systemPrompt() {
      const v = window.__aniani.vocab
      const services = (v.services || []).map((s) => s.name).join(', ') || '(none)'
      const metrics = (v.metricNames || []).join(', ') || '(none)'
      const syntax = {
        logql: 'LogQL stream selectors like {service="x", level="error"} optionally followed by a |= "substring" filter',
        promql: 'PromQL. A bare metric with optional label filter: metric_name{service="x"}. For a rate over time, put the [duration] range AFTER the closing brace, inside a range function: rate(metric_name{service="x"}[5m]). NEVER put a [duration] range inside the {} braces — only label matchers go inside {}',
        traceql: 'TraceQL like { resource.service.name = "x" }',
      }[this.lang]
      const examples = {
        logql: [
          'errors from payments => {service="payments", level="error"}',
          'gateway logs mentioning timeout => {service="gateway"} |= "timeout"',
        ],
        promql: [
          'request duration for inventory => http_request_duration_ms{service="inventory"}',
          'request rate for gateway over 5 minutes => rate(http_request_duration_ms{service="gateway"}[5m])',
          'all values of stock_level => stock_level',
        ],
        traceql: [
          'traces from payments => { resource.service.name = "payments" }',
          'traces from the gateway service => { resource.service.name = "gateway" }',
        ],
      }[this.lang]
      return (
        'You translate a natural-language request into a single ' +
        this.langName +
        ' query for the Aniani observability engine. ' +
        'Output ONLY the query, no explanation, no code fences. ' +
        'Use ' + syntax + '. ' +
        'Known service names: ' + services + '. ' +
        'Known metric names: ' + metrics + '. ' +
        'Examples (request => query): ' + examples.join(' ; ') + '.'
      )
    },
    async ask() {
      if (!this.text.trim()) return
      this.busy = true
      this.downloading = this.state === 'downloadable' || this.state === 'downloading'
      this.status = this.downloading ? 'Downloading on-device model (first use only)…' : ''
      let session
      try {
        session = await globalThis.LanguageModel.create({
          ...AI_TEXT_EN,
          initialPrompts: [{ role: 'system', content: this.systemPrompt() }],
        })
        let out = (await session.prompt(this.text)).trim()
        const fenceMatch = out.match(/```[a-z]*\n?([\s\S]*?)```/i)
        out = (fenceMatch ? fenceMatch[1] : out).trim()
        this.state = 'available'
        this.status = ''
        if (out) this.$emit('query', out)
      } catch (e) {
        this.status = 'AI error: ' + (e && e.message ? e.message : String(e))
      } finally {
        if (session && session.destroy) session.destroy()
        this.busy = false
        this.downloading = false
      }
    },
  },
}

// Recognized `level` label values, in the order their severity badges are
// defined below. Anything else (including a missing level) falls back to
// the neutral 'sev-none' badge showing an em dash.
const LOG_SEVERITIES = ['error', 'warn', 'info', 'debug']

const Logs = {
  components: { AiAsk },
  mixins: [routeAware('logs')],
  template: `
    <section class="view">
      <h2>Logs</h2>
      <form class="query-bar" @submit.prevent="onSubmit">
        <input
          v-model="query"
          name="logs-query"
          id="logs-query"
          list="logs-suggestions"
          placeholder='{service="my-service"}'
          spellcheck="false"
          autocapitalize="off"
        />
        <button type="submit" :disabled="loading">Run</button>
      </form>
      <ai-ask :lang="'logql'" @query="onAi"></ai-ask>
      <div class="picker" v-if="chips.length">
        <span class="picker-label">Quick:</span>
        <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">{{ c }}</button>
      </div>
      <datalist id="logs-suggestions">
        <option v-for="c in chips" :key="c" :value="c"></option>
      </datalist>
      <p v-if="error" class="error">{{ error }}</p>
      <pre class="err-caret" v-if="errorCaret">{{ errorCaret.query }}
{{ errorCaret.caret }}</pre>
      <p v-if="errorHint" class="muted err-hint">{{ errorHint }}</p>
      <p v-if="loading" class="muted">Loading…</p>
      <div class="log-rows" v-if="rows.length">
        <div
          class="log-row"
          v-for="(r, i) in rows"
          :key="i"
          :class="{ 'is-error': sevInfo(r).cls === 'sev-error' }"
        >
          <div class="log-line1">
            <span class="ts" :title="isoTime(r.tsNs)">{{ r.time }}</span>
            <span class="sev" :class="sevInfo(r).cls">{{ sevInfo(r).text }}</span>
            <span class="line" :class="{ clamped: isClamped(r.line) && !expandedRows[i] }">{{ r.line }}</span>
          </div>
          <button v-if="isClamped(r.line)" class="show-more-btn" @click="toggleExpand(i)">
            {{ expandedRows[i] ? 'show less' : 'show more' }}
          </button>
          <div class="log-line2" v-if="r.labels.length || r.traceId">
            <template v-for="(pair, j) in r.labels" :key="j">
              <a v-if="pair[0] === 'service'" class="lbl-chip lbl-chip-link" :href="serviceLogsHref(pair[1])">[{{ pair[0] }}={{ pair[1] }}]</a>
              <span v-else class="lbl-chip">[{{ pair[0] }}={{ pair[1] }}]</span>
            </template>
            <a v-if="r.traceId" class="lbl-chip trace-chip" :href="traceHref(r.traceId)">trace ⧉</a>
          </div>
        </div>
      </div>
      <p v-else-if="ran && !loading && !error" class="muted">No log lines matched.</p>
      <div class="load-more-row" v-if="rows.length">
        <button class="tv-btn" :disabled="limit >= 5000" @click="loadMore">Load more (currently {{ limit }})</button>
      </div>
    </section>
  `,
  data() {
    return {
      query: '',
      rows: [],
      error: '',
      errorHint: '',
      loading: false,
      ran: false,
      limit: 200,
      rangeStart: '',
      rangeEnd: '',
      expandedRows: {},
    }
  },
  computed: {
    chips() {
      const svcs = (window.__aniani.vocab.services || [])
        .filter((s) => (s.signals || []).includes('logs'))
        .map((s) => '{service="' + window.__aniani.escLabel(s.name) + '"}')
      return [...svcs, '{level="error"}']
    },
    // { query, caret } for the position-aligned error caret, or null when the
    // current error has no `position N` to point at. Built from the query
    // that produced the error (lastRunQuery), not the live input value.
    errorCaret() {
      if (!this.error) return null
      const m = this.error.match(/^parse error at position (\d+)/)
      if (!m) return null
      return { query: this.lastRunQuery, caret: ' '.repeat(parseInt(m[1], 10)) + '^' }
    },
  },
  methods: {
    applyRoute(params) {
      this.query = params.q || this.query
      this.rangeStart = params.start || ''
      this.rangeEnd = params.end || ''
      customWindow.active = !!(this.rangeStart && this.rangeEnd)
      if (params.q && params.q !== this.lastRunQuery) this.run()
    },
    onAi(q) { this.query = q; this.run() },
    pick(c) { this.query = c; this.run() },
    // Manual Run clears any explicit route-supplied window, reverting to the
    // timeRange preset. Chips and AI-ask call run() directly and keep it.
    onSubmit() {
      this.rangeStart = ''
      this.rangeEnd = ''
      customWindow.active = false
      this.run()
    },
    loadMore() {
      this.limit = Math.min(5000, this.limit * 2)
      this.run()
    },
    sevInfo(r) {
      const entry = r.labels.find((pair) => pair[0] === 'level')
      const lvl = entry ? String(entry[1]).toLowerCase() : ''
      return LOG_SEVERITIES.includes(lvl) ? { cls: 'sev-' + lvl, text: lvl } : { cls: 'sev-none', text: '—' }
    },
    isClamped(line) {
      return line.length > 600 || line.split('\n').length > 6
    },
    isoTime(tsNs) {
      return new Date(tsNs / 1_000_000).toISOString()
    },
    toggleExpand(i) {
      this.expandedRows[i] = !this.expandedRows[i]
    },
    href,
    serviceLogsHref(service) {
      return href('logs', { q: '{service="' + escLabel(service) + '"}' })
    },
    traceHref(traceId) {
      return href('traces', { trace: traceId })
    },
    async run() {
      this.error = ''
      this.errorHint = ''
      this.loading = true
      this.ran = true
      this.rows = []
      this.expandedRows = {}
      this.lastRunQuery = this.query
      const params = { q: this.query, range: rangeParam() }
      if (this.rangeStart && this.rangeEnd) {
        params.start = this.rangeStart
        params.end = this.rangeEnd
      }
      window.__aniani.setParams(params)
      try {
        const startNs = this.rangeStart || String(window.__aniani.rangeStartMs() * 1_000_000)
        const endNs = this.rangeEnd || String(Date.now() * 1_000_000)
        const url =
          '/loki/api/v1/query_range?query=' +
          encodeURIComponent(this.query) +
          '&start=' + startNs +
          '&end=' + endNs +
          '&limit=' + this.limit
        const res = await window.__aniani.apiGet(url)
        const result = (res.data && res.data.result) || []
        const rows = []
        for (const stream of result) {
          const labels = Object.entries(stream.stream || {})
          for (const v of stream.values || []) {
            rows.push({
              tsNs: Number(v[0]),
              time: window.__aniani.formatLocalTime(Number(v[0])),
              labels,
              line: v[1],
              traceId: (v[2] && v[2].trace_id) || '',
            })
          }
        }
        rows.sort((a, b) => b.tsNs - a.tsNs)
        this.rows = rows
      } catch (e) {
        this.error = e.message
        this.errorHint = e.hint || ''
      } finally {
        this.loading = false
      }
    },
  },
  activated() {
    // Only Logs has an explicit start/end window; register the clear
    // callback so App's range control can drop it when a preset is clicked.
    window.__aniani.clearExplicitWindow = () => {
      this.rangeStart = ''
      this.rangeEnd = ''
      customWindow.active = false
    }
  },
  deactivated() {
    window.__aniani.clearExplicitWindow = null
  },
}
// Build a `name{k="v", ...}` label for a PromQL result's `metric` object.
// Shared by the Metrics results table and LineChart's legend/tooltip.
function seriesLabelFor(metric) {
  const name = (metric && metric.__name__) || ''
  const labels = Object.entries(metric || {})
    .filter(([k]) => k !== '__name__')
    .map(([k, v]) => k + '="' + v + '"')
    .join(', ')
  return labels ? name + '{' + labels + '}' : name
}

// HH:MM in local time, from a unix-seconds timestamp (as Number).
function formatHM(tsSec) {
  const d = new Date(tsSec * 1000)
  const pad = (n) => String(n).padStart(2, '0')
  return pad(d.getHours()) + ':' + pad(d.getMinutes())
}

// Compact number formatting for axis/tooltip labels: 1234 -> '1.2k', 0.05 -> '0.05'.
function formatCompact(v) {
  if (!isFinite(v)) return ''
  const trim = (s) => s.replace(/\.?0+$/, '')
  const abs = Math.abs(v)
  if (abs >= 1e9) return trim((v / 1e9).toFixed(1)) + 'b'
  if (abs >= 1e6) return trim((v / 1e6).toFixed(1)) + 'm'
  if (abs >= 1e3) return trim((v / 1e3).toFixed(1)) + 'k'
  if (abs >= 1 || abs === 0) return trim(v.toFixed(2))
  return trim(v.toFixed(3))
}

// Classic "nice numbers" rounding (Heckbert): pick a step from {1, 2, 5} x 10^n
// so ticks land on round values. `round` picks the nearest nice fraction
// (used for the step itself); otherwise the smallest nice fraction >= input
// (used for the raw range, so the step derived from it isn't too coarse).
function niceNum(range, round) {
  const exponent = Math.floor(Math.log10(range))
  const fraction = range / Math.pow(10, exponent)
  let niceFraction
  if (round) {
    if (fraction < 1.5) niceFraction = 1
    else if (fraction < 3) niceFraction = 2
    else if (fraction < 7) niceFraction = 5
    else niceFraction = 10
  } else {
    if (fraction <= 1) niceFraction = 1
    else if (fraction <= 2) niceFraction = 2
    else if (fraction <= 5) niceFraction = 5
    else niceFraction = 10
  }
  return niceFraction * Math.pow(10, exponent)
}

// niceTicks(min, max, count≈5) -> { ticks: [Number], niceMin, niceMax }. Pure.
// min === max is padded (±10%, or ±1 around zero) before computing ticks so a
// flat series still gets a sensible y-axis instead of a zero-height range.
function niceTicks(min, max, count = 5) {
  if (min === max) {
    const pad = min !== 0 ? Math.abs(min) * 0.1 : 1
    min -= pad
    max += pad
  }
  const range = niceNum(max - min, false)
  const step = niceNum(range / (count - 1), true)
  const niceMin = Math.floor(min / step) * step
  const niceMax = Math.ceil(max / step) * step
  const ticks = []
  for (let v = niceMin; v <= niceMax + step / 2; v += step) {
    ticks.push(Math.round(v / step) * step)
  }
  return { ticks, niceMin, niceMax }
}

// Build an SVG path 'd' from a series' points, in internal chart coordinates.
// Non-finite samples break the line into separate `M`-started segments rather
// than being plotted; a segment with fewer than 2 finite points draws nothing
// (single isolated points are rendered as circles by the caller instead).
function buildPathD(points, xScale, yScale) {
  const segments = []
  let current = []
  for (const p of points) {
    if (isFinite(p.v)) current.push(p)
    else {
      if (current.length) segments.push(current)
      current = []
    }
  }
  if (current.length) segments.push(current)
  return segments
    .filter((seg) => seg.length >= 2)
    .map((seg) =>
      seg.map((p, i) => (i === 0 ? 'M ' : 'L ') + xScale(p.t).toFixed(2) + ' ' + yScale(p.v).toFixed(2)).join(' '),
    )
    .join(' ')
}

// Dependency-free multi-series SVG line chart for PromQL matrix results.
// Fixed internal coordinate space (800x240); CSS scales it to the container
// width via preserveAspectRatio="none". Legend and tooltip are plain HTML
// (absolutely positioned) since SVG text wrapping is a pain; axis ticks are
// short enough to render as SVG <text>.
const LineChart = {
  props: { result: { type: Array, default: () => [] } },
  data() {
    return { hidden: {}, hover: null, margin: { l: 46, r: 8, t: 8, b: 22 } }
  },
  computed: {
    plot() {
      const m = this.margin
      return { l: m.l, t: m.t, w: 800 - m.l - m.r, h: 240 - m.t - m.b }
    },
    series() {
      return (this.result || []).map((s, i) => ({
        name: seriesLabelFor(s.metric || {}),
        color: SERVICE_COLORS[i % SERVICE_COLORS.length],
        points: (s.values || [])
          .map((v) => ({ t: Number(v[0]), v: Number(v[1]) }))
          .filter((p) => isFinite(p.t))
          .sort((a, b) => a.t - b.t),
      }))
    },
    visibleSeries() {
      return this.series.filter((s) => !this.hidden[s.name])
    },
    allTimestamps() {
      const set = new Set()
      for (const s of this.visibleSeries) for (const p of s.points) if (isFinite(p.v)) set.add(p.t)
      return [...set].sort((a, b) => a - b)
    },
    xDomain() {
      const t = this.allTimestamps
      return t.length ? { min: t[0], max: t[t.length - 1] } : null
    },
    yDomainRaw() {
      let min = Infinity
      let max = -Infinity
      for (const s of this.visibleSeries) {
        for (const p of s.points) {
          if (isFinite(p.v)) {
            if (p.v < min) min = p.v
            if (p.v > max) max = p.v
          }
        }
      }
      return isFinite(min) ? { min, max } : null
    },
    yNice() {
      const raw = this.yDomainRaw
      return raw ? niceTicks(raw.min, raw.max) : niceTicks(0, 1)
    },
    hasData() {
      return this.xDomain !== null && this.yDomainRaw !== null
    },
    chartSeries() {
      return this.visibleSeries.map((s) => {
        const finite = s.points.filter((p) => isFinite(p.v))
        let d = ''
        let solo = null
        if (finite.length === 1) {
          solo = { x: this.xScale(finite[0].t), y: this.yScale(finite[0].v) }
        } else if (finite.length >= 2) {
          d = buildPathD(s.points, this.xScale, this.yScale)
        }
        return { name: s.name, color: s.color, d, solo }
      })
    },
    xTicks() {
      const dom = this.xDomain
      if (!dom) return []
      if (dom.max <= dom.min) return [{ x: this.xScale(dom.min), label: formatHM(dom.min), anchor: 'middle' }]
      const n = 5
      const out = []
      for (let i = 0; i < n; i++) {
        const t = dom.min + ((dom.max - dom.min) * i) / (n - 1)
        // First/last tick sit right at the plot edges — anchoring them by their
        // near edge (instead of centering) keeps the label inside the svg
        // instead of overflowing past x=0 or x=800.
        const anchor = i === 0 ? 'start' : i === n - 1 ? 'end' : 'middle'
        out.push({ x: this.xScale(t), label: formatHM(t), anchor })
      }
      return out
    },
    yTicks() {
      return this.yNice.ticks.map((v) => ({ y: this.yScale(v), label: formatCompact(v) }))
    },
    tooltipStyle() {
      if (!this.hover) return {}
      return this.hover.flip ? { right: 100 - this.hover.xPct + '%' } : { left: this.hover.xPct + '%' }
    },
  },
  methods: {
    formatHM,
    xScale(t) {
      const dom = this.xDomain
      if (!dom || dom.max <= dom.min) return this.plot.l + this.plot.w / 2
      return this.plot.l + ((t - dom.min) / (dom.max - dom.min)) * this.plot.w
    },
    yScale(v) {
      const yn = this.yNice
      const range = yn.niceMax - yn.niceMin
      if (!range) return this.plot.t + this.plot.h / 2
      return this.plot.t + this.plot.h - ((v - yn.niceMin) / range) * this.plot.h
    },
    toggleHidden(name) {
      this.hidden[name] = !this.hidden[name]
    },
    onMouseMove(evt) {
      const dom = this.xDomain
      const times = this.allTimestamps
      if (!dom || !times.length) {
        this.hover = null
        return
      }
      const rect = evt.currentTarget.getBoundingClientRect()
      if (!rect.width) return
      const xInternal = ((evt.clientX - rect.left) / rect.width) * 800
      const frac = this.plot.w ? (xInternal - this.plot.l) / this.plot.w : 0.5
      const targetTs = dom.min + Math.min(1, Math.max(0, frac)) * (dom.max - dom.min)
      let nearest = times[0]
      let bestDiff = Infinity
      for (const t of times) {
        const diff = Math.abs(t - targetTs)
        if (diff < bestDiff) {
          bestDiff = diff
          nearest = t
        }
      }
      const rows = []
      for (const s of this.visibleSeries) {
        const pt = s.points.find((p) => p.t === nearest)
        if (pt && isFinite(pt.v)) rows.push({ name: s.name, color: s.color, value: formatCompact(pt.v) })
      }
      const x = this.xScale(nearest)
      this.hover = { x, xPct: (x / 800) * 100, ts: nearest, rows, flip: x > 800 * 0.6 }
    },
    onMouseLeave() {
      this.hover = null
    },
  },
  template: `
    <div class="line-chart">
      <svg viewBox="0 0 800 240" preserveAspectRatio="none" class="lc-svg" @mousemove="onMouseMove" @mouseleave="onMouseLeave">
        <template v-if="hasData">
          <line v-for="yt in yTicks" :key="'gy' + yt.y" class="lc-grid" :x1="plot.l" :x2="800 - margin.r" :y1="yt.y" :y2="yt.y"></line>
          <text v-for="yt in yTicks" :key="'yl' + yt.y" class="lc-axis-label" :x="plot.l - 6" :y="yt.y + 3" text-anchor="end">{{ yt.label }}</text>
          <text v-for="xt in xTicks" :key="'xl' + xt.x" class="lc-axis-label" :x="xt.x" y="234" :text-anchor="xt.anchor">{{ xt.label }}</text>
          <g v-for="s in chartSeries" :key="s.name">
            <path v-if="s.d" :d="s.d" fill="none" :stroke="s.color" stroke-width="1.5"></path>
            <circle v-if="s.solo" :cx="s.solo.x" :cy="s.solo.y" r="3" :fill="s.color"></circle>
          </g>
          <line v-if="hover" class="lc-hover-line" :x1="hover.x" :x2="hover.x" :y1="margin.t" :y2="240 - margin.b"></line>
        </template>
        <text v-else class="lc-empty" x="400" y="120" text-anchor="middle">no data</text>
      </svg>
      <div class="lc-legend" v-if="series.length">
        <span
          class="lc-legend-item"
          :class="{ hidden: hidden[s.name] }"
          v-for="s in series"
          :key="s.name"
          @click="toggleHidden(s.name)"
        >
          <span class="lc-swatch" :style="{ background: s.color }"></span>{{ s.name || '(all)' }}
        </span>
      </div>
      <div class="lc-tooltip" v-if="hover && hover.rows.length" :style="tooltipStyle">
        <div class="lc-tooltip-time">{{ formatHM(hover.ts) }}</div>
        <div class="lc-tooltip-row" v-for="r in hover.rows" :key="r.name">
          <span class="lc-swatch" :style="{ background: r.color }"></span>{{ r.name || '(all)' }}: {{ r.value }}
        </div>
      </div>
    </div>
  `,
}

const Metrics = {
  components: { AiAsk, LineChart },
  mixins: [routeAware('metrics')],
  template: `
    <section class="view">
      <h2>Metrics</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
          name="metrics-query"
          id="metrics-query"
          list="metric-names"
          placeholder="rate(http_requests_total[5m])"
          spellcheck="false"
          autocapitalize="off"
        />
        <button type="submit" :disabled="loading">Run</button>
      </form>
      <ai-ask :lang="'promql'" @query="onAi"></ai-ask>
      <div class="picker" v-if="metricServices.length">
        <span class="picker-label">Service:</span>
        <select class="svc-select" v-model="service" @change="onService">
          <option value="">All services</option>
          <option v-for="s in metricServices" :key="s" :value="s">{{ s }}</option>
        </select>
      </div>
      <div class="picker" v-if="chips.length">
        <span class="picker-label">Metrics:</span>
        <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">{{ c }}</button>
      </div>
      <datalist id="metric-names">
        <option v-for="c in chips" :key="c" :value="c"></option>
      </datalist>
      <p v-if="error" class="error">{{ error }}</p>
      <p v-if="loading" class="muted">Loading…</p>
      <line-chart v-if="matrix.length" :result="matrix"></line-chart>
      <table v-if="rows.length" class="results">
        <thead><tr><th>Series</th><th>Latest value</th></tr></thead>
        <tbody>
          <tr v-for="(r, i) in rows" :key="i">
            <td class="labels">{{ r.series }}</td>
            <td class="value">{{ r.value }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else-if="ran && !loading && !error" class="muted">No series matched.</p>
    </section>
  `,
  data() {
    return { query: '', rows: [], matrix: [], error: '', loading: false, ran: false, service: '' }
  },
  computed: {
    metricServices() {
      return (window.__aniani.vocab.services || [])
        .filter((s) => (s.signals || []).includes('metrics'))
        .map((s) => s.name)
    },
    chips() {
      const v = window.__aniani.vocab
      if (this.service && v.catalog[this.service]) return v.catalog[this.service].metrics || []
      return v.metricNames || []
    },
  },
  methods: {
    applyRoute(params) {
      this.query = params.q || this.query
      if (params.service && params.service !== this.service) {
        this.service = params.service
        this.onService()
      }
      if (params.q && params.q !== this.lastRunQuery) this.run()
    },
    onAi(q) { this.query = q; this.run() },
    async onService() { if (this.service) await window.__aniani.loadCatalog(this.service) },
    pick(c) { this.query = c; this.run() },
    seriesLabel(metric) {
      return seriesLabelFor(metric)
    },
    async run() {
      this.error = ''
      this.loading = true
      this.ran = true
      this.rows = []
      this.matrix = []
      this.lastRunQuery = this.query
      window.__aniani.setParams({ q: this.query, service: this.service || '', range: rangeParam() })
      try {
        const endSec = Math.floor(Date.now() / 1000)
        const startSec = Math.floor(window.__aniani.rangeStartMs() / 1000)
        const url =
          '/api/v1/query_range?query=' +
          encodeURIComponent(this.query) +
          '&start=' + startSec +
          '&end=' + endSec +
          '&step=60'
        const res = await window.__aniani.apiGet(url)
        const data = res.data || {}
        this.matrix = data.resultType === 'matrix' ? (data.result || []) : []
        if (data.resultType === 'scalar' && Array.isArray(data.result)) {
          // scalar: data.result is a single [ts, value] tuple, not a series array
          this.rows = [{ series: '(scalar)', value: data.result[1] }]
        } else {
          const result = data.result || []
          this.rows = result.map((s) => {
            let value = ''
            if (Array.isArray(s.values) && s.values.length) {
              value = s.values[s.values.length - 1][1] // matrix: last sample
            } else if (Array.isArray(s.value)) {
              value = s.value[1] // vector: single sample
            }
            return { series: this.seriesLabel(s.metric || {}), value }
          })
        }
      } catch (e) {
        this.error = e.message
      } finally {
        this.loading = false
      }
    },
  },
}
const Traces = {
  components: { AiAsk, TraceView },
  mixins: [routeAware('traces')],
  template: `
    <section class="view">
      <h2>Traces</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
          name="traces-query"
          id="traces-query"
          list="traces-suggestions"
          placeholder='{ resource.service.name = "my-service" }'
          spellcheck="false"
          autocapitalize="off"
        />
        <button type="submit" :disabled="loading">Run</button>
      </form>
      <ai-ask :lang="'traceql'" @query="onAi"></ai-ask>
      <div class="picker" v-if="chips.length">
        <span class="picker-label">Quick:</span>
        <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">{{ c }}</button>
      </div>
      <datalist id="traces-suggestions">
        <option v-for="c in chips" :key="c" :value="c"></option>
      </datalist>
      <p v-if="error" class="error">{{ error }}</p>
      <p v-if="loading" class="muted">Loading…</p>
      <p v-if="!traces.length && ran && !loading && !error && !selectedId" class="muted">No traces matched.</p>
      <div class="traces-layout" v-if="traces.length || selectedId">
        <div class="tr-list-col" v-if="traces.length">
          <div class="tr-sort">
            <span class="picker-label">Sort:</span>
            <button
              v-for="m in sortModes"
              :key="m"
              class="tv-btn"
              :class="{ active: sortMode === m }"
              @click="sortMode = m"
            >{{ m }}</button>
          </div>
          <ul class="tr-list">
            <li
              v-for="t in sortedTraces"
              :key="t.traceID"
              :class="{ active: t.traceID === selectedId }"
              @click="open(t.traceID)"
            >
              <div class="tr-item-head">
                <span class="tr-item-name">{{ t.rootTraceName || t.rootServiceName }}</span>
                <span class="tr-item-dur">{{ t.durationMs }}ms</span>
              </div>
              <div class="tr-item-sub">
                <span class="tr-item-svc">{{ t.rootServiceName }}</span>
                <span class="mono">{{ shortId(t.traceID) }}</span>
              </div>
              <div class="tr-item-meta">
                <span class="tr-badge-err" v-if="t.errorCount > 0">{{ t.errorCount }} errors</span>
                <span class="tr-item-ago">{{ agoLabel(t.startTimeUnixNano) }}</span>
              </div>
            </li>
          </ul>
        </div>
        <div class="traces-detail">
          <div v-if="selectedId" class="detail">
            <h3 class="mono">Trace {{ selectedId }}</h3>
            <p v-if="detailError" class="error">{{ detailError }}</p>
            <p v-else-if="!selected" class="muted">Loading…</p>
            <trace-view v-else :key="selectedId" :detail="selected" :trace-id="selectedId"></trace-view>
          </div>
          <p v-else class="muted">Select a trace on the left to view its timeline.</p>
        </div>
      </div>
    </section>
  `,
  data() {
    return {
      query: '',
      traces: [],
      error: '',
      loading: false,
      ran: false,
      selected: null,
      selectedId: '',
      detailError: '',
      routeTraceId: '', // set by applyRoute; consumed by open() below
      sortMode: 'recent',
      sortModes: ['recent', 'slowest', 'errors'],
    }
  },
  computed: {
    chips() {
      return (window.__aniani.vocab.services || [])
        .filter((s) => (s.signals || []).includes('traces'))
        .map((s) => '{ resource.service.name = "' + window.__aniani.escLabel(s.name) + '" }')
    },
    // Client-side sort of the search results; never mutates `traces` itself.
    // Start times are big nanosecond strings — compare as BigInt so ordering
    // stays correct past Number's safe integer range.
    sortedTraces() {
      const cmpStartDesc = (a, b) => {
        const d = BigInt(b.startTimeUnixNano) - BigInt(a.startTimeUnixNano)
        return d > 0n ? 1 : d < 0n ? -1 : 0
      }
      const arr = [...this.traces]
      if (this.sortMode === 'slowest') {
        arr.sort((a, b) => b.durationMs - a.durationMs)
      } else if (this.sortMode === 'errors') {
        arr.sort((a, b) => b.errorCount - a.errorCount || cmpStartDesc(a, b))
      } else {
        arr.sort(cmpStartDesc)
      }
      return arr
    },
  },
  methods: {
    agoLabel,
    applyRoute(params) {
      this.query = params.q || this.query
      this.routeTraceId = params.trace || this.routeTraceId
      if (params.q && params.q !== this.lastRunQuery) this.run()
      if (params.trace && params.trace !== this.selectedId) this.open(params.trace)
    },
    onAi(q) { this.query = q; this.run() },
    pick(c) { this.query = c; this.run() },
    shortId(id) {
      return id && id.length > 16 ? id.slice(0, 8) + '…' + id.slice(-4) : id
    },
    async run() {
      this.error = ''
      this.loading = true
      this.ran = true
      this.traces = []
      this.selected = null
      this.selectedId = ''
      this.detailError = ''
      this.lastRunQuery = this.query
      window.__aniani.setParams({ q: this.query, range: rangeParam() })
      try {
        const endSec = Math.floor(Date.now() / 1000)
        const startSec = Math.floor(window.__aniani.rangeStartMs() / 1000)
        const url =
          '/api/search?q=' + encodeURIComponent(this.query) +
          '&start=' + startSec + '&end=' + endSec + '&limit=20'
        const res = await window.__aniani.apiGet(url)
        this.traces = res.traces || []
      } catch (e) {
        this.error = e.message
      } finally {
        this.loading = false
      }
    },
    async open(id) {
      this.detailError = ''
      this.selectedId = id
      this.selected = null
      window.__aniani.setParams({ q: this.query, trace: id, range: rangeParam() })
      try {
        const data = await window.__aniani.apiGet('/api/traces/' + encodeURIComponent(id))
        if (this.selectedId === id) this.selected = data // ignore stale responses
      } catch (e) {
        if (this.selectedId === id) this.detailError = e.message
      }
    },
  },
}

// Reactive route, kept in sync with location.hash. `seq` bumps on every
// hashchange so views can watch a single primitive instead of deep-watching
// `params`.
const route = reactive({ tab: 'home', params: {}, seq: 0 })

// Re-derive route from location.hash. Bound to the 'hashchange' event, and
// called once before the initial mount to honor a deep link on page load.
function syncFromLocation() {
  const { tab, params } = parseHash(location.hash)
  route.tab = tab
  route.params = params
  route.seq++
}

// Update the hash's query params in place (same tab) without adding a history
// entry and without re-triggering applyRoute. history.replaceState never
// fires 'hashchange' (only direct location.hash assignment or user
// navigation does), so this is loop-free with no extra flag needed.
function setParams(params) {
  const next = buildHash(route.tab, params)
  const current = location.hash || '#/'
  if (next !== current) history.replaceState(null, '', next)
  route.params = params
}
window.addEventListener('hashchange', syncFromLocation)

// Mixin factory: wires a view's applyRoute(params) to first mount, keep-alive
// reactivation, and hash changes while `tabId` is the active tab. Also owns
// the generic half of the auto-refresh contract: registers this view's run()
// as window.__aniani.activeRerun while it's the active (kept-alive) tab, so
// App's refresh timer and range-preset clicks can re-run it.
function routeAware(tabId) {
  return {
    data() {
      return { lastRunQuery: '' }
    },
    computed: {
      routeSeq() { return route.seq },
    },
    watch: {
      routeSeq() {
        if (route.tab === tabId) {
          applyRangeParam(route.params)
          this.applyRoute(route.params)
        }
      },
    },
    mounted() {
      if (route.tab === tabId) {
        applyRangeParam(route.params)
        this.applyRoute(route.params)
      }
    },
    activated() {
      if (route.tab === tabId) {
        applyRangeParam(route.params)
        this.applyRoute(route.params)
      }
      this._rerun = () => { if (this.query) this.run() }
      window.__aniani.activeRerun = this._rerun
      this._queryInput = () => this.$el.querySelector('.query-bar input')
      window.__aniani.activeQueryInput = this._queryInput
    },
    deactivated() {
      if (window.__aniani.activeRerun === this._rerun) window.__aniani.activeRerun = null
      if (window.__aniani.activeQueryInput === this._queryInput) window.__aniani.activeQueryInput = null
    },
  }
}

const App = {
  components: { Landing, Logs, Metrics, Traces },
  template: `
    <div class="app">
      <header>
        <h1>Aniani</h1>
        <nav class="tabs">
          <a
            v-for="t in tabs"
            :key="t.id"
            :href="href(t.id, {})"
            :class="{ active: route.tab === t.id }"
          >{{ t.label }}</a>
          <div class="range-ctl" v-if="route.tab !== 'home'">
            <span class="range-group">
              <button
                v-for="p in rangePresets"
                :key="p"
                class="range-btn"
                :class="{ active: (route.tab !== 'logs' || !customWindow.active) && timeRange.preset === p }"
                @click="pickRange(p)"
              >{{ p }}</button>
              <button v-if="route.tab === 'logs' && customWindow.active" class="range-btn active" disabled>custom</button>
            </span>
            <span class="range-group">
              <button
                v-for="r in refreshOptions"
                :key="r"
                class="range-btn"
                :class="{ active: timeRange.refresh === r }"
                @click="timeRange.refresh = r"
              >{{ r }}</button>
            </span>
          </div>
        </nav>
      </header>
      <main>
        <keep-alive>
          <component :is="current"></component>
        </keep-alive>
      </main>
    </div>
  `,
  data() {
    return {
      route,
      tabs: [
        { id: 'home', label: 'Overview', comp: 'Landing' },
        { id: 'logs', label: 'Logs', comp: 'Logs' },
        { id: 'metrics', label: 'Metrics', comp: 'Metrics' },
        { id: 'traces', label: 'Traces', comp: 'Traces' },
      ],
      rangePresets: Object.keys(RANGE_PRESETS),
      refreshOptions: ['off', '5s', '30s'],
      refreshTimer: null,
    }
  },
  computed: {
    current() {
      const t = this.tabs.find((x) => x.id === this.route.tab)
      return t ? t.comp : 'Landing'
    },
    // Computed indirection onto the module-scope reactive singletons: exposes
    // them to the template, and gives the refresh-mode watcher below a
    // reactive dependency it can see (a plain string-path watch can't reach
    // outside component data).
    timeRange() { return timeRange },
    customWindow() { return customWindow },
    refreshMode() { return timeRange.refresh },
  },
  watch: {
    refreshMode() {
      this.resetRefreshTimer()
    },
  },
  methods: {
    href,
    pickRange(p) {
      timeRange.preset = p
      customWindow.active = false
      if (window.__aniani.clearExplicitWindow) window.__aniani.clearExplicitWindow()
      if (window.__aniani.activeRerun) window.__aniani.activeRerun()
    },
    // Clears any existing interval before (maybe) creating a new one, so a
    // refresh-mode change never leaves a stale interval running alongside it.
    resetRefreshTimer() {
      if (this.refreshTimer) {
        clearInterval(this.refreshTimer)
        this.refreshTimer = null
      }
      const ms = { '5s': 5000, '30s': 30000 }[timeRange.refresh]
      if (!ms) return
      this.refreshTimer = setInterval(() => {
        if (!document.hidden && window.__aniani.activeRerun) window.__aniani.activeRerun()
      }, ms)
    },
  },
  mounted() {
    this.resetRefreshTimer()
  },
  beforeUnmount() {
    if (this.refreshTimer) clearInterval(this.refreshTimer)
  },
}

// Export the shared helpers so later tasks can reference them within this file.
window.__aniani = {
  apiGet, formatLocalTime, escLabel, vocab, loadVocab, loadCatalog, route, href, setParams,
  rangeStartMs, timeRange, customWindow,
  activeRerun: null,
  clearExplicitWindow: null,
  activeQueryInput: null,
}

// Global `/` shortcut: focus the active view's query input, unless the user
// is already typing in a form control (or chording with a modifier key).
window.addEventListener('keydown', (e) => {
  if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey) return
  const tag = document.activeElement && document.activeElement.tagName
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return
  if (!window.__aniani.activeQueryInput) return
  const input = window.__aniani.activeQueryInput()
  if (!input) return
  e.preventDefault()
  input.focus()
})

loadVocab()
syncFromLocation()
createApp(App).mount('#app')
