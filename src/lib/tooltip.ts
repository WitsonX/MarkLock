/**
 * 全局气泡 tooltip 指令（单例，fixed 定位，挂 body 不受滚动容器裁剪）。
 *
 * 用法：v-tip="'提示文字'"  或  v-tip="{ text: '...', place: 'right' }"
 * place 可选 'right'（默认，侧边栏条目用）| 'bottom'（顶部页签用）| 'top' | 'left'。
 * 文字中的 \n 会换行显示。
 *
 * 与纯 CSS [data-tip] 不同：气泡挂在 document.body 下，能显示在侧边栏
 * （overflow: auto 滚动容器）之外，不会被裁剪；且支持整行条目触发。
 */
import type { Directive, DirectiveBinding } from 'vue'

interface TipOptions {
  text: string
  place?: 'right' | 'bottom' | 'top' | 'left'
}

const SHOW_DELAY = 500 // 悬停后延迟显示的毫秒数

let el: HTMLDivElement | null = null
let arrow: HTMLDivElement | null = null
let showTimer: number | null = null

function ensureEl(): { el: HTMLDivElement; arrow: HTMLDivElement } {
  if (!el) {
    el = document.createElement('div')
    el.className = 'ml-tip'
    el.style.cssText =
      'position:fixed;z-index:10000;max-width:360px;padding:6px 10px;' +
      'font-size:12px;line-height:1.5;color:#fff;background:rgba(31,35,41,0.96);' +
      'border-radius:6px;box-shadow:0 4px 16px rgba(0,0,0,0.2);' +
      'white-space:pre-line;word-break:break-all;pointer-events:none;opacity:0;' +
      'transition:opacity .12s ease;'
    arrow = document.createElement('div')
    arrow.className = 'ml-tip-arrow'
    arrow.style.cssText =
      'position:fixed;z-index:10000;width:0;height:0;' +
      'border:6px solid transparent;pointer-events:none;opacity:0;' +
      'transition:opacity .12s ease;'
    document.body.appendChild(el)
    document.body.appendChild(arrow)
  }
  return { el: el!, arrow: arrow! }
}

function clearTimer() {
  if (showTimer !== null) {
    window.clearTimeout(showTimer)
    showTimer = null
  }
}

function hide() {
  clearTimer()
  if (!el || !arrow) return
  el.style.opacity = '0'
  arrow.style.opacity = '0'
}

function show(target: HTMLElement, text: string, place: 'right' | 'bottom' | 'top' | 'left') {
  if (!text) return hide()
  const { el: tip, arrow: ar } = ensureEl()
  tip.textContent = text
  tip.style.opacity = '1'
  ar.style.opacity = '1'

  const r = target.getBoundingClientRect()
  // 先按指定方位定位
  let x: number
  let y: number
  let side: 'right' | 'left' | 'top' | 'bottom' = place

  if (place === 'right') {
    x = r.right + 10
    y = r.top + r.height / 2
  } else if (place === 'left') {
    x = r.left - 10
    y = r.top + r.height / 2
  } else if (place === 'top') {
    x = r.left + r.width / 2
    y = r.top - 8
  } else {
    // bottom（页签用）
    x = r.left + r.width / 2
    y = r.bottom + 8
  }

  // 测量气泡尺寸后做翻转判断
  const tw = tip.offsetWidth
  const th = tip.offsetHeight
  const vw = window.innerWidth

  if (side === 'right' && x + tw > vw - 8) {
    side = 'left'
    x = r.left - tw - 10
  } else if (side === 'left' && x < 8) {
    side = 'right'
    x = r.right + 10
  }

  if (side === 'right') {
    tip.style.left = x + 'px'
    tip.style.top = (y - th / 2) + 'px'
    ar.style.borderColor = 'transparent transparent transparent rgba(31,35,41,0.96)'
    ar.style.left = (x - 6) + 'px'
    ar.style.top = (y - 6) + 'px'
  } else if (side === 'left') {
    tip.style.left = x + 'px'
    tip.style.top = (y - th / 2) + 'px'
    ar.style.borderColor = 'transparent rgba(31,35,41,0.96) transparent transparent'
    ar.style.left = (r.left - 6) + 'px'
    ar.style.top = (y - 6) + 'px'
  } else if (side === 'top') {
    tip.style.left = (x - tw / 2) + 'px'
    tip.style.top = (y - th) + 'px'
    ar.style.borderColor = 'rgba(31,35,41,0.96) transparent transparent transparent'
    ar.style.left = (x - 6) + 'px'
    ar.style.top = (r.top - 12) + 'px'
  } else {
    // bottom
    tip.style.left = (x - tw / 2) + 'px'
    tip.style.top = y + 'px'
    ar.style.borderColor = 'transparent transparent rgba(31,35,41,0.96) transparent'
    ar.style.left = (x - 6) + 'px'
    ar.style.top = (r.bottom - 6) + 'px'
  }

  // 水平方向约束
  const left = parseFloat(tip.style.left)
  if (left < 8) tip.style.left = '8px'
  if (left + tw > vw - 8) tip.style.left = (vw - 8 - tw) + 'px'
  // 垂直方向约束
  const top = parseFloat(tip.style.top)
  if (top < 8) tip.style.top = '8px'
  if (top + th > window.innerHeight - 8) tip.style.top = (window.innerHeight - 8 - th) + 'px'
}

function bind(target: HTMLElement, binding: DirectiveBinding<TipOptions | string>) {
  const raw = binding.value
  const opts: TipOptions =
    typeof raw === 'string' ? { text: raw, place: 'right' } : { text: raw?.text || '', place: raw?.place || 'right' }

  const onEnter = () => {
    // 子元素优先：若祖先链上有其它带 v-tip 的元素，隐藏它们的 tooltip，
    // 避免「整行 + 图标」同时弹出两个气泡重叠闪烁。
    let p = target.parentElement
    while (p) {
      if ((p as any).__mlTipBound) hide()
      p = p.parentElement
    }
    // 延迟显示：悬停 0.5s 后才出现，快速划过不打扰
    clearTimer()
    showTimer = window.setTimeout(() => {
      show(target, opts.text, opts.place || 'right')
    }, SHOW_DELAY)
  }
  const onLeave = () => hide()
  const onScroll = () => hide()

  target.addEventListener('mouseenter', onEnter)
  target.addEventListener('mouseleave', onLeave)
  target.addEventListener('mousedown', onLeave)
  window.addEventListener('scroll', onScroll, true)
  ;(target as any).__mlTipBound = true

  ;(target as any).__mlTipCleanup = () => {
    target.removeEventListener('mouseenter', onEnter)
    target.removeEventListener('mouseleave', onLeave)
    target.removeEventListener('mousedown', onLeave)
    window.removeEventListener('scroll', onScroll, true)
  }
}

function unbind(target: HTMLElement) {
  const c = (target as any).__mlTipCleanup
  if (c) c()
  hide()
}

const tip: Directive = {
  mounted: (el, binding) => bind(el as HTMLElement, binding),
  updated: (el, binding) => bind(el as HTMLElement, binding),
  unmounted: (el) => unbind(el as HTMLElement),
}

export default tip
