import { escLabel, href, sigEmoji, signalLinks, SERVICE_COLORS } from './core.js'

// --- trace-view helpers ---------------------------------------------------

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
export const TraceView = {
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
            </div>
          </div>
        </div>
      </template>
    </div>
    <p v-else class="muted">No spans in this trace.</p>
  `,
}
