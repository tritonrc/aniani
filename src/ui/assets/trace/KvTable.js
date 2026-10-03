// Shared key/value table used by the span-detail sections (tags, process,
// event and link attributes). Rows are {key, value, title} rows as produced
// by the trace model's attribute mapping.
export const KvTable = {
  name: 'KvTable',
  props: {
    rows: { type: Array, required: true },
  },
  template: `
    <table class="tl-kv"><tbody>
      <tr v-for="(a, i) in rows" :key="i"><td class="k">{{ a.key }}</td><td class="v" :title="a.title">{{ a.value }}</td></tr>
    </tbody></table>
  `,
}
