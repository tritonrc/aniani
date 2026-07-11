import { createApp, reactive } from 'vue'
import {
  apiGet,
  RANGE_PRESETS, timeRange, rangeToSec, rangeStartMs, rangeParam, customWindow, applyRangeParam,
  formatLocalTime, agoLabel, formatBytes, formatUptime,
  parseHash, buildHash, href, escLabel,
  vocab, loadVocab, loadCatalog,
  signalLinks, sigEmoji,
  route, syncFromLocation, setParams, routeAware,
} from './core.js'
import { recordHistory } from './autocomplete.js'
import { Landing } from './landing.js'
import { Logs } from './logs.js'
import { Metrics } from './metrics.js'
import { Traces } from './traces.js'

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
  rangeStartMs, timeRange, customWindow, recordHistory,
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
