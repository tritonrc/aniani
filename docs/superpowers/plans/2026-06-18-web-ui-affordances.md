# Web UI Quality-of-Life Affordances — Implementation Plan

> **For agentic workers:** task-by-task; each task is implemented by a subagent and spec+quality reviewed. Steps use checkbox syntax.

**Goal:** Make the Aniani web UI self-populating and assistive: autocomplete (#1), click-to-run chips (#2), catalog-aware metric picking (#5), and an optional, feature-detected on-device-LLM "Ask" box (#6) that turns natural language into LogQL/PromQL/TraceQL.

**Architecture:** Purely additive to `src/ui/assets/app.js` and `src/ui/assets/style.css`. No server changes — everything is driven by existing endpoints: `/api/v1/services`, `/api/v1/label/__name__/values`, `/api/v1/catalog?service=X`. A shared reactive `vocab` cache is loaded once at startup and read by all explorers. #6 uses Chrome's built-in Prompt API (`globalThis.LanguageModel`) and is hidden entirely when unavailable.

**Tech Stack:** Vue 3 (CDN, Options API, string templates), native `<datalist>`, Chrome Prompt API (Gemini Nano) as progressive enhancement.

---

## Shared contracts (all tasks must follow exactly)

**Reactive vocab (added to `app.js`, exposed on `window.__aniani` to match the existing access pattern):**

```js
import { createApp, reactive } from 'vue'
// vocab cache
const vocab = reactive({
  services: [],     // [{ name, signals: [] }]
  metricNames: [],  // ["http_request_duration_ms", ...]
  catalog: {},      // { [service]: { metrics: [], log_labels: [] } }
})
async function loadVocab() {
  try { const s = await apiGet('/api/v1/services'); vocab.services = (s.data && s.data.services) || [] } catch (_) {}
  try { const m = await apiGet('/api/v1/label/__name__/values'); vocab.metricNames = m.data || [] } catch (_) {}
}
async function loadCatalog(service) {
  if (!service) return null
  if (vocab.catalog[service]) return vocab.catalog[service]
  try {
    const c = await apiGet('/api/v1/catalog?service=' + encodeURIComponent(service))
    vocab.catalog[service] = (c && c.data) || {}
    return vocab.catalog[service]
  } catch (_) { return null }
}
```
(The catalog URL is `'/api/v1/catalog?service=' + encodeURIComponent(service)`.) Expose `vocab`, `loadVocab`, `loadCatalog` on `window.__aniani` and call `loadVocab()` once just before `createApp(App).mount('#app')`.

**CSS class-name contract (Task C styles these; Tasks A/B emit them):**
- `.picker` — wrapper for a chip row (flex, wraps).
- `.chip` — a clickable suggestion button.
- `.picker-label` — small muted leading label inside a picker ("Services:", "Metrics:").
- `.svc-select` — the per-service `<select>` on the Metrics tab.
- `.ai-ask` — wrapper for the #6 Ask box.
- `.ai-ask input` / `.ai-ask button` — its controls.
- `.ai-status` — small muted status/error text under the Ask box.

---

### Task A: Shared vocab + chips (auto-run) + datalist + catalog-aware metrics

**Files:** Modify `src/ui/assets/app.js`.

- [ ] **Step 1: Add the vocab cache + loaders** at module scope (after `hourAgoMs`), per the Shared contracts above, and add them to the `window.__aniani` object, and call `loadVocab()` immediately before `createApp(App).mount('#app')`. Also change the import to `import { createApp, reactive } from 'vue'`.

- [ ] **Step 2: Logs explorer — add a service/level chip row + datalist.** Inside the `Logs` component template, immediately after the `</form>`, insert:

```html
<div class="picker" v-if="chips.length">
  <span class="picker-label">Quick:</span>
  <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">{{ c }}</button>
</div>
<datalist id="logs-suggestions">
  <option v-for="c in chips" :key="c" :value="c"></option>
</datalist>
```
Add `list="logs-suggestions"` to the `<input>`. Add a computed and a method:
```js
computed: {
  chips() {
    const svcs = (window.__aniani.vocab.services || [])
      .filter((s) => (s.signals || []).includes('logs'))
      .map((s) => '{service="' + s.name + '"}')
    return [...svcs, '{level="error"}']
  },
},
```
and in `methods`, add:
```js
pick(c) { this.query = c; this.run() },
```

- [ ] **Step 3: Traces explorer — service chips + datalist.** After its `</form>` insert the same `.picker`/`<datalist id="traces-suggestions">` block (use id `traces-suggestions`, iterate `chips`), add `list="traces-suggestions"` to the input, and add:
```js
computed: {
  chips() {
    return (window.__aniani.vocab.services || [])
      .filter((s) => (s.signals || []).includes('traces'))
      .map((s) => '{ .service.name = "' + s.name + '" }')
  },
},
pick(c) { this.query = c; this.run() },
```
(Traces already has methods `pretty`, `run`, `open` — add `pick` alongside them; add a `computed` block.)

- [ ] **Step 4: Metrics explorer — service `<select>` (#5) + metric chips + datalist.** After its `</form>` insert:
```html
<div class="picker">
  <span class="picker-label">Service:</span>
  <select class="svc-select" v-model="service" @change="onService">
    <option value="">All services</option>
    <option v-for="s in metricServices" :key="s" :value="s">{{ s }}</option>
  </select>
</div>
<div class="picker" v-if="chips.length">
  <span class="picker-label">Metrics:</span>
  <button class="chip" v-for="c in chips" :key="c" @click="pick(c)">{{ c }}</button>
</div>
<datalist id="metric-names">
  <option v-for="c in chips" :key="c" :value="c"></option>
</datalist>
```
Add `list="metric-names"` to the input. Add `service: ''` to `data()`. Add computeds + methods:
```js
computed: {
  metricServices() {
    return (window.__aniani.vocab.services || [])
      .filter((s) => (s.signals || []).includes('metrics'))
      .map((s) => s.name)
  },
  chips() {
    const v = window.__aniani.vocab
    if (this.service && v.catalog[this.service]) return v.catalog[this.service].metrics || []
    return v.metricNames || []
  },
},
methods: {
  async onService() { if (this.service) await window.__aniani.loadCatalog(this.service) },
  pick(c) { this.query = c; this.run() },
  // ...existing seriesLabel + run stay...
},
```

- [ ] **Step 5: Verify serving.** Run `cargo test --features ui --lib ui::tests::test_app_js_served_as_javascript` — expect PASS.

- [ ] **Step 6: Commit.** `git add src/ui/assets/app.js && git commit -m "ui: self-populating chips, datalist autocomplete, catalog-aware metrics"`

---

### Task B: Optional on-device-LLM "Ask" box (#6)  — depends on Task A

**Files:** Modify `src/ui/assets/app.js`.

- [ ] **Step 1: Add a reusable `AiAsk` component** (define it before `const App`). It feature-detects `globalThis.LanguageModel`, renders nothing when unsupported, and emits a generated query string:

```js
const AiAsk = {
  props: { lang: { type: String, required: true } },
  emits: ['query'],
  template: `
    <div class="ai-ask" v-if="supported">
      <input
        v-model="text"
        :placeholder="placeholder"
        spellcheck="false"
        @keydown.enter.prevent="ask"
      />
      <button @click="ask" :disabled="busy || state === 'downloading'">
        {{ state === 'downloading' ? 'Downloading model…' : 'Ask AI' }}
      </button>
      <span class="ai-status" v-if="status">{{ status }}</span>
    </div>
  `,
  data() {
    return { supported: false, state: 'unknown', text: '', busy: false, status: '' }
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
    if (!('LanguageModel' in globalThis)) return
    try {
      this.state = await globalThis.LanguageModel.availability()
      this.supported = this.state !== 'unavailable'
    } catch (_) {
      this.supported = false
    }
  },
  methods: {
    systemPrompt() {
      const v = window.__aniani.vocab
      const services = (v.services || []).map((s) => s.name).join(', ') || '(none)'
      const metrics = (v.metricNames || []).join(', ') || '(none)'
      const syntax = {
        logql: 'LogQL stream selectors like {service="x", level="error"} optionally followed by a |= "substring" filter',
        promql: 'PromQL using the metric names below, e.g. metric_name or rate(metric_name[5m])',
        traceql: 'TraceQL like { .service.name = "x" }',
      }[this.lang]
      return (
        'You translate a natural-language request into a single ' +
        this.langName +
        ' query for the Aniani observability engine. ' +
        'Output ONLY the query, no explanation, no code fences. ' +
        'Use ' + syntax + '. ' +
        'Known service names: ' + services + '. ' +
        'Known metric names: ' + metrics + '.'
      )
    },
    async ask() {
      if (!this.text.trim()) return
      this.busy = true
      this.status = ''
      try {
        if (this.state === 'downloadable' || this.state === 'downloading') {
          this.state = 'downloading'
          this.status = 'Downloading on-device model (first use only)…'
        }
        const session = await globalThis.LanguageModel.create({
          initialPrompts: [{ role: 'system', content: this.systemPrompt() }],
        })
        let out = (await session.prompt(this.text)).trim()
        out = out.replace(/^```[a-z]*\n?/i, '').replace(/```$/, '').trim()
        session.destroy && session.destroy()
        this.state = 'available'
        this.status = ''
        if (out) this.$emit('query', out)
      } catch (e) {
        this.status = 'AI error: ' + (e && e.message ? e.message : String(e))
      } finally {
        this.busy = false
      }
    },
  },
}
```

- [ ] **Step 2: Register and wire `AiAsk` into the three explorers.** Add `components: { AiAsk }` to each of `Logs`, `Metrics`, `Traces` (merge with any existing keys). In each template, immediately after the `</form>`, before the `.picker` row, add:
```html
<ai-ask :lang="'logql'" @query="onAi"></ai-ask>
```
(use `'promql'` for Metrics, `'traceql'` for Traces). Add the handler to each component's `methods`:
```js
onAi(q) { this.query = q; this.run() },
```

- [ ] **Step 3: Verify serving.** Run `cargo test --features ui --lib ui::tests::test_app_js_served_as_javascript` — expect PASS.

- [ ] **Step 4: Commit.** `git add src/ui/assets/app.js && git commit -m "ui: optional on-device-LLM Ask box (Chrome Prompt API)"`

---

### Task C: Styling — can run in parallel with Tasks A/B

**Files:** Modify `src/ui/assets/style.css` (append; do not alter existing rules).

- [ ] **Step 1: Append styles for the new class-name contract:**

```css
.picker { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin-bottom: 10px; }
.picker-label { color: var(--muted); font-size: 12px; margin-right: 2px; }
.chip {
  background: var(--panel);
  border: 1px solid var(--border);
  color: var(--fg);
  padding: 3px 9px;
  border-radius: 12px;
  cursor: pointer;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
}
.chip:hover { border-color: var(--accent); color: var(--accent); }
.svc-select {
  background: var(--panel);
  border: 1px solid var(--border);
  color: var(--fg);
  padding: 4px 8px;
  border-radius: 4px;
}
.ai-ask { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 10px; }
.ai-ask input {
  flex: 1;
  min-width: 220px;
  background: var(--panel);
  border: 1px dashed var(--border);
  color: var(--fg);
  padding: 7px 10px;
  border-radius: 4px;
}
.ai-ask button {
  background: transparent;
  border: 1px solid var(--accent);
  color: var(--accent);
  padding: 7px 14px;
  border-radius: 4px;
  cursor: pointer;
}
.ai-ask button:disabled { opacity: 0.5; cursor: default; }
.ai-status { color: var(--muted); font-size: 12px; }
```

- [ ] **Step 2: Verify serving.** Run `cargo test --features ui --lib ui::tests::test_style_css_served_as_css` — expect PASS.

- [ ] **Step 3: Commit.** `git add src/ui/assets/style.css && git commit -m "ui: styles for chips, datalist, service select, and AI ask box"`

---

## Notes
- No JS test harness; correctness verified by Rust serving tests + live browser check at the end.
- #6 must degrade silently: when `LanguageModel` is absent or `availability()` is `unavailable`, the `.ai-ask` block renders nothing.
- The Metrics `chips` computed depends on `vocab.catalog[service]`, which is populated asynchronously by `onService`; Vue reactivity re-renders the chips when it arrives (vocab is `reactive`).
