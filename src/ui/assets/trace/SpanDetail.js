import { attrVal, formatDuration, isException, shortId, spanKindLabel, statusClass, statusLabel } from '../trace-model.js'
import { KvTable } from './KvTable.js'

// Expanded detail panel below a span row: metadata grid, recorded exceptions,
// tags vs. process (resource) attributes, non-exception events, and span
// links. Host-specific pivots (logs window, linked-trace navigation, service
// signal links) are injected as optional builder props and simply don't
// render when the host provides none.
export const SpanDetail = {
  name: 'SpanDetail',
  components: { KvTable },
  props: {
    span: { type: Object, required: true }, // normalized span from the trace model
    // span → href for the correlated-logs pivot, or null to hide it.
    buildLogsHref: { type: Function, default: null },
    // traceId → href for a linked trace, or null to render links as plain text.
    buildTraceHref: { type: Function, default: null },
    // service → [{ label, href, icon }] cross-signal pivots, or null to hide.
    serviceLinks: { type: Function, default: null },
  },
  computed: {
    logsHref() {
      return this.buildLogsHref ? this.buildLogsHref(this.span) : null
    },
    svcLinks() {
      return this.serviceLinks ? this.serviceLinks(this.span.service) || [] : []
    },
    spanTags() {
      return this.span.attributes
        .filter((a) => a.key.indexOf('resource.') !== 0)
        .map((a) => ({ key: a.key.indexOf('span.') === 0 ? a.key.slice(5) : a.key, value: a.value, title: a.title || '' }))
    },
    processTags() {
      return this.span.attributes
        .filter((a) => a.key.indexOf('resource.') === 0)
        .map((a) => ({ key: a.key.slice(9), value: a.value, title: a.title || '' }))
    },
    exceptions() {
      return this.span.events.filter(isException).map((ev) => ({
        type: attrVal(ev, 'exception.type'),
        message: attrVal(ev, 'exception.message'),
        stacktrace: attrVal(ev, 'exception.stacktrace'),
        offsetNs: ev.offsetNs,
      }))
    },
    otherEvents() {
      return this.span.events.filter((ev) => !isException(ev))
    },
  },
  methods: {
    formatDuration,
    kindLabel: spanKindLabel,
    statusLabel,
    statusClass,
    shortId,
  },
  template: `
    <div class="tl-detail">
      <div class="tl-meta">
        <div>
          <span class="k">Service</span>
          <span class="v tl-svc-cell">
            <span class="tl-svc-name" :title="span.service">{{ span.service }}</span>
            <span class="sig-icons" v-if="svcLinks.length">
              <a
                v-for="l in svcLinks"
                :key="l.label"
                class="sig-icon"
                :href="l.href"
                :title="'View ' + l.label + ' for ' + span.service"
                :aria-label="'View ' + l.label + ' for ' + span.service"
              >{{ l.icon }}</a>
            </span>
          </span>
        </div>
        <div><span class="k">Operation</span><span class="v">{{ span.name }}</span></div>
        <div v-if="logsHref"><span class="k">Logs</span><span class="v"><a :href="logsHref" class="pivot-link">View logs (±30s)</a></span></div>
        <div><span class="k">Kind</span><span class="v">{{ kindLabel(span.kind) }}</span></div>
        <div><span class="k">Status</span><span class="v" :class="statusClass(span.statusCode)">{{ statusLabel(span.statusCode) }}<span class="tl-status-msg" v-if="span.statusMessage"> — {{ span.statusMessage }}</span></span></div>
        <div><span class="k">Duration</span><span class="v">{{ formatDuration(span.durationNs) }}</span></div>
        <div><span class="k">Start</span><span class="v">+{{ formatDuration(span.offsetNs) }}</span></div>
        <div><span class="k">Span ID</span><span class="v mono">{{ span.spanId }}</span></div>
        <div v-if="span.parentSpanId"><span class="k">Parent</span><span class="v mono">{{ span.parentSpanId }}</span></div>
      </div>
      <div class="tl-section tl-exc-section" v-if="exceptions.length">
        <h4>Exceptions</h4>
        <div class="tl-exc" v-for="(ex, i) in exceptions" :key="'ex' + i">
          <div class="tl-exc-head">{{ ex.type || 'exception' }}<span class="tl-at"> @ +{{ formatDuration(ex.offsetNs) }}</span></div>
          <div class="tl-exc-msg" v-if="ex.message">{{ ex.message }}</div>
          <pre class="tl-exc-stack" v-if="ex.stacktrace">{{ ex.stacktrace }}</pre>
        </div>
      </div>
      <div class="tl-cols">
        <div class="tl-section" v-if="spanTags.length">
          <h4>Tags</h4>
          <kv-table :rows="spanTags"></kv-table>
        </div>
        <div class="tl-section" v-if="processTags.length">
          <h4>Process</h4>
          <kv-table :rows="processTags"></kv-table>
        </div>
      </div>
      <div class="tl-section" v-if="otherEvents.length">
        <h4>Events</h4>
        <div class="tl-event" v-for="(ev, i) in otherEvents" :key="'e' + i">
          <div class="tl-event-head">{{ ev.name }}<span class="tl-at"> @ +{{ formatDuration(ev.offsetNs) }}</span></div>
          <kv-table v-if="ev.attributes.length" :rows="ev.attributes"></kv-table>
        </div>
      </div>
      <div class="tl-section" v-if="span.links && span.links.length">
        <h4>Links</h4>
        <div class="tl-link" v-for="(lk, i) in span.links" :key="'l' + i">
          <div class="tl-link-head">
            <a v-if="buildTraceHref" class="pivot-link" :href="buildTraceHref(lk.traceId)" :title="'Open linked trace ' + lk.traceId">{{ shortId(lk.traceId) }}</a>
            <span v-else>{{ shortId(lk.traceId) }}</span>
            <span class="tl-link-span mono">/{{ shortId(lk.spanId) }}</span>
          </div>
          <table class="tl-kv" v-if="lk.traceState || lk.flags || lk.attributes.length"><tbody>
            <tr v-if="lk.traceState"><td class="k">traceState</td><td class="v mono">{{ lk.traceState }}</td></tr>
            <tr v-if="lk.flags"><td class="k">flags</td><td class="v mono">{{ lk.flags }}</td></tr>
            <tr v-for="(a, j) in lk.attributes" :key="j"><td class="k">{{ a.key }}</td><td class="v" :title="a.title">{{ a.value }}</td></tr>
          </tbody></table>
        </div>
      </div>
    </div>
  `,
}
