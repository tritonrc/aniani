import { createApp } from 'vue'

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

// Placeholder components replaced in Tasks 4-6.
const Logs = {
  template: `
    <section class="view">
      <h2>Logs</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
          placeholder='{service="my-service"}'
          spellcheck="false"
          autocapitalize="off"
        />
        <button type="submit" :disabled="loading">Run</button>
      </form>
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
  methods: {
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
  template: `
    <section class="view">
      <h2>Metrics</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
          placeholder="rate(http_requests_total[5m])"
          spellcheck="false"
          autocapitalize="off"
        />
        <button type="submit" :disabled="loading">Run</button>
      </form>
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
    return { query: '', rows: [], error: '', loading: false, ran: false }
  },
  methods: {
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
  template: `
    <section class="view">
      <h2>Traces</h2>
      <form class="query-bar" @submit.prevent="run">
        <input
          v-model="query"
          placeholder='{ .service.name = "my-service" }'
          spellcheck="false"
          autocapitalize="off"
        />
        <button type="submit" :disabled="loading">Run</button>
      </form>
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
      <div v-if="selected" class="detail">
        <h3>Trace {{ selectedId }}</h3>
        <p v-if="detailError" class="error">{{ detailError }}</p>
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
  methods: {
    pretty(v) {
      return JSON.stringify(v, null, 2)
    },
    async run() {
      this.error = ''
      this.loading = true
      this.ran = true
      this.traces = []
      this.selected = null
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
window.__aniani = { apiGet, hourAgoMs }

createApp(App).mount('#app')
