import { agoLabel, apiGet, escLabel, rangeParam, rangeStartMs, routeAware, setParams, vocab } from './core.js'
import { QueryBar } from './querybar.js'
import { AiAsk } from './ai-ask.js'
import { TraceView } from './trace.js'

export const Traces = {
  components: { AiAsk, TraceView, QueryBar },
  mixins: [routeAware('traces')],
  template: `
    <section class="view">
      <h2>Traces</h2>
      <query-bar
        v-model="query"
        lang="traceql"
        :loading="loading"
        placeholder='{ resource.service.name = "my-service" }'
        @run="run"
      ></query-bar>
      <ai-ask :lang="'traceql'" @query="onAi"></ai-ask>
      <div class="picker" v-if="chips.length">
        <span class="picker-label">Quick:</span>
        <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">{{ c }}</button>
      </div>
      <p v-if="error" class="error">{{ error }}</p>
      <p v-if="loading" class="muted">Loading…</p>
      <div class="tr-aggregate" v-if="aggregate.length">
        <button class="tv-btn" @click="showAggregate = !showAggregate">{{ showAggregate ? 'Hide' : 'Show' }} operation breakdown</button>
        <div class="tr-aggregate-bars" v-if="showAggregate">
          <div class="tr-aggregate-row" v-for="(r, i) in aggregate" :key="i" :title="r.count + ' matched span(s)' + (r.errors ? ', ' + r.errors + ' error(s)' : '')">
            <span class="tr-aggregate-label"><span class="tr-aggregate-svc">{{ r.service }}</span><span class="tr-aggregate-op">{{ r.name }}</span></span>
            <span class="tr-aggregate-track"><span class="tr-aggregate-bar" :class="{ err: r.errors }" :style="{ width: r.pct + '%' }"></span></span>
            <span class="tr-aggregate-count">{{ r.count }}<span class="tr-aggregate-err" v-if="r.errors"> ({{ r.errors }}err)</span></span>
          </div>
        </div>
      </div>
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
      showAggregate: false,
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
    // Flame-graph-style breakdown: flatten matched spans across all result
    // traces, group by (service, operation), and size horizontal bars by
    // count. Surfaces which operations dominate a multi-trace result set.
    aggregate() {
      const map = new Map()
      for (const t of this.traces) {
        for (const ss of t.spanSets || []) {
          for (const sp of ss.spans || []) {
            const key = (sp.serviceName || '?') + '\u0000' + (sp.name || '?')
            const e = map.get(key) || { service: sp.serviceName || '?', name: sp.name || '?', count: 0, errors: 0 }
            e.count++
            if (sp.status === 'error') e.errors++
            map.set(key, e)
          }
        }
      }
      const rows = [...map.values()].sort((a, b) => b.count - a.count)
      const max = rows.length ? rows[0].count : 1
      return rows.map((r) => ({ ...r, pct: Math.max(3, (r.count / max) * 100) }))
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
        window.__aniani.recordHistory('traceql', this.query)
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
