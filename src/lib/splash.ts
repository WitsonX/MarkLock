/**
 * 启动屏（splash）控制。
 *
 * splash 标记内联在 index.html 中，随 HTML 首帧立即绘制；本模块负责在前端
 * 就绪（Vue 挂载完成）后将其淡出移除，并保证最短展示时长，避免 bundle 加载
 * 很快时启动屏「一闪而过」造成的闪烁。
 */

/** HTML head 内联脚本记录的页面解析时刻（早于 bundle 加载，不受导入顺序影响）。 */
declare global {
  interface Window {
    __splashBootAt?: number
  }
}

const bootAt = window.__splashBootAt ?? performance.now()

/** 启动屏最短展示时长（毫秒）。 */
const MIN_DISPLAY_MS = 300

/** 淡出动画时长（与 index.html 中 #splash 的 transition 保持一致）。 */
const FADE_MS = 300

export function dismissSplash(): void {
  const el = document.getElementById('splash')
  if (!el) return
  const wait = Math.max(0, MIN_DISPLAY_MS - performance.now() + bootAt)
  window.setTimeout(() => {
    el.classList.add('sp-hide')
    window.setTimeout(() => el.remove(), FADE_MS)
  }, wait)
}
