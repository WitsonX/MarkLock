//! 代码块语言：常用语言清单、`<code class="language-x">` 的读写，以及代码块左上角的语言菜单。
//!
//! 预览区是 `v-html` 拼出来的 DOM（就地编辑时它还是真源），挂不了 Vue 组件，所以菜单用原生
//! DOM 画在 body 上；各视图只负责把点击委托进来（主编辑器 `onPreviewLinkClick`、便签
//! `MarkdownPreview.onClick`）。选中语言后直接改 `<code>` 的 class 并按新语言重排高亮，
//! 回写源码交给调用方的防抖流程——序列化器本来就按 `language-x` 生成围栏信息串。

import hljs from 'highlight.js'
import { t } from '../locales'
import { highlightCode } from './preview'
import { codeText } from './wysiwyg'

/** 语言项：`id` 同时是写进围栏的信息串（取 highlight.js 认得的别名），空串表示不写语言、交给自动识别。 */
type Lang = { id: string; label: string }

/** 常用语言清单。只收 highlight.js 认识的名字：选了不上色比没有这个选项更让人困惑。 */
const LANGS: Lang[] = [
  { id: '', label: '' },
  { id: 'text', label: '' },
  { id: 'js', label: 'JavaScript' },
  { id: 'ts', label: 'TypeScript' },
  { id: 'jsx', label: 'JSX' },
  { id: 'tsx', label: 'TSX' },
  { id: 'html', label: 'HTML' },
  { id: 'css', label: 'CSS' },
  { id: 'scss', label: 'SCSS' },
  { id: 'less', label: 'Less' },
  { id: 'json', label: 'JSON' },
  { id: 'yaml', label: 'YAML' },
  { id: 'xml', label: 'XML' },
  { id: 'md', label: 'Markdown' },
  { id: 'bash', label: 'Bash' },
  { id: 'shell', label: 'Shell' },
  { id: 'powershell', label: 'PowerShell' },
  { id: 'python', label: 'Python' },
  { id: 'java', label: 'Java' },
  { id: 'kotlin', label: 'Kotlin' },
  { id: 'scala', label: 'Scala' },
  { id: 'c', label: 'C' },
  { id: 'cpp', label: 'C++' },
  { id: 'csharp', label: 'C#' },
  { id: 'go', label: 'Go' },
  { id: 'rust', label: 'Rust' },
  { id: 'php', label: 'PHP' },
  { id: 'ruby', label: 'Ruby' },
  { id: 'swift', label: 'Swift' },
  { id: 'objectivec', label: 'Objective-C' },
  { id: 'dart', label: 'Dart' },
  { id: 'lua', label: 'Lua' },
  { id: 'r', label: 'R' },
  { id: 'sql', label: 'SQL' },
  { id: 'pgsql', label: 'PostgreSQL' },
  { id: 'graphql', label: 'GraphQL' },
  { id: 'dockerfile', label: 'Dockerfile' },
  { id: 'makefile', label: 'Makefile' },
  { id: 'ini', label: 'INI / TOML' },
  { id: 'diff', label: 'Diff' },
  { id: 'nginx', label: 'Nginx' },
  { id: 'http', label: 'HTTP' },
  { id: 'protobuf', label: 'Protobuf' },
  { id: 'latex', label: 'LaTeX' },
  { id: 'haskell', label: 'Haskell' },
  { id: 'elixir', label: 'Elixir' },
  { id: 'clojure', label: 'Clojure' },
  { id: 'vim', label: 'Vim Script' },
]

/** 语言的显示名：自动 / 纯文本跟随界面语言，其余用技术名（各语言都这么写，不必翻译）。 */
export function langLabel(id: string): string {
  if (!id) return t('editor.codeLangAuto')
  if (id === 'text') return t('editor.codeLangPlain')
  return LANGS.find((l) => l.id === id)?.label || id
}

/** 代码块当前语言（`<code class="hljs language-x">` 里的 x）；没写语言返回空串。 */
export function codeLangOf(pre: HTMLElement): string {
  const cls = pre.querySelector('code')?.getAttribute('class') || ''
  const m = cls.match(/language-([\w+.-]+)/)
  return m ? m[1] : ''
}

/** 代码里的光标位置（按 `codeText` 的口径，`<br>` 算一个换行）；光标不在代码里返回 null。 */
function caretOffset(code: HTMLElement): number | null {
  const s = document.getSelection()
  if (!s || !s.rangeCount) return null
  const r = s.getRangeAt(0)
  if (!r.collapsed || !code.contains(r.startContainer)) return null
  const head = document.createRange()
  head.selectNodeContents(code)
  head.setEnd(r.startContainer, r.startOffset)
  const box = document.createElement('div')
  box.appendChild(head.cloneContents())
  return codeText(box).length
}

/** 把光标放回文本偏移处：重排高亮后整棵子树都是新节点，只能按偏移重新找位置。 */
function setCaret(code: HTMLElement, offset: number): void {
  let left = offset
  const place = (node: Node, off: number): void => {
    const s = document.getSelection()
    if (!s) return
    const r = document.createRange()
    r.setStart(node, off)
    r.collapse(true)
    s.removeAllRanges()
    s.addRange(r)
  }
  const walk = (el: Node): boolean => {
    for (const c of Array.from(el.childNodes)) {
      if (c.nodeType === Node.TEXT_NODE) {
        const len = (c.nodeValue ?? '').length
        if (left <= len) {
          place(c, left)
          return true
        }
        left -= len
        continue
      }
      if (c.nodeType !== Node.ELEMENT_NODE) continue
      if ((c as Element).tagName === 'BR') {
        if (left === 0) {
          place(c.parentNode as Node, Array.prototype.indexOf.call(c.parentNode!.childNodes, c))
          return true
        }
        left -= 1
        continue
      }
      if (walk(c)) return true
    }
    return false
  }
  if (!walk(code)) {
    // 偏移超出（重排后文本变短之类）：退到末尾，至少不丢光标
    const r = document.createRange()
    r.selectNodeContents(code)
    r.collapse(false)
    const s = document.getSelection()
    if (s) {
      s.removeAllRanges()
      s.addRange(r)
    }
  }
}

/** 改代码块语言：写 `<code>` 的 language class、按新语言重排高亮、同步左上角标签。
 *  高亮重排会重建整棵子树，所以先记下输入框的文本偏移、重排后再放回去；焦点一并交还预览根，
 *  选完语言可以直接接着打字。返回是否改成功（没有 `<code>` 就什么都不做）。 */
export function applyCodeLang(pre: HTMLElement, id: string): boolean {
  const code = pre.querySelector('code') as HTMLElement | null
  if (!code) return false
  const text = codeText(code)
  const offset = caretOffset(code)
  code.className = 'hljs' + (id ? ` language-${id}` : '')
  code.innerHTML = highlightCode(text, id)
  const chip = pre.querySelector('.code-lang-btn') as HTMLElement | null
  if (chip) {
    chip.textContent = langLabel(id)
    chip.dataset.lang = id
  }
  const root = pre.closest('[contenteditable="true"]') as HTMLElement | null
  if (root) root.focus()
  if (offset !== null) setCaret(code, offset)
  return true
}

// ---------- 语言菜单 ----------

let menu: HTMLElement | null = null
let menuAnchor: HTMLElement | null = null
let menuCleanup: (() => void) | null = null

/** 关掉语言菜单（没开时是无操作）：窗口滚动、改窗口大小、点别处都会走这里。
 *  标签上的 `.menu-on` 同步收回——控件带是「悬停才浮出」的，菜单开着时鼠标往往已经离开代码块，
 *  不把标签按住会看着它淡出。 */
export function closeCodeLangMenu(): void {
  if (menuCleanup) menuCleanup()
  menuCleanup = null
  if (menuAnchor) menuAnchor.classList.remove('menu-on')
  menuAnchor = null
  if (menu) menu.remove()
  menu = null
}

/** 在语言标签下方弹出语言菜单；`onPick` 拿到选中的语言 id（空串 = 自动识别）。 */
export function openCodeLangMenu(anchor: HTMLElement, current: string, onPick: (id: string) => void): void {
  // 再点同一个标签就是收起；换一个代码块则换位置重开
  if (menu && menuAnchor === anchor) {
    closeCodeLangMenu()
    return
  }
  closeCodeLangMenu()
  menuAnchor = anchor
  // 菜单开着时标签保持可见（同注：控件带默认不显示，只能靠 hover 浮出）
  anchor.classList.add('menu-on')

  const box = document.createElement('div')
  box.className = 'code-lang-menu'
  box.setAttribute('role', 'listbox')
  const search = document.createElement('input')
  search.className = 'clm-search'
  search.type = 'text'
  search.placeholder = t('editor.codeLangSearch')
  search.spellcheck = false
  box.appendChild(search)

  const list = document.createElement('div')
  list.className = 'clm-list'
  box.appendChild(list)

  const items: HTMLButtonElement[] = []
  for (const l of LANGS) {
    const b = document.createElement('button')
    b.type = 'button'
    b.className = 'clm-item' + (l.id === current ? ' on' : '')
    b.dataset.lang = l.id
    b.dataset.label = langLabel(l.id)
    b.innerHTML = `<span class="clm-name"></span><span class="clm-id"></span>`
    ;(b.firstChild as HTMLElement).textContent = langLabel(l.id)
    ;(b.lastChild as HTMLElement).textContent = l.id
    b.addEventListener('click', () => {
      closeCodeLangMenu()
      onPick(l.id)
    })
    list.appendChild(b)
    items.push(b)
  }

  const visible = (): HTMLButtonElement[] => items.filter((b) => !b.classList.contains('off'))
  /** 把高亮项在列表内滚出来。不能用 scrollIntoView：它会把所有可滚动祖先（预览区、窗口）
   *  一起滚到该项，用户点一下语言标签，整篇预览就跳了；而且那次滚动还会被下面的 scroll 监听当成
   *  页面滚动，把刚开的菜单关掉。 */
  const reveal = (b: HTMLButtonElement): void => {
    const br = b.getBoundingClientRect()
    const lr = list.getBoundingClientRect()
    if (br.top < lr.top) list.scrollTop -= lr.top - br.top
    else if (br.bottom > lr.bottom) list.scrollTop += br.bottom - lr.bottom
  }
  const mark = (b: HTMLButtonElement | null): void => {
    for (const x of items) x.classList.toggle('cur', x === b)
    if (b) reveal(b)
  }
  const filter = (): void => {
    const q = search.value.trim().toLowerCase()
    for (const b of items) {
      const label = (b.dataset.label || '').toLowerCase()
      const id = (b.dataset.lang || '').toLowerCase()
      // 用 class 而不是 hidden 属性：`.clm-item` 自己带 display，属性选择器会输给类选择器
      b.classList.toggle('off', !!q && !label.includes(q) && !id.includes(q))
    }
    // 当前高亮项被过滤掉了才回到第一项；否则保持（打开时停在当前语言上）
    const list0 = visible()
    mark(list0.find((b) => b.classList.contains('cur')) ?? list0[0] ?? null)
  }
  search.addEventListener('input', filter)
  search.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      // 便签窗口把 Esc 绑成「保存并收起」，菜单开着时它只该关菜单
      e.preventDefault()
      e.stopPropagation()
      closeCodeLangMenu()
      // 焦点交回预览根：菜单节点被删后焦点会掉到 body，用户接着打字就落空了
      anchor.closest<HTMLElement>('[contenteditable="true"]')?.focus()
      return
    }
    if (e.key === 'Enter') {
      e.preventDefault()
      visible().find((b) => b.classList.contains('cur'))?.click()
      return
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      const list0 = visible()
      if (!list0.length) return
      const i = list0.findIndex((b) => b.classList.contains('cur'))
      const next = e.key === 'ArrowDown' ? (i + 1) % list0.length : (i - 1 + list0.length) % list0.length
      mark(list0[next])
    }
  })

  document.body.appendChild(box)
  // 定位：贴标签下方，右侧与标签对齐；下方放不下就翻到上方，横向夹在可视区内
  const a = anchor.getBoundingClientRect()
  const w = box.offsetWidth
  const h = box.offsetHeight
  const left = Math.max(8, Math.min(a.right - w, window.innerWidth - w - 8))
  const top = a.bottom + 6 + h > window.innerHeight - 8 ? Math.max(8, a.top - h - 6) : a.bottom + 6
  box.style.left = `${left}px`
  box.style.top = `${top}px`
  // 打开时高亮当前语言（把键盘方向键的起点放在用户平时改语言的位置）
  mark(items.find((b) => b.dataset.lang === current) ?? items[0])
  filter()

  const onDown = (e: MouseEvent): void => {
    const n = e.target as Node | null
    if (n && (box.contains(n) || anchor.contains(n))) return
    closeCodeLangMenu()
  }
  // 菜单自己的列表滚动（高亮项滚入视野、用户翻列表）不是页面滚动——把菜单关掉会让
  // 「列表长、当前语言靠后」的块永远打不开菜单（以前只有排在前几位的语言能用）
  const onScroll = (e: Event): void => {
    const n = e.target as Node | null
    if (n && n.nodeType === Node.ELEMENT_NODE && box.contains(n)) return
    closeCodeLangMenu()
  }
  const onResize = (): void => closeCodeLangMenu()
  document.addEventListener('mousedown', onDown, true)
  window.addEventListener('resize', onResize)
  document.addEventListener('scroll', onScroll, true)
  menuCleanup = () => {
    document.removeEventListener('mousedown', onDown, true)
    window.removeEventListener('resize', onResize)
    document.removeEventListener('scroll', onScroll, true)
  }
  menu = box
  // preventScroll：聚焦搜索框时别把预览区／窗口滚动一下
  search.focus({ preventScroll: true })
}
