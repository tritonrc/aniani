// Bird's-eye Gantt of every span (ignoring collapse state), shown for large
// traces where the full waterfall needs scrolling. Thin colored bars pack
// the whole timeline so clusters and gaps are visible at a glance.
export const TraceMinimap = {
  name: 'TraceMinimap',
  props: {
    bars: { type: Array, required: true }, // [{ uid, leftPct, widthPct, color, error }]
    count: { type: Number, default: 0 },
  },
  template: `
    <div class="tv-minimap" :title="count + ' spans — overview'">
      <span
        v-for="b in bars"
        :key="'m' + b.uid"
        class="tv-minimap-bar"
        :class="{ err: b.error }"
        :style="{ left: b.leftPct + '%', width: b.widthPct + '%', background: b.color }"
      ></span>
    </div>
  `,
}
