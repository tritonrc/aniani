// --- trace view model: OTLP payload → render model --------------------------
// This module is deliberately dependency-free (no core.js, no Vue) so the
// trace viewer can be vendored into another no-build app as-is. Anything that
// touches aniani specifically (hash-router hrefs, signal pivots) stays out and
// reaches the components through props instead.

// Stable per-trace service color palette (assigned by sorted service order).
export const SERVICE_COLORS = [
  '#4ea1ff', '#ff9f43', '#26de81', '#fc5c65', '#a55eea',
  '#fed330', '#2bcbba', '#fd9644', '#778ca3', '#eb3b5a',
]

// OTLP `SpanKind` numeric codes, by array position.
const SPAN_KIND_LABELS = ['unspecified', 'internal', 'server', 'client', 'producer', 'consumer']

export function spanKindLabel(k) {
  return (SPAN_KIND_LABELS[k] || 'unspecified').toUpperCase()
}

export function statusLabel(code) {
  return code === 2 ? 'ERROR' : code === 1 ? 'OK' : 'UNSET'
}

export function statusClass(code) {
  return code === 2 ? 'err' : code === 1 ? 'ok' : 'muted'
}

// Human-readable duration. Nanoseconds in, adaptive unit out.
export function formatDuration(ns) {
  const n = Number(ns) || 0
  if (n >= 1e9) return (n / 1e9).toFixed(2) + 's'
  if (n >= 1e6) return (n / 1e6).toFixed(2) + 'ms'
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'µs'
  return Math.round(n) + 'ns'
}

// Abbreviate a 16/8-byte hex id for compact display.
export function shortId(id) {
  if (!id) return ''
  return id.length > 12 ? id.slice(0, 8) + '…' : id
}

// Parse a stringified-nanosecond field as BigInt; never throw on bad input.
function toBigNs(v) {
  try {
    if (v === null || v === undefined || v === '') return 0n
    return BigInt(v)
  } catch {
    return 0n
  }
}

function serviceFromResource(resource) {
  const attrs = (resource && resource.attributes) || []
  const a = attrs.find((x) => x.key === 'service.name')
  return (a && a.value && a.value.stringValue) || 'unknown'
}

// Map OTLP attributes into flat {key, value, title} rows. Long values —
// common for the structured types (arrays, byte strings, key-value lists)
// — are truncated for table density with the full text carried in `title`
// for hover. `title` is empty for short values so the cell renders cleanly.
const ATTR_VALUE_MAX = 96
function mapAttrs(list) {
  return (list || []).map((a) => {
    const raw = (a.value && a.value.stringValue) != null ? String(a.value.stringValue) : ''
    const tooLong = raw.length > ATTR_VALUE_MAX
    return {
      key: a.key,
      value: tooLong ? raw.slice(0, ATTR_VALUE_MAX) + '…' : raw,
      title: tooLong ? raw : '',
    }
  })
}

// Normalize one OTLP span (from /api/traces/{id}) into a flat view model.
// Absolute nanosecond timestamps exceed Number's safe integer range, so they
// are kept as BigInt; only small intra-trace offsets get narrowed to Number.
function normalizeSpan(sp, service) {
  return {
    spanId: sp.spanId || '',
    parentSpanId: sp.parentSpanId || '',
    name: sp.name || '(unnamed)',
    service,
    startBig: toBigNs(sp.startTimeUnixNano),
    endBig: toBigNs(sp.endTimeUnixNano),
    statusCode: (sp.status && sp.status.code) || 0,
    statusMessage: (sp.status && sp.status.message) || '',
    kind: sp.kind || 0,
    attributes: mapAttrs(sp.attributes),
    events: (sp.events || []).map((ev) => ({
      name: ev.name || '',
      timeBig: toBigNs(ev.timeUnixNano),
      attributes: mapAttrs(ev.attributes),
    })),
    links: (sp.links || []).map((l) => ({
      traceId: l.traceId || '',
      spanId: l.spanId || '',
      traceState: l.traceState || '',
      flags: l.flags || 0,
      attributes: mapAttrs(l.attributes),
    })),
  }
}

// An event is an exception if it is named `exception` or carries any
// `exception.*` attribute (OTLP's recorded-exception convention).
export function isException(ev) {
  return ev.name === 'exception' || ev.attributes.some((a) => a.key && a.key.indexOf('exception.') === 0)
}

export function attrVal(ev, key) {
  const a = ev.attributes.find((x) => x.key === key)
  return a ? a.value : ''
}

// Parse the /api/traces/{id} payload into a render model: flat spans with
// intra-trace offsets, a parent/child tree, per-service colors, and totals.
export function buildTraceModel(detail) {
  const spans = []
  for (const b of (detail && detail.batches) || []) {
    const service = serviceFromResource(b.resource)
    for (const ss of b.scopeSpans || []) {
      for (const sp of ss.spans || []) spans.push(normalizeSpan(sp, service))
    }
  }
  if (!spans.length) return null
  spans.forEach((s, i) => { s.uid = i }) // stable unique id; spanIds may collide

  // Timeline bounds: ignore spans with a missing (zero) start so one bad span
  // can't collapse minStart to 0 and skew every offset to ~now.
  const timed = spans.filter((s) => s.startBig > 0n)
  const base = timed.length ? timed : spans
  let minStart = base[0].startBig
  let maxEnd = base[0].endBig
  for (const s of base) {
    if (s.startBig < minStart) minStart = s.startBig
    if (s.endBig > maxEnd) maxEnd = s.endBig
  }
  if (maxEnd < minStart) maxEnd = minStart
  const totalNs = Number(maxEnd - minStart)
  const clamp = (n) => Math.min(totalNs, Math.max(0, n))
  for (const s of spans) {
    s.offsetNs = clamp(Number(s.startBig - minStart))
    s.durationNs = Math.max(0, Number(s.endBig - s.startBig))
    for (const ev of s.events) ev.offsetNs = clamp(Number(ev.timeBig - minStart))
  }

  // One node per span. byId resolves a parentSpanId to the FIRST span carrying
  // that id, so duplicate spanIds keep distinct nodes instead of clobbering.
  const nodes = spans.map((s) => ({ span: s, children: [] }))
  const byId = {}
  for (const n of nodes) if (!(n.span.spanId in byId)) byId[n.span.spanId] = n
  const roots = []
  for (const n of nodes) {
    const parent = n.span.parentSpanId ? byId[n.span.parentSpanId] : null
    if (parent && parent !== n) parent.children.push(n)
    else roots.push(n) // no parent, parent absent, or self-parent
  }
  // Recover spans trapped in a parent cycle (unreachable from any root) so they
  // surface as roots instead of silently vanishing from the waterfall.
  const reached = new Set()
  const rstack = [...roots]
  while (rstack.length) {
    const n = rstack.pop()
    if (reached.has(n.span.uid)) continue
    reached.add(n.span.uid)
    for (const c of n.children) rstack.push(c)
  }
  for (const n of nodes) if (!reached.has(n.span.uid)) roots.push(n)
  for (const n of nodes) n.span.hasChildren = n.children.length > 0

  // Sort siblings by start time, iteratively and cycle-safe (no recursion limit).
  const sortSiblings = (arr) =>
    arr.sort((a, b) =>
      a.span.startBig < b.span.startBig ? -1 : a.span.startBig > b.span.startBig ? 1 : a.span.name.localeCompare(b.span.name),
    )
  sortSiblings(roots)
  const sstack = [...roots]
  const sseen = new Set()
  while (sstack.length) {
    const n = sstack.pop()
    if (sseen.has(n.span.uid)) continue
    sseen.add(n.span.uid)
    sortSiblings(n.children)
    for (const c of n.children) sstack.push(c)
  }

  // Per-service color: fixed palette, then procedural hues past its length so
  // wide fan-outs (11+ services) stay visually distinct.
  const services = [...new Set(spans.map((s) => s.service))].sort()
  const serviceColor = {}
  services.forEach((svc, i) => {
    serviceColor[svc] =
      i < SERVICE_COLORS.length
        ? SERVICE_COLORS[i]
        : 'hsl(' + Math.round((i * 360) / services.length) + ' 60% 62%)'
  })

  return {
    spans,
    roots,
    totalNs,
    services,
    serviceColor,
    count: spans.length,
    errorCount: spans.filter((s) => s.statusCode === 2).length,
  }
}
