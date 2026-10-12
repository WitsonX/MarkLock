//! 轻量级「检查更新」：不接入 Tauri updater 插件，只比对最新 Release 版本号，
//! 发现新版后由调用方提示并跳转发布页手动下载。
//!
//! 设计要点（与用户确认）：
//! - 双源顺序回落：GitHub 与 Gitee 哪个先通就用哪个；按界面语言决定优先顺序
//!   （中文优先 Gitee、英文优先 GitHub），访问不到第一个源时自动试第二个。
//! - 频率门控：daily/weekly/monthly 记录上次检测时间戳，未到间隔直接跳过；
//!   manual 不自动检测、off 完全关闭。手动检查按钮不受频率限制。
//! - 纯静态 GET 请求，公开仓库接口无需 token、支持 CORS，用量极低不会触发限流。

import { getVersion } from '@tauri-apps/api/app'
import { Modal, message } from 'ant-design-vue'
import { t } from '../locales'
import { openUrl } from './tauri'

/** 检查更新频率档位（与 Settings.updateCheckFreq 对齐） */
export type UpdateFreq = 'daily' | 'weekly' | 'monthly' | 'manual' | 'off'

/** 一次检查的结果 */
export interface UpdateResult {
  /** available=有新版本；upToDate=已最新；failed=两源均不可达 */
  status: 'available' | 'upToDate' | 'failed'
  /** 当前版本（去掉前缀 v） */
  current: string
  /** 最新版本号（去掉前缀 v），failed 时缺省 */
  latest?: string
  /** 发布页地址（跳转到对应源的 Release 页），failed 时缺省 */
  url?: string
  /** 实际命中的源，便于日志/调试 */
  source?: 'github' | 'gitee'
}

// localStorage 键：上次「自动检测」的时间戳（marklock. 前缀，resetAll 会一并清除）
const LAST_CHECK_KEY = 'marklock.lastUpdateCheck'

// 两源的最新 Release 接口（公开仓库，返回 tag_name；GitHub 另带 html_url）
const GITHUB_API = 'https://api.github.com/repos/WitsonX/MarkLock/releases/latest'
const GITEE_API = 'https://gitee.com/api/v5/repos/Witson/MarkLock/releases/latest'
// Gitee 接口不返回 html_url，按其发布页规则用 tag 拼出跳转地址
const GITEE_RELEASE_BASE = 'https://gitee.com/Witson/MarkLock/releases/tag/'

// 各频率对应的最小检测间隔（毫秒）；manual/off 不在此表内
const FREQ_MS: Record<'daily' | 'weekly' | 'monthly', number> = {
  daily: 86_400_000,
  weekly: 7 * 86_400_000,
  monthly: 30 * 86_400_000,
}

/** 去掉 tag 前缀 v，得到纯版本号字符串。 */
function stripV(tag: string): string {
  return tag.trim().replace(/^v/i, '')
}

/** 把版本号解析为可比较的数字数组（忽略 -beta/+build 等后缀）。 */
function parseVersion(tag: string): number[] {
  return stripV(tag)
    .split(/[-+]/)[0]
    .split('.')
    .map((n) => parseInt(n, 10) || 0)
}

/** 比较两个版本号：a>b 返回正数，a<b 返回负数，相等返回 0。 */
export function compareVersions(a: string, b: string): number {
  const va = parseVersion(a)
  const vb = parseVersion(b)
  const len = Math.max(va.length, vb.length)
  for (let i = 0; i < len; i++) {
    const diff = (va[i] ?? 0) - (vb[i] ?? 0)
    if (diff !== 0) return diff
  }
  return 0
}

/**
 * 带超时的 JSON GET；失败（网络/超时/非 2xx）抛出异常交由调用方回落。
 * @param fresh true 时绕过缓存（cache:'no-store'），用于用户手动「立即检查」，
 *   保证每次都向服务器取最新 Release；自动静默检测走缓存以省流量、防限流。
 */
async function fetchJson(url: string, timeoutMs = 7000, fresh = false): Promise<any> {
  const ctrl = new AbortController()
  const timer = setTimeout(() => ctrl.abort(), timeoutMs)
  try {
    const res = await fetch(url, {
      signal: ctrl.signal,
      headers: { Accept: 'application/json' },
      cache: fresh ? 'no-store' : 'default',
    })
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    return await res.json()
  } finally {
    clearTimeout(timer)
  }
}

interface ReleaseInfo {
  tag: string
  url: string
}

/** 拉取 GitHub 最新 Release（html_url 直接指向该 tag 发布页）。 */
async function fetchGitHub(fresh = false): Promise<ReleaseInfo> {
  const j = await fetchJson(GITHUB_API, 7000, fresh)
  const tag = typeof j?.tag_name === 'string' ? j.tag_name : ''
  if (!tag) throw new Error('no tag')
  return { tag, url: typeof j?.html_url === 'string' ? j.html_url : `https://github.com/WitsonX/MarkLock/releases/tag/${tag}` }
}

/** 拉取 Gitee 最新 Release（接口无 html_url，用 tag 拼发布页地址）。 */
async function fetchGitee(fresh = false): Promise<ReleaseInfo> {
  const j = await fetchJson(GITEE_API, 7000, fresh)
  const tag = typeof j?.tag_name === 'string' ? j.tag_name : ''
  if (!tag) throw new Error('no tag')
  return { tag, url: `${GITEE_RELEASE_BASE}${tag}` }
}

/**
 * 立即执行一次检查（不受频率门控约束）。
 * @param preferZh 中文环境为 true：优先 Gitee、回落后试 GitHub；反之优先 GitHub。
 * @param fresh true 时绕过缓存取最新 Release（手动检查用）。
 */
export async function checkForUpdate(preferZh: boolean, fresh = false): Promise<UpdateResult> {
  let current = ''
  try {
    current = await getVersion()
  } catch {
    current = ''
  }
  const order: Array<['github' | 'gitee', (fresh: boolean) => Promise<ReleaseInfo>]> = preferZh
    ? [['gitee', fetchGitee], ['github', fetchGitHub]]
    : [['github', fetchGitHub], ['gitee', fetchGitee]]

  for (const [source, fn] of order) {
    try {
      const info = await fn(fresh)
      const base: UpdateResult = { status: 'upToDate', current, latest: stripV(info.tag), url: info.url, source }
      // 当前版本尚不可知（getVersion 失败）时保守认为有更新可看
      if (!current || compareVersions(info.tag, current) > 0) base.status = 'available'
      return base
    } catch {
      // 该源不可达，继续尝试下一个
    }
  }
  return { status: 'failed', current }
}

/** 距上次自动检测是否已达到所选频率的间隔（manual/off 恒为 false）。 */
export function shouldAutoCheck(freq: UpdateFreq, now = Date.now()): boolean {
  if (freq === 'off' || freq === 'manual') return false
  const last = Number(localStorage.getItem(LAST_CHECK_KEY) || 0)
  if (!Number.isFinite(last) || last <= 0) return true
  return now - last >= FREQ_MS[freq]
}

/** 记录本次自动检测时间戳（无论结果如何都记，避免启动时高频重试失败源）。 */
export function markChecked(now = Date.now()): void {
  localStorage.setItem(LAST_CHECK_KEY, String(now))
}

export interface UpdateUiOptions {
  /** auto=true 为启动静默检测：仅在发现新版时弹窗，已最新/失败不打扰用户 */
  auto?: boolean
}

/**
 * 检查并按结果展示 UI（弹窗 / 轻提示），供启动自动检测、原生菜单、设置页、关于页共用。
 * @param preferZh 中文环境为 true（优先 Gitee）
 */
export async function runUpdateCheck(preferZh: boolean, opts: UpdateUiOptions = {}): Promise<void> {
  // 手动触发（opts.auto !== true）绕过缓存取最新 Release；启动静默检测走缓存省流量
  const r = await checkForUpdate(preferZh, !opts.auto)
  if (r.status === 'available' && r.latest && r.url) {
    const url = r.url
    Modal.confirm({
      title: t('settings.update.title'),
      content: t('settings.update.body', { latest: r.latest, current: r.current }),
      okText: t('settings.update.go'),
      cancelText: t('settings.update.later'),
      onOk: () => {
        openUrl(url).catch(() => {})
      },
    })
  } else if (!opts.auto) {
    // 手动触发（菜单 / 按钮）才有反馈；静默模式下已最新/失败不提示
    if (r.status === 'upToDate') message.success(t('settings.update.upToDate', { current: r.current }))
    else message.error(t('settings.update.failed'))
  }
}
