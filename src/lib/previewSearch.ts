/**
 * 预览内搜索（预览模式 ⌘F）：直接在渲染出的 DOM 里查找并高亮命中，不再切回分屏借道 CodeMirror。
 *
 * 做法：把 `.md` 里的正文文本按文档序拼成一条 haystack（块级边界垫 `\n`，命中不能跨块），
 * 记录每个文本节点在 haystack 中的区间；正则命中后按区间反查回文本节点，把命中片段包成
 * `<mark class="pv-search">`（当前项再加 `pv-search-on`）。一个命中被行内语法拆到多个文本节点时
 * 会包出多个同组 mark，导航时整组一起点亮。
 *
 * 高亮是临时 chrome，不属于正文：wysiwyg.ts 的序列化对 MARK 透明穿过、原始 HTML 整块保留前
 * 会剥掉 pv-search / pv-hit（见 rawHtml），所以就地编辑回写与导出都不会把高亮标签写进源码。
 * v-html 重新注入（切页签 / 外部重载）时高亮随旧 DOM 一起消失，调用方需在新 DOM 上重跑。
 */

export interface PreviewSearchOptions {
  caseSensitive: boolean
  regex: boolean
}

export interface PreviewSearchResult {
  /** 每个命中的 mark 组（跨文本节点的命中含多个 mark），按文档序 */
  groups: HTMLElement[][]
  /** 起始当前项：视口顶以下第一个命中，没有则回绕到 0 */
  index: number
  /** 正则模式下表达式非法 */
  badRegex: boolean
}

/** 这些子树里没有可搜索的正文：应用注入的控件（复制按钮）、表单控件、脚本样式与嵌入式媒体。
 *  SVG 的 <text> 也不搜：mark 不是合法 SVG 子元素，包进去会破坏图形渲染。 */
const SKIP_TAGS = new Set([
  'BUTTON', 'INPUT', 'TEXTAREA', 'SELECT', 'STYLE', 'SCRIPT', 'SVG',
  'IFRAME', 'CANVAS', 'VIDEO', 'AUDIO', 'OBJECT', 'EMBED',
])

/** 块级边界：前后各垫一个不映射到任何文本节点的 `\n`，跨块的「命中」因此无法完整映射而被丢弃，
 *  避免上一段段尾 + 下一段段首拼出假命中。行内标签（strong/em/a/code…）不垫，跨行内语法的命中照常找到。 */
const BLOCK_TAGS = new Set([
  'P', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'UL', 'OL', 'LI', 'BLOCKQUOTE', 'PRE',
  'TABLE', 'THEAD', 'TBODY', 'TR', 'TD', 'TH', 'DIV', 'SECTION', 'ARTICLE', 'ASIDE',
  'HEADER', 'FOOTER', 'NAV', 'MAIN', 'FIGURE', 'FIGCAPTION', 'DETAILS', 'SUMMARY',
  'DL', 'DT', 'DD', 'HR', 'BR', 'FORM', 'CENTER',
])

interface Segment {
  node: Text
  from: number
  to: number
}

/** 收集正文文本节点及其在 haystack 中的区间。 */
function collectText(root: HTMLElement): { hay: string; segs: Segment[] } {
  let hay = ''
  const segs: Segment[] = []
  const walk = (n: Node) => {
    if (n.nodeType === Node.TEXT_NODE) {
      const v = (n as Text).nodeValue ?? ''
      if (v) {
        segs.push({ node: n as Text, from: hay.length, to: hay.length + v.length })
        hay += v
      }
      return
    }
    if (n.nodeType !== Node.ELEMENT_NODE) return
    const el = n as HTMLElement
    if (SKIP_TAGS.has(el.tagName)) return
    const block = BLOCK_TAGS.has(el.tagName)
    if (block) hay += '\n'
    for (const c of Array.from(el.childNodes)) walk(c)
    if (block) hay += '\n'
  }
  walk(root)
  return { hay, segs }
}

const escapeRegExp = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

/** 还原上一次的高亮：mark 换回纯文本节点再 normalize 合并碎片，
 *  保证下一次搜索仍能在完整的文本节点里匹配（与 _markPreviewHit 同口径）。 */
export function clearPreviewSearch(root: HTMLElement | null | undefined): void {
  if (!root) return
  const marks = Array.from(root.querySelectorAll('mark.pv-search'))
  if (!marks.length) return
  for (const m of marks) {
    const parent = m.parentNode
    if (parent) parent.replaceChild(document.createTextNode(m.textContent ?? ''), m)
    else m.remove()
  }
  root.normalize()
}

/** 点亮第 index 组命中并滚入视口：纵向对齐到视口上三分之一（与全局搜索命中定位同口径），
 *  命中落在横向可滚的代码块里时再把代码块横向滚到命中处。 */
export function setActivePreviewSearch(
  root: HTMLElement,
  pane: HTMLElement,
  groups: HTMLElement[][],
  index: number,
): void {
  for (const m of Array.from(root.querySelectorAll('mark.pv-search-on'))) m.classList.remove('pv-search-on')
  const group = groups[index]
  if (!group || !group.length) return
  for (const m of group) m.classList.add('pv-search-on')
  const first = group[0]
  const mr = first.getBoundingClientRect()
  const pr = pane.getBoundingClientRect()
  const max = Math.max(0, pane.scrollHeight - pane.clientHeight)
  const top = mr.top - pr.top + pane.scrollTop
  pane.scrollTop = Math.max(0, Math.min(top - pane.clientHeight * 0.33, max))
  const pre = first.closest('pre')
  if (pre && pre.scrollWidth > pre.clientWidth) {
    const pmax = pre.scrollWidth - pre.clientWidth
    const left = mr.left - pre.getBoundingClientRect().left + pre.scrollLeft
    pre.scrollLeft = Math.max(0, Math.min(left - pre.clientWidth * 0.3, pmax))
  }
}

/** 在预览 DOM 里跑一次搜索：先还原旧高亮，再包裹全部命中并定位起始项。
 *  大小写不敏感是默认（与 CodeMirror 搜索面板一致）；字面量模式转义后仍走正则，
 *  避免 toLowerCase 在个别 Unicode 字符上改变串长导致偏移错位。 */
export function runPreviewSearch(
  root: HTMLElement,
  pane: HTMLElement,
  query: string,
  opts: PreviewSearchOptions,
): PreviewSearchResult {
  clearPreviewSearch(root)
  const empty: PreviewSearchResult = { groups: [], index: 0, badRegex: false }
  if (!query) return empty

  let re: RegExp
  try {
    const pattern = opts.regex ? query : escapeRegExp(query)
    // m 让 ^/$ 落在块边界上（haystack 在块级元素前后垫了不映射的 \n），与编辑器逐行搜索的语义对齐；
    // 字面量模式不受影响 —— escapeRegExp 把 ^ $ 也转义了。
    re = new RegExp(pattern, opts.caseSensitive ? 'gm' : 'gim')
  } catch {
    return { groups: [], index: 0, badRegex: true }
  }

  const { hay, segs } = collectText(root)
  const ranges: Array<[number, number]> = []
  for (let m = re.exec(hay); m; m = re.exec(hay)) {
    if (m[0].length === 0) {
      // 零宽命中（如 a*）：手动前进避免死循环，也不值得高亮
      re.lastIndex++
      continue
    }
    ranges.push([m.index, m.index + m[0].length])
  }
  if (!ranges.length) return empty

  // 命中区间 → 各文本节点上的片段；跨块（垫了不映射的 \n）的命中无法完整映射，整条丢弃
  const groups: HTMLElement[][] = []
  const byNode = new Map<Text, Array<{ start: number; end: number; gi: number }>>()
  let si = 0
  for (const [s, e] of ranges) {
    while (si < segs.length && segs[si].to <= s) si++
    const parts: Array<{ seg: Segment; start: number; end: number }> = []
    let pos = s
    let j = si
    while (j < segs.length && pos < e && segs[j].from <= pos) {
      const seg = segs[j]
      const end = Math.min(e, seg.to)
      parts.push({ seg, start: pos - seg.from, end: end - seg.from })
      pos = end
      j++
    }
    if (pos !== e) continue
    const gi = groups.length
    groups.push([])
    for (const p of parts) {
      let list = byNode.get(p.seg.node)
      if (!list) {
        list = []
        byNode.set(p.seg.node, list)
      }
      list.push({ start: p.start, end: p.end, gi })
    }
  }
  if (!groups.length) return empty

  // 逐节点重建：一次 replaceChild 换入「文本 / mark 交替」的碎片，
  // 同一节点上的多个命中互不干扰（先算好全部偏移再动 DOM）
  for (const [node, parts] of byNode) {
    const v = node.nodeValue ?? ''
    const frag = document.createDocumentFragment()
    let pos = 0
    for (const p of parts) {
      if (p.start > pos) frag.appendChild(document.createTextNode(v.slice(pos, p.start)))
      const mark = document.createElement('mark')
      mark.className = 'pv-search'
      mark.textContent = v.slice(p.start, p.end)
      frag.appendChild(mark)
      groups[p.gi].push(mark)
      pos = p.end
    }
    if (pos < v.length) frag.appendChild(document.createTextNode(v.slice(pos)))
    node.parentNode?.replaceChild(frag, node)
  }

  // 起始项取视口顶以下的第一个命中（浏览器查找同款行为）；全都滚过了就回绕到第一个
  let index = 0
  const paneTop = pane.getBoundingClientRect().top
  for (let i = 0; i < groups.length; i++) {
    const r = groups[i][0]?.getBoundingClientRect()
    if (r && r.top >= paneTop - 2) {
      index = i
      break
    }
  }
  setActivePreviewSearch(root, pane, groups, index)
  return { groups, index, badRegex: false }
}