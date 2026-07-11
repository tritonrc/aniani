import { reactive } from 'vue'
import { apiGet, vocab } from './core.js'

// Lazily-fetched label key/value cache for a query language's labels. `keys`
// is null until first fetched (then an array, possibly empty); `values[key]`
// follows the same convention per label name. A failed fetch caches an empty
// array rather than retrying every keystroke.
function makeLabelCache(keysUrl, valuesUrl) {
  const cache = reactive({ keys: null, values: {} })
  async function loadKeys() {
    if (cache.keys) return cache.keys
    try {
      const res = await apiGet(keysUrl)
      cache.keys = res.data || []
    } catch (_) {
      cache.keys = []
    }
    return cache.keys
  }
  async function loadValues(key) {
    if (cache.values[key]) return cache.values[key]
    try {
      const res = await apiGet(valuesUrl(key))
      cache.values[key] = res.data || []
    } catch (_) {
      cache.values[key] = []
    }
    return cache.values[key]
  }
  return { cache, loadKeys, loadValues }
}
const logqlLabels = makeLabelCache(
  '/loki/api/v1/labels',
  (k) => '/loki/api/v1/label/' + encodeURIComponent(k) + '/values',
)
const promqlLabels = makeLabelCache(
  '/api/v1/labels',
  (k) => '/api/v1/label/' + encodeURIComponent(k) + '/values',
)

// PromQL functions Aniani actually evaluates (the eval_call arms + aggregation
// ops), each with a signature + one-line doc surfaced in the completion drop.
// `cat` drives snippet insertion (see contextItems metrics-fns). Nothing beyond
// this set parses successfully.
const PROMQL_FN_META = [
  { name: 'rate', cat: 'range', sig: 'rate(v range-vector)', doc: 'Per-second average rate of increase of a counter over the range.' },
  { name: 'increase', cat: 'range', sig: 'increase(v range-vector)', doc: 'Total increase of a counter over the range.' },
  { name: 'irate', cat: 'range', sig: 'irate(v range-vector)', doc: 'Per-second instant rate from the last two samples in the range.' },
  { name: 'delta', cat: 'range', sig: 'delta(v range-vector)', doc: 'Difference between the first and last value of a gauge over the range.' },
  { name: 'deriv', cat: 'range', sig: 'deriv(v range-vector)', doc: 'Per-second derivative of a gauge via linear regression.' },
  { name: 'histogram_quantile', cat: 'hq', sig: 'histogram_quantile(φ, v)', doc: 'φ-quantile (0–1) estimated from classic histogram buckets.' },
  { name: 'sum', cat: 'agg', sig: 'sum(v)', doc: 'Sum of values over the grouping dimensions.' },
  { name: 'avg', cat: 'agg', sig: 'avg(v)', doc: 'Average of values over the grouping dimensions.' },
  { name: 'max', cat: 'agg', sig: 'max(v)', doc: 'Maximum value over the grouping dimensions.' },
  { name: 'min', cat: 'agg', sig: 'min(v)', doc: 'Minimum value over the grouping dimensions.' },
  { name: 'count', cat: 'agg', sig: 'count(v)', doc: 'Count of elements in the vector.' },
  { name: 'topk', cat: 'topk', sig: 'topk(k, v)', doc: 'The k largest elements by value.' },
  { name: 'bottomk', cat: 'topk', sig: 'bottomk(k, v)', doc: 'The k smallest elements by value.' },
  { name: 'abs', cat: 'simple', sig: 'abs(v)', doc: 'Absolute value of each sample.' },
  { name: 'ceil', cat: 'simple', sig: 'ceil(v)', doc: 'Round each sample up to the nearest integer.' },
  { name: 'floor', cat: 'simple', sig: 'floor(v)', doc: 'Round each sample down to the nearest integer.' },
  { name: 'round', cat: 'simple', sig: 'round(v, to)', doc: 'Round each sample to the nearest multiple of to (default 1).' },
  { name: 'sort', cat: 'simple', sig: 'sort(v)', doc: 'Sort the vector ascending by value.' },
  { name: 'sort_desc', cat: 'simple', sig: 'sort_desc(v)', doc: 'Sort the vector descending by value.' },
  { name: 'absent', cat: 'simple', sig: 'absent(v)', doc: 'Returns 1 if the vector is empty, otherwise nothing.' },
  { name: 'scalar', cat: 'simple', sig: 'scalar(v)', doc: 'Value of a single-series vector as a scalar (else NaN).' },
  { name: 'vector', cat: 'simple', sig: 'vector(s)', doc: 'Convert a scalar into a single-element vector.' },
  { name: 'clamp', cat: 'simple', sig: 'clamp(v, min, max)', doc: 'Clamp values to the [min, max] range.' },
  { name: 'clamp_min', cat: 'simple', sig: 'clamp_min(v, min)', doc: 'Clamp values to a lower bound.' },
  { name: 'clamp_max', cat: 'simple', sig: 'clamp_max(v, max)', doc: 'Clamp values to an upper bound.' },
  { name: 'label_replace', cat: 'simple', sig: 'label_replace(v, dst, repl, src, regex)', doc: 'Set label dst from a regex capture of label src.' },
  { name: 'label_join', cat: 'simple', sig: 'label_join(v, dst, sep, src…)', doc: 'Join src label values with sep into label dst.' },
  { name: 'time', cat: 'nilad', sig: 'time()', doc: 'Current Unix time in seconds.' },
]

// Completion item(s) for a PromQL function: snippet text + caret position that
// teaches the call shape. Aggregations yield a second `f by (…)` grouping form.
function promqlFnItems(f) {
  const base = { detail: f.sig, doc: f.doc }
  if (f.cat === 'range') {
    // rate([5m]) with the caret before `[` so the metric is typed next.
    return [{ text: f.name, insert: f.name + '([5m])', caret: f.name.length + 1, ...base }]
  }
  if (f.cat === 'hq') {
    const ins = f.name + '(0.95, )'
    return [{ text: f.name, insert: ins, caret: ins.length - 1, ...base }]
  }
  if (f.cat === 'topk') {
    const ins = f.name + '(5, )'
    return [{ text: f.name, insert: ins, caret: ins.length - 1, ...base }]
  }
  if (f.cat === 'agg') {
    return [
      { text: f.name, insert: f.name + '()', caret: f.name.length + 1, ...base },
      { text: f.name + ' by (…)', insert: f.name + ' by () ()', caret: f.name.length + 5, ...base },
    ]
  }
  if (f.cat === 'nilad') {
    const ins = f.name + '()'
    return [{ text: f.name, insert: ins, caret: ins.length, ...base }]
  }
  // simple single-/multi-arg: caret parked inside the parens.
  return [{ text: f.name, insert: f.name + '()', caret: f.name.length + 1, ...base }]
}

// Completion item(s) for a metric name: the bare metric, plus — for counter-
// named metrics — a rate(metric[5m]) form teaching the counter→rate pattern.
function promqlMetricItems(name) {
  const items = [{ text: name, insert: name, caret: name.length }]
  if (/_(total|count)$/.test(name)) {
    const ins = 'rate(' + name + '[5m])'
    items.push({ text: ins, insert: ins, caret: ins.length, detail: 'per-second rate', doc: 'Per-second rate of the ' + name + ' counter over 5m.' })
  }
  return items
}

const PROMQL_DURATIONS = ['1m', '5m', '15m', '1h']
const LOGQL_LINE_OPS = ['|=', '!=', '|~', '!~']
const TRACEQL_STATUS_VALUES = ['error', 'ok', 'unset']
const TRACEQL_DURATIONS = ['100ms', '250ms', '500ms', '1s']
// The only keys src/query/traceql/parser.rs gives dedicated grammar to
// (parse_condition dispatches on "duration", "status", "name" by keyword;
// everything else falls through to a resource./span. attribute). `span.` and
// `resource.` are offered as explorable scope prefixes that chain into
// attribute-name completion (see suggestTraceql's trace-attrs handling).
const TRACEQL_KEYS = ['resource.service.name', 'span.', 'resource.', 'duration', 'status', 'name']
// Completion text for a trace key primes the next keystroke into its own
// suggestion context — e.g. accepting "resource.service.name" leaves the
// cursor right after `= "`, which immediately triggers the label-values
// context below. The bare `span.`/`resource.` inserts leave the cursor at
// the end, re-opening the drop to offer attribute names.
const TRACEQL_KEY_INSERT = {
  'resource.service.name': 'resource.service.name = "',
  'span.': 'span.',
  'resource.': 'resource.',
  name: 'name = "',
  status: 'status = ',
  duration: 'duration ',
}

// Union of span attribute keys across all trace-reporting services, lazily
// fetched from /api/v1/catalog (keys are open-ended, so we enumerate them from
// what's actually been ingested). null until first load, then a sorted array.
const traceAttrs = reactive({ keys: null })
async function loadTraceAttrs() {
  if (traceAttrs.keys) return traceAttrs.keys
  const set = new Set()
  try {
    const services = (vocab.services || [])
      .filter((s) => (s.signals || []).includes('traces'))
      .map((s) => s.name)
    const results = await Promise.all(
      services.map((svc) =>
        apiGet('/api/v1/catalog?service=' + encodeURIComponent(svc)).catch(() => null),
      ),
    )
    for (const res of results) {
      const attrs = res && res.data && res.data.span_attributes
      if (Array.isArray(attrs)) attrs.forEach((a) => set.add(a))
    }
  } catch (_) {
    // leave the accumulated set as-is
  }
  traceAttrs.keys = [...set].sort()
  return traceAttrs.keys
}

// Index where the partial token being typed begins: the run of `[\w.-]` chars
// immediately before the cursor (dots for dotted attribute keys, hyphens for
// label values like "us-east"; without the hyphen the context detector would
// flip back to label-keys mid-value).
function partialTokenStart(before) {
  const m = before.match(/[\w.-]*$/)
  return before.length - (m ? m[0].length : 0)
}

// Count of unmatched `open` brackets in `text`. Naive (it doesn't understand
// quoting), which is fine for a lightweight suggestion heuristic.
function bracketDepth(text, open, close) {
  let depth = 0
  for (const c of text) {
    if (c === open) depth++
    else if (c === close) depth = Math.max(0, depth - 1)
  }
  return depth
}

const LABEL_VALUE_RE = /([\w]+)\s*(=~?|!~|!=)\s*"$/
const TRACE_VALUE_RE = /([\w.]+)\s*(=~?|!~|!=)\s*"$/

function suggestLogql(text, pos) {
  const before = text.slice(0, pos)
  const partialStart = partialTokenStart(before)
  const beforePartial = before.slice(0, partialStart)
  const valueMatch = beforePartial.match(LABEL_VALUE_RE)
  if (valueMatch) return { kind: 'label-values', key: valueMatch[1], partialStart }
  if (bracketDepth(before, '{', '}') > 0) return { kind: 'label-keys', partialStart }
  if (before.includes('}')) {
    // The partial for an operator is the trailing run of operator chars, not
    // word chars — so an already-typed `|` is replaced by the accepted `|=`
    // instead of doubled.
    const opPartial = before.match(/[|!~=]*$/)
    return { kind: 'line-ops', partialStart: before.length - opPartial[0].length }
  }
  return null
}

function suggestPromql(text, pos) {
  const before = text.slice(0, pos)
  const partialStart = partialTokenStart(before)
  const beforePartial = before.slice(0, partialStart)
  if (bracketDepth(before, '{', '}') > 0) {
    const valueMatch = beforePartial.match(LABEL_VALUE_RE)
    if (valueMatch) return { kind: 'label-values', key: valueMatch[1], partialStart }
    return { kind: 'label-keys', partialStart }
  }
  if (bracketDepth(before, '[', ']') > 0) return { kind: 'durations', partialStart }
  // Inside an unclosed by(…)/without(…) grouping list → suggest label names.
  if (/\b(?:by|without)\s*\([^)]*$/.test(beforePartial)) {
    return { kind: 'grouping-labels', partialStart }
  }
  return { kind: 'metrics-fns', partialStart }
}

function suggestTraceql(text, pos) {
  const before = text.slice(0, pos)
  if (bracketDepth(before, '{', '}') <= 0) return null
  const partialStart = partialTokenStart(before)
  const beforePartial = before.slice(0, partialStart)
  const valueMatch = beforePartial.match(TRACE_VALUE_RE)
  if (valueMatch) return { kind: 'label-values', key: valueMatch[1], partialStart }
  if (/status\s*(=|!=)\s*$/.test(beforePartial)) return { kind: 'status-values', partialStart }
  if (/duration\s*(=|!=|>=|<=|>|<)\s*$/.test(beforePartial)) return { kind: 'durations', partialStart }
  // After a completed condition (closed quoted value, or a status/duration
  // comparison) → offer && / || to chain another condition.
  if (
    /"\s*$/.test(beforePartial) ||
    /status\s*(?:=|!=)\s*(?:error|ok|unset)\s*$/.test(beforePartial) ||
    /duration\s*(?:>=|<=|>|<|=|!=)\s*[\d.]+(?:ns|us|µs|ms|s|m|h)?\s*$/.test(beforePartial)
  ) {
    return { kind: 'logic-ops', partialStart }
  }
  // Typing a span./resource.-scoped token → complete against the catalog's
  // fully-qualified attribute keys. partialStart stays at the token start so
  // the whole `span.`/`resource.` prefix is replaced (the keys already carry
  // it); the typed prefix just fuzzy-filters the list.
  const partial = before.slice(partialStart)
  if (partial.startsWith('span.') || partial.startsWith('resource.')) {
    return { kind: 'trace-attrs', partialStart }
  }
  return { kind: 'trace-keys', partialStart }
}

// Context-aware suggestion classifier, shared by all three query languages.
// Returns null when there's nothing contextual to suggest (the caller falls
// back to history in that case). `partialStart` is where the in-progress
// token — the text an accepted suggestion replaces — begins.
export function suggestContext(lang, text, pos) {
  if (lang === 'promql') return suggestPromql(text, pos)
  if (lang === 'traceql') return suggestTraceql(text, pos)
  return suggestLogql(text, pos)
}

// Candidate list for a resolved context, as { text, insert } pairs ready for
// display and insertion. Reads the lazily-fetched label caches and the shared
// vocab directly (both reactive, so callers re-render once data loads).
export function contextItems(lang, ctx, metricNamesProp) {
  const kind = ctx.kind
  if (kind === 'label-keys') {
    const cache = lang === 'promql' ? promqlLabels.cache : logqlLabels.cache
    return (cache.keys || []).map((k) => ({ text: k, insert: k + '="' }))
  }
  if (kind === 'label-values') {
    let values
    if (lang === 'traceql') {
      values = ctx.key === 'resource.service.name' ? (vocab.services || []).map((s) => s.name) : []
    } else {
      const cache = lang === 'promql' ? promqlLabels.cache : logqlLabels.cache
      values = cache.values[ctx.key] || []
    }
    return values.map((v) => ({ text: v, insert: v + '"' }))
  }
  if (kind === 'line-ops') {
    return LOGQL_LINE_OPS.map((op) => ({ text: op, insert: op + ' "' }))
  }
  if (kind === 'durations') {
    const list = lang === 'traceql' ? TRACEQL_DURATIONS : PROMQL_DURATIONS
    return list.map((d) => ({ text: d, insert: d }))
  }
  if (kind === 'status-values') {
    return TRACEQL_STATUS_VALUES.map((s) => ({ text: s, insert: s }))
  }
  if (kind === 'trace-keys') {
    return TRACEQL_KEYS.map((k) => ({ text: k, insert: TRACEQL_KEY_INSERT[k] }))
  }
  if (kind === 'trace-attrs') {
    return (traceAttrs.keys || []).map((k) => ({ text: k, insert: k + ' = "' }))
  }
  if (kind === 'logic-ops') {
    return [
      { text: '&&', insert: '&& ', detail: 'AND' },
      { text: '||', insert: '|| ', detail: 'OR' },
    ]
  }
  if (kind === 'metrics-fns') {
    const names = metricNamesProp || vocab.metricNames || []
    const metricItems = names.flatMap(promqlMetricItems)
    const fnItems = PROMQL_FN_META.flatMap(promqlFnItems)
    return [...metricItems, ...fnItems]
  }
  if (kind === 'grouping-labels') {
    return (promqlLabels.cache.keys || []).map((k) => ({ text: k, insert: k, caret: k.length }))
  }
  return []
}

// Ensures the label cache data a resolved context needs is loaded (a no-op
// once cached). TraceQL contexts never need a fetch — their data is static or
// drawn from the already-loaded vocab.
export function ensureContextData(lang, ctx) {
  if (lang === 'traceql') {
    return ctx.kind === 'trace-attrs' ? loadTraceAttrs() : Promise.resolve()
  }
  const cache = lang === 'promql' ? promqlLabels : logqlLabels
  if (ctx.kind === 'label-keys' || ctx.kind === 'grouping-labels') return cache.loadKeys()
  if (ctx.kind === 'label-values') return cache.loadValues(ctx.key)
  return Promise.resolve()
}

// --- query history: localStorage-backed, most-recent-first, capped ---------
const QHISTORY_CAP = 50

function historyKey(lang) {
  return 'aniani.qhistory.' + lang
}
export function historyFor(lang) {
  try {
    const raw = localStorage.getItem(historyKey(lang))
    const arr = raw ? JSON.parse(raw) : []
    return Array.isArray(arr) ? arr : []
  } catch (_) {
    return []
  }
}
export function recordHistory(lang, q) {
  if (!q || !q.trim()) return
  try {
    const cur = historyFor(lang).filter((x) => x !== q)
    cur.unshift(q)
    localStorage.setItem(historyKey(lang), JSON.stringify(cur.slice(0, QHISTORY_CAP)))
  } catch (_) {
    // localStorage unavailable (private mode, quota) — history just won't persist.
  }
}

// --- starred queries: localStorage-backed, pinned, capped -----------------
const QSTAR_CAP = 20

function starKey(lang) {
  return 'aniani.qstarred.' + lang
}
export function starredFor(lang) {
  try {
    const raw = localStorage.getItem(starKey(lang))
    const arr = raw ? JSON.parse(raw) : []
    return Array.isArray(arr) ? arr : []
  } catch (_) {
    return []
  }
}
// Add q if absent, remove it if present. Most-recent-first, capped.
export function toggleStarred(lang, q) {
  if (!q || !q.trim()) return
  try {
    const cur = starredFor(lang)
    const i = cur.indexOf(q)
    if (i >= 0) cur.splice(i, 1)
    else cur.unshift(q)
    localStorage.setItem(starKey(lang), JSON.stringify(cur.slice(0, QSTAR_CAP)))
  } catch (_) {
    // localStorage unavailable — stars just won't persist.
  }
}

// Human-readable group header per suggestion kind, shown above the dropdown items.
export const KIND_LABELS = {
  'label-keys': 'labels',
  'label-values': 'values',
  'line-ops': 'operators',
  durations: 'durations',
  'metrics-fns': 'metrics & functions',
  'trace-keys': 'keys',
  'trace-attrs': 'attributes',
  'status-values': 'status',
  'grouping-labels': 'group by',
  'logic-ops': 'operators',
}

// Starter queries shown in the empty-state drop (before any history exists),
// so a new user sees valid, runnable syntax for each language.
export const EXAMPLE_QUERIES = {
  logql: ['{service="payments", level="error"}', '{service="gateway"} |= "timeout"'],
  promql: [
    'rate(http_requests_total{service="gateway"}[5m])',
    'sum by (service) (rate(http_requests_total[5m]))',
    'http_request_duration_ms{service="inventory"}',
  ],
  traceql: ['{ resource.service.name = "payments" }', '{ duration > 500ms }', '{ status = error }'],
}

// Rank completion items against the in-progress token: exact-prefix beats
// substring beats subsequence; non-matches are dropped. Stable within a tier
// (preserves the caller's order). Empty partial → items unchanged.
export function fuzzyRank(items, partial) {
  const p = (partial || '').toLowerCase()
  if (!p) return items
  const scored = []
  for (const it of items) {
    const t = it.text.toLowerCase()
    let score
    if (t.startsWith(p)) score = 0
    else if (t.includes(p)) score = 1
    else if (isSubsequence(p, t)) score = 2
    else continue
    scored.push({ it, score })
  }
  scored.sort((a, b) => a.score - b.score)
  return scored.map((s) => s.it)
}

// True if every char of `p` appears in `t` in order (not necessarily adjacent).
function isSubsequence(p, t) {
  let i = 0
  for (const c of t) {
    if (c === p[i]) i++
    if (i === p.length) return true
  }
  return i === p.length
}
