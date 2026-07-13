import { contextItems, suggestContext, ensureContextData, historyFor, KIND_LABELS, EXAMPLE_QUERIES, fuzzyRank, starredFor, toggleStarred } from './autocomplete.js'

// Shared autocomplete input for the Logs/Metrics/Traces query bars: keeps the
// `.query-bar input` structure routeAware and the `/` shortcut depend on,
// layers a context-aware suggestion dropdown and query history on top.
export const QueryBar = {
  props: {
    lang: { type: String, required: true },
    modelValue: { type: String, default: '' },
    placeholder: { type: String, default: '' },
    loading: { type: Boolean, default: false },
    metricNames: { type: Array, default: null },
  },
  emits: ['update:modelValue', 'run'],
  data() {
    return { open: false, selIdx: -1, ctx: null, cursorPos: 0, starTick: 0 }
  },
  computed: {
    isEmpty() {
      return !this.modelValue || !this.modelValue.trim()
    },
    displayItems() {
      this.starTick // reactive dependency: re-run after a star toggle
      if (this.isEmpty) {
        const starredList = starredFor(this.lang)
        const starred = starredList.map((q) => ({ text: q, insert: q, replaceAll: true, group: 'starred', star: true, starred: true }))
        const recent = historyFor(this.lang)
          .filter((q) => !starredList.includes(q))
          .slice(0, 6)
          .map((q) => ({ text: q, insert: q, replaceAll: true, group: 'recent', star: true, starred: false }))
        const seen = new Set([...starredList, ...recent.map((r) => r.text)])
        const examples = (EXAMPLE_QUERIES[this.lang] || [])
          .filter((q) => !seen.has(q))
          .map((q) => ({ text: q, insert: q, replaceAll: true, group: 'examples' }))
        return [...starred, ...recent, ...examples]
      }
      if (!this.ctx) return []
      const partial = this.modelValue.slice(this.ctx.partialStart, this.cursorPos)
      const group = KIND_LABELS[this.ctx.kind] || ''
      const items = contextItems(this.lang, this.ctx, this.metricNames)
      return fuzzyRank(items, partial).slice(0, 12).map((it) => ({ ...it, group }))
    },
    // Doc string of the highlighted item, shown as a footer line in the drop.
    selDoc() {
      const it = this.displayItems[this.selIdx]
      return (it && it.doc) || ''
    },
  },
  methods: {
    // `text` is the live input value. onInput must pass e.target.value: the
    // modelValue prop lags the emit by a render, so computing the context
    // from it would classify against the previous keystroke's text.
    refreshCtx(text) {
      const el = this.$refs.input
      this.cursorPos = el ? el.selectionStart : text.length
      this.ctx = suggestContext(this.lang, text, this.cursorPos)
      if (this.ctx) ensureContextData(this.lang, this.ctx)
    },
    openDrop(text) {
      this.refreshCtx(typeof text === 'string' ? text : this.modelValue)
      this.open = true
    },
    onInput(e) {
      this.$emit('update:modelValue', e.target.value)
      this.selIdx = -1
      this.openDrop(e.target.value)
    },
    onBlur() {
      // Delayed so a mousedown-triggered accept() below fires before the
      // dropdown unmounts (blur otherwise beats click).
      setTimeout(() => { this.open = false }, 120)
    },
    onKeydown(e) {
      if (e.ctrlKey && e.key === ' ') {
        e.preventDefault()
        this.openDrop()
        return
      }
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault()
        if (!this.open) this.openDrop()
        const n = this.displayItems.length
        if (!n) return
        if (e.key === 'ArrowDown') this.selIdx = (this.selIdx + 1) % n
        else this.selIdx = this.selIdx <= 0 ? n - 1 : this.selIdx - 1
      } else if (e.key === 'Enter') {
        if (this.open && this.selIdx >= 0) {
          e.preventDefault()
          this.accept(this.displayItems[this.selIdx])
        }
        // else: dropdown closed or nothing selected — let the form submit.
      } else if (e.key === 'Tab') {
        if (this.open && this.selIdx >= 0) {
          e.preventDefault()
          this.accept(this.displayItems[this.selIdx])
        }
      } else if (e.key === 'Escape') {
        this.open = false
      }
    },
    accept(item) {
      if (!item) return
      let next
      let cursor
      if (item.replaceAll) {
        next = item.insert
        cursor = next.length
      } else {
        const start = this.ctx ? this.ctx.partialStart : this.modelValue.length
        const end = this.cursorPos
        next = this.modelValue.slice(0, start) + item.insert + this.modelValue.slice(end)
        cursor = start + (typeof item.caret === 'number' ? item.caret : item.insert.length)
      }
      this.$emit('update:modelValue', next)
      this.selIdx = -1
      this.$nextTick(() => {
        const el = this.$refs.input
        if (el) {
          el.focus()
          el.setSelectionRange(cursor, cursor)
        }
        // Re-evaluate at the new cursor so completions chain: accepting a
        // label key (which inserts `key="`) immediately offers its values.
        // Recalled history entries are complete queries — close instead.
        if (item.replaceAll) this.open = false
        else this.openDrop()
      })
    },
    // Pin/unpin a recalled query. Called from the star affordance's mousedown
    // (which stops propagation so the row's accept() does not also fire).
    toggleStar(it) {
      toggleStarred(this.lang, it.text)
      this.starTick++
    },
  },
  template: `
    <form class="query-bar" @submit.prevent="$emit('run')">
      <div class="qb-wrap">
        <input
          ref="input"
          :id="lang + '-query'"
          :name="lang + '-query'"
          :value="modelValue"
          @input="onInput"
          @focus="openDrop"
          @click="openDrop"
          @keydown="onKeydown"
          @blur="onBlur"
          :placeholder="placeholder"
          spellcheck="false"
          autocapitalize="off"
          autocomplete="off"
        />
        <div class="qb-drop" v-if="open && displayItems.length">
          <template v-for="(it, i) in displayItems" :key="i + it.text">
            <div class="qb-group-label" v-if="it.group !== (displayItems[i - 1] && displayItems[i - 1].group)">{{ it.group }}</div>
            <div
              class="qb-item"
              :class="{ sel: i === selIdx }"
              @mousedown.prevent="accept(it)"
              @mouseenter="selIdx = i"
            >
              <span class="qb-item-text">{{ it.text }}</span>
              <span class="qb-item-detail" v-if="it.detail">{{ it.detail }}</span>
              <span
                v-if="it.star"
                class="qb-star"
                :class="{ on: it.starred }"
                :title="it.starred ? 'Unpin' : 'Pin'"
                @mousedown.prevent.stop="toggleStar(it)"
              >{{ it.starred ? '★' : '☆' }}</span>
            </div>
          </template>
          <div class="qb-doc" v-if="selDoc">{{ selDoc }}</div>
        </div>
      </div>
      <button type="submit" :disabled="loading">Run</button>
    </form>
  `,
}
