//! 皮肤（配色方案）注册表与自定义皮肤的样式注入。
//!
//! 分层：外观模式（`settings.theme` = light/dark/system）决定 `<html data-theme>` 的明暗基线，
//! 皮肤（`settings.skin`）决定 `<html data-skin>` 的配色。两者正交——每套皮肤都给出浅、深两份令牌。
//!
//! 内置皮肤的令牌写在 `src/styles/skins.css`；自定义皮肤由本文件按 `settings.customSkins`
//! 生成**同构选择器**（`html[data-skin="…"][data-theme='…']`）注入到
//! `<style id="marklock-custom-skins">`，因此二者覆盖优先级一致、可无缝切换。

/** 一个明暗模式下的令牌覆盖表（键含 `--` 前缀，值为任意合法 CSS 颜色；空值表示继承基线） */
export type TokenMap = Record<string, string>

/** 用户自定义皮肤 */
export interface CustomSkin {
  /** 内部 id（写入 settings.skin 与 data-skin），仅 [A-Za-z0-9_-] */
  id: string
  /** 显示名（用户可改） */
  name: string
  /** 浅色 / 深色各自的令牌覆盖 */
  tokens: { light: TokenMap; dark: TokenMap }
  /** 高级：原样注入的 CSS，可写任意选择器（含 .md 预览与 .cm-editor） */
  css: string
}

/** 内置皮肤（'default' 即 global.css 的基线，不在列表中单列令牌） */
export interface BuiltinSkin {
  id: string
  /** 名称词条键 */
  nameKey: string
  /** 色卡：底色 / 面板 / 主色，浅色与深色各一组 */
  swatch: { light: [string, string, string]; dark: [string, string, string] }
}

export const BUILTIN_SKINS: BuiltinSkin[] = [
  {
    id: 'default',
    nameKey: 'skin.builtin.default',
    swatch: { light: ['#f5f6f8', '#ffffff', '#1677ff'], dark: ['#141619', '#1d2025', '#1677ff'] },
  },
  {
    id: 'nord',
    nameKey: 'skin.builtin.nord',
    swatch: { light: ['#eceff4', '#ffffff', '#5e81ac'], dark: ['#2e3440', '#3b4252', '#88c0d0'] },
  },
  {
    id: 'nocturne',
    nameKey: 'skin.builtin.nocturne',
    swatch: { light: ['#dfe2e8', '#f3f5f9', '#3465d4'], dark: ['#1a1b26', '#16161e', '#7aa2f7'] },
  },
  {
    id: 'catppuccin',
    nameKey: 'skin.builtin.catppuccin',
    swatch: { light: ['#e6e9ef', '#eff1f5', '#8839ef'], dark: ['#181825', '#1e1e2e', '#cba6f7'] },
  },
  {
    id: 'sepia',
    nameKey: 'skin.builtin.sepia',
    swatch: { light: ['#f4ecdb', '#fdf9f0', '#a9662a'], dark: ['#211c15', '#2a241b', '#d99a4e'] },
  },
  {
    id: 'solarized',
    nameKey: 'skin.builtin.solarized',
    swatch: { light: ['#eee8d5', '#fdf6e3', '#268bd2'], dark: ['#002b36', '#073642', '#3d9ee0'] },
  },
  {
    id: 'gruvbox',
    nameKey: 'skin.builtin.gruvbox',
    swatch: { light: ['#f2e5bc', '#fbf1c7', '#076678'], dark: ['#282828', '#32302f', '#83a598'] },
  },
]

/** 色板可覆盖的令牌分组（自定义皮肤编辑器据此渲染；词条见 locales 的 skin.group / skin.token） */
export const SKIN_TOKEN_GROUPS: { id: string; tokens: string[] }[] = [
  { id: 'accent', tokens: ['--primary', '--primary-hover', '--primary-active', '--primary-bg'] },
  { id: 'surface', tokens: ['--bg', '--panel', '--sidebar-bg', '--editor-bg', '--hover'] },
  { id: 'text', tokens: ['--text', '--text-2', '--text-3', '--text-4'] },
  { id: 'border', tokens: ['--border', '--border-2', '--border-pop'] },
  { id: 'selection', tokens: ['--selection', '--selection-focus'] },
  {
    id: 'preview',
    tokens: [
      '--md-link', '--md-heading-border', '--md-code-bg', '--md-code-text', '--md-pre-bg',
      '--md-pre-border', '--md-quote-border', '--md-quote-bg', '--md-quote-text',
      '--md-table-border', '--md-th-bg', '--md-stripe', '--md-hr',
    ],
  },
  {
    id: 'code',
    tokens: [
      '--cm-heading', '--cm-strong', '--cm-em', '--cm-link', '--cm-url', '--cm-code',
      '--cm-quote', '--cm-marker', '--hl-comment', '--hl-keyword', '--hl-string', '--hl-number',
      '--hl-title', '--hl-attr', '--hl-built', '--hl-meta',
    ],
  },
]

const STYLE_ID = 'marklock-custom-skins'

/** 自定义皮肤 id 只保留安全字符，避免污染属性选择器 */
function safeId(id: string): string {
  return String(id || '').replace(/[^A-Za-z0-9_-]/g, '').slice(0, 64)
}

/** 令牌名必须是 `--xxx` 形式，否则丢弃（用户可手改导入的 JSON） */
function safeVar(name: string): string {
  return /^--[A-Za-z0-9-]{1,64}$/.test(name) ? name : ''
}

/** 令牌值里剔掉能截断声明的字符；注入用 textContent，无需防 `</style>` 逃逸 */
function safeValue(value: string): string {
  return String(value).replace(/[;{}<>]/g, '').trim().slice(0, 200)
}

/** 生成全部自定义皮肤的 CSS（选择器与 skins.css 同构） */
export function buildCustomSkinCss(skins: CustomSkin[]): string {
  const blocks: string[] = []
  for (const s of skins || []) {
    const id = safeId(s.id)
    if (!id) continue
    for (const mode of ['light', 'dark'] as const) {
      const decls = Object.entries(s.tokens?.[mode] || {})
        .map(([k, v]) => [safeVar(k), safeValue(v)] as const)
        .filter(([k, v]) => k && v)
        .map(([k, v]) => `  ${k}: ${v};`)
      if (decls.length) {
        blocks.push(`html[data-skin="${id}"][data-theme='${mode}'] {\n${decls.join('\n')}\n}`)
      }
    }
    if (s.css && s.css.trim()) blocks.push(s.css.trim())
  }
  return blocks.join('\n\n')
}

/** 把自定义皮肤写入页面（幂等）：主窗与便签窗各自调用，共用同一份 localStorage 设置 */
export function applyCustomSkins(skins: CustomSkin[]): void {
  if (typeof document === 'undefined') return
  let el = document.getElementById(STYLE_ID) as HTMLStyleElement | null
  if (!el) {
    el = document.createElement('style')
    el.id = STYLE_ID
    document.head.appendChild(el)
  }
  el.textContent = buildCustomSkinCss(skins)
}

const PREVIEW_STYLE_ID = 'marklock-skin-preview'
/** 预览用的临时皮肤 id：与用户皮肤隔离，保存后由正式注入接管 */
const PREVIEW_SKIN_ID = '__preview__'

/** 临时预览一个尚未保存的皮肤：注入独立 <style> 并把 data-skin 指向预览 id。
 *  传 null 结束预览并恢复到 restoreTo（一般是保存后的正式 id 或取消前的原皮肤）。 */
export function previewCustomSkin(skin: CustomSkin | null, restoreTo = 'default'): void {
  if (typeof document === 'undefined') return
  let el = document.getElementById(PREVIEW_STYLE_ID) as HTMLStyleElement | null
  if (!skin) {
    if (el) el.remove()
    if (document.documentElement.dataset.skin === PREVIEW_SKIN_ID) {
      document.documentElement.dataset.skin = restoreTo
    }
    return
  }
  if (!el) {
    el = document.createElement('style')
    el.id = PREVIEW_STYLE_ID
    document.head.appendChild(el)
  }
  el.textContent = buildCustomSkinCss([{ ...skin, id: PREVIEW_SKIN_ID }])
  document.documentElement.dataset.skin = PREVIEW_SKIN_ID
}

/** 生成一个新的自定义皮肤 id（时间戳 + 随机后缀，仅安全字符） */
export function newSkinId(): string {
  return 'c' + Date.now().toString(36) + Math.floor(Math.random() * 1296).toString(36)
}

/** 新建一个空白自定义皮肤（默认继承当前基线，不带任何覆盖） */
export function newCustomSkin(name: string): CustomSkin {
  return {
    id: newSkinId(),
    name: name || 'My Skin',
    tokens: { light: {}, dark: {} },
    css: '',
  }
}

/** 正则转义，避免皮肤 id 里的特殊字符破坏匹配 */
function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/**
 * 读取某皮肤在指定档位「显式声明」的令牌覆盖（不含继承来的基线值）。
 * 直接解析已加载的样式表：内置皮肤来自 `skins.css`，选择器形如
 * `html[data-skin='nord'][data-theme='light']`（引号可能为单/双）。读不到时返回空表（安全兜底）。
 */
export function snapshotSkinTokens(id: string, mode: 'light' | 'dark'): TokenMap {
  const out: TokenMap = {}
  if (typeof document === 'undefined' || !id) return out
  const skinRe = new RegExp(`\\[data-skin=["']?${escapeRe(id)}["']?\\]`)
  const themeRe = new RegExp(`\\[data-theme=["']?${mode}["']?\\]`)
  for (const sheet of Array.from(document.styleSheets)) {
    let rules: CSSRuleList | null = null
    try {
      rules = (sheet as CSSStyleSheet).cssRules
    } catch {
      continue // 跨域样式表读取 cssRules 会抛 SecurityError，跳过
    }
    if (!rules) continue
    for (const rule of Array.from(rules)) {
      const sr = rule as CSSStyleRule
      if (!sr.selectorText || !sr.style) continue
      if (!skinRe.test(sr.selectorText) || !themeRe.test(sr.selectorText)) continue
      for (const prop of Array.from(sr.style)) {
        if (prop.startsWith('--')) {
          const v = sr.style.getPropertyValue(prop).trim()
          if (v) out[prop] = v
        }
      }
    }
  }
  return out
}

/**
 * 基于现有皮肤（内置或自定义）复制出一份新草稿（新 id），供「复制并在其基础上编辑」。
 * 自定义皮肤直接深拷数据模型里的令牌与高级 CSS；内置皮肤从样式表快照其显式覆盖。
 * 显示名 `name` 由调用方（组件）本地化后传入。
 */
export function forkSkin(id: string, name: string, customSkins: CustomSkin[]): CustomSkin {
  const src = (customSkins || []).find((s) => s.id === id)
  if (src) {
    return {
      id: newSkinId(),
      name,
      tokens: { light: { ...src.tokens.light }, dark: { ...src.tokens.dark } },
      css: src.css,
    }
  }
  return {
    id: newSkinId(),
    name,
    tokens: { light: snapshotSkinTokens(id, 'light'), dark: snapshotSkinTokens(id, 'dark') },
    css: '',
  }
}

/** 取某皮肤在指定明暗模式下的色卡三色（底色 / 面板 / 主色）；自定义皮肤读令牌，缺项按模式兜底 */
export function skinSwatch(
  id: string,
  mode: 'light' | 'dark',
  customSkins: CustomSkin[],
): [string, string, string] {
  const builtin = BUILTIN_SKINS.find((s) => s.id === id)
  if (builtin) return builtin.swatch[mode]
  const tokens = customSkins.find((s) => s.id === id)?.tokens?.[mode] || {}
  const fb = mode === 'dark'
    ? { bg: '#141619', panel: '#1d2025', primary: '#1677ff' }
    : { bg: '#f5f6f8', panel: '#ffffff', primary: '#1677ff' }
  return [tokens['--bg'] || fb.bg, tokens['--panel'] || fb.panel, tokens['--primary'] || fb.primary]
}

/** 读取当前生效的某个 CSS 变量值（导出 HTML 等需要具体色值的场景用） */
export function cssVar(name: string, fallback = ''): string {
  if (typeof document === 'undefined') return fallback
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
  return v || fallback
}
