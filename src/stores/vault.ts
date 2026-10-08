//! 全局状态：解锁会话、打开的文件、最近库列表、设置。
//!
//! 采用 Pinia + localStorage 持久化（用户拍板：本轮先用 localStorage 暂存）。
//! 密钥材料绝不进 store —— 这里只存「库的路径、名称、锁定状态」等元信息，
//! 真正的密码/明文都通过 `src/lib/tauri.ts` 交给 Rust 处理。

import { defineStore } from 'pinia'
import * as tauri from '../lib/tauri'

/** 一个已登记的库（元信息，不含密钥） */
export interface VaultInfo {
  /** 会话 id（目录库=目录路径，单文件库=文件路径） */
  id: string
  /** 显示名称 */
  name: string
  /** 绝对路径 */
  path: string
  /** 是目录库（true）还是单文件（false） */
  isDir: boolean
  /** 是否为单文件库（`.mdlb`），区别于单文件 `.mdl` */
  isFileVault: boolean
  /** 已解锁 */
  unlocked: boolean
  /** 最近解锁时间戳 */
  lastOpened: number
  /** 库色（用于标签区分） */
  color: string
}

/** 页签对应磁盘文件的外部状态（由外部修改监控写入，仅内存态、不持久化）。 */
export type ExternalState = '' | 'modified' | 'missing'

/** 一个打开的文件页签 */
export interface OpenFile {
  /** 文件标识路径：普通文件=绝对路径；单文件库内文件=`库路径#相对路径` */
  path: string
  /** 文件名 */
  name: string
  /** 所属库 id（普通明文文件为空字符串） */
  vaultId: string
  /** 是否加密文件（true 走加密读写，false 走明文读写） */
  encrypted: boolean
  /** 是否未落盘的草稿（新建的空白页签，保存时才选目标+密码） */
  isNew: boolean
  /** 加密文件页签是否尚未解锁（true 时内容区显示密码框，输入密码解锁后读取） */
  locked: boolean
  /** 是否有未保存修改 */
  dirty: boolean
  /** 当前编辑内容（明文，仅内存态） */
  content: string
  /** 单文件库内文件的相对路径（仅当 vaultId 是单文件库时有效） */
  relPath: string
  /**
   * 磁盘文件被其它程序改动（modified）或已删除/移动（missing）。
   * 无未保存修改时监控会直接重新加载并清空该标记；仅在「页签有未保存内容」时保留，
   * 用于页签角标提醒 + 手动「重新加载」。
   */
  external?: ExternalState
}

/** 一个最近打开的文件条目 */
export interface RecentFile {
  /** 文件绝对路径 */
  path: string
  /** 文件名 */
  name: string
  /** 所属库 id（普通文件为空） */
  vaultId: string
  /** 是否加密文件 */
  encrypted: boolean
  /** 最近打开时间戳 */
  lastOpened: number
}

/** 已关闭页签的快照（撤销重开用，仅会话内有效不持久化）。 */
export interface ClosedTab {
  path: string
  name: string
  vaultId: string
  encrypted: boolean
  /** 未落盘草稿：撤销时连同正文恢复 */
  isNew: boolean
  dirty: boolean
  /** 仅草稿（isNew）保存正文，普通页签按路径重开即可 */
  content: string
}

/** 库色板（循环使用） */
const VAULT_COLORS = ['#1677ff', '#722ed1', '#13c2c2', '#fa8c16', '#eb2f96', '#52c41a']

const RECENT_KEY = 'marklock.recent'
const SETTINGS_KEY = 'marklock.settings'
const FAVORITES_KEY = 'marklock.favorites'
const RECENT_FILES_KEY = 'marklock.recentFiles'
const FAVORITE_FILES_KEY = 'marklock.favoriteFiles'
const OPEN_FILES_KEY = 'marklock.openFiles'
const SECTION_ORDER_KEY = 'marklock.sectionOrder'

/** 侧边栏分组 id（按默认顺序：打开的文件、库、工作目录、收藏、最近打开、大纲） */
const SECTION_IDS = ['open', 'vaults', 'workdir', 'favorites', 'recent', 'outline'] as const
type SectionId = (typeof SECTION_IDS)[number]

export interface Settings {
  /** 自动锁定超时（秒），0 表示不自动锁定 */
  autoLockSecs: number
  /** 是否显示顶部页签栏 */
  showTabs: boolean
  /** 是否显示侧边栏 */
  showSidebar: boolean
  /** 侧边栏是否显示"打开的文件"分组 */
  showOpenFiles: boolean
  /** 侧边栏是否显示"加密库"分组（原"打开的库"，现锁定后仍保留在列表便于二次打开） */
  showUnlockedVaults: boolean
  /** 侧边栏是否显示"收藏"分组 */
  showFavorites: boolean
  /** 侧边栏是否显示"最近打开"分组 */
  showRecent: boolean
  /** 侧边栏是否显示"工作目录"分组 */
  showWorkdir: boolean
  /** 侧边栏是否显示"大纲"分组（当前文件的 Markdown 标题结构） */
  showOutline: boolean
  /** 工作目录是否显示隐藏文件（以 . 开头） */
  showHiddenFiles: boolean
  /** 恢复上次打开的文件 */
  restoreLastSession: boolean
  /** 剪贴板定时清除（秒） */
  clipboardClearSecs: number
  /** 窗口最小化时锁定 */
  lockOnMinimize: boolean
  /** 系统休眠 / 锁屏时锁定 */
  lockOnSleep: boolean
  /** 锁定后清除剪贴板 */
  clearClipboardOnLock: boolean
  /** 外观主题：浅色 / 深色 / 跟随系统 */
  theme: 'light' | 'dark' | 'system'
  /** 代码高亮主题 */
  codeTheme: 'github' | 'monokai' | 'dracula' | 'atom-one-dark'
  /** 代码 Tab 缩进空格数 */
  codeIndent: 2 | 4
  /** 编辑器与预览字体大小（px），范围 10–28，⌘+/⌘- 可调 */
  editorFontSize: number
  /** 预览内容栏最大宽度（px），0 表示不限制、铺满面板宽度 */
  editorMaxWidth: number
  /** 是否显示 Markdown 格式工具栏 */
  showToolbar: boolean
  /** 是否显示状态栏 */
  showStatusbar: boolean
  /** 双击预览区域时是否在分屏/预览之间切换 */
  previewDblClickToggle: boolean
  /** 编辑器模式：编辑 / 分屏 / 预览 */
  editorMode: 'edit' | 'split' | 'preview'
  /** 首次使用时是否已询问过「设为 .mdl/.mdlb 默认打开程序」（避免重复弹窗） */
  assocPromptAsked: boolean
  /** 监控外部修改：打开的文件被其它程序改动时及时重新加载 */
  watchExternalChanges: boolean
  /** 自动保存间隔（秒），0 表示关闭自动保存（默认关闭，用户显式开启后才定时保存） */
  autoSaveSecs: number
  /** 普通 md 转为加密文件（.mdl）后对原明文文件的处理：每次询问 / 删除原文件 / 保留原文件 */
  convertMdAction: 'ask' | 'delete' | 'keep'
}

// 导出供 UI 标注「默认」档位（如状态栏字号 / 自动锁定菜单），避免各处硬编码默认值
export const DEFAULT_SETTINGS: Settings = {
  autoLockSecs: 300,
  showTabs: true,
  showSidebar: true,
  // 初始不展示「打开的文件」：页签栏已能切换文件，侧边栏这份重复工作列表默认收起，
  // 可在设置页或侧边栏分组菜单里开启（开启后不会因打开了文件而自动显示）
  showOpenFiles: false,
  showUnlockedVaults: true,
  showFavorites: true,
  // 初始不展示「最近打开」：新装/重置时该分组多为噪音，可在设置或侧边栏菜单里开启
  showRecent: false,
  showWorkdir: true,
  showOutline: true,
  showHiddenFiles: false,
  restoreLastSession: true,
  clipboardClearSecs: 30,
  lockOnMinimize: true,
  lockOnSleep: true,
  clearClipboardOnLock: false,
  theme: 'system',
  codeTheme: 'github',
  codeIndent: 4,
  editorFontSize: 14,
  editorMaxWidth: 1280,
  showToolbar: true,
  showStatusbar: true,
  previewDblClickToggle: true,
  editorMode: 'split',
  assocPromptAsked: false,
  watchExternalChanges: true,
  autoSaveSecs: 0,
  convertMdAction: 'ask',
}

function loadRecent(): VaultInfo[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY)
    if (!raw) return []
    const list = JSON.parse(raw) as VaultInfo[]
    // 会话只存在于 Rust 内存中，重启后必然失效；
    // 持久化里残留的 unlocked 标记一律清除，避免「已解锁」徽标与实际脱节。
    return list.map((v) => ({ ...v, unlocked: false }))
  } catch {
    return []
  }
}

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY)
    return raw ? { ...DEFAULT_SETTINGS, ...JSON.parse(raw) } : { ...DEFAULT_SETTINGS }
  } catch {
    return { ...DEFAULT_SETTINGS }
  }
}

function loadFavorites(): VaultInfo[] {
  try {
    const raw = localStorage.getItem(FAVORITES_KEY)
    if (!raw) return []
    const list = JSON.parse(raw) as any[]
    if (!list.length) return []
    // 新格式：已是库对象快照，直接返回
    if (typeof list[0] === 'object') return list as VaultInfo[]
    // 旧格式：id 字符串数组 → 从 recent 取元数据迁移为自持快照
    const recentRaw = localStorage.getItem(RECENT_KEY)
    const recent: VaultInfo[] = recentRaw ? (JSON.parse(recentRaw) as VaultInfo[]) : []
    const byId = new Map(recent.map((v) => [v.id, v]))
    const out: VaultInfo[] = []
    for (const id of list as string[]) {
      const v = byId.get(id)
      if (v) out.push({ ...v, unlocked: false })
    }
    localStorage.setItem(FAVORITES_KEY, JSON.stringify(out))
    return out
  } catch {
    return []
  }
}

function loadRecentFiles(): RecentFile[] {
  try {
    const raw = localStorage.getItem(RECENT_FILES_KEY)
    return raw ? JSON.parse(raw) : []
  } catch {
    return []
  }
}

function loadFavoriteFiles(): RecentFile[] {
  try {
    const raw = localStorage.getItem(FAVORITE_FILES_KEY)
    if (!raw) return []
    const list = JSON.parse(raw) as any[]
    if (!list.length) return []
    // 新格式：已是文件对象快照
    if (typeof list[0] === 'object') return list as RecentFile[]
    // 旧格式：path 字符串数组 → 从 recentFiles 取元数据；查不到则用路径合成最小快照
    const recentRaw = localStorage.getItem(RECENT_FILES_KEY)
    const recent: RecentFile[] = recentRaw ? (JSON.parse(recentRaw) as RecentFile[]) : []
    const byPath = new Map(recent.map((f) => [f.path, f]))
    const out: RecentFile[] = []
    for (const p of list as string[]) {
      const f = byPath.get(p)
      if (f) out.push({ ...f })
      else {
        const base = p.split('#')[0]
        const tail = p.includes('#') ? p.split('#').pop() || p : p
        out.push({ path: p, name: tail.split(/[\\/]/).pop() || p, vaultId: p.includes('#') ? base : p, encrypted: /\.(mdl|mdlb)$/i.test(base), lastOpened: 0 })
      }
    }
    localStorage.setItem(FAVORITE_FILES_KEY, JSON.stringify(out))
    return out
  } catch {
    return []
  }
}

/** 打开页签清单（仅元信息，不含内容；内容启动时重读）。 */
interface OpenFileMeta {
  path: string
  name: string
  vaultId: string
  encrypted: boolean
  /** 单文件库内文件的相对路径（仅单文件库有效） */
  relPath?: string
}

function loadOpenFiles(): OpenFileMeta[] {
  try {
    const raw = localStorage.getItem(OPEN_FILES_KEY)
    return raw ? JSON.parse(raw) : []
  } catch {
    return []
  }
}

/** 载入侧边栏分组顺序；旧数据/缺失时回退默认顺序，并补齐可能新增的分组。 */
function loadSectionOrder(): string[] {
  try {
    const raw = localStorage.getItem(SECTION_ORDER_KEY)
    if (!raw) return [...SECTION_IDS]
    const list = JSON.parse(raw) as string[]
    // 只保留合法的分组 id，去掉未知项
    const valid = list.filter((id) => (SECTION_IDS as readonly string[]).includes(id))
    // 补齐缺失的分组（追加到末尾），保证全部 5 个分组都在
    for (const id of SECTION_IDS) {
      if (!valid.includes(id)) valid.push(id)
    }
    return valid
  } catch {
    return [...SECTION_IDS]
  }
}

export const useVaultStore = defineStore('vault', {
  state: () => ({
    /** 最近/已登记库列表（含锁定状态）。旧版本曾把单文件 .mdl 登记为库，这里迁移清理：库保留目录库（.mdlb 目录）与单文件库（.mdlb 文件），剔除单文件 .mdl */
    recent: loadRecent().filter((v) => v.isDir || v.isFileVault) as VaultInfo[],
    /** 当前打开的文件（页签） */
    openFiles: [] as OpenFile[],
    /** 上次退出时打开的页签清单（仅元信息，启动恢复用） */
    savedOpenFiles: loadOpenFiles() as OpenFileMeta[],
    /** 当前激活的文件路径 */
    activePath: '' as string,
    /** 当前激活的库 id */
    activeVaultId: '' as string,
    /** 解锁后待自动打开的文件（拖入库内文件/单文件库时暂存，会话内有效） */
    pendingOpen: null as { vaultId: string; path: string; name: string } | null,
    /** 刚解锁、待在侧边栏「加密库」自动展开一级树的库 id（解锁回来后消费并清空） */
    pendingExpandVault: '' as string,
    /** 待处理的退出请求：非编辑器页收到关闭事件时置位并跳回编辑器，由未保存检查确认后退出 */
    pendingQuit: false,
    /** 收藏的库（自持对象快照） */
    favorites: loadFavorites() as VaultInfo[],
    /** 最近打开的文件列表 */
    recentFiles: loadRecentFiles() as RecentFile[],
    /** 收藏的文件（自持对象快照） */
    favoriteFiles: loadFavoriteFiles() as RecentFile[],
    /** 侧边栏分组显示顺序（id 数组） */
    sectionOrder: loadSectionOrder() as string[],
    /** 最近关闭的页签栈（撤销关闭用，最新关闭在前；仅会话内，上限 20 条） */
    recentlyClosed: [] as ClosedTab[],
    /** 设置 */
    settings: loadSettings() as Settings,
  }),

  getters: {
    /** 已解锁的库列表（目录库 + 单文件库；单文件 .mdl 不是「库」，不在此列出） */
    unlockedVaults(state): VaultInfo[] {
      return state.recent.filter((v) => v.unlocked && (v.isDir || v.isFileVault))
    },
    /** 全部已登记的加密库（目录库 + 单文件库），保留登记顺序；供「加密库」分组同时显示锁定/解锁两类，锁定后不移除 */
    registeredVaults(state): VaultInfo[] {
      return state.recent.filter((v) => v.isDir || v.isFileVault)
    },
    /** 当前激活文件对象 */
    activeFile(state): OpenFile | undefined {
      return state.openFiles.find((f) => f.path === state.activePath)
    },
    /** 当前激活库对象 */
    activeVault(state): VaultInfo | undefined {
      return state.recent.find((v) => v.id === state.activeVaultId)
    },
    /** 收藏的库对象列表（按收藏顺序排列，不随 recent 乱跳） */
    favoriteVaults(state): VaultInfo[] {
      const byId = new Map(state.recent.map((v) => [v.id, v]))
      // 优先用 recent 里的实时库对象（反映解锁状态），库被移出登记后回退到收藏快照
      return state.favorites.map((f) => byId.get(f.id) || f)
    },
    /** 收藏的文件对象列表（元数据来自自持快照，recentFiles 清空后仍能显示，且不随最近列表乱跳） */
    favoriteFileList(state): RecentFile[] {
      const byPath = new Map(state.recentFiles.map((f) => [f.path, f]))
      return state.favoriteFiles.map((f) => byPath.get(f.path) || f)
    },
    /** 侧边栏分组 id（按用户排好的顺序，已过滤隐藏项与空「打开的库」/空「收藏」/无标题「大纲」组）。 */
    visibleSections(state): string[] {
      const show: Record<string, boolean> = {
        open: state.settings.showOpenFiles,
        // 「加密库」分组显示全部已登记库（锁定后仍保留），只要有任意登记库即占位，不再要求处于解锁态
        vaults: state.settings.showUnlockedVaults && state.recent.some((v) => v.isDir || v.isFileVault),
        // 收藏为空（无收藏的库也无收藏的文件）时不占位，与「打开的库」的空组隐藏策略一致
        favorites: state.settings.showFavorites && state.favorites.length + state.favoriteFiles.length > 0,
        recent: state.settings.showRecent,
        workdir: state.settings.showWorkdir,
        outline: state.settings.showOutline && !!state.openFiles.find((f) => f.path === state.activePath)?.content,
      }
      return state.sectionOrder.filter((id) => show[id])
    },
  },

  actions: {
    // ---------- 持久化 ----------
    persistRecent() {
      localStorage.setItem(RECENT_KEY, JSON.stringify(this.recent))
    },
    persistSettings() {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(this.settings))
    },
    persistFavorites() {
      localStorage.setItem(FAVORITES_KEY, JSON.stringify(this.favorites))
    },
    /** 持久化当前打开的页签清单（不含内容，草稿页签不持久化；同 path 只留一份）。 */
    persistOpenFiles() {
      const seen = new Set<string>()
      const meta = this.openFiles
        .filter((f) => !f.isNew)
        // 去重兜底：历史版本可能已存下重复 path，写回时顺手洗掉，避免下次启动又恢复出两个页签
        .filter((f) => (seen.has(f.path) ? false : (seen.add(f.path), true)))
        .map((f) => ({
          path: f.path,
          name: f.name,
          vaultId: f.vaultId,
          encrypted: f.encrypted,
          relPath: f.relPath || '',
        }))
      localStorage.setItem(OPEN_FILES_KEY, JSON.stringify(meta))
    },

    // ---------- 收藏 ----------
    isFavorite(id: string) {
      return this.favorites.some((v) => v.id === id)
    },
    toggleFavorite(id: string) {
      const idx = this.favorites.findIndex((v) => v.id === id)
      if (idx >= 0) {
        this.favorites.splice(idx, 1)
      } else {
        const v = this.recent.find((x) => x.id === id)
        if (v) this.favorites.push({ ...v, unlocked: false })
      }
      this.persistFavorites()
    },
    isFavoriteFile(path: string) {
      return this.favoriteFiles.some((f) => f.path === path)
    },
    toggleFavoriteFile(path: string) {
      const idx = this.favoriteFiles.findIndex((f) => f.path === path)
      if (idx >= 0) {
        this.favoriteFiles.splice(idx, 1)
      } else {
        const m =
          this.recentFiles.find((f) => f.path === path) ||
          this.openFiles.find((f) => f.path === path)
        if (m) {
          this.favoriteFiles.push({ path, name: m.name, vaultId: m.vaultId, encrypted: m.encrypted, lastOpened: 'lastOpened' in m ? m.lastOpened : Date.now() })
        }
      }
      localStorage.setItem(FAVORITE_FILES_KEY, JSON.stringify(this.favoriteFiles))
    },
    /** 拖放排序：移动收藏的库（fromId 移动到 toId 的位置）。 */
    reorderFavorites(fromId: string, toId: string) {
      const from = this.favorites.findIndex((v) => v.id === fromId)
      const to = this.favorites.findIndex((v) => v.id === toId)
      if (from < 0 || to < 0 || from === to) return
      const [item] = this.favorites.splice(from, 1)
      this.favorites.splice(to, 0, item)
      this.persistFavorites()
    },
    /** 拖放排序：移动收藏的文件。 */
    reorderFavoriteFiles(fromPath: string, toPath: string) {
      const from = this.favoriteFiles.findIndex((f) => f.path === fromPath)
      const to = this.favoriteFiles.findIndex((f) => f.path === toPath)
      if (from < 0 || to < 0 || from === to) return
      const [item] = this.favoriteFiles.splice(from, 1)
      this.favoriteFiles.splice(to, 0, item)
      localStorage.setItem(FAVORITE_FILES_KEY, JSON.stringify(this.favoriteFiles))
    },
    /** 拖放排序：移动「加密库」列表中的库（在 recent 内换位；收藏组各自维护顺序，不受影响）。 */
    reorderVaults(fromId: string, toId: string) {
      const from = this.recent.findIndex((v) => v.id === fromId)
      const to = this.recent.findIndex((v) => v.id === toId)
      if (from < 0 || to < 0 || from === to) return
      const [item] = this.recent.splice(from, 1)
      this.recent.splice(to, 0, item)
      this.persistRecent()
    },
    /** 拖放排序：移动「打开的文件」页签顺序。 */
    reorderOpenFiles(fromPath: string, toPath: string) {
      const from = this.openFiles.findIndex((f) => f.path === fromPath)
      const to = this.openFiles.findIndex((f) => f.path === toPath)
      if (from < 0 || to < 0 || from === to) return
      const [item] = this.openFiles.splice(from, 1)
      this.openFiles.splice(to, 0, item)
      this.persistOpenFiles()
    },
    /** 拖放排序：移动侧边栏分组顺序（fromId 移动到 toId 的位置）。 */
    reorderSection(fromId: string, toId: string) {
      const from = this.sectionOrder.indexOf(fromId)
      const to = this.sectionOrder.indexOf(toId)
      if (from < 0 || to < 0 || from === to) return
      this.sectionOrder.splice(from, 1)
      this.sectionOrder.splice(to, 0, fromId)
      this.persistSectionOrder()
    },
    /** 持久化分组顺序。 */
    persistSectionOrder() {
      localStorage.setItem(SECTION_ORDER_KEY, JSON.stringify(this.sectionOrder))
    },

    // ---------- 库登记 ----------
    registerVault(info: Omit<VaultInfo, 'unlocked' | 'lastOpened' | 'color'>) {
      const existing = this.recent.find((v) => v.id === info.id)
      if (existing) {
        existing.name = info.name
        existing.path = info.path
        existing.isDir = info.isDir
        existing.isFileVault = info.isFileVault
        existing.lastOpened = Date.now()
        this.persistRecent()
        return existing
      }
      const color = VAULT_COLORS[this.recent.length % VAULT_COLORS.length]
      const vault: VaultInfo = {
        ...info,
        unlocked: false,
        lastOpened: Date.now(),
        color,
      }
      this.recent.unshift(vault)
      this.persistRecent()
      return vault
    },

    removeVault(id: string) {
      this.recent = this.recent.filter((v) => v.id !== id)
      this.persistRecent()
    },

    // ---------- 解锁 / 锁定 ----------
    async unlockVault(id: string, password: string) {
      const vault = this.recent.find((v) => v.id === id)
      if (!vault) throw new Error('未找到该库')
      if (vault.isFileVault) {
        await tauri.unlockFileVault(id, vault.path, password, this.settings.autoLockSecs)
      } else if (vault.isDir) {
        await tauri.unlockVault(id, vault.path, password, this.settings.autoLockSecs)
      } else {
        await tauri.unlock(id, vault.path, password, this.settings.autoLockSecs)
      }
      vault.unlocked = true
      vault.lastOpened = Date.now()
      this.persistRecent()
      // 解锁后请求侧边栏自动展开该库的一级树（由 EditorView 消费）
      this.pendingExpandVault = id
      // 解锁成功后，重新读取该库内所有已打开页签的内容（锁定时明文已被清空）
      for (const f of this.openFiles) {
        if (!f.encrypted || f.vaultId !== id || f.vaultId === f.path) continue
        try {
          if (vault.isFileVault) {
            // 单文件库内文件：path 是 `库路径#相对路径`
            const rel = f.relPath || (f.path.includes('#') ? f.path.split('#').slice(1).join('#') : f.path)
            f.content = await tauri.readFileVault(id, vault.path, rel)
          } else {
            f.content = await tauri.readFile(id, f.path)
          }
        } catch {
          // 单个文件恢复失败不阻断整体解锁（页签保持空白，可手动重新打开）
        }
      }
    },

    /**
     * 修改库/单文件主密码（仅重包裹 DEK）：成功后刷新会话，
     * 并用新会话重读该库已打开页签的明文（旧 DEK 已失效，不重灌则保存会报错）。
     * 未登记的库（如工作目录直接解锁的）传 meta 补登记。
     */
    async changeVaultPassword(id: string, passwordPath: string, isFileVault: boolean, oldPwd: string, newPwd: string, meta?: { name: string; path: string }) {
      let vault = this.recent.find((v) => v.id === id)
      if (!vault && meta) {
        vault = this.registerVault({ id, name: meta.name, path: meta.path, isDir: !isFileVault, isFileVault }) || undefined
      }
      if (isFileVault) {
        await tauri.changeFileVaultPassword(id, passwordPath, oldPwd, newPwd)
      } else {
        await tauri.changePassword(id, passwordPath, oldPwd, newPwd)
      }
      if (vault) {
        vault.unlocked = true
        vault.lastOpened = Date.now()
        this.persistRecent()
      }
      // 重读该库内已打开页签（跳过单文件 .mdl 自身与未保存修改的页签，后者内存内容仍有效）
      for (const f of this.openFiles) {
        if (!f.encrypted || f.vaultId !== id || f.path === id || f.dirty) continue
        try {
          if (isFileVault) {
            const rel = f.relPath || (f.path.includes('#') ? f.path.split('#').slice(1).join('#') : f.path)
            f.content = await tauri.readFileVault(id, passwordPath, rel)
          } else {
            f.content = await tauri.readFile(id, f.path)
          }
        } catch {
          // 单个页签重读失败不阻断（页签保留旧内容，保存时会被拒绝）
        }
      }
      // 单文件 .mdl：会话 id 即文件路径，重读唯一页签（未保存修改时保留内存内容）
      const self = this.openFiles.find((f) => f.path === id && f.encrypted && !f.dirty)
      if (self) {
        try {
          self.content = await tauri.readFile(id, id)
        } catch {
          // 同上
        }
      }
    },

    /**
     * 锁定前自动保存：把有未保存修改的加密文件（.mdl）与库内文件写回磁盘，
     * 防止密钥清零后丢失编辑。传 vaultId 只处理该库；不传处理全部。
     * 草稿（isNew）与锁定态页签不在此列；单个写回失败不阻断其余文件与锁定流程。
     */
    async autoSaveEncryptedBeforeLock(vaultId?: string) {
      const targets = this.openFiles.filter(
        (f) => f.encrypted && f.dirty && !f.isNew && !f.locked && (!vaultId || f.vaultId === vaultId),
      )
      for (const f of targets) {
        try {
          await this.saveFile(f.path, f.content)
        } catch {
          // 写回失败（磁盘错误等）：保留内存内容，交由后续锁定清空；不阻断其他文件
        }
      }
    },

    async lockVault(id: string) {
      // 密钥清零前先把该库内有未保存修改的加密/库文件写回，避免锁定丢数据
      await this.autoSaveEncryptedBeforeLock(id)
      await tauri.lock(id)
      const vault = this.recent.find((v) => v.id === id)
      if (vault) {
        vault.unlocked = false
        this.persistRecent()
      }
      // 锁定后：清空该库内加密文件的明文内容，页签保留为锁定占位；
      // 单文件 .mdl 页签则回到锁定态（内容区显示密码框）；普通文件不受影响。
      this.openFiles.forEach((f) => {
        if (f.encrypted && f.vaultId === id) {
          f.content = ''
          f.dirty = false
          f.external = ''
          // 单文件加密（vaultId 即文件路径）→ 显示密码框
          if (f.vaultId === f.path) f.locked = true
        }
      })
      if (this.settings.clearClipboardOnLock) this.clearClipboard()
    },

    async lockAll() {
      // 密钥清零前先把所有有未保存修改的加密/库文件写回（含 ⌘L / 最小化 / 休眠锁屏等入口）
      await this.autoSaveEncryptedBeforeLock()
      await tauri.lockAll()
      this.recent.forEach((v) => (v.unlocked = false))
      this.persistRecent()
      // 只清空加密文件的明文内容；单文件 .mdl 回到锁定态，普通文件完全保留
      this.openFiles.forEach((f) => {
        if (f.encrypted) {
          f.content = ''
          f.dirty = false
          f.external = ''
          if (f.vaultId === f.path) f.locked = true
        }
      })
      if (this.settings.clearClipboardOnLock) this.clearClipboard()
    },

    /** 清空系统剪贴板，防止解密内容残留。 */
    clearClipboard() {
      try {
        navigator.clipboard?.writeText('').catch(() => {})
      } catch {
        /* 剪贴板不可用时忽略 */
      }
    },

    /**
     * 重置所有配置：偏好设置回默认，并清空全部本地记录（登记的库、收藏、
     * 最近打开、页签恢复清单、工作目录、侧边栏宽度与展开状态等）。
     * 磁盘上的库与文件不受影响，只是不再登记、解锁会话失效需重新输密码。
     * 流程：锁定前自动保存加密页签 → 全部锁定（内存密钥清零）→ 清空 marklock.* 存储 → 重置内存状态。
     * 调用方负责接着重新载入窗口，让所有组件按重置后的默认状态重新初始化。
     */
    async resetAll() {
      await this.lockAll()
      for (let i = localStorage.length - 1; i >= 0; i--) {
        const k = localStorage.key(i)
        if (k && k.startsWith('marklock.')) localStorage.removeItem(k)
      }
      this.settings = { ...DEFAULT_SETTINGS }
      this.recent = []
      this.favorites = []
      this.recentFiles = []
      this.favoriteFiles = []
      this.openFiles = []
      this.savedOpenFiles = []
      this.activePath = ''
      this.activeVaultId = ''
      this.pendingOpen = null
      this.sectionOrder = [...SECTION_IDS]
    },

    // ---------- 打开文件 ----------
    async openFile(vaultId: string, path: string, name: string) {
      // 已有则激活
      const existing = this.openFiles.find((f) => f.path === path)
      if (existing) {
        this.activePath = path
        this.activeVaultId = vaultId
        this.touchRecentFile(path, name, vaultId, true)
        return existing
      }
      // 读取解密内容
      const content = await tauri.readFile(vaultId, path)
      // 读盘期间可能已有另一条路径（冷启动恢复页签 + Finder 双击）为同一 path 建好页签，
      // push 前必须复查，否则同 path 会出现两个页签（重复 :key、双份编辑器实例）。
      const raced = this.openFiles.find((f) => f.path === path)
      if (raced) {
        this.activePath = path
        this.activeVaultId = vaultId
        this.touchRecentFile(path, name, vaultId, true)
        return raced
      }
      const file: OpenFile = { path, name, vaultId, encrypted: true, isNew: false, locked: false, dirty: false, content, relPath: '' }
      this.openFiles.push(file)
      this.activePath = path
      this.activeVaultId = vaultId
      this.touchRecentFile(path, name, vaultId, true)
      this.persistOpenFiles()
      return file
    },

    /** 打开一个普通（未加密）文件，明文读取。 */
    async openPlainFile(path: string, name: string) {
      const existing = this.openFiles.find((f) => f.path === path)
      if (existing) {
        this.activePath = path
        this.activeVaultId = ''
        this.touchRecentFile(path, name, '', false)
        return existing
      }
      const content = await tauri.readPlainFile(path)
      // 同上：await 让位窗口内的并发打开会造出重复页签，push 前复查
      const raced = this.openFiles.find((f) => f.path === path)
      if (raced) {
        this.activePath = path
        this.activeVaultId = ''
        this.touchRecentFile(path, name, '', false)
        return raced
      }
      const file: OpenFile = { path, name, vaultId: '', encrypted: false, isNew: false, locked: false, dirty: false, content, relPath: '' }
      this.openFiles.push(file)
      this.activePath = path
      this.activeVaultId = ''
      this.touchRecentFile(path, name, '', false)
      this.persistOpenFiles()
      return file
    },

    /** 新建一个未落盘的空白草稿页签（保存时才选目标 + 密码）。 */
    openNewDraft(name = '未命名') {
      let n = 0
      let path = `__draft__:${name}`
      // 确保唯一（多个草稿）
      while (this.openFiles.some((f) => f.path === path)) {
        n++
        path = `__draft__:${name} ${n}`
      }
      const file: OpenFile = {
        path,
        name,
        vaultId: '',
        encrypted: false,
        isNew: true,
        locked: false,
        dirty: false,
        content: '',
        relPath: '',
      }
      this.openFiles.push(file)
      this.activePath = path
      this.activeVaultId = ''
      return file
    },

    /**
     * 打开一个单文件加密文件（`.mdl`）：新开一个页签，不读内容、
     * 不登记为库，页签处于锁定态（内容区显示密码框），解锁后才解密读取。
     */
    openEncryptedFile(path: string, name: string) {
      const existing = this.openFiles.find((f) => f.path === path)
      if (existing) {
        this.activePath = path
        this.activeVaultId = path
        this.touchRecentFile(path, name, path, true)
        return existing
      }
      const file: OpenFile = {
        path,
        name,
        vaultId: path,
        encrypted: true,
        isNew: false,
        locked: true,
        dirty: false,
        content: '',
        relPath: '',
      }
      this.openFiles.push(file)
      this.activePath = path
      this.activeVaultId = path
      this.touchRecentFile(path, name, path, true)
      this.persistOpenFiles()
      return file
    },

    /**
     * 打开单文件库（`.mdlb`）内的一个文件。库需已解锁。
     * path 用 `库路径#相对路径` 标识，vaultId 为库文件路径，relPath 为库内相对路径。
     */
    async openFileVaultFile(vaultPath: string, relPath: string, name: string) {
      const key = `${vaultPath}#${relPath}`
      const existing = this.openFiles.find((f) => f.path === key)
      if (existing) {
        this.activePath = key
        this.activeVaultId = vaultPath
        return existing
      }
      const content = await tauri.readFileVault(vaultPath, vaultPath, relPath)
      // 同上：库内文件同样可能在 await 窗口内被并发打开，push 前复查
      const raced = this.openFiles.find((f) => f.path === key)
      if (raced) {
        this.activePath = key
        this.activeVaultId = vaultPath
        return raced
      }
      const file: OpenFile = {
        path: key,
        name,
        vaultId: vaultPath,
        encrypted: true,
        isNew: false,
        locked: false,
        dirty: false,
        content,
        relPath,
      }
      this.openFiles.push(file)
      this.activePath = key
      this.activeVaultId = vaultPath
      this.touchRecentFile(key, name, vaultPath, true)
      this.persistOpenFiles()
      return file
    },

    /**
     * 解锁一个单文件加密页签：用密码建立会话（以文件路径为 id），
     * 解密读取内容后把页签从锁定态转为可编辑态。
     */
    async unlockFile(path: string, password: string) {
      const file = this.openFiles.find((f) => f.path === path)
      if (!file) throw new Error('文件未打开')
      await tauri.unlock(path, path, password, this.settings.autoLockSecs)
      const content = await tauri.readFile(path, path)
      file.locked = false
      file.content = content
      file.dirty = false
    },

    /**
     * 保存一个草稿页签：落盘为加密文件、存入已解锁库、或保存为普通明文 .md 文件。
     * 落盘成功后把草稿页签转换为常规页签（更新 path/vaultId/encrypted/isNew）。
     */
    async saveDraft(path: string, content: string, target: { kind: 'file'; filePath: string; password: string } | { kind: 'vault'; vaultId: string; relPath: string } | { kind: 'plain'; filePath: string }) {
      const file = this.openFiles.find((f) => f.path === path)
      if (!file) throw new Error('草稿未打开')
      if (target.kind === 'file') {
        await tauri.createFile(target.filePath, target.password, content)
        file.path = target.filePath
        file.name = target.filePath.split(/[\\/]/).pop() || target.filePath
        file.vaultId = target.filePath
        file.encrypted = true
        file.isNew = false
        file.locked = false
        file.dirty = false
        // 用同一密码建立读写会话（不登记为「库」，单文件只作为加密文件页签）
        await tauri.unlock(target.filePath, target.filePath, target.password, this.settings.autoLockSecs)
        this.touchRecentFile(file.path, file.name, file.vaultId, true)
      } else if (target.kind === 'plain') {
        await tauri.createPlainFile(target.filePath, content)
        file.path = target.filePath
        file.name = target.filePath.split(/[\\/]/).pop() || target.filePath
        file.vaultId = ''
        file.relPath = ''
        file.encrypted = false
        file.isNew = false
        file.locked = false
        file.dirty = false
        this.touchRecentFile(file.path, file.name, '', false)
      } else {
        const v = this.recent.find((x) => x.id === target.vaultId)
        if (v?.isFileVault) {
          await tauri.createInFileVault(target.vaultId, v.path, target.relPath, content)
          const key = `${target.vaultId}#${target.relPath}`
          file.path = key
          file.name = target.relPath.split('/').pop() || target.relPath
          file.vaultId = target.vaultId
          file.relPath = target.relPath
          file.encrypted = true
          file.isNew = false
          file.dirty = false
          this.touchRecentFile(key, file.name, target.vaultId, true)
        } else {
          await tauri.createInVault(target.vaultId, target.relPath, content)
          const full = `${target.vaultId}/${target.relPath}`
          file.path = full
          file.name = target.relPath.split('/').pop() || target.relPath
          file.vaultId = target.vaultId
          file.encrypted = true
          file.isNew = false
          file.dirty = false
          this.touchRecentFile(full, file.name, target.vaultId, true)
        }
      }
      // 更新激活项与持久化
      this.activePath = file.path
      this.activeVaultId = file.vaultId
      this.persistOpenFiles()
      return file
    },

    /** 记录/更新一个文件到「最近打开」（去重 + 置顶）。 */
    touchRecentFile(path: string, name: string, vaultId: string, encrypted: boolean) {
      this.recentFiles = this.recentFiles.filter((f) => f.path !== path)
      this.recentFiles.unshift({ path, name, vaultId, encrypted, lastOpened: Date.now() })
      // 最多保留 50 条
      if (this.recentFiles.length > 50) this.recentFiles = this.recentFiles.slice(0, 50)
      localStorage.setItem(RECENT_FILES_KEY, JSON.stringify(this.recentFiles))
    },

    /** 从「最近打开」移除某个文件（文件被移动/删除后的失效清理）。 */
    forgetFile(path: string) {
      this.recentFiles = this.recentFiles.filter((f) => f.path !== path)
      localStorage.setItem(RECENT_FILES_KEY, JSON.stringify(this.recentFiles))
    },

    // ---------- 撤销关闭（最近关闭页签栈） ----------
    /** 把被关闭的页签记入撤销栈（最新关闭的排最前，批量关闭时栈内按页签条倒序）。 */
    noteClosedTabs(files: OpenFile[]) {
      if (!files.length) return
      const snaps: ClosedTab[] = files.map((f) => ({
        path: f.path,
        name: f.name,
        vaultId: f.vaultId,
        encrypted: f.encrypted,
        isNew: f.isNew,
        dirty: f.dirty,
        content: f.isNew ? f.content : '',
      }))
      this.recentlyClosed = [...snaps.reverse(), ...this.recentlyClosed].slice(0, 20)
    },
    /** 从撤销栈取出最近一个可重开的条目（路径已被打开页签占用的跳过）；无则返回 undefined。 */
    takeClosedTab(): ClosedTab | undefined {
      const i = this.recentlyClosed.findIndex((t) => !this.openFiles.some((f) => f.path === t.path))
      return i < 0 ? undefined : this.recentlyClosed.splice(i, 1)[0]
    },
    /** 文件被磁盘删除时同步清掉撤销栈条目，避免撤销重开已不存在的文件。 */
    dropClosedTab(path: string) {
      this.recentlyClosed = this.recentlyClosed.filter((t) => t.path !== path)
    },
    /** 恢复一个未落盘草稿页签（连同关闭前的正文与脏标记）。 */
    restoreDraftTab(t: ClosedTab) {
      const file: OpenFile = {
        path: t.path,
        name: t.name,
        vaultId: '',
        encrypted: false,
        isNew: true,
        locked: false,
        dirty: t.dirty,
        content: t.content,
        relPath: '',
      }
      this.openFiles.push(file)
      this.activePath = file.path
      this.activeVaultId = ''
    },

    /** 清空「最近打开的文件」列表（收藏已自持快照，不受影响）。 */
    clearRecentFiles() {
      this.recentFiles = []
      localStorage.setItem(RECENT_FILES_KEY, JSON.stringify(this.recentFiles))
    },
    /** 清空「最近打开/已登记」的库列表（收藏已自持快照，不受影响；点击收藏会用快照重新登记）。 */
    clearRecentVaults() {
      this.recent = []
      this.persistRecent()
    },

    closeFile(path: string) {
      this.noteClosedTabs(this.openFiles.filter((f) => f.path === path))
      this.openFiles = this.openFiles.filter((f) => f.path !== path)
      if (this.activePath === path) {
        const next = this.openFiles[this.openFiles.length - 1]
        this.activePath = next ? next.path : ''
        this.activeVaultId = next ? next.vaultId : ''
      }
      this.persistOpenFiles()
    },

    /** 关闭除指定页签外的其它所有页签。 */
    closeOthers(path: string) {
      this.noteClosedTabs(this.openFiles.filter((f) => f.path !== path))
      this.openFiles = this.openFiles.filter((f) => f.path === path)
      const kept = this.openFiles[0]
      this.activePath = kept ? kept.path : ''
      this.activeVaultId = kept ? kept.vaultId : ''
      this.persistOpenFiles()
    },

    /** 关闭所有已保存（无未保存修改）的页签，保留 dirty 项。 */
    closeSaved() {
      this.noteClosedTabs(this.openFiles.filter((f) => !f.dirty))
      this.openFiles = this.openFiles.filter((f) => f.dirty)
      if (!this.openFiles.some((f) => f.path === this.activePath)) {
        const next = this.openFiles[this.openFiles.length - 1]
        this.activePath = next ? next.path : ''
        this.activeVaultId = next ? next.vaultId : ''
      }
      this.persistOpenFiles()
    },

    /** 保存当前文件内容（加密写回或明文写回）。 */
    async saveFile(path: string, content: string) {
      const file = this.openFiles.find((f) => f.path === path)
      if (!file) throw new Error('文件未打开')
      if (file.encrypted) {
        const v = this.recent.find((x) => x.id === file.vaultId)
        if (v?.isFileVault) {
          // 单文件库内文件：走库内写回
          await tauri.writeFileVault(file.vaultId, file.vaultId, file.relPath, content)
        } else {
          await tauri.writeFile(file.vaultId, path, content)
        }
      } else {
        await tauri.writePlainFile(path, content)
      }
      file.content = content
      file.dirty = false
      // 写盘后磁盘内容即内存内容，外部冲突标记随之失效
      file.external = ''
    },

    /**
     * 按页签类型从磁盘重读正文明文（不改动页签状态），供外部修改监控比对用。
     * 路由与 `saveFile` 一致：未加密走明文读，单文件库走库内读，其余走单文件解密读。
     */
    async readDiskContent(f: OpenFile): Promise<string> {
      if (!f.encrypted) return tauri.readPlainFile(f.path)
      const v = this.recent.find((x) => x.id === f.vaultId)
      if (v?.isFileVault) {
        const rel = f.relPath || (f.path.includes('#') ? f.path.split('#').slice(1).join('#') : f.path)
        return tauri.readFileVault(f.vaultId, f.vaultId, rel)
      }
      return tauri.readFile(f.vaultId, f.path)
    },

    /**
     * 丢弃内存修改、用磁盘版本重载页签（外部修改后用户主动选择「重新加载」）。
     * 读取失败时抛错并保持原内容与标记。
     */
    async reloadFromDisk(path: string) {
      const file = this.openFiles.find((f) => f.path === path)
      if (!file) throw new Error('文件未打开')
      const content = await this.readDiskContent(file)
      file.content = content
      file.dirty = false
      file.external = ''
      this.persistOpenFiles()
      return file
    },

    /** 标记文件为已修改 */
    markDirty(path: string) {
      const file = this.openFiles.find((f) => f.path === path)
      if (file) file.dirty = true
    },

    // ---------- 设置 ----------
    updateSettings(patch: Partial<Settings>) {
      this.settings = { ...this.settings, ...patch }
      this.persistSettings()
    },

    // ---------- 自动锁定回收 ----------
    async reapExpired() {
      const ids = await tauri.reapExpired()
      if (ids.length) {
        ids.forEach((id) => {
          const vault = this.recent.find((v) => v.id === id)
          if (vault) vault.unlocked = false
        })
        this.persistRecent()
        // 清空被锁定库内加密文件的明文内容（页签保留），普通文件不动；
        // 单文件 .mdl 页签回到锁定态（显示密码框）
        this.openFiles.forEach((f) => {
          if (f.encrypted && ids.includes(f.vaultId)) {
            f.content = ''
            f.dirty = false
            f.external = ''
            if (f.vaultId === f.path) f.locked = true
          }
        })
      }
      return ids
    },

    // ---------- 恢复上次会话 ----------
    /**
     * 恢复一个「库内加密文件」页签为锁定占位（不读内容）。
     * 库解锁后由 unlockVault 统一重读内容；库未解锁时显示就地密码框。
     */
    restoreVaultFileTab(m: OpenFileMeta) {
      const existing = this.openFiles.find((f) => f.path === m.path)
      if (existing) return existing
      const file: OpenFile = {
        path: m.path,
        name: m.name,
        vaultId: m.vaultId,
        encrypted: true,
        isNew: false,
        locked: false,
        dirty: false,
        content: '',
        relPath: m.relPath || '',
      }
      this.openFiles.push(file)
      this.persistOpenFiles()
      return file
    },
    /**
     * 恢复上次退出时打开的页签。内容不持久化，这里按类型重新读取：
     * - 普通文件：明文重读，直接恢复页签；
     * - 加密文件：所属库本会话已解锁则解密重读；未解锁则恢复为「锁定占位」
     *   （保留页签 + 库名，就地输密码解锁后自动恢复内容）。
     * 返回本次成功恢复的页签数。
     */
    async restoreSession(): Promise<number> {
      if (!this.settings.restoreLastSession) return 0
      let restored = 0
      const meta = this.savedOpenFiles.slice()
      this.savedOpenFiles = [] // 清空，避免下次重复恢复
      for (const m of meta) {
        try {
          if (m.encrypted) {
            // 单文件 .mdl（vaultId 即文件路径）：恢复为锁定态页签（内容区显示密码框）
            if (m.vaultId === m.path) {
              this.openEncryptedFile(m.path, m.name)
            } else {
              // 库内加密文件：所属库本会话已解锁则解密重读；未解锁则恢复为锁定占位页签
              const v = this.recent.find((x) => x.id === m.vaultId)
              if (!v) {
                // 库已不在最近列表（被清除/未登记），跳过该页签
                continue
              }
              if (!v.unlocked) {
                this.restoreVaultFileTab(m)
              } else if (v.isFileVault) {
                const rel = m.relPath || (m.path.includes('#') ? m.path.split('#').slice(1).join('#') : m.path)
                await this.openFileVaultFile(m.vaultId, rel, m.name)
              } else {
                await this.openFile(m.vaultId, m.path, m.name)
              }
            }
          } else {
            await this.openPlainFile(m.path, m.name)
          }
          restored++
        } catch {
          // 文件已不存在/无权限：跳过该页签
        }
      }
      return restored
    },
  },
})
