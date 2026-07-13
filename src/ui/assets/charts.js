import { SERVICE_COLORS } from './core.js'

// Build a `name{k="v", ...}` label for a PromQL result's `metric` object.
// Shared by the Metrics results table and LineChart's legend/tooltip.
export function seriesLabelFor(metric) {
  const name = (metric && metric.__name__) || ''
  const labels = Object.entries(metric || {})
    .filter(([k]) => k !== '__name__')
    .map(([k, v]) => k + '="' + v + '"')
    .join(', ')
  return labels ? name + '{' + labels + '}' : name
}

// HH:MM in local time, from a unix-seconds timestamp (as Number).
function formatHM(tsSec) {
  const d = new Date(tsSec * 1000)
  const pad = (n) => String(n).padStart(2, '0')
  return pad(d.getHours()) + ':' + pad(d.getMinutes())
}

// Compact number formatting for axis/tooltip labels: 1234 -> '1.2k', 0.05 -> '0.05'.
function formatCompact(v) {
  if (!isFinite(v)) return ''
  const trim = (s) => s.replace(/\.?0+$/, '')
  const abs = Math.abs(v)
  if (abs >= 1e9) return trim((v / 1e9).toFixed(1)) + 'b'
  if (abs >= 1e6) return trim((v / 1e6).toFixed(1)) + 'm'
  if (abs >= 1e3) return trim((v / 1e3).toFixed(1)) + 'k'
  if (abs >= 1 || abs === 0) return trim(v.toFixed(2))
  return trim(v.toFixed(3))
}

// Classic "nice numbers" rounding (Heckbert): pick a step from {1, 2, 5} x 10^n
// so ticks land on round values. `round` picks the nearest nice fraction
// (used for the step itself); otherwise the smallest nice fraction >= input
// (used for the raw range, so the step derived from it isn't too coarse).
function niceNum(range, round) {
  const exponent = Math.floor(Math.log10(range))
  const fraction = range / Math.pow(10, exponent)
  let niceFraction
  if (round) {
    if (fraction < 1.5) niceFraction = 1
    else if (fraction < 3) niceFraction = 2
    else if (fraction < 7) niceFraction = 5
    else niceFraction = 10
  } else {
    if (fraction <= 1) niceFraction = 1
    else if (fraction <= 2) niceFraction = 2
    else if (fraction <= 5) niceFraction = 5
    else niceFraction = 10
  }
  return niceFraction * Math.pow(10, exponent)
}

// niceTicks(min, max, count≈5) -> { ticks: [Number], niceMin, niceMax }. Pure.
// min === max is padded (±10%, or ±1 around zero) before computing ticks so a
// flat series still gets a sensible y-axis instead of a zero-height range.
function niceTicks(min, max, count = 5) {
  if (min === max) {
    const pad = min !== 0 ? Math.abs(min) * 0.1 : 1
    min -= pad
    max += pad
  }
  const range = niceNum(max - min, false)
  const step = niceNum(range / (count - 1), true)
  const niceMin = Math.floor(min / step) * step
  const niceMax = Math.ceil(max / step) * step
  const ticks = []
  for (let v = niceMin; v <= niceMax + step / 2; v += step) {
    ticks.push(Math.round(v / step) * step)
  }
  return { ticks, niceMin, niceMax }
}

// Build an SVG path 'd' from a series' points, in internal chart coordinates.
// Non-finite samples break the line into separate `M`-started segments rather
// than being plotted; a segment with fewer than 2 finite points draws nothing
// (single isolated points are rendered as circles by the caller instead).
function buildPathD(points, xScale, yScale) {
  const segments = []
  let current = []
  for (const p of points) {
    if (isFinite(p.v)) current.push(p)
    else {
      if (current.length) segments.push(current)
      current = []
    }
  }
  if (current.length) segments.push(current)
  return segments
    .filter((seg) => seg.length >= 2)
    .map((seg) =>
      seg.map((p, i) => (i === 0 ? 'M ' : 'L ') + xScale(p.t).toFixed(2) + ' ' + yScale(p.v).toFixed(2)).join(' '),
    )
    .join(' ')
}

// Dependency-free multi-series SVG line chart for PromQL matrix results.
// Fixed internal coordinate space (800x240); CSS scales it to the container
// width via preserveAspectRatio="none". Legend and tooltip are plain HTML
// (absolutely positioned) since SVG text wrapping is a pain; axis ticks are
// short enough to render as SVG <text>.
export const LineChart = {
  props: { result: { type: Array, default: () => [] } },
  data() {
    return { hidden: {}, hover: null, margin: { l: 46, r: 8, t: 8, b: 22 } }
  },
  computed: {
    plot() {
      const m = this.margin
      return { l: m.l, t: m.t, w: 800 - m.l - m.r, h: 240 - m.t - m.b }
    },
    series() {
      return (this.result || []).map((s, i) => ({
        name: seriesLabelFor(s.metric || {}),
        color: SERVICE_COLORS[i % SERVICE_COLORS.length],
        points: (s.values || [])
          .map((v) => ({ t: Number(v[0]), v: Number(v[1]) }))
          .filter((p) => isFinite(p.t))
          .sort((a, b) => a.t - b.t),
      }))
    },
    visibleSeries() {
      return this.series.filter((s) => !this.hidden[s.name])
    },
    allTimestamps() {
      const set = new Set()
      for (const s of this.visibleSeries) for (const p of s.points) if (isFinite(p.v)) set.add(p.t)
      return [...set].sort((a, b) => a - b)
    },
    xDomain() {
      const t = this.allTimestamps
      return t.length ? { min: t[0], max: t[t.length - 1] } : null
    },
    yDomainRaw() {
      let min = Infinity
      let max = -Infinity
      for (const s of this.visibleSeries) {
        for (const p of s.points) {
          if (isFinite(p.v)) {
            if (p.v < min) min = p.v
            if (p.v > max) max = p.v
          }
        }
      }
      return isFinite(min) ? { min, max } : null
    },
    yNice() {
      const raw = this.yDomainRaw
      return raw ? niceTicks(raw.min, raw.max) : niceTicks(0, 1)
    },
    hasData() {
      return this.xDomain !== null && this.yDomainRaw !== null
    },
    chartSeries() {
      return this.visibleSeries.map((s) => {
        const finite = s.points.filter((p) => isFinite(p.v))
        let d = ''
        let solo = null
        if (finite.length === 1) {
          solo = { x: this.xScale(finite[0].t), y: this.yScale(finite[0].v) }
        } else if (finite.length >= 2) {
          d = buildPathD(s.points, this.xScale, this.yScale)
        }
        return { name: s.name, color: s.color, d, solo }
      })
    },
    xTicks() {
      const dom = this.xDomain
      if (!dom) return []
      if (dom.max <= dom.min) return [{ x: this.xScale(dom.min), label: formatHM(dom.min), anchor: 'middle' }]
      const n = 5
      const out = []
      for (let i = 0; i < n; i++) {
        const t = dom.min + ((dom.max - dom.min) * i) / (n - 1)
        // First/last tick sit right at the plot edges — anchoring them by their
        // near edge (instead of centering) keeps the label inside the svg
        // instead of overflowing past x=0 or x=800.
        const anchor = i === 0 ? 'start' : i === n - 1 ? 'end' : 'middle'
        out.push({ x: this.xScale(t), label: formatHM(t), anchor })
      }
      return out
    },
    yTicks() {
      return this.yNice.ticks.map((v) => ({ y: this.yScale(v), label: formatCompact(v) }))
    },
    tooltipStyle() {
      if (!this.hover) return {}
      return this.hover.flip ? { right: 100 - this.hover.xPct + '%' } : { left: this.hover.xPct + '%' }
    },
  },
  methods: {
    formatHM,
    xScale(t) {
      const dom = this.xDomain
      if (!dom || dom.max <= dom.min) return this.plot.l + this.plot.w / 2
      return this.plot.l + ((t - dom.min) / (dom.max - dom.min)) * this.plot.w
    },
    yScale(v) {
      const yn = this.yNice
      const range = yn.niceMax - yn.niceMin
      if (!range) return this.plot.t + this.plot.h / 2
      return this.plot.t + this.plot.h - ((v - yn.niceMin) / range) * this.plot.h
    },
    toggleHidden(name) {
      this.hidden[name] = !this.hidden[name]
    },
    onMouseMove(evt) {
      const dom = this.xDomain
      const times = this.allTimestamps
      if (!dom || !times.length) {
        this.hover = null
        return
      }
      const rect = evt.currentTarget.getBoundingClientRect()
      if (!rect.width) return
      const xInternal = ((evt.clientX - rect.left) / rect.width) * 800
      const frac = this.plot.w ? (xInternal - this.plot.l) / this.plot.w : 0.5
      const targetTs = dom.min + Math.min(1, Math.max(0, frac)) * (dom.max - dom.min)
      let nearest = times[0]
      let bestDiff = Infinity
      for (const t of times) {
        const diff = Math.abs(t - targetTs)
        if (diff < bestDiff) {
          bestDiff = diff
          nearest = t
        }
      }
      const rows = []
      for (const s of this.visibleSeries) {
        const pt = s.points.find((p) => p.t === nearest)
        if (pt && isFinite(pt.v)) rows.push({ name: s.name, color: s.color, value: formatCompact(pt.v) })
      }
      const x = this.xScale(nearest)
      this.hover = { x, xPct: (x / 800) * 100, ts: nearest, rows, flip: x > 800 * 0.6 }
    },
    onMouseLeave() {
      this.hover = null
    },
  },
  template: `
    <div class="line-chart">
      <svg viewBox="0 0 800 240" preserveAspectRatio="none" class="lc-svg" @mousemove="onMouseMove" @mouseleave="onMouseLeave">
        <template v-if="hasData">
          <line v-for="yt in yTicks" :key="'gy' + yt.y" class="lc-grid" :x1="plot.l" :x2="800 - margin.r" :y1="yt.y" :y2="yt.y"></line>
          <text v-for="yt in yTicks" :key="'yl' + yt.y" class="lc-axis-label" :x="plot.l - 6" :y="yt.y + 3" text-anchor="end">{{ yt.label }}</text>
          <text v-for="xt in xTicks" :key="'xl' + xt.x" class="lc-axis-label" :x="xt.x" y="234" :text-anchor="xt.anchor">{{ xt.label }}</text>
          <g v-for="s in chartSeries" :key="s.name">
            <path v-if="s.d" :d="s.d" fill="none" :stroke="s.color" stroke-width="1.5"></path>
            <circle v-if="s.solo" :cx="s.solo.x" :cy="s.solo.y" r="3" :fill="s.color"></circle>
          </g>
          <line v-if="hover" class="lc-hover-line" :x1="hover.x" :x2="hover.x" :y1="margin.t" :y2="240 - margin.b"></line>
        </template>
        <text v-else class="lc-empty" x="400" y="120" text-anchor="middle">no data</text>
      </svg>
      <div class="lc-legend" v-if="series.length">
        <span
          class="lc-legend-item"
          :class="{ hidden: hidden[s.name] }"
          v-for="s in series"
          :key="s.name"
          @click="toggleHidden(s.name)"
        >
          <span class="lc-swatch" :style="{ background: s.color }"></span>{{ s.name || '(all)' }}
        </span>
      </div>
      <div class="lc-tooltip" v-if="hover && hover.rows.length" :style="tooltipStyle">
        <div class="lc-tooltip-time">{{ formatHM(hover.ts) }}</div>
        <div class="lc-tooltip-row" v-for="r in hover.rows" :key="r.name">
          <span class="lc-swatch" :style="{ background: r.color }"></span>{{ r.name || '(all)' }}: {{ r.value }}
        </div>
      </div>
    </div>
  `,
}
