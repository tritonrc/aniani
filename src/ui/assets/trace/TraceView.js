import { buildTraceModel, formatDuration } from '../trace-model.js'
import { TraceRuler } from './TraceRuler.js'
import { TraceMinimap } from './TraceMinimap.js'
import { SpanRow } from './SpanRow.js'
import { SpanDetail } from './SpanDetail.js'

// Jaeger-style trace waterfall: a service-colored timeline of spans with a
// collapsible tree and click-to-expand detail (tags, process, events,
// exceptions, links). Owns collapse/expand state and derives the visible
// rows; the subtree components are presentational. Host-specific pivot links
// are passed through to the span detail as optional builder props.
export const TraceView = {
  name: 'TraceView',
  components: { TraceRuler, TraceMinimap, SpanRow, SpanDetail },
  props: {
    detail: { type: Object, required: true }, // raw /api/traces/{id} payload
    traceId: { type: String, default: '' },
    buildLogsHref: { type: Function, default: null },
    buildTraceHref: { type: Function, default: null },
    serviceLinks: { type: Function, default: null },
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
    // Bird's-eye Gantt of every span (ignoring collapse state), shown for large
    // traces where the full waterfall needs scrolling.
    showMinimap() {
      return (this.model && this.model.count >= 40) || false
    },
    minimapBars() {
      const m = this.model
      if (!m) return []
      const total = m.totalNs || 1
      return m.spans.map((s) => {
        let leftPct = Math.min(100, Math.max(0, (s.offsetNs / total) * 100))
        let widthPct = (s.durationNs / total) * 100
        if (!isFinite(widthPct) || widthPct < 0) widthPct = 0
        widthPct = Math.min(100, Math.max(widthPct, 0.3))
        if (leftPct + widthPct > 100) leftPct = Math.max(0, 100 - widthPct)
        return { uid: s.uid, leftPct, widthPct, color: m.serviceColor[s.service], error: s.statusCode === 2 }
      })
    },
  },
  methods: {
    formatDuration,
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
    pretty(v) {
      return JSON.stringify(v, null, 2)
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
      <trace-minimap v-if="showMinimap" :bars="minimapBars" :count="model.count"></trace-minimap>
      <pre class="json" v-if="showRaw">{{ pretty(detail) }}</pre>
      <template v-else>
        <trace-ruler :ticks="ticks"></trace-ruler>
        <div class="tl-rows">
          <div class="tl-row-group" v-for="row in rows" :key="row.span.uid">
            <span-row
              :row="row"
              :collapsed="!!collapsed[row.span.uid]"
              :open="!!expanded[row.span.uid]"
              @toggle-collapse="toggleCollapse(row.span.uid)"
              @toggle-detail="toggleDetail(row.span.uid)"
            ></span-row>
            <span-detail
              v-if="expanded[row.span.uid]"
              :span="row.span"
              :build-logs-href="buildLogsHref"
              :build-trace-href="buildTraceHref"
              :service-links="serviceLinks"
            ></span-detail>
          </div>
        </div>
      </template>
    </div>
    <p v-else class="muted">No spans in this trace.</p>
  `,
}
