// Timeline ruler above the waterfall: a tick label every 25% of the trace.
export const TraceRuler = {
  name: 'TraceRuler',
  props: {
    ticks: { type: Array, required: true }, // [{ pct, label }]
  },
  template: `
    <div class="tl-ruler">
      <div class="tl-name-col"></div>
      <div class="tl-track">
        <span class="tl-tick" v-for="t in ticks" :key="t.pct" :style="{ left: t.pct + '%' }">{{ t.label }}</span>
      </div>
    </div>
  `,
}
