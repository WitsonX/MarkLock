<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import * as tauri from '../lib/tauri'
import { toggleTaskMarker } from '../lib/preview'
import { isMac } from '../lib/platform'
import { bindHoverTip } from '../lib/tooltip'
import { t } from '../locales'

/**
 * 共享 Markdown 预览面板：外观走 global.css 的 `.md` 规则，保证便签与主编辑器观感一致。
 * 渲染结果由父级注入（`html`）而不是这里现算——就地编辑要求「编辑期间不再重设 innerHTML」，
 * 只有持有冻结标志的那一侧才决定得了什么时候换 DOM。
 * 点击交互在此统一收口：代码复制 / 任务清单回改（上抛新源码）/ 外链打开；
 * 就地编辑态（`editable`）下勾选交给浏览器翻转（序列化器读 `input.checked`），普通链接点击
 * 只落光标（跳转会连光标一起弄丢），⌘/Ctrl+点击才打开链接——与主编辑器 `onPreviewLinkClick` 同一套分叉。
 */
export default defineComponent({
  name: 'MarkdownPreview',
  props: {
    /** 父级渲染好的 HTML（就地编辑期间父级会把它冻住，按键只改 DOM） */
    html: { type: String, default: '' },
    /** 对应的 Markdown 源码，仅用于按 DOM 序号回改任务清单标记 */
    source: { type: String, default: '' },
    /** 预览即编辑面 */
    editable: { type: Boolean, default: false },
  },
  emits: ['task-toggle', 'edit-input', 'edit-keydown', 'edit-paste', 'edit-compositionstart', 'edit-compositionend'],
  data() {
    return {
      // ⌘/Ctrl 按住中：编辑态链接此刻可点击打开，pv-link-hot 类给出光标/下划线提示
      linkHot: false,
      _onModKey: null as ((e: KeyboardEvent) => void) | null,
      _onWinBlur: null as (() => void) | null,
      _linkTipUnbind: null as (() => void) | null,
    }
  },
  mounted() {
    this._onModKey = (e: KeyboardEvent) => {
      if (e.key === 'Meta' || e.key === 'Control') this.linkHot = e.type === 'keydown'
    }
    window.addEventListener('keydown', this._onModKey)
    window.addEventListener('keyup', this._onModKey)
    // ⌘Tab 切走等场景收不到 keyup：失焦时兜底清掉，避免提示态卡住
    this._onWinBlur = () => { this.linkHot = false }
    window.addEventListener('blur', this._onWinBlur)
    // 编辑态链接的悬停提示（与主编辑器同语义；v-html 内容绑不了 v-tip，走委托气泡）
    this._linkTipUnbind = bindHoverTip(this.$el as HTMLElement, {
      pick: (tg) => {
        if (!this.editable) return null
        const a = tg.closest('a') as HTMLAnchorElement | null
        // # 锚点普通点击就能页内滚动，不需要「按住修饰键」提示
        if (!a || (a.getAttribute('href') || '').startsWith('#')) return null
        return a
      },
      text: () =>
        this.linkHot
          ? t('editor.pvLinkOpenTip')
          : t('editor.pvLinkTip', { mod: isMac ? '⌘' : 'Ctrl' }),
      enabled: () => this.editable,
    })
  },
  unmounted() {
    if (this._onModKey) {
      window.removeEventListener('keydown', this._onModKey)
      window.removeEventListener('keyup', this._onModKey)
    }
    if (this._onWinBlur) window.removeEventListener('blur', this._onWinBlur)
    if (this._linkTipUnbind) this._linkTipUnbind()
  },
  methods: {
    onClick(e: MouseEvent) {
      const target = e.target as HTMLElement
      // 代码复制按钮（代码块 / 行内代码共用 .code-copy-btn）：编辑态同样可用，故排在 editable 分叉之前
      const btn = target.closest('.code-copy-btn') as HTMLElement | null
      if (btn) {
        e.preventDefault()
        this.copyCode(btn)
        return
      }
      if (this.editable) {
        if (target.closest('input.task-cb')) return
        const box = target.closest('a') as HTMLAnchorElement | null
        // 普通点击只落光标；⌘/Ctrl+点击打开链接，与只读态同一套分派
        if (box && (e.metaKey || e.ctrlKey)) this.openLink(e, box)
        else if (box && !(box.getAttribute('href') || '').startsWith('#')) e.preventDefault()
        return
      }
      // 任务清单复选框：按 DOM 顺序定位源码第几个任务标记，上抛翻转后的新源码 + 供确认弹窗展示的任务原文与勾选意图
      const cb = target.closest('input.task-cb') as HTMLInputElement | null
      if (cb) {
        e.preventDefault()
        const md = cb.closest('.md')
        if (!md) return
        const idx = Array.prototype.indexOf.call(md.querySelectorAll('input.task-cb'), cb)
        if (idx < 0) return
        const next = toggleTaskMarker(this.source, idx)
        if (next !== null) {
          // 浏览器在 click 派发前的 pre-activation 阶段已把 checked 翻成新态，故 cb.checked 即用户意图（新状态）
          const text = (cb.closest('li')?.textContent ?? '').replace(/\s+/g, ' ').trim()
          this.$emit('task-toggle', { next, text, checked: cb.checked })
        }
        return
      }
      // 链接：仅放行 http/https/mailto 交系统浏览器，其余（相对路径等）在便签里不跳转
      const a = target.closest('a') as HTMLAnchorElement | null
      if (!a) return
      this.openLink(e, a)
    },
    /** 链接分派（只读点击 与 编辑态 ⌘/Ctrl+点击 共用）：便签里仅外链进系统浏览器。 */
    openLink(e: MouseEvent, a: HTMLAnchorElement) {
      const raw = a.getAttribute('href') || ''
      const lower = raw.toLowerCase()
      if (lower.startsWith('http://') || lower.startsWith('https://') || lower.startsWith('mailto:')) {
        e.preventDefault()
        tauri.openUrl(raw).catch((err) => message.error(String(err)))
      } else {
        e.preventDefault()
      }
    },
    async copyCode(btn: HTMLElement) {
      const wrap = btn.closest('.codespan-wrap')
      const code = wrap
        ? wrap.querySelector('code')
        : (btn.parentElement?.querySelector('code') as HTMLElement | null)
      const text = (code?.textContent ?? '').replace(/\n$/, '')
      try {
        await navigator.clipboard.writeText(text)
      } catch {
        return
      }
      btn.classList.add('copied')
      const txt = btn.querySelector('.cc-txt') as HTMLElement | null
      if (txt) txt.textContent = t('common.copied')
      window.setTimeout(() => {
        btn.classList.remove('copied')
        if (txt) txt.textContent = t('common.copy')
      }, 1400)
    },
  },
})
</script>

<template>
  <div
    class="md md-preview"
    :class="{ 'wysiwyg-on': editable, 'pv-link-hot': linkHot }"
    spellcheck="false"
    :contenteditable="editable ? 'true' : 'false'"
    v-html="html"
    @click="onClick"
    @input="$emit('edit-input')"
    @keydown="$emit('edit-keydown', $event)"
    @paste="$emit('edit-paste', $event)"
    @compositionstart="$emit('edit-compositionstart')"
    @compositionend="$emit('edit-compositionend')"
  ></div>
</template>
