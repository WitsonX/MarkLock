/**
 * 便携便签「快速启动」应用图标的内存缓存。
 *
 * 图标由 Rust 侧（macOS NSWorkspace）读成 PNG data URL，按 path 去重后异步填充。
 * 只驻内存、不写 localStorage：单个图标 data URL 可达几十~几百 KB，持久化会让设置存档急剧膨胀。
 * 空串表示「已取过但没有图标」（非 macOS、或读取失败），据此避免重复请求。
 */
import { reactive } from 'vue'
import * as tauri from './tauri'

/** path → 图标 data URL（'' 代表无图标）。组件可直接读取本对象做渲染，reactive 保证更新可见。 */
export const appIcons = reactive<Record<string, string>>({})
const pending = new Set<string>()

/** 确保某路径的图标已被取（幂等：已缓存 / 进行中则跳过）。 */
export function ensureAppIcon(path: string) {
  if (path in appIcons || pending.has(path)) return
  pending.add(path)
  tauri
    .appIcon(path)
    .then((url) => {
      appIcons[path] = url || ''
    })
    .catch(() => {
      appIcons[path] = ''
    })
    .finally(() => {
      pending.delete(path)
    })
}

/** 批量确保一组路径的图标都已取。 */
export function ensureAppIcons(paths: string[]) {
  for (const p of paths) ensureAppIcon(p)
}
