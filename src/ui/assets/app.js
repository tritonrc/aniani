import { createApp, reactive } from 'vue'

// --- shared fetch helper: parses JSON, throws a useful message on error ---
async function apiGet(url) {
  let res
  try {
    res = await fetch(url)
  } catch (e) {
    throw new Error('network error: ' + e.message)
  }
  const text = await res.text()
  let json = null
  try {
    json = JSON.parse(text)
  } catch {
    // leave json null; fall through to error handling below
  }
  if (!res.ok || (json && json.status === 'error')) {
    const msg = (json && (json.error || json.message)) || text || res.statusText
    throw new Error(msg)
  }
  return json
}

// last-hour time window helpers
function hourAgoMs() {
  return Date.now() - 3600 * 1000
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

const Landing = {
  template: `
    <section class="view">
      <h2>Overview</h2>
      <p v-if="error" class="error">{{ error }}</p>
      <h3>Services</h3>
      <ul class="services" v-if="services.length">
        <li v-for="s in services" :key="s.name">
          <strong>{{ s.name }}</strong>
          <span class="signals">{{ (s.signals || []).join(', ') }}</span>
        </li>
      </ul>
      <p v-else-if="!error" class="muted">No services have reported telemetry yet.</p>
      <h3>Status</h3>
      <pre class="json" v-if="status">{{ pretty(status) }}</pre>
    </section>
  `,
  data() {
    return { services: [], status: null, error: '' }
  },
  methods: {
    pretty(v) {
      return JSON.stringify(v, null, 2)
    },
  },
  async mounted() {
    try {
      const svc = await apiGet('/api/v1/services')
      this.services = (svc.data && svc.data.services) || []
      this.status = await apiGet('/api/v1/status')
    } catch (e) {
      this.error = e.message
    }
  },
}

const AiAsk = {
  props: { lang: { type: String, required: true } },
  emits: ['query'],
  template: `
    <div class="ai-ask" v-if="supported">
      <input
        v-model="text"
        :placeholder="placeholder"
        spellcheck="false"
        @keydown.enter.prevent="ask"
      />
      <button @click="ask" :disabled="busy || state === 'downloading'">
        {{ state === 'downloading' ? 'Downloading model…' : 'Ask AI' }}
      </button>
      <span class="ai-status" v-if="status">{{ status }}</span>
    </div>
  `,
  data() {
    return { supported: false, state: 'unknown', text: '', busy: false, status: '' }
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
    if (!('LanguageModel' in globalThis)) return
    try {
      this.state = await globalThis.LanguageModel.availability()
      this.supported = this.state !== 'unavailable'
    } catch (_) {
      this.supported = false
    }
  },
  methods: {
    systemPrompt() {
      const v = window.__aniani.vocab
      const services = (v.services || []).map((s) => s.name).join(', ') || '(none)'
      const metrics = (v.metricNames || []).join(', ') || '(none)'
      const syntax = {
        logql: 'LogQL stream selectors like {service="x", level="error"} optionally followed by a |= "substring" filter',
        promql: 'PromQL using the metric names below, e.g. metric_name or rate(metric_name[5m])',
        traceql: 'TraceQL like { resource.service.name = "x" }',
      }[this.lang]
      return (
        'You translate a natural-language request into a single ' +
        this.langName +
        ' query for the Aniani observability engine. ' +
        'Output ONLY the query, no explanation, no code fences. ' +
        'Use ' + syntax + '. ' +
        'Known service names: ' + services + '. ' +
        'Known metric names: ' + metrics + '.'
      )
    },
    async ask() {
      if (!this.text.trim()) return
      this.busy = true
      this.status = ''
      try {
        if (this.state === 'downloadable' || this.state === 'downloading') {
          this.state = 'downloading'
          this.status = 'Downloading on-device model (first use only)…'
        }
        const session = await globalThis.LanguageModel.create({
          initialPrompts: [{ role: 'system', content: this.systemPrompt() }],
        })
        let out = (await session.prompt(this.text)).trim()
        const fenceMatch = out.match(/```[a-z]*\n?([\s\S]*?)```/i)
        out = (fenceMatch ? fenceMatch[1] : out).trim()
        session.destroy && session.destroy()
        this.state = 'available'
        this.status = ''
        if (out) this.$emit('query', out)
      } catch (e) {
        this.status = 'AI error: ' + (e && e.message ? e.message : String(e))
      } finally {
        this.busy = false
      }
    },
  },
}

// Placeholder components replaced in Tasks 4-6.
const Logs = {
  components: { AiAsk },
  template: `
    <section class="view">
      <h2>Logs</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
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
      <p v-if="loading" class="muted">Loading…</p>
      <table v-if="rows.length" class="results">
        <thead><tr><th>Time</th><th>Labels</th><th>Line</th></tr></thead>
        <tbody>
          <tr v-for="(r, i) in rows" :key="i">
            <td class="ts">{{ r.time }}</td>
            <td class="labels">{{ r.labels }}</td>
            <td class="line">{{ r.line }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else-if="ran && !loading && !error" class="muted">No log lines matched.</p>
    </section>
  `,
  data() {
    return { query: '', rows: [], error: '', loading: false, ran: false }
  },
  computed: {
    chips() {
      const svcs = (window.__aniani.vocab.services || [])
        .filter((s) => (s.signals || []).includes('logs'))
        .map((s) => '{service="' + s.name + '"}')
      return [...svcs, '{level="error"}']
    },
  },
  methods: {
    onAi(q) { this.query = q; this.run() },
    pick(c) { this.query = c; this.run() },
    async run() {
      this.error = ''
      this.loading = true
      this.ran = true
      this.rows = []
      try {
        const endNs = String(Date.now() * 1_000_000)
        const startNs = String(window.__aniani.hourAgoMs() * 1_000_000)
        const url =
          '/loki/api/v1/query_range?query=' +
          encodeURIComponent(this.query) +
          '&start=' + startNs +
          '&end=' + endNs +
          '&limit=200'
        const res = await window.__aniani.apiGet(url)
        const result = (res.data && res.data.result) || []
        const rows = []
        for (const stream of result) {
          const labels = Object.entries(stream.stream || {})
            .map(([k, v]) => k + '=' + v)
            .join(' ')
          for (const [tsNs, line] of stream.values || []) {
            rows.push({
              tsNs: Number(tsNs),
              time: new Date(Number(tsNs) / 1_000_000).toISOString(),
              labels,
              line,
            })
          }
        }
        rows.sort((a, b) => b.tsNs - a.tsNs)
        this.rows = rows
      } catch (e) {
        this.error = e.message
      } finally {
        this.loading = false
      }
    },
  },
}
const Metrics = {
  components: { AiAsk },
  template: `
    <section class="view">
      <h2>Metrics</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
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
    return { query: '', rows: [], error: '', loading: false, ran: false, service: '' }
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
    onAi(q) { this.query = q; this.run() },
    async onService() { if (this.service) await window.__aniani.loadCatalog(this.service) },
    pick(c) { this.query = c; this.run() },
    seriesLabel(metric) {
      const name = metric.__name__ || ''
      const labels = Object.entries(metric)
        .filter(([k]) => k !== '__name__')
        .map(([k, v]) => k + '="' + v + '"')
        .join(', ')
      return labels ? name + '{' + labels + '}' : name
    },
    async run() {
      this.error = ''
      this.loading = true
      this.ran = true
      this.rows = []
      try {
        const endSec = Math.floor(Date.now() / 1000)
        const startSec = Math.floor(window.__aniani.hourAgoMs() / 1000)
        const url =
          '/api/v1/query_range?query=' +
          encodeURIComponent(this.query) +
          '&start=' + startSec +
          '&end=' + endSec +
          '&step=60'
        const res = await window.__aniani.apiGet(url)
        const result = (res.data && res.data.result) || []
        this.rows = result.map((s) => {
          let value = ''
          if (Array.isArray(s.values) && s.values.length) {
            value = s.values[s.values.length - 1][1] // matrix: last sample
          } else if (Array.isArray(s.value)) {
            value = s.value[1] // vector: single sample
          }
          return { series: this.seriesLabel(s.metric || {}), value }
        })
      } catch (e) {
        this.error = e.message
      } finally {
        this.loading = false
      }
    },
  },
}
const Traces = {
  components: { AiAsk },
  template: `
    <section class="view">
      <h2>Traces</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
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
      <table v-if="traces.length" class="results">
        <thead><tr><th>Trace ID</th><th>Root service</th><th>Duration (ms)</th></tr></thead>
        <tbody>
          <tr v-for="t in traces" :key="t.traceID" @click="open(t.traceID)" class="clickable">
            <td class="mono">{{ t.traceID }}</td>
            <td>{{ t.rootServiceName }}</td>
            <td class="value">{{ t.durationMs }}</td>
          </tr>
        </tbody>
      </table>
      <p v-else-if="ran && !loading && !error" class="muted">No traces matched.</p>
      <div v-if="selectedId" class="detail">
        <h3>Trace {{ selectedId }}</h3>
        <p v-if="detailError" class="error">{{ detailError }}</p>
        <p v-else-if="!selected" class="muted">Loading…</p>
        <pre class="json" v-else>{{ pretty(selected) }}</pre>
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
    }
  },
  computed: {
    chips() {
      return (window.__aniani.vocab.services || [])
        .filter((s) => (s.signals || []).includes('traces'))
        .map((s) => '{ resource.service.name = "' + s.name + '" }')
    },
  },
  methods: {
    onAi(q) { this.query = q; this.run() },
    pick(c) { this.query = c; this.run() },
    pretty(v) {
      return JSON.stringify(v, null, 2)
    },
    async run() {
      this.error = ''
      this.loading = true
      this.ran = true
      this.traces = []
      this.selected = null
      this.selectedId = ''
      this.detailError = ''
      try {
        const url =
          '/api/search?q=' + encodeURIComponent(this.query) + '&limit=20'
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
      try {
        this.selected = await window.__aniani.apiGet(
          '/api/traces/' + encodeURIComponent(id),
        )
      } catch (e) {
        this.detailError = e.message
      }
    },
  },
}

const App = {
  components: { Landing, Logs, Metrics, Traces },
  template: `
    <div class="app">
      <header>
        <h1>Aniani</h1>
        <nav class="tabs">
          <button
            v-for="t in tabs"
            :key="t.id"
            :class="{ active: activeTab === t.id }"
            @click="activeTab = t.id"
          >{{ t.label }}</button>
        </nav>
      </header>
      <main>
        <component :is="current"></component>
      </main>
    </div>
  `,
  data() {
    return {
      activeTab: 'home',
      tabs: [
        { id: 'home', label: 'Overview', comp: 'Landing' },
        { id: 'logs', label: 'Logs', comp: 'Logs' },
        { id: 'metrics', label: 'Metrics', comp: 'Metrics' },
        { id: 'traces', label: 'Traces', comp: 'Traces' },
      ],
    }
  },
  computed: {
    current() {
      const t = this.tabs.find((x) => x.id === this.activeTab)
      return t ? t.comp : 'Landing'
    },
  },
}

// Export the shared helpers so later tasks can reference them within this file.
window.__aniani = { apiGet, hourAgoMs, vocab, loadVocab, loadCatalog }

loadVocab()
createApp(App).mount('#app')
