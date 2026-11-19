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

/** 应用自己注入的控件不属于正文，绝不能进源码（复制按钮、语言标签，以及它们的图标与文字）。 */
function isChrome(el: HTMLElement): boolean {
  return el.classList.contains('code-copy-btn') || el.classList.contains('code-lang-btn') || el.classList.contains('code-ctl') || el.classList.contains('cc-txt') || el.classList.contains('cc-ico')
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
      const src = imgSrc(el)
      // 路径含空格/括号得用 `<…>` 包住，否则回写的 `![alt](a b.png)` 重新解析会在空格处断掉
      // （从剪贴板粘贴的截图文件名常带空格）。已带尖括号的原样输出，避免双层。
      const dest = !src || /^<.*>$/.test(src) ? src : linkDest(src)
      return `![${el.getAttribute('alt') ?? ''}](${dest}${title ? ` "${title}"` : ''})`
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
 *  并成一行——这是真丢内容，不是排版规整。这里把 `<br>` 与块边界都记作换行。
 *  也供 `lib/codeLang.ts` 重排高亮时保留内容与光标偏移。 */
export function codeText(el: HTMLElement): string {
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
  | { op: 'codeblock'; placeholder: string }
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
  const last = [...blocks].reverse().find((b) => (b.textContent || '').trim())
  // 空文档没有块可落点：补一个空段落。绝不把光标放进 root —— 落在容器根上的插入点
  // 会被 WebKit 画到容器边框盒左上角（浮层模式下正好压在工具栏一带），见 `caretInBlankArea`
  collapseInto(last ?? ensureTrailingParagraph(root), false)
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
  // 插入复选框后光标可能只剩元素级位置（画在复选框左边）：收进所在项的文字里（见 `fixElementLevelCaret`）
  const cur = pvAncestor<HTMLElement>(root, 'li')
  if (cur) fixElementLevelCaret(cur)
}

/** 插入一段 Markdown 渲染出的块（表格、分隔线）：直接借 marked 的输出，
 *  省掉一份「HTML 长什么样」的第二定义，插入形状与渲染形状天然一致。
 *  已经在代码块里则不动：块级标签嵌进 `<pre>` 属于非法结构，回写也表达不出来。 */
function applyInsertMd(root: HTMLElement, md: string): boolean {
  if (pvAncestor(root, 'pre')) return false
  const html = renderMarkdown(md).trim()
  splitBeforeBlock(root)
  runCmd('insertHTML', html)
  return true
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

function applyCodeBlock(root: HTMLElement, placeholder: string): boolean {
  const s = pvSelection()
  if (!s) return false
  // 已经在代码块里：不再嵌一层代码块（源码也表达不出嵌套围栏），直接忽略这次点击
  if (pvAncestor(root, 'pre')) return false
  const selected = s.isCollapsed ? '' : s.toString()
  splitBeforeBlock(root)
  // 空选中时不能插一个空的 <code>：WebKit 的 insertHTML 会把光标留在**上一个段落**末尾
  // （空 <code> 没有落点），接着打字就写进了上文。这里插一段占位文字再按下文选中它，
  // 光标必然落在代码块里，打字即替换占位。
  const before = new Set(Array.from(root.querySelectorAll('pre')))
  runCmd('insertHTML', renderMarkdown(fencedCodeMd(selected || placeholder, '')).trim())
  // 回插后的落点在两个内核并不一致（Chromium 落在 <code> 内、WebKit 落在前一个段落末尾），
  // 不能用「光标前一个兄弟」推断插进来的是谁，按插入前后的差集认领新代码块最稳。
  const pre = Array.from(root.querySelectorAll('pre')).find((p) => !before.has(p))
  const code = pre?.querySelector('code')
  if (!code) return false
  const r = document.createRange()
  r.selectNodeContents(code)
  // 有选中文字：光标落到代码末尾继续敲；占位：整段保持选中，直接输入即替换
  if (selected) r.collapse(false)
  s.removeAllRanges()
  s.addRange(r)
  return true
}

/** 就地编辑下执行一次格式化；返回是否真的下发了命令，调用方据此决定要不要回写源码。 */
export function applyPreviewFormat(root: HTMLElement, fmt: PreviewFmt): boolean {
  markTaskCaretActivity()
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
      return applyCodeBlock(root, fmt.placeholder)
    case 'insertMd':
      return applyInsertMd(root, fmt.md)
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
  markTaskCaretActivity()
  if (!(li.textContent ?? '').trim()) return false
  e.preventDefault()
  runCmd('insertParagraph')
  runCmd('insertHTML', '<input type="checkbox" contenteditable="false" class="task-cb"> ')
  // 新建出来的项常常只拿到「元素级」插入点：WebKit 会把它画在复选框左边、低于文字行。
  // 把光标收进新项的文字里（优先下一项——回车拆出来的就是它）。
  const nx = li.nextElementSibling as HTMLElement | null
  let holder: HTMLElement | null = nx && nx.tagName === 'LI' ? nx : null
  if (!holder) {
    const focus = document.getSelection()?.focusNode ?? null
    const node = focus
      ? focus.nodeType === Node.ELEMENT_NODE
        ? (focus as HTMLElement)
        : focus.parentElement
      : null
    holder = node?.closest('li') ?? null
  }
  if (holder) fixElementLevelCaret(holder)
  return true
}

/** 光标是否落在代码块里。`anchorNode` 可能是文本节点（读 parentElement），
 *  也可能直接就是 `<pre>` / `<code>` 本身（读它自己）——只认前者会漏掉后者。 */
export function inCodeBlock(root: HTMLElement): boolean {
  const s = pvSelection()
  const n = s ? (s.focusNode ?? s.anchorNode) : null
  if (!n || !root.contains(n)) return false
  const el = n.nodeType === Node.TEXT_NODE ? n.parentElement : (n as Element)
  return !!el && root.contains(el) && !!el.closest('pre')
}

/** 代码块里按回车 = 软换行，返回是否消费了这次按键（preventDefault 在这里做）。
 *  不能用 `insertParagraph`，也不能用 `insertText('\n')`：WebKit 对这个 `<pre>` 里的两种写法
 *  都会把块**断成两个 `<pre>`**，也就是凭空多出一个代码块；`insertLineBreak` 只落换行、块不动。 */
export function enterInCodeBlock(e: KeyboardEvent, root: HTMLElement): boolean {
  if (!root.isContentEditable || !inCodeBlock(root)) return false
  e.preventDefault()
  // 命令返回 false 说明这个内核没实现它：退回纯文本换行，至少不会把这次回车吞掉
  if (!runCmd('insertLineBreak')) runCmd('insertText', '\n')
  return true
}

/** 代码块里的一处行首位置（DOM 位置）。 */
type DomPos = { node: Node; offset: number }

/** 代码块里的行首位置，按文档顺序：块首、文本里的 `\n` 之后、`<br>` 之后。
 *  只认这三种：渲染层把源码里的换行写成文本里的 `\n`，就地编辑敲出来的换行则是 `<br>`。 */
function codeLineStarts(host: HTMLElement): DomPos[] {
  const out: DomPos[] = [{ node: host, offset: 0 }]
  const walk = (el: Node): void => {
    let i = 0
    for (const c of Array.from(el.childNodes)) {
      if (c.nodeType === Node.TEXT_NODE) {
        const v = c.nodeValue ?? ''
        for (let k = 0; k < v.length; k++) if (v[k] === '\n') out.push({ node: c, offset: k + 1 })
      } else if (c.nodeType === Node.ELEMENT_NODE) {
        if ((c as Element).tagName === 'BR') out.push({ node: el, offset: i + 1 })
        else walk(c)
      }
      i++
    }
  }
  walk(host)
  return out
}

/** 行首位置相对某个文档位置的前后：-1 前 / 0 同一处 / 1 后。 */
function cmpPos(pos: DomPos, node: Node, offset: number): number {
  const q = document.createRange()
  q.setStart(node, offset)
  q.collapse(true)
  return q.comparePoint(pos.node, pos.offset)
}

/** 文档顺序上下一个节点（越过父节点往上找，到 `stop` 为止）。 */
function followingNode(n: Node, stop: Node): Node | null {
  let cur: Node | null = n
  while (cur && cur !== stop) {
    if (cur.nextSibling) return cur.nextSibling
    cur = cur.parentNode
  }
  return null
}

/** 元素里的第一段文本；先碰到 `<br>` 说明这一行是空的。 */
function firstText(el: Element): Text | null {
  for (const c of Array.from(el.childNodes)) {
    if (c.nodeType === Node.TEXT_NODE) return c as Text
    if (c.nodeType === Node.ELEMENT_NODE) {
      if ((c as Element).tagName === 'BR') return null
      const t = firstText(c as Element)
      if (t) return t
    }
  }
  return null
}

/** 某个节点的开头能看到的文本位置；`<br>` 返回 null。 */
function textAt(n: Node): { node: Text; offset: number } | null {
  if (n.nodeType === Node.TEXT_NODE) return { node: n as Text, offset: 0 }
  if (n.nodeType !== Node.ELEMENT_NODE || (n as Element).tagName === 'BR') return null
  const t = firstText(n as Element)
  return t ? { node: t, offset: 0 } : null
}

/** 行首位置往后、本行第一段文本的起点。文本里的 `\n` 之后可能正好停在节点末尾，
 *  这时要看同行的下一个节点；元素位置（`<code>` 开头、`<br>` 之后）则看下一个子节点。 */
function lineTextAt(code: HTMLElement, pos: DomPos): { node: Text; offset: number } | null {
  if (pos.node.nodeType === Node.TEXT_NODE) {
    const t = pos.node as Text
    if (pos.offset < (t.nodeValue ?? '').length) return { node: t, offset: pos.offset }
    const next = followingNode(t, code)
    return next ? textAt(next) : null
  }
  const next = pos.node.childNodes[pos.offset] as Node | undefined
  return next ? textAt(next) : null
}

/** 行首最多 `n` 格缩进的范围（前导空格；制表符算一格）。没有缩进返回 null。 */
function leadingIndent(
  code: HTMLElement,
  pos: DomPos,
  n: number,
): { node: Text; start: number; end: number } | null {
  const t = lineTextAt(code, pos)
  if (!t) return null
  const v = t.node.nodeValue ?? ''
  if (v[t.offset] === '\t') return { node: t.node, start: t.offset, end: t.offset + 1 }
  let k = 0
  while (k < n && v[t.offset + k] === ' ') k++
  return k ? { node: t.node, start: t.offset, end: t.offset + k } : null
}

/** 代码块里按 Tab / Shift+Tab = 缩进 / 退一级，返回是否消费了这次按键（preventDefault 在这里做）。
 *  浏览器把 Tab 当成焦点切换，正文里又没有制表位可言，所以只在代码块里接管：按设置项
 *  `codeIndent` 插 2 / 4 个空格（源码里本来就有的制表符仍由 CSS 的 `tab-size` 决定宽度）。
 *  有选区时按行处理：直接在选区上 `insertText` 会把选中的代码整段替换成空格，
 *  必须先算出选区碰到的每个行首、逐个插入 / 删除。 */
export function tabInCodeBlock(e: KeyboardEvent, root: HTMLElement, width: number): boolean {
  // 带修饰键的 Tab（⌘/Ctrl/Alt+Tab）是系统或应用的快捷键，不能被缩进吃掉
  if (!root.isContentEditable || e.metaKey || e.ctrlKey || e.altKey || !inCodeBlock(root)) return false
  const s = pvSelection()
  if (!s) return false
  const pre = pvAncestor<HTMLElement>(root, 'pre')
  const code = (pre?.querySelector('code') as HTMLElement | null) ?? pre
  if (!code) return false
  const n = width === 2 ? 2 : 4
  const spaces = ' '.repeat(n)
  const r = s.getRangeAt(0)
  const starts = codeLineStarts(code)
  // 只有光标：行首位置就是光标所在行的行首
  if (s.isCollapsed) {
    e.preventDefault()
    // Tab：在光标处插空格，插入后光标自然停在缩进之后
    if (!e.shiftKey) {
      runCmd('insertText', spaces)
      return true
    }
    // Shift+Tab：删掉这一行行首最多一格缩进，光标跟着活 Range 一起左移
    let idx = 0
    for (let i = 0; i < starts.length; i++) if (cmpPos(starts[i], r.startContainer, r.startOffset) <= 0) idx = i
    const lead = leadingIndent(code, starts[idx], n)
    if (!lead) return true
    const keep = r.cloneRange()
    const q = document.createRange()
    q.setStart(lead.node, lead.start)
    q.setEnd(lead.node, lead.end)
    s.removeAllRanges()
    s.addRange(q)
    runCmd('delete')
    s.removeAllRanges()
    s.addRange(keep)
    return true
  }
  // 跨出代码块的选区不接：缩进半个文档不是这里该干的事，交回浏览器
  if (!code.contains(r.startContainer) || !code.contains(r.endContainer)) return false
  e.preventDefault()
  // 选区碰到哪几行：起点所在行到终点所在行；终点正好停在下一行行首时，那一行不算被选中
  let i0 = 0
  for (let i = 0; i < starts.length; i++) if (cmpPos(starts[i], r.startContainer, r.startOffset) <= 0) i0 = i
  let i1 = i0
  for (let i = i0; i < starts.length; i++) if (cmpPos(starts[i], r.endContainer, r.endOffset) <= 0) i1 = i
  if (i1 > i0 && cmpPos(starts[i1], r.endContainer, r.endOffset) === 0) i1--
  const keep = r.cloneRange()
  // 从后往前改：先动的行都在后面，不会让还没处理的行首位置失效
  for (let i = i1; i >= i0; i--) {
    const q = document.createRange()
    if (e.shiftKey) {
      const lead = leadingIndent(code, starts[i], n)
      if (!lead) continue
      q.setStart(lead.node, lead.start)
      q.setEnd(lead.node, lead.end)
    } else {
      q.setStart(starts[i].node, starts[i].offset)
      q.collapse(true)
    }
    s.removeAllRanges()
    s.addRange(q)
    if (e.shiftKey) runCmd('delete')
    else runCmd('insertText', spaces)
  }
  // 改完把选区放回原处：keep 是活 Range，前面增删的字符会自动把它推开
  s.removeAllRanges()
  s.addRange(keep)
  return true
}

/** 点在最后一个块的下方空白处时，把光标落到块后。
 *  代码块 / 表格 / 分隔线这类块没有「块后」的落笔位置：浏览器会把光标塞进块里
 *  （代码块直接塞到代码末尾），于是写不出块后的正文。命中时补一个空段落并把光标放进去，
 *  返回是否接管（调用方据此 preventDefault）。空段落在序列化时被 `blockMd` 丢掉，不进源码。 */
export function caretBelowLastBlock(root: HTMLElement, clientY: number): boolean {
  const last = root.lastElementChild as HTMLElement | null
  if (!last) return false
  const block = last.classList.contains('mb') ? (last.firstElementChild as HTMLElement | null) : last
  if (!block || !/^(PRE|TABLE|HR)$/.test(block.tagName)) return false
  if (clientY <= block.getBoundingClientRect().bottom) return false
  const p = ensureTrailingParagraph(root)
  root.focus({ preventScroll: true })
  const r = document.createRange()
  r.selectNodeContents(p)
  r.collapse(true)
  const s = document.getSelection()
  if (s) {
    s.removeAllRanges()
    s.addRange(r)
  }
  return true
}

/** 末尾的空段落：已经有了就复用（连点空白不该堆出一串 `<p><br></p>`）。 */
function ensureTrailingParagraph(root: HTMLElement): HTMLElement {
  const last = root.lastElementChild as HTMLElement | null
  const tail = last && last.classList.contains('mb') ? (last.firstElementChild as HTMLElement | null) : last
  if (tail && tail.tagName === 'P' && !(tail.textContent ?? '').trim()) return tail
  const p = document.createElement('p')
  p.appendChild(document.createElement('br'))
  root.appendChild(p)
  return p
}

/** 浏览器在该点算出的插入位置（WebKit / Chromium 的 caretRangeFromPoint，标准名 caretPositionFromPoint 兜底）。 */
function probeCaretAt(x: number, y: number): Range | null {
  const d = document as Document & {
    caretRangeFromPoint?: (x: number, y: number) => Range | null
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null
  }
  const r = d.caretRangeFromPoint?.(x, y)
  if (r) return r
  const p = d.caretPositionFromPoint?.(x, y)
  if (!p) return null
  const out = document.createRange()
  try {
    out.setStart(p.offsetNode, p.offset)
  } catch {
    return null
  }
  out.collapse(true)
  return out
}

/** 最近一次「产生落点」的动作（点击 / 回车 / 工具条命令）发生的时间。
 *  任务项项首有一批坏落点（复选框邻域），点击 / 回车后会短暂停留；方向键来回走也会经过
 *  同样的位置，所以只在动作后一小段时间里纠正，避免把方向键锁死在项首（见 `normalizeTaskCaret`）；
 *  按键（keydown）到来时清除，用户一按方向键就回到原生行为。 */
let taskCaretActivityAt = 0
export function markTaskCaretActivity(): void {
  taskCaretActivityAt = Date.now()
}
export function clearTaskCaretActivity(): void {
  taskCaretActivityAt = 0
}

/** 任务项的复选框是 contenteditable=false 的行内控件：光标停在它邻域时（紧邻复选框的文本
 *  节点 0 位、复选框与文本之间的元素级位置）WebKit 按上游亲和把光标画到复选框左边、高度取整行
 *  行盒——看着像光标跑到复选框外面去了。点击 / 回车后把这类落点收进项内文字的正确位置
 *  （空项落到复选框后的 nbsp 之后，见 `placeCaretInText`）。幂等，适合挂在 selectionchange
 *  上兜住点击 / 回车 / 工具条命令；只在动作后的短时间窗里动手（见 `markTaskCaretActivity`），
 *  方向键路过项首也经过同样的位置，一直拉回会把人锁死在项首。返回是否做了纠正。 */
export function normalizeTaskCaret(root: HTMLElement): boolean {
  const s = document.getSelection()
  if (!s || !s.rangeCount || !s.isCollapsed) return false
  if (Date.now() - taskCaretActivityAt > 600) return false
  const r = s.getRangeAt(0)
  if (!root.contains(r.startContainer)) return false
  const isCb = (n: Node | null): n is HTMLElement =>
    !!n && n.nodeType === Node.ELEMENT_NODE && (n as HTMLElement).matches('input.task-cb')
  const sc = r.startContainer
  let li: HTMLElement | null = null
  let toEnd = true
  if (sc.nodeType === Node.TEXT_NODE) {
    // 只有紧跟在复选框后面的文本节点才有邻域问题
    if (!isCb(sc.previousSibling)) return false
    const v = sc.nodeValue ?? ''
    const lead = (/^[ \u00a0]+/.exec(v) ?? [''])[0].length
    // 非空文本里落在前导空格之后就是好位置（画在复选框右边），不用动
    if (/\S/.test(v) && r.startOffset >= lead) return false
    li = sc.parentElement
    toEnd = false
  } else if (sc.nodeType === Node.ELEMENT_NODE) {
    // 元素级落点：只有「复选框是第一个子节点」的列表项才属于坏位置
    const el = sc as HTMLElement
    if (el.tagName !== 'LI' || !isCb(el.firstChild)) return false
    li = el
    // 复选框邻域（0/1）按项首收；其后的按项尾收
    toEnd = r.startOffset > 1
  }
  if (!li || !root.contains(li)) return false
  placeCaretInText(li, toEnd)
  return true
}

/** 光标当前只有「元素级」位置时，把它收进该元素的文字里；已经在文本节点里就什么也不做
 *  （浏览器自己放对了，别把拆分后的位置推回项首 / 项尾）。 */
function fixElementLevelCaret(el: HTMLElement): void {
  const s = document.getSelection()
  const r = s?.rangeCount ? s.getRangeAt(0) : null
  if (!r || r.startContainer.nodeType === Node.TEXT_NODE) return
  // 落在元素开头（复选框之前）→ 项首；否则视为项尾
  placeCaretInText(el, !(r.startContainer === el && r.startOffset === 0))
}

/** 把折叠光标放进元素首 / 末个有字的文本节点。
 *  直接 `selectNodeContents` 再折叠是「元素级插入点」，WebKit 会把它画在盒子边缘：
 *  列表项上正好是复选框左边那一列、还低于文字行（段落没内缩，所以以前在 p / h 上看不出来）。
 *  任务项（复选框是第一个子节点）里唯一文本是纯空白时，落点换成复选框后的 nbsp——
 *  纯空格在行尾是悬挂空白会被折叠，光标右边没有真实行内盒，WebKit 照样画到复选框左边。 */
function placeCaretInText(el: HTMLElement, toEnd: boolean): boolean {
  const s = document.getSelection()
  if (!s) return false
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT)
  let first: Text | null = null
  let firstAny: Text | null = null
  let last: Text | null = null
  let lastAny: Text | null = null
  while (walker.nextNode()) {
    const t = walker.currentNode as Text
    const v = t.nodeValue ?? ''
    if (!v.length) continue
    if (!firstAny) firstAny = t
    lastAny = t
    if (!first && /\S/.test(v)) first = t
    if (/\S/.test(v)) last = t
  }
  const taskLi = el.tagName === 'LI' && !!el.firstElementChild?.matches?.('input.task-cb')
  const target = toEnd ? (last ?? lastAny) : (first ?? firstAny)
  if (!target) {
    // 列表项里可能一个文本节点都不剩（插入复选框后光标只拿到元素级位置）：
    // 补一个空格文本节点让它落在行内；序列化时 li 的文本会 trim 掉，不进源码。
    // 任务项补 nbsp：纯空格会被画到复选框左边（见 `normalizeTaskCaret`）
    if (el.tagName !== 'LI' || !el.isContentEditable) return false
    const sp = document.createTextNode(taskLi ? '\u00a0' : ' ')
    el.appendChild(sp)
    const r0 = document.createRange()
    r0.setStart(sp, taskLi ? 1 : toEnd ? 1 : 0)
    r0.collapse(true)
    s.removeAllRanges()
    s.addRange(r0)
    return true
  }
  const v = target.nodeValue ?? ''
  // 任务项的纯空白文本（典型是复选框后的单个空格）：整个节点换成 nbsp 再落点。
  // nbsp 不参与尾随空白折叠，光标停在它后面就有真实行内盒参照，画在复选框右边
  if (taskLi && !/\S/.test(v)) {
    if (v !== '\u00a0') target.nodeValue = '\u00a0'
    const r = document.createRange()
    r.setStart(target, 1)
    r.collapse(true)
    s.removeAllRanges()
    s.addRange(r)
    return true
  }
  const r = document.createRange()
  if (toEnd) r.setStart(target, /\S/.test(v) ? v.search(/\s*$/) : v.length)
  else r.setStart(target, v.length - v.replace(/^\s+/, '').length)
  r.collapse(true)
  s.removeAllRanges()
  s.addRange(r)
  return true
}

/** 把折叠光标放进某个元素内部：优先落在文本节点里（见 `placeCaretInText`），
 *  空元素再补 `<br>`。落点必须是真正的块（p / h1-6 / li / pre …），不能是 `.md` 容器本身。 */
function collapseInto(el: HTMLElement, toEnd: boolean): void {
  const s = document.getSelection()
  if (!s) return
  if (placeCaretInText(el, toEnd)) return
  if (!el.firstChild) el.appendChild(document.createElement('br'))
  const r = document.createRange()
  r.selectNodeContents(el)
  r.collapse(toEnd)
  s.removeAllRanges()
  s.addRange(r)
}

/** 正文第一个可落笔的块：拆掉 `.mb` 外壳，列表 / 表格取里层第一个块。 */
function firstCaretBlock(root: HTMLElement): HTMLElement | null {
  for (const el of Array.from(root.children)) {
    const inner = (el.classList.contains('mb') ? el.firstElementChild : el) as HTMLElement | null
    if (!inner) continue
    if (inner.matches(PV_BLOCK)) return inner
    const nested = inner.querySelector(PV_BLOCK) as HTMLElement | null
    if (nested) return nested
  }
  return null
}

/** 点在正文根（`.md` 容器本身，典型是顶部 / 底部那几段内边距）的空白处时，把光标落进真实块，
 *  返回是否接管（调用方据此 preventDefault）。
 *  这些空白点下去浏览器给的是「容器根」上的插入点：WebKit 会把它画到容器的边框盒左上角——
 *  浮层模式下正好落在工具栏那一条上（看着像光标跑进了工具栏），而且它不属于任何文本流，
 *  点正文后旧光标还留在那儿、也点不到。
 *  y 在首块之上→首块开头；末块之下→末块末尾（代码块 / 表格这类块交给 `caretBelowLastBlock`）；
 *  块与块之间→离得近的那一端；空文档→补一个空段落（序列化时被 `blockMd` 丢掉，不进源码）。 */
export function caretInBlankArea(root: HTMLElement, clientY?: number): boolean {
  const first = firstCaretBlock(root)
  if (!first) {
    const p = ensureTrailingParagraph(root)
    root.focus({ preventScroll: true })
    collapseInto(p, false)
    return true
  }
  const blocks = Array.from(root.querySelectorAll(PV_BLOCK)) as HTMLElement[]
  const last = blocks[blocks.length - 1] ?? first
  let target = last
  let toEnd = true
  if (clientY !== undefined) {
    const head = root.firstElementChild?.getBoundingClientRect()
    if (head && clientY <= head.top) {
      target = first
      toEnd = false
    } else if (clientY < last.getBoundingClientRect().bottom) {
      // 块与块之间的空白：挑离点最近的那一条边，避免把光标甩到整篇的开头或结尾
      let best = Infinity
      for (const b of blocks) {
        const box = b.getBoundingClientRect()
        const dTop = Math.abs(clientY - box.top)
        const dBottom = Math.abs(clientY - box.bottom)
        if (dTop < best) {
          best = dTop
          target = b
          toEnd = false
        }
        if (dBottom < best) {
          best = dBottom
          target = b
          toEnd = true
        }
      }
    }
  }
  root.focus({ preventScroll: true })
  collapseInto(target, toEnd)
  return true
}

/** 点在列表的缩进区（`.md ul` 的 padding 左那一带）或列表项里文字之外的空白时，
 *  浏览器给的是「元素级」插入点：WebKit 会把它画在列表内容盒左缘（复选框那一列外侧）、
 *  比文字低一截，看着像光标跑到复选框外面去了。把这类落点收进最近列表项的文本里，返回是否接管。 */
export function caretInListGap(root: HTMLElement, x: number, y: number): boolean {
  const probe = probeCaretAt(x, y)
  if (!probe) return false
  // 落在文字上：浏览器自己放得对，不接管
  if (probe.startContainer.nodeType === Node.TEXT_NODE) return false
  const node = probe.startContainer
  const el = (node.nodeType === Node.ELEMENT_NODE ? (node as HTMLElement) : node.parentElement) as HTMLElement | null
  if (!el || !root.contains(el)) return false
  const li = el.closest('li') as HTMLElement | null
  const list = el.closest('ul,ol') as HTMLElement | null
  if (!li && !list) return false
  // 落在 li 上就直接用它；落在列表容器上（缩进区 / 末项下方）按纵坐标挑最近的一项
  let target: HTMLElement | null = li
  if (!target && list) {
    let best = Infinity
    for (const child of Array.from(list.children)) {
      if (child.tagName !== 'LI') continue
      const b = child.getBoundingClientRect()
      const d = y < b.top ? b.top - y : y > b.bottom ? y - b.bottom : 0
      if (d < best) {
        best = d
        target = child as HTMLElement
      }
    }
  }
  if (!target) return false
  root.focus({ preventScroll: true })
  const box = target.getBoundingClientRect()
  return placeCaretInText(target, y > box.top + box.height / 2)
}
