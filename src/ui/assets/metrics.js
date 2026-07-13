import { apiGet, loadCatalog, rangeParam, rangeStartMs, routeAware, setParams, vocab } from './core.js'
import { QueryBar } from './querybar.js'
import { AiAsk } from './ai-ask.js'
import { LineChart, seriesLabelFor } from './charts.js'

export const Metrics = {
  components: { AiAsk, LineChart, QueryBar },
  mixins: [routeAware('metrics')],
  template: `
    <section class="view">
      <h2>Metrics</h2>
      <query-bar
        v-model="query"
        lang="promql"
        :loading="loading"
        :metric-names="chips"
        placeholder="rate(http_requests_total[5m])"
        @run="run"
      ></query-bar>
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
        <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">
          {{ c }}<span class="chip-type" v-if="metricType(c)">{{ metricType(c) }}</span>
        </button>
      </div>
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
    metricType(name) {
      const t = (window.__aniani.vocab.metricMeta[name] || {}).type
      if (!t || t === 'unknown') return ''
      // Short badge: c=counter, g=gauge, h=histogram, s=summary
      return ({ counter: 'c', gauge: 'g', histogram: 'h', summary: 's' })[t] || ''
    },
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
        window.__aniani.recordHistory('promql', this.query)
      } catch (e) {
        this.error = e.message
      } finally {
        this.loading = false
      }
    },
  },
}
