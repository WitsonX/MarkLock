//! Markdown 预览的共享渲染层：marked 全局渲染配置 + 代码高亮 + 复制按钮 + 图片 asset 解析 + TODO 回改。
//!
//! 主编辑器（EditorView）与便携便签（StickyView）共用本模块，保证两处的预览观感与
//! 交互语义（代码复制、任务清单可点、本地图片加载）完全一致。EditorView 另有一套
//! 「逐顶层块渲染并标 data-l」的滚动同步逻辑，属其专属，不在本模块内。

import { marked } from 'marked'
import hljs from 'highlight.js'
import { convertFileSrc } from '@tauri-apps/api/core'
import { t as translate } from '../locales'

/** 代码块高亮失败时的兜底转义 */
function escapeCode(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

/** marked v5+ 移除了 highlight 选项，改用自定义 renderer 接 highlight.js */
function highlightCode(text: string, lang: string): string {
  try {
    if (lang && hljs.getLanguage(lang)) return hljs.highlight(text, { language: lang }).value
    return hljs.highlightAuto(text).value
  } catch {
    return escapeCode(text)
  }
}

/** 预览代码块右上角复制按钮：图标内联 SVG（双矩形「复制」+ 对勾「已复制」），
 *  文字/状态切换由 CSS 与 .copied 类驱动；绝对定位不参与 pre 文本流，textContent 复制不受影响。 */
const CODE_COPY_BTN = (txt: string = translate('common.copy'), aria: string = translate('editor.copyCodeAria')) => `<button type="button" class="code-copy-btn" aria-label="${aria}" title="${txt}"><svg class="cc-ico cc-copy" viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 3.5v-.5a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h.5"/></svg><svg class="cc-ico cc-check" viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5 6.5 12 13 4.5"/></svg><span class="cc-txt">${txt}</span></button>`

/** 行内代码复制按钮：复用 .code-copy-btn 状态类（.cc-txt 由 CSS 隐藏只留图标），
 *  与 code 同为 .codespan-wrap 的兄弟节点，选中 code 文本时不会带入按钮文字。 */
const INLINE_COPY_BTN = () => CODE_COPY_BTN(translate('common.copy'), translate('editor.copyInlineAria'))

/** 模块级：当前预览文件所在目录（解析正文里相对图片路径的基准）。
 *  渲染前由调用方经 setAssetBaseDir 写入，image 渲染器读取它拼绝对路径再转 asset URL。 */
let __assetBaseDir = ''

/** 设置相对图片解析的基准目录（当前文件所在目录）；空串表示无基准（如未保存草稿）。 */
export function setAssetBaseDir(dir: string) {
  __assetBaseDir = dir
}

/** 读取当前基准目录（导出 HTML 等需要临时切换并还原基准的场景）。 */
export function getAssetBaseDir(): string {
  return __assetBaseDir
}

/** HTML 属性值转义（image 渲染器自管 alt/title，marked 默认转义被我们接管后需自行处理）。 */
function escAttr(s: string): string {
  return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

/** 以 '/' 连接目录与相对片段（预览图片链接统一用正斜杠，asset 协议按平台解析）。 */
function joinFs(dir: string, rel: string): string {
  const win = dir.includes('\\') && !dir.includes('/')
  const sep = win ? '\\' : '/'
  const base = dir.replace(win ? /[\\/]+$/ : /\/+$/, '')
  return (base ? base + sep : sep) + rel.replace(/^[\\/]+/, '')
}

/** 把 markdown 图片 src 解析成可加载的 URL：
 *  外部/内联/协议地址原样返回；相对路径基于当前文件目录拼成绝对路径后走 asset 协议；
 *  绝对本地路径直接转 asset 协议。拿不到基准（如未保存草稿）时原样返回。 */
function resolveImgSrc(href: string): string {
  if (!href) return href
  const lower = href.toLowerCase()
  if (/^(https?:|data:|file:|asset:|blob:)/.test(lower)) return href
  const decoded = (() => {
    try {
      return decodeURI(href)
    } catch {
      return href
    }
  })()
  const isAbs = decoded.startsWith('/') || /^[a-zA-Z]:[\\/]/.test(decoded)
  const abs = isAbs || !__assetBaseDir ? decoded : joinFs(__assetBaseDir, decoded)
  try {
    return convertFileSrc(abs)
  } catch {
    return href
  }
}

marked.use({
  gfm: true,
  breaks: true,
  renderer: {
    code({ text, lang }: { text: string; lang?: string }) {
      const name = (lang || '').trim().split(/\s+/)[0]
      // 围栏信息串要进 class 属性，限定为语言名常见字符，避免引号/尖括号提前闭合属性破坏标签
      const safe = /^[\w+.-]+$/.test(name) ? name : ''
      return `<pre>${CODE_COPY_BTN()}<code class="hljs${safe ? ` language-${safe}` : ''}">${highlightCode(text, name)}</code></pre>\n`
    },
    // 行内代码：code 后跟复制按钮，外包 inline 容器供 CSS 悬停显现与事件定位；
    // 必须自己转义：marked 只在「默认 renderer」里对 codespan 做 escapeHtml，token.text 是原始文本，
    // 覆写 renderer 后不转义会让 `<script>` 这类内容输出成真标签，无闭合时浏览器把后续整篇吞进脚本。
    // 导出 HTML 时按钮会被现有正则一并剔除。
    codespan({ text }: { text: string }) {
      return `<span class="codespan-wrap"><code>${escapeCode(text)}</code>${INLINE_COPY_BTN()}</span>`
    },
    // 本地图片：相对/绝对磁盘路径经 asset 协议在应用内加载（http/data 原样交给 WKWebView）
    image({ href, title, text }: { href: string; title?: string | null; text: string }) {
      const t = title ? ` title="${escAttr(title)}"` : ''
      return `<img src="${escAttr(resolveImgSrc(href))}" alt="${escAttr(text || '')}"${t}>`
    },
    // 任务清单复选框：去掉 marked 默认的 disabled，预览里可点击回改源码标记（见 toggleTaskMarker）；
    // 勾选态完全由源码渲染驱动，导出 HTML 时再补回 disabled（离线页面无回写能力）。
    checkbox({ checked }: { checked: boolean }) {
      return `<input type="checkbox" class="task-cb"${checked ? ' checked=""' : ''}>`
    },
  },
})

/** 整篇渲染（不分块、不标 data-l）：便签等无需行级滚动同步的场景直接用。 */
export function renderMarkdown(src: string): string {
  if (!src) return ''
  return marked.parse(src) as string
}

/** 翻转源码中第 idx 个（0 起、按源码顺序）任务清单标记：`[ ]`→`[x]`、`[x]`/`[X]`→`[ ]`；找不到返回 null。 */
export function toggleTaskMarker(src: string, idx: number): string | null {
  const lines = src.split('\n')
  let n = 0
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^(\s*(?:[-*+]|\d{1,9}[.)])\s+\[)([ xX])(\].*)$/)
    if (!m) continue
    if (n === idx) {
      lines[i] = m[1] + (m[2] === ' ' ? 'x' : ' ') + m[3]
      return lines.join('\n')
    }
    n++
  }
  return null
}
