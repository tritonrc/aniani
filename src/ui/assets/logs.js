import { apiGet, customWindow, escLabel, formatLocalTime, href, rangeParam, rangeStartMs, route, routeAware, setParams, vocab } from './core.js'
import { QueryBar } from './querybar.js'
import { AiAsk } from './ai-ask.js'

// Recognized `level` label values, in the order their severity badges are
// defined below. Anything else (including a missing level) falls back to
// the neutral 'sev-none' badge showing an em dash.
const LOG_SEVERITIES = ['error', 'warn', 'info', 'debug']

export const Logs = {
  components: { AiAsk, QueryBar },
  mixins: [routeAware('logs')],
  template: `
    <section class="view">
      <h2>Logs</h2>
      <query-bar
        v-model="query"
        lang="logql"
        :loading="loading"
        placeholder='{service="my-service"}'
        @run="onSubmit"
      ></query-bar>
      <ai-ask :lang="'logql'" @query="onAi"></ai-ask>
      <div class="picker" v-if="chips.length">
        <span class="picker-label">Quick:</span>
        <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">{{ c }}</button>
      </div>
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
        window.__aniani.recordHistory('logql', this.query)
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
