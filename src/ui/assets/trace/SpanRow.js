import { formatDuration } from '../trace-model.js'

// One waterfall row: service-colored name column (tree indent + collapse
// caret) and a time track with the span bar and duration label. Collapse/open
// state and toggling live in TraceView. The row itself is not interactive;
// detail and subtree toggles are separate named buttons (native keyboard
// activation, no nested interactive controls).
export const SpanRow = {
  name: 'SpanRow',
  props: {
    row: { type: Object, required: true }, // { span, depth, hasChildren, leftPct, widthPct, color }
    collapsed: { type: Boolean, default: false },
    open: { type: Boolean, default: false },
  },
  emits: ['toggle-collapse', 'toggle-detail'],
  methods: {
    formatDuration,
    durStyle() {
      const end = this.row.leftPct + this.row.widthPct
      // Room to the right of the bar → label sits just after the bar's end.
      if (end <= 85) return { left: end + '%', paddingLeft: '5px' }
      // Wide bar (no room to the right) → tuck the label inside the bar's right
      // end with a translucent backing so it stays legible on any service color
      // and never collides with the span name/service in the left column.
      return {
        right: 100 - end + '%',
        textAlign: 'right',
        padding: '0 4px',
        color: 'var(--fg)',
        background: 'rgba(15, 20, 25, 0.55)',
        borderRadius: '3px',
      }
    },
  },
  template: `
    <div
      class="tl-row"
      :class="{ open: open, error: row.span.statusCode === 2 }"
    >
      <div class="tl-name-col" :style="{ paddingLeft: (row.depth * 14 + 4) + 'px' }">
        <button
          v-if="row.hasChildren"
          type="button"
          class="tl-toggle"
          :aria-expanded="!collapsed"
          :aria-label="(collapsed ? 'Expand subtree of ' : 'Collapse subtree of ') + row.span.name"
          @click="$emit('toggle-collapse', row.span.uid)"
        >{{ collapsed ? '▸' : '▾' }}</button>
        <span class="tl-toggle ghost" v-else></span>
        <span class="tl-svc-bar" :style="{ background: row.color }"></span>
        <button
          type="button"
          class="tl-row-main"
          :aria-expanded="open"
          :aria-label="'Span details for ' + row.span.service + ' ' + row.span.name"
          @click="$emit('toggle-detail', row.span.uid)"
        >
          <span class="tl-span-name" :title="row.span.name">{{ row.span.name }}</span>
          <span class="tl-svc-tag">{{ row.span.service }}</span>
          <span class="tl-err-dot" v-if="row.span.statusCode === 2" title="error">●</span>
        </button>
      </div>
      <div class="tl-track">
        <div class="tl-bar" :style="{ left: row.leftPct + '%', width: row.widthPct + '%', background: row.color }"></div>
        <span class="tl-dur" :style="durStyle()">{{ formatDuration(row.span.durationNs) }}</span>
      </div>
    </div>
  `,
}
