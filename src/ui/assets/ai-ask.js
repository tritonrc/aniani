// English text in/out — silences Chrome's "no output language" warning and
// improves output quality. Shared by availability() and create().
const AI_TEXT_EN = {
  expectedInputs: [{ type: 'text', languages: ['en'] }],
  expectedOutputs: [{ type: 'text', languages: ['en'] }],
}

export const AiAsk = {
  props: { lang: { type: String, required: true } },
  emits: ['query'],
  template: `
    <template v-if="apiPresent">
      <div class="ai-ask" v-if="state !== 'unavailable' && state !== 'unknown'">
        <input
          v-model="text"
          :placeholder="placeholder"
          spellcheck="false"
          @keydown.enter.prevent="ask"
        />
        <button @click="ask" :disabled="busy">
          {{ busy ? (downloading ? 'Downloading model…' : 'Thinking…') : 'Ask AI' }}
        </button>
        <span class="ai-status" v-if="status">{{ status }}</span>
      </div>
      <p class="muted ai-hint" v-else-if="state === 'unavailable'">AI assist requires Chrome's built-in model.</p>
    </template>
  `,
  data() {
    return {
      apiPresent: 'LanguageModel' in globalThis,
      state: 'unknown',
      text: '',
      busy: false,
      downloading: false,
      status: '',
    }
  },
  computed: {
    placeholder() {
      const ex = {
        logql: 'e.g. errors from payments in the last hour',
        promql: 'e.g. request latency for the gateway service',
        traceql: 'e.g. slow traces in payments',
      }
      return 'Ask in plain English — ' + (ex[this.lang] || '')
    },
    langName() {
      return { logql: 'LogQL', promql: 'PromQL', traceql: 'TraceQL' }[this.lang] || this.lang
    },
  },
  async mounted() {
    if (!this.apiPresent) return
    try {
      this.state = await globalThis.LanguageModel.availability(AI_TEXT_EN)
    } catch (_) {
      this.state = 'unavailable'
    }
  },
  methods: {
    systemPrompt() {
      const v = window.__aniani.vocab
      const services = (v.services || []).map((s) => s.name).join(', ') || '(none)'
      const metrics = (v.metricNames || []).join(', ') || '(none)'
      const syntax = {
        logql: 'LogQL stream selectors like {service="x", level="error"} optionally followed by a |= "substring" filter',
        promql: 'PromQL. A bare metric with optional label filter: metric_name{service="x"}. For a rate over time, put the [duration] range AFTER the closing brace, inside a range function: rate(metric_name{service="x"}[5m]). NEVER put a [duration] range inside the {} braces — only label matchers go inside {}',
        traceql: 'TraceQL like { resource.service.name = "x" }',
      }[this.lang]
      const examples = {
        logql: [
          'errors from payments => {service="payments", level="error"}',
          'gateway logs mentioning timeout => {service="gateway"} |= "timeout"',
        ],
        promql: [
          'request duration for inventory => http_request_duration_ms{service="inventory"}',
          'request rate for gateway over 5 minutes => rate(http_request_duration_ms{service="gateway"}[5m])',
          'all values of stock_level => stock_level',
        ],
        traceql: [
          'traces from payments => { resource.service.name = "payments" }',
          'traces from the gateway service => { resource.service.name = "gateway" }',
        ],
      }[this.lang]
      return (
        'You translate a natural-language request into a single ' +
        this.langName +
        ' query for the Aniani observability engine. ' +
        'Output ONLY the query, no explanation, no code fences. ' +
        'Use ' + syntax + '. ' +
        'Known service names: ' + services + '. ' +
        'Known metric names: ' + metrics + '. ' +
        'Examples (request => query): ' + examples.join(' ; ') + '.'
      )
    },
    async ask() {
      if (!this.text.trim()) return
      this.busy = true
      this.downloading = this.state === 'downloadable' || this.state === 'downloading'
      this.status = this.downloading ? 'Downloading on-device model (first use only)…' : ''
      let session
      try {
        session = await globalThis.LanguageModel.create({
          ...AI_TEXT_EN,
          initialPrompts: [{ role: 'system', content: this.systemPrompt() }],
        })
        let out = (await session.prompt(this.text)).trim()
        const fenceMatch = out.match(/```[a-z]*\n?([\s\S]*?)```/i)
        out = (fenceMatch ? fenceMatch[1] : out).trim()
        this.state = 'available'
        this.status = ''
        if (out) this.$emit('query', out)
      } catch (e) {
        this.status = 'AI error: ' + (e && e.message ? e.message : String(e))
      } finally {
        if (session && session.destroy) session.destroy()
        this.busy = false
        this.downloading = false
      }
    },
  },
}
