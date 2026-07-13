import { apiGet, formatBytes, formatUptime, href, loadVocab, signalLinks, vocab } from './core.js'

export const Landing = {
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
