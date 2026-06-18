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
const Logs = { template: `<section class="view"><h2>Logs</h2></section>` }
const Metrics = { template: `<section class="view"><h2>Metrics</h2></section>` }
const Traces = { template: `<section class="view"><h2>Traces</h2></section>` }

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
