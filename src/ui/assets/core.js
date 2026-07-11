import { reactive } from 'vue'

// --- shared fetch helper: parses JSON, throws a useful message on error ---
export async function apiGet(url) {
  let res
  let text
  try {
    res = await fetch(url)
    text = await res.text()
  } catch (e) {
    throw new Error('network error: ' + e.message)
  }
  let json = null
  try {
    json = JSON.parse(text)
  } catch {
    // leave json null; fall through to error handling below
  }
  if (!res.ok || (json && json.status === 'error')) {
    const msg = (json && (json.error || json.message)) || text || res.statusText
    const err = new Error(msg)
    if (json && json.hint) err.hint = json.hint
    throw err
  }
  return json
}

// --- shared time-range state ------------------------------------------------
// Preset window (used by Logs/Metrics/Traces queries) and auto-refresh mode,
// shared across views via the App header control.
export const RANGE_PRESETS = { '5m': 300, '15m': 900, '1h': 3600, '2h': 7200 }
export const timeRange = reactive({ preset: '1h', refresh: 'off' })

export function rangeToSec() {
  return RANGE_PRESETS[timeRange.preset] || 3600
}
export function rangeStartMs() {
  return Date.now() - rangeToSec() * 1000
}

// Range value for the `range` URL param: the default '1h' preset is omitted
// (implicit), everything else is passed through explicitly.
export function rangeParam() {
  return timeRange.preset === '1h' ? '' : timeRange.preset
}

// True when a view is showing an explicit start/end window (currently only
// Logs, via the span → logs pivot) instead of the timeRange preset; drives
// the 'custom' chip in App's range control.
export const customWindow = reactive({ active: false })

// Sync timeRange.preset from a hash `range` param, when present and known.
// Called by routeAware before a view's own applyRoute runs, so the header
// control reflects the URL on every route change.
export function applyRangeParam(params) {
  if (params && params.range && RANGE_PRESETS[params.range]) {
    timeRange.preset = params.range
  }
}

// HH:MM:SS.mmm in local time, from a nanosecond timestamp (as Number).
export function formatLocalTime(tsNs) {
  const d = new Date(tsNs / 1_000_000)
  const pad = (n, len) => String(n).padStart(len, '0')
  return pad(d.getHours(), 2) + ':' + pad(d.getMinutes(), 2) + ':' + pad(d.getSeconds(), 2) + '.' + pad(d.getMilliseconds(), 3)
}

// Relative-time label from a nanosecond timestamp (string or number): '12s ago',
// '3m ago', '1h ago'. Rounded to the nearest unit; never negative.
export function agoLabel(startNs) {
  const diffSec = (Date.now() * 1e6 - Number(startNs)) / 1e9
  const s = Math.max(0, Math.round(diffSec))
  if (s < 60) return s + 's ago'
  if (s < 3600) return Math.round(s / 60) + 'm ago'
  return Math.round(s / 3600) + 'h ago'
}

// Human byte size: '512 B', '63.1 KB', '1.2 MB'.
export function formatBytes(n) {
  if (n < 1024) return n + ' B'
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB'
  return (n / (1024 * 1024)).toFixed(1) + ' MB'
}

// Human uptime from a seconds count: '42s', '12m 3s', '2h 13m'.
export function formatUptime(sec) {
  const s = Math.floor(sec)
  if (s < 60) return s + 's'
  if (s < 3600) return Math.floor(s / 60) + 'm ' + (s % 60) + 's'
  return Math.floor(s / 3600) + 'h ' + Math.floor((s % 3600) / 60) + 'm'
}

// --- hash-router helpers ---------------------------------------------------

// '#/logs?q=%7B...%7D' -> { tab: 'logs', params: { q: '{...}' } }
// Empty/unknown hash -> { tab: 'home', params: {} }. Unknown params kept verbatim.
export function parseHash(hash) {
  const body = (hash || '').replace(/^#\/?/, '')
  const qIdx = body.indexOf('?')
  const tab = (qIdx === -1 ? body : body.slice(0, qIdx)) || 'home'
  const qs = qIdx === -1 ? '' : body.slice(qIdx + 1)
  return { tab, params: Object.fromEntries(new URLSearchParams(qs)) }
}

// buildHash('logs', { q: '{service="x"}', start: '123' }) -> '#/logs?q=...&start=123'
// Omits empty/null params. buildHash('home', {}) -> '#/'
export function buildHash(tab, params) {
  const usp = new URLSearchParams()
  for (const [k, v] of Object.entries(params || {})) {
    if (v === null || v === undefined || v === '') continue
    usp.set(k, v)
  }
  const qs = usp.toString()
  const path = tab && tab !== 'home' ? tab : ''
  return '#/' + path + (qs ? '?' + qs : '')
}
export const href = buildHash // alias used in templates for <a> links

// Escape a label value for safe insertion into a double-quoted query literal.
export function escLabel(v) {
  return String(v).replace(/\\/g, '\\\\').replace(/"/g, '\\"')
}

// Stable per-trace service color palette (assigned by sorted service order).
export const SERVICE_COLORS = [
  '#4ea1ff', '#ff9f43', '#26de81', '#fc5c65', '#a55eea',
  '#fed330', '#2bcbba', '#fd9644', '#778ca3', '#eb3b5a',
]

export const vocab = reactive({
  services: [],     // [{ name, signals: [] }]
  metricNames: [],  // ["http_request_duration_ms", ...]
  catalog: {},      // { [service]: { metrics: [], log_labels: [] } }
})
export async function loadVocab() {
  const [s, m] = await Promise.allSettled([
    apiGet('/api/v1/services'),
    apiGet('/api/v1/label/__name__/values'),
  ])
  if (s.status === 'fulfilled') vocab.services = (s.value.data && s.value.data.services) || []
  if (m.status === 'fulfilled') vocab.metricNames = m.value.data || []
}
export async function loadCatalog(service) {
  if (!service) return null
  if (vocab.catalog[service]) return vocab.catalog[service]
  try {
    const c = await apiGet('/api/v1/catalog?service=' + encodeURIComponent(service))
    vocab.catalog[service] = (c && c.data) || {}
    return vocab.catalog[service]
  } catch (_) { return null }
}

// Cross-signal pivot links for a service: one entry per signal it reports
// (per vocab.services), each pre-filled so landing on the target tab needs no
// further typing. The metrics link carries only `service` (pre-selects the
// dropdown and loads its catalog chips) since there's no single query to run.
export function signalLinks(service) {
  const entry = (vocab.services || []).find((s) => s.name === service)
  const signals = (entry && entry.signals) || []
  const links = []
  if (signals.includes('logs')) {
    links.push({ label: 'logs', href: href('logs', { q: '{service="' + escLabel(service) + '"}' }) })
  }
  if (signals.includes('metrics')) {
    links.push({ label: 'metrics', href: href('metrics', { service }) })
  }
  if (signals.includes('traces')) {
    links.push({ label: 'traces', href: href('traces', { q: '{ resource.service.name = "' + escLabel(service) + '" }' }) })
  }
  return links
}

// Emoji glyph for a signalLinks() entry, used as a compact icon in the
// TraceView span detail (see the tl-svc-cell / sig-icons markup above).
export function sigEmoji(label) {
  if (label === 'logs') return '📄'
  if (label === 'metrics') return '📈'
  return '🔍'
}

// Reactive route, kept in sync with location.hash. `seq` bumps on every
// hashchange so views can watch a single primitive instead of deep-watching
// `params`.
export const route = reactive({ tab: 'home', params: {}, seq: 0 })

// Re-derive route from location.hash. Bound to the 'hashchange' event, and
// called once before the initial mount to honor a deep link on page load.
export function syncFromLocation() {
  const { tab, params } = parseHash(location.hash)
  route.tab = tab
  route.params = params
  route.seq++
}

// Update the hash's query params in place (same tab) without adding a history
// entry and without re-triggering applyRoute. history.replaceState never
// fires 'hashchange' (only direct location.hash assignment or user
// navigation does), so this is loop-free with no extra flag needed.
export function setParams(params) {
  const next = buildHash(route.tab, params)
  const current = location.hash || '#/'
  if (next !== current) history.replaceState(null, '', next)
  route.params = params
}
window.addEventListener('hashchange', syncFromLocation)

// Mixin factory: wires a view's applyRoute(params) to first mount, keep-alive
// reactivation, and hash changes while `tabId` is the active tab. Also owns
// the generic half of the auto-refresh contract: registers this view's run()
// as window.__aniani.activeRerun while it's the active (kept-alive) tab, so
// App's refresh timer and range-preset clicks can re-run it.
export function routeAware(tabId) {
  return {
    data() {
      return { lastRunQuery: '' }
    },
    computed: {
      routeSeq() { return route.seq },
    },
    watch: {
      routeSeq() {
        if (route.tab === tabId) {
          applyRangeParam(route.params)
          this.applyRoute(route.params)
        }
      },
    },
    mounted() {
      if (route.tab === tabId) {
        applyRangeParam(route.params)
        this.applyRoute(route.params)
      }
    },
    activated() {
      if (route.tab === tabId) {
        applyRangeParam(route.params)
        this.applyRoute(route.params)
      }
      this._rerun = () => { if (this.query) this.run() }
      window.__aniani.activeRerun = this._rerun
      this._queryInput = () => this.$el.querySelector('.query-bar input')
      window.__aniani.activeQueryInput = this._queryInput
    },
    deactivated() {
      if (window.__aniani.activeRerun === this._rerun) window.__aniani.activeRerun = null
      if (window.__aniani.activeQueryInput === this._queryInput) window.__aniani.activeQueryInput = null
    },
  }
}
