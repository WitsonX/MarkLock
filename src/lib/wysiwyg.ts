//! 就地编辑（预览模式所见即所得）的两条腿：把 marked 渲染出的 DOM 写回 Markdown 源码，
//! 以及把工具栏/菜单的格式化命令作用在这套 DOM 上。
//!
//! 反向序列化只有「DOM → 源码」一个方向，源码 → DOM 仍走 preview.ts 的 marked 渲染。两者共用同一套
//! 标签约定（`<pre><code class="language-x">`、`<input class="task-cb">`、`.codespan-wrap`、
//! 顶层块外包 `.mb`），所以 **marked 的 renderer 一旦改动，本文件必须同步跟上**。
//!
//! 有损是这套做法的固有代价，不是实现瑕疵：源码的字面排版（Setext 标题、`__粗__`、表格对齐空白、
//! 行尾空格、列表用 `-` 还是 `*`）在回写时会被规整成一种形式，渲染结果不变但字节变了。

import { renderMarkdown } from './preview'

/** Markdown 有自己的语法能表达的块级标签。 */
const MD_BLOCK_TAGS = new Set(['P', 'UL', 'OL', 'BLOCKQUOTE', 'PRE', 'TABLE', 'HR'])
/** 只能按原始 HTML 整块保留的标签：没有对应的 Markdown 语法，拍平成纯文本会丢掉用户手写的标记。 */
const RAW_BLOCK_TAGS = new Set([
  'DIV', 'SECTION', 'ARTICLE', 'ASIDE', 'HEADER', 'FOOTER', 'NAV', 'MAIN', 'FIGURE', 'FIGCAPTION',
  'DETAILS', 'SUMMARY', 'DL', 'DT', 'DD', 'IFRAME', 'VIDEO', 'AUDIO', 'CANVAS', 'SVG', 'FORM', 'CENTER',
])

/** 词字符判定：下划线只在贴到词边界时才可能被解析成强调，词内（snake_case）放行不用转义。 */
const WORD_CHAR = /[\p{L}\p{N}_]/u

function isBlockTag(el: HTMLElement): boolean {
  return MD_BLOCK_TAGS.has(el.tagName) || RAW_BLOCK_TAGS.has(el.tagName) || /^H[1-6]$/.test(el.tagName)
}

/** 行内代码的反引号定界：内容含反引号时加长定界符并前后补空格，否则 CommonMark 认不出来。 */
function codeSpanMd(text: string): string {
  if (!text) return '``'
  const runs = text.match(/`+/g)
  let fence = '`'
  if (runs) fence = '`'.repeat(Math.max(...runs.map((r) => r.length)) + 1)
  const needsPad = text.startsWith('`') || text.endsWith('`') || text.startsWith(' ') || text.endsWith(' ')
  const pad = needsPad ? ' ' : ''
  return `${fence}${pad}${text}${pad}${fence}`
}

/** 围栏代码块：定界长度同样按内容里的最长反引号串加长，语言名从 hljs 写的 language-x class 取。 */
function fencedCodeMd(text: string, lang: string): string {
  const runs = text.match(/`+/g)
  let fence = '```'
  if (runs) {
    const longest = Math.max(...runs.map((r) => r.length))
    if (longest >= 3) fence = '`'.repeat(longest + 1)
  }
  return `${fence}${lang}\n${text.replace(/\n$/, '')}\n${fence}`
}

/** 行内文本转义，策略是「只转真的会被重新解析成标记的字符」，否则源码里会满是反斜杠。
 *  逐字符扫描而不是正则替换：既避免了后行断言（老 WebKitGTK 解析到就直接语法报错，
 *  连带整个模块加载失败），也保证 `\\` 先转后不会把后面新加的转义符再转一遍。 */
function escapeText(s: string): string {
  let out = ''
  for (let i = 0; i < s.length; i++) {
    const c = s[i]
    const prev = i > 0 ? s[i - 1] : ''
    const next = i + 1 < s.length ? s[i + 1] : ''
    if (c === '\\' || c === '`' || c === '*' || c === '[' || c === ']') {
      out += '\\' + c
      continue
    }
    if (c === '_' && !(WORD_CHAR.test(prev) && WORD_CHAR.test(next))) {
      out += '\\_'
      continue
    }
    // 连续两个波浪号才成删除线，单个 ~ 是普通字符，原样保留
    if (c === '~' && next === '~') {
      out += '\\~\\~'
      i++
      continue
    }
    // 尖括号后面像标签名时才转义，否则 `<script>` 这类正文回写后会变成真元素被吞掉
    if (c === '<' && next !== '' && /[A-Za-z/!]/.test(next)) {
      out += '\\<'
      continue
    }
    out += c
  }
  return out
}

/** 行首转义：正文恰好以块标记开头时补反斜杠，否则回写后会被解析成标题 / 列表 / 引用。 */
function escapeLineHead(s: string): string {
  return s
    .replace(/^(#{1,6}\s)/, '\\$1')
    .replace(/^([-+*]\s)/, '\\$1')
    .replace(/^(\d{1,9})([.)]\s)/, '$1\\$2')
    .replace(/^(>\s)/, '\\$1')
}

/** 表格单元格：竖线会切列必须转义，换行会把一行劈成两行，压成空格。 */
function escapeCell(s: string): string {
  return s.replace(/\|/g, '\\|').replace(/\s*\n\s*/g, ' ').trim()
}

/** 应用自己注入的控件不属于正文，绝不能进源码（复制按钮及其图标与文字）。 */
function isChrome(el: HTMLElement): boolean {
  return el.classList.contains('code-copy-btn') || el.classList.contains('cc-txt') || el.classList.contains('cc-ico')
}

/** 原始 HTML 整块保留前先剥掉应用注入的临时搜索高亮（预览内搜索 pv-search / 全局搜索命中定位 pv-hit），
 *  否则就地编辑回写会把 <mark> 写进源码。行内序列化对 MARK 本就透明穿过（见 inlineNode），
 *  只有 outerHTML 这条路会把高亮标签原样带出去；用户手写的 <mark>（不带这两个类）不受影响。 */
function rawHtml(el: HTMLElement): string {
  if (!el.querySelector('mark.pv-search, mark.pv-hit')) return el.outerHTML
  const clone = el.cloneNode(true) as HTMLElement
  for (const m of Array.from(clone.querySelectorAll('mark.pv-search, mark.pv-hit'))) {
    const parent = m.parentNode
    if (parent) parent.replaceChild(document.createTextNode(m.textContent ?? ''), m)
    else m.remove()
  }
  return clone.outerHTML
}

/** 取图片的原始 src：marked 渲染时相对路径被 resolveImgSrc 换成了 asset:// 地址，
 *  反解析必须读 preview.ts 写在 data-src 上的原值，否则回写会把 `a.png` 变成一长串协议地址。
 *  data-src 缺失（老缓存渲染的 DOM、外链）时退回 src。 */
function imgSrc(el: HTMLElement): string {
  return el.getAttribute('data-src') || el.getAttribute('src') || ''
}

function inlineChildren(el: Node): string {
  let out = ''
  for (const n of Array.from(el.childNodes)) out += inlineNode(n)
  return out
}

function inlineNode(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) return escapeText(node.nodeValue ?? '')
  if (node.nodeType !== Node.ELEMENT_NODE) return ''
  const el = node as HTMLElement
  if (isChrome(el)) return ''
  // 行内代码：MarkLock 的 renderer 外面包了一层 .codespan-wrap 放复制按钮，只取其中 code 的纯文本
  if (el.classList.contains('codespan-wrap')) {
    return codeSpanMd(el.querySelector('code')?.textContent ?? '')
  }
  // 搜索命中的临时高亮不参与正文
  if (el.tagName === 'MARK') return inlineChildren(el)

  switch (el.tagName) {
    case 'BR':
      return '\n'
    case 'STRONG':
    case 'B':
      return `**${inlineChildren(el)}**`
    case 'EM':
    case 'I':
      return `*${inlineChildren(el)}*`
    case 'DEL':
    case 'S':
    case 'STRIKE':
      return `~~${inlineChildren(el)}~~`
    case 'CODE':
      return codeSpanMd(el.textContent ?? '')
    case 'INPUT':
      // 任务清单复选框：编辑期间 DOM 就是真源，勾选态直接取当前值
      return (el as HTMLInputElement).checked ? '[x]' : '[ ]'
    case 'IMG': {
      const title = el.getAttribute('title')
      return `![${el.getAttribute('alt') ?? ''}](${imgSrc(el)}${title ? ` "${title}"` : ''})`
    }
    case 'A': {
      const href = el.getAttribute('href') ?? ''
      const text = inlineChildren(el)
      const title = el.getAttribute('title')
      // 自动链接（显示文字就是地址本身）还原成裸地址，别把源码写胖。但只在「这个链接就是整段」时还原：
      // GFM 的裸网址会一直吃到下一个空白，紧挨着正文时会把后面的字一起吞进链接。
      const parent = el.parentElement
      const alone =
        !!parent &&
        !parent.nextElementSibling &&
        !parent.previousElementSibling &&
        /^P|H[1-6]$/.test(parent.tagName) &&
        (parent.textContent ?? '').trim() === (el.textContent ?? '').trim()
      if (!title && text === href && alone) return href
      return `[${text}](${href}${title ? ` "${title}"` : ''})`
    }
    case 'SPAN': {
      // span 多是浏览器编辑时自己插的样式壳子。命令层已关掉了 styleWithCSS，但用户粘贴富文本
      // 或某些内核仍会留下 <span style="font-weight:bold">，透明穿过等于把加粗吃掉——按行内样式认一次。
      const st = el.style
      let out = inlineChildren(el)
      if (/line-through/.test(st.textDecoration || st.textDecorationLine || '')) out = `~~${out}~~`
      if (st.fontStyle === 'italic' || st.fontStyle === 'oblique') out = `*${out}*`
      if (st.fontWeight === 'bold' || st.fontWeight === 'bolder' || Number(st.fontWeight) >= 600)
        out = `**${out}**`
      return out
    }
    default:
      // 其余未知行内元素（<u>、<sub>、<kbd> 等）Markdown 没有对应语法，按原始 HTML 保留
      return rawHtml(el)
  }
}

/** 逐行加前缀（嵌套缩进、引用标记）。 */
function prefixLines(s: string, pad: string): string {
  return s
    .split('\n')
    .map((l) => (l ? pad + l : l))
    .join('\n')
}

/** 裁掉块尾的空白与软换行；块与块之间由 blocksMd 统一补一个空行。 */
function trimEnd(s: string): string {
  return s.replace(/[\s\n]+$/, '')
}

/** 列表（ul/ol）。marked 有三处形状必须分开处理：
 *  1) 嵌套子列表挂在父 `<li>` **内部**，且与父项文本之间夹着一个换行文本节点——它不是空行，
 *     带上就会把紧凑列表变成宽松列表（子项各自成段），所以纯空白文本节点直接丢弃、行首行尾空白裁掉；
 *  2) 宽松列表把项内容包进 `<p>`，第一个 `<p>` 是本项正文而非续行，要并入行首，其余块才缩进；
 *  3) 任务项 `- [ ] todo` 里 `]` 后的空格被 marked 的 tokenizer 吃掉、渲染时不补，
 *     回写必须自己加回来，否则重新解析就掉出任务语法（`[ ]todo` 会变成纯文本）。 */
function listMd(el: HTMLElement): string {
  const ordered = el.tagName === 'OL'
  const start = parseInt(el.getAttribute('start') || '1', 10) || 1
  const loose = Array.from(el.children).some(
    (li) => li.tagName === 'LI' && Array.from(li.children).some((c) => c.tagName === 'P')
  )
  let n = 0
  const items: string[] = []
  for (const li of Array.from(el.children)) {
    if (li.tagName !== 'LI') continue
    n++
    const marker = ordered ? `${start + n - 1}. ` : '- '
    const pad = ' '.repeat(marker.length)
    let head = ''
    const rest: string[] = []
    for (const c of Array.from(li.childNodes)) {
      if (c.nodeType === Node.ELEMENT_NODE) {
        const e = c as HTMLElement
        if (e.tagName === 'P' && !rest.length && !head.trim()) {
          head += inlineChildren(e)
          continue
        }
        // 续行与子列表都要按本项标记宽度缩进，否则回写后层级会塌
        if (isBlockTag(e)) {
          rest.push(prefixLines(blockMd(e), pad))
          continue
        }
      } else if (c.nodeType === Node.TEXT_NODE && !(c.nodeValue ?? '').trim()) {
        continue
      }
      head += inlineNode(c)
    }
    head = head.replace(/^[\s]+/, '').replace(/[\s]+$/, '')
    head = head.replace(/^(\[[ xX]\])[\s]*/, '$1 ')
    const sep = loose ? '\n\n' : '\n'
    items.push([marker + head, ...rest].join(sep))
  }
  return items.join(loose ? '\n\n' : '\n')
}

/** 表格：对齐信息在 marked 输出的 th/td 的 align 属性上，据此生成定界行。 */
function tableMd(el: HTMLElement): string {
  const headRow = el.querySelector('thead tr') ?? el.querySelector('tr')
  if (!headRow) return rawHtml(el)
  const aligns = Array.from(headRow.children).map((c) => (c.getAttribute('align') || '').toLowerCase())
  const rowMd = (tr: Element) =>
    `| ${Array.from(tr.children).map((td) => escapeCell(inlineChildren(td as HTMLElement))).join(' | ')} |`
  const delim =
    `| ${aligns
      .map((a) => (a === 'center' ? ':--:' : a === 'right' ? '--:' : a === 'left' ? ':-' : '--'))
      .join(' | ')} |`

  const body = el.querySelector('tbody')
  const rows = body ? Array.from(body.children) : Array.from(el.querySelectorAll('tr')).slice(1)
  return [rowMd(headRow), delim, ...rows.map(rowMd)].join('\n')
}

/** 引用块：逐子块序列化再加 `> ` 前缀。空行也必须带上标记（写成 `>`），
 *  否则 `> a` / 空行 / `> b` 回写后会裂成两个独立引用块。 */
function blockquoteMd(el: HTMLElement): string {
  const hasBlockChild = Array.from(el.children).some((c) => isBlockTag(c as HTMLElement))
  const inner = hasBlockChild
    ? (Array.from(el.children) as HTMLElement[]).map(blockMd).filter(Boolean).join('\n\n')
    : inlineChildren(el)
  return inner
    .split('\n')
    .map((l) => (l ? `> ${l}` : '>'))
    .join('\n')
}

/** 代码块文本必须按 DOM 走，不能用 `textContent`：编辑器在 `<pre>` 里敲回车，浏览器留下的是一
 *  个 `<br>`（Chromium）或一个 `<div>`（WebKit），`textContent` 把两者都读成零宽，两行代码会被
 *  并成一行——这是真丢内容，不是排版规整。这里把 `<br>` 与块边界都记作换行。 */
function codeText(el: HTMLElement): string {
  if (isChrome(el)) return ''
  let out = ''
  for (const c of Array.from(el.childNodes)) {
    const e = c as HTMLElement
    // 块边界就是一个换行；浏览器代码高亮用的 span / code 不是行边界，不能一律加换行
    if (e.nodeType === Node.ELEMENT_NODE && /^(DIV|P|LI)$/.test(e.tagName)) {
      if (out && !out.endsWith('\n')) out += '\n'
      out += codeText(e) + '\n'
      continue
    }
    if (c.nodeType === Node.TEXT_NODE) {
      out += c.nodeValue ?? ''
      continue
    }
    if (c.nodeType !== Node.ELEMENT_NODE) continue
    out += e.tagName === 'BR' ? '\n' : codeText(e)
  }
  return out
}

/** 单个块级元素 → Markdown。
 *  末尾的软换行统一裁掉：contenteditable 会在空段落里留下一个孤立的 `<br>` 占位、
 *  删字符时也常在行尾留一个，不裁就会回写成多余空行（Markdown 里空行会改变块归属）。 */
function blockMd(el: HTMLElement): string {
  if (isChrome(el)) return ''
  const t = el.tagName
  if (/^H[1-6]$/.test(t)) {
    const txt = trimEnd(inlineChildren(el)).replace(/^\s+/, '')
    // 标题被清空就整块丢弃：`#` 后面没有内容不是标题，写回去会渲染成一个 '#' 段落
    if (!txt) return ''
    return `${'#'.repeat(parseInt(t[1], 10))} ${txt}`
  }
  switch (t) {
    case 'P':
      // Blink 在段落里转列表会把 ul 留在 p 内部（`<p><ul>…</ul></p>`，非法嵌套但浏览器允许），
      // 直接走行内序列化会把整个列表写成原始 HTML 残渣；带块级子节点时按容器拆开处理。
      if (Array.from(el.children).some((c) => isBlockTag(c as HTMLElement))) return blocksMd(el)
      return trimEnd(escapeLineHead(inlineChildren(el)))
    case 'UL':
    case 'OL':
      return listMd(el)
    case 'BLOCKQUOTE':
      return blockquoteMd(el)
    case 'PRE': {
      const code = el.querySelector('code')
      const lang = (code?.getAttribute('class') || '').match(/language-([\w+.-]+)/)
      return fencedCodeMd(codeText(code ?? el), lang ? lang[1] : '')
    }
    case 'TABLE':
      return tableMd(el)
    case 'HR':
      return '---'
    case 'DIV':
      // EditorView 把每个顶层块包进 .mb（data-l 只服务滚动同步，不是正文），拆包继续走块序列化；
      // 其它 div 是用户手写的原始 HTML，整块保留
      if (el.classList.contains('mb')) return blocksMd(el)
      return rawHtml(el)
    default:
      if (RAW_BLOCK_TAGS.has(t)) return rawHtml(el)
      // 顶层的行内元素（例如以图片单独成段时 marked 仍会给 <p>，这里兜住 <img> 裸挂的情况）
      return trimEnd(escapeLineHead(inlineChildren(el)))
  }
}

/** 容器（`.mb` 包块或预览根节点）的子节点序列化成 Markdown。
 *  必须按 childNodes 而不是 children 走：就地编辑取消列表时会留下「块之间夹着裸文本节点」的形状
 *  （`<p></p>文本<ul>…`），只认元素子节点会把这段文本整段丢掉。 */
function blocksMd(el: HTMLElement): string {
  const out: string[] = []
  let run: Node[] = []
  const flush = () => {
    if (!run.length) return
    run = []
    const txt = trimEnd(escapeLineHead(run.map(inlineNode).join('')))
    if (txt) out.push(txt)
  }
  for (const c of Array.from(el.childNodes)) {
    if (c.nodeType === Node.ELEMENT_NODE && isBlockTag(c as HTMLElement)) {
      flush()
      const s = blockMd(c as HTMLElement)
      if (s) out.push(s)
      continue
    }
    if (c.nodeType === Node.TEXT_NODE && !(c.nodeValue ?? '').trim()) continue
    if (c.nodeType !== Node.TEXT_NODE && c.nodeType !== Node.ELEMENT_NODE) continue
    run.push(c)
  }
  flush()
  return out.join('\n\n')
}

/** 预览容器（`.md`，直接子级是 `.mb` 包块或裸块）→ Markdown 正文，不含结尾换行。 */
export function domToMarkdown(root: HTMLElement): string {
  return blocksMd(root)
}

/** 回写时保留原文件结尾的换行习惯（多数 Markdown 以单个换行收尾，也有留空行或不留的）。 */
export function preserveTrailing(oldSrc: string, newSrc: string): string {
  const body = newSrc.replace(/\n+$/, '')
  if (!oldSrc) return body ? `${body}\n` : ''
  const trail = oldSrc.match(/\n*$/)?.[0] || '\n'
  return body + trail
}

// ─── 格式化命令层 ──────────────────────────────────────────────────────────
// 预览模式下工具栏 / macOS 主菜单 / 快捷键的格式化落点。原则：一律走 document.execCommand，
// 不手工改 DOM —— 只有命令会被浏览器记进原生撤销栈，⌘Z 才能逐步撤回；产出的标签也限定在
// 上面这套序列化认识的集合里（bold/italic/strikeThrough 关掉 styleWithCSS 就是为了拿到
// <b>/<i>/<s>，而不是 Chromium 默认的 <span style=…> 壳子）。

/** 格式化命令，参数沿用 Markdown 口径（`**`、`> `），这样源码模式与就地编辑共用同一批入口。 */
export type PreviewFmt =
  | { op: 'heading'; level: number }
  | { op: 'wrap'; before: string; placeholder: string }
  | { op: 'prefix'; prefix: string }
  | { op: 'codeblock' }
  | { op: 'insertMd'; md: string }
  | { op: 'link'; text: string; url: string; placeholder: string }
  | { op: 'image'; alt: string; url: string }

/** 就地编辑眼里的「一个块」：格式化命令的作用对象。 */
const PV_BLOCK = 'p,h1,h2,h3,h4,h5,h6,li,pre,blockquote,td,th'

const PV_INLINE_CMD: Record<string, string> = { '**': 'bold', '*': 'italic', '~~': 'strikeThrough' }

function escAttr(s: string): string {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

function runCmd(name: string, value?: string): boolean {
  try {
    return document.execCommand(name, false, value)
  } catch {
    return false
  }
}

function pvSelection(): Selection | null {
  const s = document.getSelection()
  return s && s.rangeCount ? s : null
}

/** 选区/光标所在、且在预览容器内的最近祖先元素。不在容器内一律返回 null——
 *  格式化不该悄悄作用到别的窗格或未开启就地编辑的那一侧。 */
function pvAncestor<E extends Element>(root: HTMLElement, selector: string): E | null {
  const s = pvSelection()
  if (!s) return null
  const n = s.focusNode ?? s.anchorNode
  if (!n || !root.contains(n)) return null
  const e = (n.nodeType === Node.TEXT_NODE ? n.parentElement : (n as Element)) as Element | null
  if (!e || !root.contains(e)) return null
  const hit = e.closest(selector)
  return hit && root.contains(hit) ? (hit as E) : null
}

function pvBlock(root: HTMLElement): HTMLElement | null {
  return pvAncestor<HTMLElement>(root, PV_BLOCK)
}

/** 点工具栏不会产生选区：预览还没被点过时把光标放到正文最后一个非空块尾部，给命令一个明确落点。
 *  另外 focus() 在 Chromium 里会把光标折到内容末端，所以进焦点前先把现有选区存一份、回来再放回。 */
function focusPreview(root: HTMLElement): boolean {
  const s = document.getSelection()
  if (!s) return false
  const r0 = s.rangeCount === 1 ? s.getRangeAt(0) : null
  const had = r0 && root.contains(r0.commonAncestorContainer) ? r0.cloneRange() : null
  if (document.activeElement !== root) root.focus({ preventScroll: true })
  if (had) {
    const cur = s.rangeCount === 1 ? s.getRangeAt(0) : null
    const intact =
      !!cur &&
      cur.startContainer === had.startContainer &&
      cur.startOffset === had.startOffset &&
      cur.endContainer === had.endContainer &&
      cur.endOffset === had.endOffset
    if (!intact) {
      s.removeAllRanges()
      s.addRange(had)
    }
    return true
  }
  const blocks = Array.from(root.querySelectorAll(PV_BLOCK)) as HTMLElement[]
  const last = [...blocks].reverse().find((b) => (b.textContent || '').trim()) ?? root
  const r = document.createRange()
  r.selectNodeContents(last)
  r.collapse(false)
  s.removeAllRanges()
  s.addRange(r)
  return true
}

/** formatBlock 的参数形态各家容忍度不同（'h1' 还是 '<h1>'），验一次再换写法重试。 */
function formatBlock(root: HTMLElement, tag: string): void {
  runCmd('styleWithCSS', 'false')
  runCmd('formatBlock', tag)
  const b = pvBlock(root)
  if (!b || b.tagName !== tag.toUpperCase()) runCmd('formatBlock', `<${tag}>`)
}

/** 刚插入的内容：execCommand 之后光标紧跟在插入节点之后，所以「光标前一个兄弟元素」就是它。
 *  传选择器时先试元素本身，再试它内部的第一个命中（插进来的是 .codespan-wrap / pre 这类壳子）。 */
function justInserted(selector: string): HTMLElement | null {
  const s = pvSelection()
  if (!s) return null
  const r = s.getRangeAt(0)
  const host = r.startContainer
  const prev =
    host.nodeType === Node.ELEMENT_NODE
      ? ((host as HTMLElement).childNodes[r.startOffset - 1] ?? null)
      : ((host as HTMLElement).previousSibling)
  if (!prev || prev.nodeType !== Node.ELEMENT_NODE) return null
  const el = prev as HTMLElement
  if (el.matches(selector)) return el
  return el.querySelector(selector)
}

/** 把「刚用 insertText 打进去的那段文字」重新选出来：光标此刻停在它末尾，往前数 txt 的长度即可。
 *  内核有时会把光标放到后一个文本节点的开头，所以两种落点都试一次。 */
function selectJustInsertedText(txt: string): boolean {
  const s = pvSelection()
  if (!s || !s.isCollapsed) return false
  const r = s.getRangeAt(0)
  const nodes: Array<[Node, number]> =
    r.startContainer.nodeType === Node.TEXT_NODE
      ? [[r.startContainer, r.startOffset]]
      : []
  const prev = r.startContainer.nodeType === Node.TEXT_NODE ? null : r.startContainer.previousSibling
  if (prev && prev.nodeType === Node.TEXT_NODE) nodes.push([prev, (prev.nodeValue ?? '').length])
  for (const [n, end] of nodes) {
    const data = n.nodeValue ?? ''
    if (end < txt.length || data.slice(end - txt.length, end) !== txt) continue
    const q = document.createRange()
    q.setStart(n, end - txt.length)
    q.setEnd(n, end)
    s.removeAllRanges()
    s.addRange(q)
    return true
  }
  return false
}

function selectContents(el: HTMLElement, collapseToEnd = false): void {
  const s = document.getSelection()
  if (!s) return
  const r = document.createRange()
  r.selectNodeContents(el)
  r.collapse(collapseToEnd)
  s.removeAllRanges()
  s.addRange(r)
}

/** 块级内容不能塞在段落中间：光标后面还有正文时先分行，让插入落进独立的块。 */
function splitBeforeBlock(root: HTMLElement): void {
  const b = pvBlock(root)
  if (!b || !(/^P$/.test(b.tagName) || /^H[1-6]$/.test(b.tagName))) return
  const s = pvSelection()
  if (!s) return
  const r = s.getRangeAt(0)
  const tail = document.createRange()
  tail.setStart(r.endContainer, r.endOffset)
  tail.setEndAfter(b)
  if (tail.toString().trim()) runCmd('insertParagraph')
}

function pvListItems(root: HTMLElement): HTMLElement[] {
  const li = pvAncestor<HTMLElement>(root, 'li')
  const s = pvSelection()
  if (!s || s.isCollapsed || !li) return li ? [li] : []
  const r = s.getRangeAt(0)
  const all = Array.from(root.querySelectorAll('li')) as HTMLElement[]
  const hit = all.filter((x) => r.intersectsNode(x) || x.contains(r.startContainer))
  // 嵌套列表时只处理最里层，父项跟着被打上复选框是误伤
  return hit.filter((x) => !hit.some((o) => o !== x && x.contains(o)))
}

function applyWrap(root: HTMLElement, before: string, placeholder: string): void {
  const cmd = PV_INLINE_CMD[before]
  if (cmd) {
    runCmd('styleWithCSS', 'false')
    runCmd(cmd)
    return
  }
  if (before !== '`') return
  // 行内代码没有对应命令。MarkLock 渲染的 codespan 外面还包着 .codespan-wrap（复制按钮），
  // 取消时整壳换成交还纯文本的选区，否则壳子留着会被序列化认成一个空的 `` ``。
  const code = pvAncestor<HTMLElement>(root, 'code')
  if (code) {
    const s = pvSelection()
    const target = (code.closest('.codespan-wrap') ?? code) as HTMLElement
    const r = s?.getRangeAt(0)
    if (!s || !r) return
    r.selectNode(target)
    s.removeAllRanges()
    s.addRange(r)
    // codeText 跳过复制按钮（它的「复制」文字属于控件不属于正文）；
    // 这里用 insertHTML 而不是 insertText：后者会让 Blink 把替换出来的文字重新包成
    // <font face="monospace">，Markdown 没有这个标签，回写就成了原始 HTML 残渣。
    runCmd('insertHTML', escAttr(codeText(target)))
    return
  }
  const s = pvSelection()
  const hadSel = !!s && !s.isCollapsed
  const txt = hadSel ? s!.toString() : placeholder
  // 行内代码交给 marked 渲染，形状（.codespan-wrap + 复制按钮）与整篇渲染时完全一致
  runCmd('insertHTML', inlineHtml(codeSpanMd(txt)))
  if (!hadSel) {
    const el = justInserted('code')
    if (el) selectContents(el)
  }
}

function applyTask(root: HTMLElement): void {
  if (!pvAncestor(root, 'ul,ol')) runCmd('insertUnorderedList')
  for (const li of pvListItems(root)) {
    const cb = li.firstElementChild
    const s = document.getSelection()
    if (!s) return
    if (cb && cb.tagName === 'INPUT' && cb.classList.contains('task-cb')) {
      const r = document.createRange()
      r.setStartBefore(cb)
      const nx = cb.nextSibling
      if (nx?.nodeType === Node.TEXT_NODE && /^[ \u00a0]/.test(nx.nodeValue ?? '')) r.setEnd(nx, 1)
      else r.setEndAfter(cb)
      s.removeAllRanges()
      s.addRange(r)
      runCmd('delete')
      continue
    }
    // 勾选框必须逐字复刻 preview.ts 的 checkbox renderer：class 决定序列化读它，
    // contenteditable=false 决定它是不是正文。
    const r = document.createRange()
    r.setStart(li, 0)
    r.collapse(true)
    s.removeAllRanges()
    s.addRange(r)
    runCmd('insertHTML', '<input type="checkbox" contenteditable="false" class="task-cb"> ')
  }
}

/** 插入一段 Markdown 渲染出的块（表格、分隔线）：直接借 marked 的输出，
 *  省掉一份「HTML 长什么样」的第二定义，插入形状与渲染形状天然一致。 */
function applyInsertMd(root: HTMLElement, md: string): void {
  const html = renderMarkdown(md).trim()
  splitBeforeBlock(root)
  runCmd('insertHTML', html)
}

function applyImage(root: HTMLElement, alt: string, url: string): void {
  runCmd('insertHTML', inlineHtml(`![${escapeText(alt)}](${linkDest(url)})`))
}

/** 链接目的地：含空格或括号时按 CommonMark 用尖括号包起来，否则解析会断。 */
function linkDest(url: string): string {
  return /[()\s]/.test(url) ? `<${url}>` : url
}

/** 把一小段 Markdown 渲染成能直接插进现有块的行内 HTML：marked 会包一层 `<p>`，剥掉它。 */
function inlineHtml(md: string): string {
  return renderMarkdown(md).trim().replace(/^<p>/, '').replace(/<\/p>$/, '')
}

function applyCodeBlock(root: HTMLElement): void {
  const s = pvSelection()
  const txt = s && !s.isCollapsed ? s.toString() : ''
  splitBeforeBlock(root)
  runCmd('insertHTML', renderMarkdown(fencedCodeMd(txt, '')).trim())
  if (!txt) {
    const code = justInserted('code')
    if (code) selectContents(code, true)
  }
}

/** 就地编辑下执行一次格式化；返回是否真的下发了命令，调用方据此决定要不要回写源码。 */
export function applyPreviewFormat(root: HTMLElement, fmt: PreviewFmt): boolean {
  // 只读预览（contenteditable=false）下命令没有落点：execCommand 会作用到别处或干脆无效，
  // 而调用方还会照旧回写一次源码，等于把没编辑过的文档重排一遍，所以这里先挡掉。
  if (!root.isConnected || !root.isContentEditable || !focusPreview(root)) return false
  switch (fmt.op) {
    case 'heading': {
      const lv = Math.min(6, Math.max(1, Math.round(fmt.level)))
      const b = pvBlock(root)
      formatBlock(root, b && b.tagName === `H${lv}` ? 'p' : `h${lv}`)
      return true
    }
    case 'wrap':
      applyWrap(root, fmt.before, fmt.placeholder)
      return true
    case 'prefix':
      switch (fmt.prefix) {
        case '> ':
          if (pvAncestor(root, 'blockquote')) runCmd('outdent')
          else formatBlock(root, 'blockquote')
          return true
        case '- ':
          runCmd('styleWithCSS', 'false')
          runCmd('insertUnorderedList')
          return true
        case '1. ':
          runCmd('styleWithCSS', 'false')
          runCmd('insertOrderedList')
          return true
        case '- [ ] ':
          applyTask(root)
          return true
        default:
          return false
      }
    case 'codeblock':
      applyCodeBlock(root)
      return true
    case 'insertMd':
      applyInsertMd(root, fmt.md)
      return true
    case 'link': {
      if (!fmt.text) {
        // 空选区：留占位文字并选中它，用户直接打字就能替换。文字刻意不取地址本身，见下面那条注释。
        runCmd('insertHTML', `<a href="${escAttr(fmt.url)}">${escAttr(fmt.placeholder)}</a>`)
        const a = justInserted('a')
        if (a) selectContents(a)
        return true
      }
      if (fmt.text === fmt.url) {
        // 显示文字就是地址时不能用 insertHTML 造链接：Blink 认定这是「手敲的网址链接」，
        // 会把相邻正文并进链接文字并按新文字重写 href（实测 https://x.test 把后面的「尾段」吃了进去）。
        // 先按纯文本插入，再选中这段文字交给 createLink，就没有可被合并的新插入内容。
        runCmd('insertText', fmt.text)
        if (selectJustInsertedText(fmt.text)) runCmd('createLink', fmt.url)
        else runCmd('insertHTML', `<a href="${escAttr(fmt.url)}">${escAttr(fmt.text)}</a>`)
        return true
      }
      runCmd('insertHTML', `<a href="${escAttr(fmt.url)}">${escAttr(fmt.text)}</a>`)
      return true
    }
    case 'image':
      applyImage(root, fmt.alt, fmt.url)
      return true
  }
}

/** 任务项里按回车要续成一条 todo：浏览器只断出普通 `<li>`，勾选框是 `<input>` 而不是列表样式，
 *  不会自己延续。空条目不接管，让默认行为退出列表。两次 execCommand 都进原生撤销栈，⌘Z 能整步撤回。
 *  返回是否消费了这次按键；调用方（preventDefault 在这里做）据此决定要不要安排回写。 */
export function enterInTaskItem(e: KeyboardEvent, root: HTMLElement): boolean {
  if (!root.isContentEditable) return false
  const li = pvAncestor(root, 'li')
  if (!li) return false
  const cb = li.firstElementChild
  if (!cb || cb.tagName !== 'INPUT' || !cb.classList.contains('task-cb')) return false
  if (!(li.textContent ?? '').trim()) return false
  e.preventDefault()
  runCmd('insertParagraph')
  runCmd('insertHTML', '<input type="checkbox" contenteditable="false" class="task-cb"> ')
  return true
}
