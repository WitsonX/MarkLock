//! 外部修改监控（两个轮询器）：
//! 1. `ExternalChangePoller`：按「磁盘目标路径」轮询文件戳记（mtime + size），与上一轮基线比对，
//!    报告被其它程序改动的路径与已消失（删除 / 重命名）的路径。
//! 2. `DirChangePoller`：按「根 + 已列过的目录」轮询一层列目录结果的条目签名，
//!    报告发生文件 / 文件夹增减的路径，供侧边栏工作目录树及时刷新。
//!
//! 设计要点：
//! - 只做 `stat` 级比对（一次 invoke 批量拿回全部页签的戳记），开销恒定；
//!   「内容是否真的变了」由调用方重读正文比对，因为一个 `.mdlb` 容器对应库内多个页签，
//!   容器 mtime 变了不代表每个已打开文件的内容都变了。
//! - 轮询而非系统文件事件：与本项目既有「窗口最小化 / 休眠」检测一致用轮询，
//!   免去引入 notify crate 与跨平台事件流，也天然覆盖「应用未聚焦时被改」的场景。
//! - 报告后立刻把基线推进到当前戳记：同一处外部改动只提醒一次，不重复弹窗。
//! - 模块级单例：路由切到设置/解锁页会销毁 EditorView，基线与定时器跨页保持，
//!   回到编辑器时第一轮比对即可发现「离开期间」发生的外部改动。

import { fileStamps, listDir } from './tauri'
import type { FileStamp, FsNode } from './tauri'

/** 戳记变化处理器：入参为本轮检测到变化 / 消失的磁盘路径。 */
export type StampHandler = (paths: string[]) => void | Promise<void>

/** 零值戳记（文件不存在）。 */
const MISSING: FileStamp = { mtime_ms: 0, size: 0 }

function same(a: FileStamp, b: FileStamp): boolean {
  return a.mtime_ms === b.mtime_ms && a.size === b.size
}

/** 文件戳记轮询器；同一进程内只需一个实例（见文件末尾的单例导出）。 */
export class ExternalChangePoller {
  /** 基线：路径 → 上一轮观测到的戳记（跨路由切换保持） */
  private base = new Map<string, FileStamp>()
  private timer: ReturnType<typeof setInterval> | null = null
  /** 上一轮仍在进行中时跳过本轮（重读文件可能慢于轮询间隔） */
  private busy = false
  /** 是否已接入处理器；未接入时轮询空转（不动基线），避免销毁中的组件被回调 */
  private attached = false
  private getTargets: () => string[] = () => []
  private onChange: StampHandler = () => {}
  private onMissing: StampHandler = () => {}

  constructor(private intervalMs = 2000) {}

  /** 接入调用方（编辑器挂载时调用）；重复调用只替换处理器，定时器与基线保持不变。 */
  attach(handlers: { targets: () => string[]; onChange: StampHandler; onMissing: StampHandler }) {
    this.getTargets = handlers.targets
    this.onChange = handlers.onChange
    this.onMissing = handlers.onMissing
    this.attached = true
    this.start()
    // 立即跑一轮：既为新增路径建立基线，也补上「接入前发生的外部改动」
    this.tick().catch(() => {})
  }

  /** 摘除处理器（编辑器卸载）：保留定时器与基线，回到编辑器后由 attach 恢复上报。 */
  detach() {
    this.attached = false
  }

  /** 彻底停止监控（设置里关掉开关）：停表 + 清空基线。 */
  stop() {
    this.detach()
    if (this.timer) {
      clearInterval(this.timer)
      this.timer = null
    }
    this.base.clear()
  }

  /** 主动触发一轮检查（窗口重新获得焦点 / 从后台恢复时用，比等下一个间隔更及时）。 */
  check() {
    if (!this.attached) return Promise.resolve()
    return this.tick()
  }

  /**
   * 把自己的写入登记进基线：保存成功后立刻刷新戳记，
   * 避免下一轮把我们自己的保存当成外部修改（少一次无谓的全文重读）。
   */
  async sync(paths: string[]) {
    const uniq = [...new Set(paths)]
    if (!uniq.length) return
    try {
      const stamps = await fileStamps(uniq)
      uniq.forEach((p, i) => {
        const s = stamps[i]
        if (s) this.base.set(p, s)
      })
    } catch {
      // 刷新失败只是少了一次优化，下一轮照常比对（内容比对仍能排除自身保存）
    }
  }

  private start() {
    if (this.timer) return
    this.timer = setInterval(() => {
      this.tick().catch(() => {})
    }, this.intervalMs)
  }

  private async tick() {
    if (!this.attached || this.busy) return
    const paths = [...new Set(this.getTargets())]
    if (!paths.length) {
      this.base.clear()
      return
    }
    this.busy = true
    try {
      const stamps = await fileStamps(paths)
      // 重读期间调用方可能已卸载（路由切换）：基线照旧推进，但不再回调
      if (!this.attached) return
      const changed: string[] = []
      const missing: string[] = []
      for (let i = 0; i < paths.length; i++) {
        const p = paths[i]
        const cur = stamps[i] ?? MISSING
        const prev = this.base.get(p)
        this.base.set(p, cur)
        // 首次见到该路径只建立基线（刚打开的文件不算外部修改）
        if (!prev || same(prev, cur)) continue
        if (cur.mtime_ms === 0) missing.push(p)
        else changed.push(p)
      }
      // 剪掉已关闭页签的基线，避免 Map 无上限增长
      if (this.base.size > paths.length) {
        const alive = new Set(paths)
        for (const k of [...this.base.keys()]) {
          if (!alive.has(k)) this.base.delete(k)
        }
      }
      if (changed.length) await this.onChange(changed)
      if (missing.length) await this.onMissing(missing)
    } finally {
      this.busy = false
    }
  }
}

/** 全应用共用的监控器（页签集合由编辑器视图接入时提供）。 */
export const externalChanges = new ExternalChangePoller()

// ==================== 目录增减监控 ====================

/**
 * 目录条目签名：只编码「有哪些条目、各自是目录 / 库 / 普通文件」。
 * 不看大小与 mtime：目录里文件内容的改动属于页签外部修改监控的职责，
 * 在这里会造成每次保存都触发一次无谓的重列目录。
 * 签名前先排序：只关心「条目集合」是否变化，不依赖后端返回顺序
 * （同名仅大小写不同的条目在区分大小写的卷上排序不稳定，否则每轮都会误报变化而反复刷新）。
 */
function dirSignature(nodes: FsNode[]): string {
  return nodes
    .map((n) => `${n.name}${n.is_dir ? '/' : n.is_vault ? '#' : ''}`)
    .sort()
    .join('\u0001')
}

/** 目录变化处理器：入参为本轮检测到条目增减 / 重命名的目录绝对路径。 */
export type DirChangeHandler = (dirs: string[]) => void | Promise<void>

/**
 * 工作目录增减监控：按「根目录 + 已列过内容的子目录」轮询一层列目录结果，
 * 与上一轮签名基线比对，及时刷新侧边栏工作目录树。
 *
 * 与 ExternalChangePoller 同一套轮询思路（见文件头注释）：不引入 notify crate
 * 与跨平台事件流，也天然覆盖「应用未聚焦时别的程序往目录里增删文件」的场景。
 * 目标由接入方提供：只关心已经在树里列过内容的目录（收起但未列过的目录不在监控内，
 * 展开时本来就会重新列目录，读到的是最新状态）。
 */
export class DirChangePoller {
  /** 基线：目录绝对路径 → 上一轮观测到的条目签名 */
  private base = new Map<string, string>()
  private timer: ReturnType<typeof setInterval> | null = null
  private busy = false
  private attached = false
  private getTargets: () => string[] = () => []
  private onChange: DirChangeHandler = () => {}

  constructor(private intervalMs = 2000) {}

  /** 接入调用方（编辑器挂载时调用）；重复调用只替换处理器，定时器与基线保持不变。 */
  attach(handlers: { targets: () => string[]; onChange: DirChangeHandler }) {
    this.getTargets = handlers.targets
    this.onChange = handlers.onChange
    this.attached = true
    this.start()
    this.tick().catch(() => {})
  }

  /** 摘除处理器（编辑器卸载）：保留定时器与基线，回到编辑器后由 attach 恢复上报。 */
  detach() {
    this.attached = false
  }

  /** 彻底停止监控：停表 + 清空基线。 */
  stop() {
    this.detach()
    if (this.timer) {
      clearInterval(this.timer)
      this.timer = null
    }
    this.base.clear()
  }

  /**
   * 忘记基线（下一轮只重新建立、不当成变化）：换挂新工作目录、
   * 或应用自己刚做完增删并重列过目录时调用，避免多一次重复刷新。
   */
  reset(dirs?: string[]) {
    if (!dirs) {
      this.base.clear()
      return
    }
    for (const d of dirs) this.base.delete(d)
  }

  /** 主动触发一轮检查（窗口重新获得焦点 / 回到前台时用，比等下一个间隔更及时）。 */
  check() {
    if (!this.attached) return Promise.resolve()
    return this.tick()
  }

  private start() {
    if (this.timer) return
    this.timer = setInterval(() => {
      this.tick().catch(() => {})
    }, this.intervalMs)
  }

  private async tick() {
    // 目标为空（未挂载 / 侧边栏隐藏 / 分组收起）时只跳过本轮，不清基线：
    // 重新展开时第一轮比对就能补上「收起期间」目录里的增减
    if (!this.attached || this.busy || !this.getTargets().length) return
    this.busy = true
    const dirs = [...new Set(this.getTargets())]
    try {
      const changed: string[] = []
      // 各目录并发列目录（数量已由调用方上限约束）；读不了的目录（被删 / 移走 / 无权限）
      // 只抹掉基线不报变化——它的消失会由父目录的签名变化反映出来
      const results = await Promise.all(
        dirs.map(async (d) => {
          try {
            return [d, dirSignature(await listDir(d))] as const
          } catch {
            return [d, null] as const
          }
        }),
      )
      // 比对期间调用方可能已卸载（路由切换）：基线照旧推进，但不再回调
      if (!this.attached) return
      const alive = new Set(dirs)
      for (const [d, sig] of results) {
        if (sig === null) {
          this.base.delete(d)
          continue
        }
        const prev = this.base.get(d)
        this.base.set(d, sig)
        // 首次见到该目录只建立基线（刚展开的目录不算变化）
        if (prev !== undefined && prev !== sig) changed.push(d)
      }
      // 剪掉已收起 / 已消失目录的基线，避免 Map 无上限增长
      for (const k of [...this.base.keys()]) {
        if (!alive.has(k)) this.base.delete(k)
      }
      if (changed.length) await this.onChange(changed)
    } finally {
      this.busy = false
    }
  }
}

/** 全应用共用的工作目录监控器（监控目标由编辑器视图接入时提供）。 */
export const workdirChanges = new DirChangePoller()
