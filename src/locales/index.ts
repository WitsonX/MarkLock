//! 轻量国际化（i18n）：中/英切换，零依赖。
//!
//! 设计要点：
//! - `lang` 为解析后的「实际语言」（'zh' | 'en'），用 Vue `ref` 承载；模板里 `$t()`
//!   在渲染期读取 `lang.value`，因此切换语言时所有用到 `$t` 的节点自动重渲染。
//! - `t(key, params?)` 以点路径查词典，缺失回退 zh，再回退键名本身；`{name}` 插值。
//! - 偏好（'system' | 'zh' | 'en'）由 vault store 的 settings.language 持久化，
//!   App.vue 负责把 'system' 解析成实际语言并调用 setLang（与主题跟随系统同一模式）。

import { ref, type Ref } from 'vue'
import zh from './zh'
import en from './en'

// 让模板里的 $t 与组件方法内的 this.$t 具备类型（main.ts 通过 globalProperties 注入实现）。
declare module 'vue' {
  interface ComponentCustomProperties {
    $t: (key: string, params?: Record<string, string | number>) => string
  }
}

export type ActualLang = 'zh' | 'en'
export type LangPref = 'system' | ActualLang

const dicts: Record<ActualLang, Record<string, unknown>> = { zh, en }

/** 解析后的当前界面语言（响应式）。默认中文，App.vue 会在启动时按偏好覆盖。 */
export const lang: Ref<ActualLang> = ref('zh')

/** 把用户偏好解析成实际语言：'system' 按浏览器/系统语言判定。 */
export function resolveLang(pref: LangPref): ActualLang {
  if (pref === 'zh' || pref === 'en') return pref
  const nav = (typeof navigator !== 'undefined' && navigator.language) || 'zh'
  return nav.toLowerCase().startsWith('zh') ? 'zh' : 'en'
}

/** 设置实际界面语言，并同步 <html lang>。 */
export function setLang(actual: ActualLang): void {
  lang.value = actual
  if (typeof document !== 'undefined') document.documentElement.lang = actual === 'zh' ? 'zh' : 'en'
}

/** 按点路径从对象取值；任一层非对象则返回 undefined。 */
function lookup(obj: Record<string, unknown>, path: string): string | undefined {
  let cur: unknown = obj
  for (const seg of path.split('.')) {
    if (cur && typeof cur === 'object') cur = (cur as Record<string, unknown>)[seg]
    else return undefined
  }
  return typeof cur === 'string' ? cur : undefined
}

/** 插值：把 {name} 替换为 params.name（缺失保留原样）。 */
function interpolate(str: string, params?: Record<string, string | number>): string {
  if (!params) return str
  return str.replace(/\{(\w+)\}/g, (_, k: string) =>
    Object.prototype.hasOwnProperty.call(params, k) ? String(params[k]) : `{${k}}`,
  )
}

/**
 * 翻译函数：按当前语言取词条。
 * @param key 点路径键（如 'unlock.title'）
 * @param params 插值参数（如 { count: 3 }）
 */
export function t(key: string, params?: Record<string, string | number>): string {
  const cur = lookup(dicts[lang.value], key) ?? lookup(dicts.zh, key)
  return interpolate(cur ?? key, params)
}

/** 在 <script setup> 之外（如 .ts 逻辑）也可直接 import { t } 使用。 */
export default t
