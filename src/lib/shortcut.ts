//! 快捷键文案的平台改写：词典与模板统一用 macOS 记法（⌘⇧⌥⌃ + 字母），
//! 渲染到 Windows / Linux 时换成 Ctrl+Shift+… 的写法（Windows 没有 Cmd 键）。
//! 只用于「纯快捷键串」（tooltip、菜单右侧 kbd 提示），
//! 不要拿去改写正文说明——设置页里讲两平台差异的段落本就要保留 ⌘ 字样。

import { isMac } from './platform'

/** macOS 修饰符记号 → Windows/Linux 按键名（顺序无关，各符号逐字符替换） */
const MOD_MAP: Array<[RegExp, string]> = [
  [/⌘/g, 'Ctrl+'],
  [/⇧/g, 'Shift+'],
  [/⌥/g, 'Alt+'],
  [/⌃/g, 'Ctrl+'],
]

/**
 * 按平台改写快捷键串：'⌘⇧T' → 'Ctrl+Shift+T'，'⌘+ / ⌘-' → 'Ctrl++ / Ctrl+-'。
 * macOS 原样返回；非快捷键串（不含修饰符记号）也原样返回。
 */
export function scLabel(mac: string | undefined): string {
  if (!mac || isMac) return mac ?? ''
  return MOD_MAP.reduce((s, [re, to]) => s.replace(re, to), mac)
}