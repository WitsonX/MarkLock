<script lang="ts">
import { defineComponent, nextTick } from 'vue'
import { message } from 'ant-design-vue'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { convertFileSrc } from '@tauri-apps/api/core'
import { useVaultStore, DEFAULT_SETTINGS } from '../stores/vault'
import type { OpenFile } from '../stores/vault'
import * as tauri from '../lib/tauri'
import type { FsNode } from '../lib/tauri'
import { externalChanges, workdirChanges } from '../lib/filewatch'
import type { UnlistenFn } from '@tauri-apps/api/event'
import VaultTreeNode from '../components/VaultTreeNode.vue'
import EncIcon from '../components/EncIcon.vue'
import CodeMirrorEditor from '../components/CodeMirrorEditor.vue'
import { marked } from 'marked'
import hljs from 'highlight.js'

/** 代码块高亮失败时的兵底转义 */
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
const CODE_COPY_BTN = `<button type="button" class="code-copy-btn" aria-label="复制代码" title="复制"><svg class="cc-ico cc-copy" viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 3.5v-.5a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h.5"/></svg><svg class="cc-ico cc-check" viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5 6.5 12 13 4.5"/></svg><span class="cc-txt">复制</span></button>`

/** 行内代码复制按钮：复用 .code-copy-btn 状态类（.cc-txt 由 CSS 隐藏只留图标），
 *  与 code 同为 .codespan-wrap 的兄弟节点，选中 code 文本时不会带入按钮文字。 */
const INLINE_COPY_BTN = CODE_COPY_BTN.replace('aria-label="复制代码"', 'aria-label="复制行内代码"')

/** 模块级：当前预览文件所在目录（解析正文里相对图片路径的基准）。
 *  renderMarkdown 每次渲染前按 activeFile 写入，image 渲染器读取它拼绝对路径再转 asset URL。 */
let __assetBaseDir = ''

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
      return `<pre>${CODE_COPY_BTN}<code class="hljs${safe ? ` language-${safe}` : ''}">${highlightCode(text, name)}</code></pre>\n`
    },
    // 行内代码：code 后跟复制按钮，外包 inline 容器供 CSS 悬停显现与事件定位；
    // 必须自己转义：marked 只在「默认 renderer」里对 codespan 做 escapeHtml，token.text 是原始文本，
    // 覆写 renderer 后不转义会让 `<script>` 这类内容输出成真标签，无闭合时浏览器把后续整篇吞进脚本。
    // 导出 HTML 时按钮会被现有正则一并剔除。
    codespan({ text }: { text: string }) {
      return `<span class="codespan-wrap"><code>${escapeCode(text)}</code>${INLINE_COPY_BTN}</span>`
    },
    // 本地图片：相对/绝对磁盘路径经 asset 协议在应用内加载（http/data 原样交给 WKWebView）
    image({ href, title, text }: { href: string; title?: string | null; text: string }) {
      const t = title ? ` title="${escAttr(title)}"` : ''
      return `<img src="${escAttr(resolveImgSrc(href))}" alt="${escAttr(text || '')}"${t}>`
    },
    // 任务清单复选框：去掉 marked 默认的 disabled，预览里可点击回改源码标记（见 onPreviewTaskToggle）；
    // 勾选态完全由源码渲染驱动，导出 HTML 时再补回 disabled（离线页面无回写能力）。
    checkbox({ checked }: { checked: boolean }) {
      return `<input type="checkbox" class="task-cb"${checked ? ' checked=""' : ''}>`
    },
  },
})

/** 侧边栏里工作目录的树节点（扩展折叠态） */
interface WorkNode extends FsNode {
  open?: boolean
  loaded?: boolean
}

/** 「定位文件到系统文件管理器」菜单项文案：按平台区分（macOS 访达 /
 *  Windows 资源管理器 / 其它 文件管理器），避免在 Windows 上仍写「访达」。检测与 main.ts 一致。 */
const REVEAL_LABEL = (() => {
  const ua = navigator.userAgent.toLowerCase()
  if (ua.includes('mac os') || (navigator.platform || '').toLowerCase().includes('mac')) return '在访达中显示'
  if (ua.includes('windows')) return '在资源管理器中显示'
  return '在文件管理器中显示'
})()

export default defineComponent({
  name: 'EditorView',
  components: { VaultTreeNode, CodeMirrorEditor, EncIcon },
  data() {
    return {
      currentMode: 'split' as 'edit' | 'split' | 'preview',
      moreOpen: false,
      // 自绘右键上下文菜单（标题栏/状态栏/页签栏/页签项/侧边栏共用）；title 仅部分菜单用（如状态栏字号/自动锁定）
      ctxMenu: { visible: false, x: 0, y: 0, items: [] as any[], title: '' },
      secClosed: {} as Record<string, boolean>,
      treeClosed: {} as Record<string, boolean>,
      wordWrap: true,
      syncingScroll: false,

      // 大纲当前定位的标题所在行号（随编辑光标 / 滚动同步）
      outlineActiveLine: 0,

      // 侧边栏宽度（可拖拽调整）
      sidebarWidth: 264,
      _sidebarDragging: false,

      // 分屏比例（编辑器占比，0.2~0.8）
      splitRatio: 0.5,
      _splitDragging: false,

      // 工作目录根（用户选择的系统目录）
      workdirRoot: '' as string,
      // 工作目录树（一层，懒加载子目录）
      workdir: [] as WorkNode[],
      // 已解锁库的树（key = vault id）
      vaultTrees: {} as Record<string, FsNode[]>,

      // 新建文件/文件夹的弹窗
      showNewFile: false,
      newFileName: '',
      newFileVaultId: '',
      newFileDir: '', // 目标文件夹（相对库根，空 = 库根）
      // 工作目录新建文件类型：普通 md / 独立加密文件 .mdl / 单文件库 .mdlb（库内新建恒为加密）
      newFileType: 'plain' as 'plain' | 'encrypted' | 'vault',
      newFilePwd: '',
      newFileConfirm: '',
      showNewFolder: false,
      newFolderName: '',
      newFolderVaultId: '',
      newFolderDir: '',

      // 「另存为」弹窗（保存草稿页签时选择目标）
      showSaveAs: false,
      saveAsMode: 'file' as 'file' | 'vault' | 'plain',
      saveAsName: '',
      saveAsPwd: '',
      saveAsConfirm: '',
      saveAsVaultId: '',
      saveAsPath: '',
      saveAsDir: '',
      saving: false,
      // 「转为加密文件」复用另存为弹窗：标记转换态 + 源明文页签路径
      saveAsConvert: false,
      saveAsSourcePath: '',
      // 转换成功后对原明文文件的处理询问弹窗
      showConvertPrompt: false,
      convertOriginalPath: '',
      convertRemember: false,
      // 单文件 .mdl 页签内解锁（内容区密码框）
      unlockPwd: '',
      unlocking: false,
      vaultUnlockPwd: '',
      vaultUnlocking: false,
      vaultUnlockError: '',
      // 侧边栏拖放排序（鼠标事件实现，源 key + 目标 key + 拖拽中状态）
      dragKey: '' as string,
      dragOverKey: '' as string,
      dragging: false,
      dragMoveY: 0,
      dragList: [] as any[],

      // 拖放/系统打开事件监听器清理句柄
      _unlistenDrag: null as UnlistenFn | null,
      _unlistenOpen: null as UnlistenFn | null,
      /** 冷启动「恢复上次页签」的进行中 Promise，供外部打开（双击/Dock 拖入）串行等它跑完 */
      _sessionRestore: null as Promise<number> | null,
      // 退出（关窗）请求事件监听清理句柄
      _unlistenClose: null as UnlistenFn | null,
      // macOS 锁屏/切换用户事件监听清理句柄
      _unlistenScreenLock: null as UnlistenFn | null,
      // macOS 原生菜单点击事件监听清理句柄
      _unlistenMenu: null as UnlistenFn | null,
      // 窗口聚焦事件监听清理句柄（聚焦时顺带带一次外部修改检查）
      _unlistenFocus: null as UnlistenFn | null,
      // 全局键盘监听清理句柄（Cmd/Ctrl+W 关闭页签）
      _onKeydown: null as ((e: KeyboardEvent) => void) | null,
      _onDocMousedown: null as ((e: MouseEvent) => void) | null,
      _onWinReposition: null as (() => void) | null,
      _minimizeTimer: null as ReturnType<typeof setInterval> | null,
      _sleepTimer: null as ReturnType<typeof setInterval> | null,
      _autoSaveTimer: null as ReturnType<typeof setInterval> | null,
      _onVisChange: null as (() => void) | null,
      _onSidebarMousemove: null as ((e: MouseEvent) => void) | null,
      _onSidebarMouseup: null as ((e: MouseEvent) => void) | null,
      _onSplitMousemove: null as ((e: MouseEvent) => void) | null,
      _onSplitMouseup: null as ((e: MouseEvent) => void) | null,
      // 外部修改监控：「重新加载磁盘版本」的丢弃确认弹窗（监控器为模块级单例，见 lib/filewatch）
      showReloadConfirm: false,
      reloadTarget: '',
      // 拖放高亮（拖入时提示可打开）
      dragOver: false,
      // 拖入资源（图片/文件）询问弹窗：待处理的一组非文档文件路径 + 忙态
      showAssetPrompt: false,
      assetPromptFiles: [] as string[],
      assetBusy: false,
      // 粘贴图片/文件监听句柄（document 捕获阶段，优先于 CodeMirror 默认粘贴）
      _onPaste: null as ((e: ClipboardEvent) => void) | null,

      // 「新建加密库」弹窗（右上角菜单 / 保存选库时创建）
      showCreateVault: false,
      createVaultForm: { name: '', path: '', pwd: '', confirm: '' },
      creatingVault: false,

      // 首次使用：询问是否将 MarkLock 设为 .mdl/.mdlb 默认打开程序
      showAssocPrompt: false,

      // 「复制到加密库」弹窗
      showCopyToVault: false,
      copySourcePath: '',
      copyVaultId: '',
      copyDir: '',
      copyName: '',
      copying: false,

      // 库内文件/目录 重命名（移动）弹窗
      showRename: false,
      renameVaultId: '',
      renameNodePath: '',
      renameIsDir: false,
      renameName: '',
      renaming: false,

      // 库内文件/目录 删除确认弹窗
      showDelete: false,
      deleteVaultId: '',
      deleteNodePath: '',
      deleteIsDir: false,
      deleting: false,

      // 预览模式勾选任务清单的确认弹窗（pendingTaskSrc 为改好勾选的源码，确认后写回并保存；
      // text/checked 仅供弹窗展示：哪条任务、要勾上还是取消）
      showTaskConfirm: false,
      pendingTaskSrc: '',
      pendingTaskText: '',
      pendingTaskChecked: false,

      // 关闭未保存文件的确认弹窗
      showCloseConfirm: false,
      closeConfirmText: '',
      closeConfirmTitle: '未保存的修改',
      closeConfirmDiscard: '不保存',
      closeConfirmSave: '保存',
      pendingClose: null as null | { kind: 'one' | 'others' | 'all'; path?: string; files: OpenFile[] },
      // 当前确认弹窗是针对“退出应用”而非“关闭页签”
      pendingQuit: false,

      // 工作目录（普通明文目录）节点操作：wdOp 标记当前新建/重命名/删除弹窗针对明文文件而非库内
      wdOp: false,
      wdDir: '', // 新建目标目录（绝对路径）
      wdPath: '', // 重命名/删除目标（绝对路径）

      // 修改主密码弹窗（库 / 单文件 .mdl 通用）
      showChangePwd: false,
      changePwdTarget: { id: '', path: '', title: '', isFileVault: false, isPlainMdl: false },
      changePwdForm: { current: '', next: '', confirm: '' },
      changingPwd: false,

      // 全局搜索（已解锁库 + 工作目录明文文件）
      searchQuery: '',
      searchResults: [] as tauri.SearchHit[],
      searching: false,
      searchOpen: false,
      /** 当前键盘选中的结果下标（-1 表示未选中） */
      searchIndex: -1,
      _searchTimer: null as ReturnType<typeof setTimeout> | null,

      // 批量加密弹窗（右上角菜单入口）
      showBatchEncrypt: false,
      bePaths: [] as string[],
      bePassword: '',
      beConfirm: '',
      beMoveOriginals: false,
      beBusy: false,
      beResult: null as tauri.BatchEncryptResult | null,

      // 导入目录到库弹窗
      showImportDir: false,
      idSrcDir: '',
      idTargetKind: 'existing' as 'existing' | 'new',
      idVaultId: '',
      idCreateForm: { name: '', path: '', pwd: '', confirm: '' },
      idBusy: false,
      idResult: null as tauri.ImportResult | null,
    }
  },
  computed: {
    store() {
      return useVaultStore()
    },
    openFiles() {
      return this.store.openFiles
    },
    activePath() {
      return this.store.activePath
    },
    activeFile() {
      return this.store.activeFile
    },
    unlockedVaults() {
      return this.store.unlockedVaults
    },
    /** 「加密库」分组展示的库列表：全部已登记库（锁定后仍保留，便于二次点击解锁） */
    registeredVaults() {
      return this.store.registeredVaults
    },
    favoriteVaults() {
      return this.store.favoriteVaults
    },
    favoriteFileList() {
      return this.store.favoriteFileList
    },
    activeVault() {
      return this.store.activeVault
    },
    /** 侧边栏分组 id（按用户排好的顺序，已过滤隐藏项）。未挂载工作目录时连「工作目录」分组也不显示。 */
    visibleSections() {
      const secs = this.store.visibleSections as string[]
      if (!this.workdirRoot) return secs.filter((id) => id !== 'workdir')
      return secs
    },
    /** 侧边栏空态：按可见分组的实际内容判定——未勾选「打开的文件」时即使有页签，侧边栏也是空的，同样显示「打开库 / 选择工作目录」引导。 */
    sidebarEmpty() {
      const s = this.store.settings
      const hasContent =
        (s.showOpenFiles && this.store.openFiles.length > 0) ||
        (s.showUnlockedVaults && this.store.registeredVaults.length > 0) ||
        (s.showWorkdir && !!this.workdirRoot) ||
        (s.showFavorites && this.store.favorites.length + this.store.favoriteFiles.length > 0) ||
        (s.showRecent && this.store.recentFiles.length + this.store.registeredVaults.length > 0) ||
        (s.showOutline && this.outline.length > 0)
      return !hasContent
    },
    /** 工作目录分组标题：已挂载目录时显示目录名（悬停看完整路径），未挂载时仍显示「工作目录」。 */
    workdirTitle() {
      if (!this.workdirRoot) return '工作目录'
      // 兼容 Windows 反斜杠：去掉尾部分隔符后取最后一段
      const segs = this.workdirRoot.replace(/[\\/]+$/, '').split(/[\\/]/)
      // 盘符根（C:\）等去分隔符后只剩盘符或为空，直接回退显示原路径
      return segs[segs.length - 1] || this.workdirRoot
    },
    /** 当前文件的大纲（Markdown 标题列表）：level 层级、text 标题文本、line 所在源码行号。 */
    outline(): { level: number; text: string; line: number }[] {
      const f = this.activeFile
      if (!f || f.locked || !f.content) return []
      const out: { level: number; text: string; line: number }[] = []
      const lines = f.content.split('\n')
      let fence: string | null = null // 当前代码围栏标记（``` 或 ~~~），围栏内的 # 不是标题
      for (let i = 0; i < lines.length; i++) {
        const raw = lines[i]
        const fm = raw.trim().match(/^(`{3,}|~{3,})/)
        if (fm) {
          if (!fence) fence = fm[1][0]
          else if (fm[1][0] === fence) fence = null
          continue
        }
        if (fence) continue
        const m = raw.match(/^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/)
        if (!m) continue
        out.push({ level: m[1].length, text: (m[2] || '').trim() || '（无标题）', line: i + 1 })
      }
      return out
    },
    activeFileName() {
      return this.activeFile?.name || '未打开文件'
    },
    /** 当前文件是否因库锁定而不可读（仅库内文件；单文件 .mdl 走页签内密码框，不走这里） */
    isActiveLocked() {
      const f = this.activeFile
      if (!f) return false
      // 单文件加密（vaultId 即文件路径）：永远用页签内密码框解锁
      if (f.vaultId === f.path) return false
      const v = this.store.recent.find((v) => v.id === f.vaultId)
      return v ? !v.unlocked : false
    },
    /** 当前激活的单文件 .mdl 页签是否处于锁定态（显示页签内密码框） */
    isActiveFileLocked() {
      return !!this.activeFile && this.activeFile.locked
    },
    // 当前编辑内容（双向绑定到 textarea）
    activeContent: {
      get(): string {
        return this.activeFile?.content ?? ''
      },
      set(val: string) {
        const f = this.activeFile
        if (!f) return
        // 内容未变则不记脏：外部修改自动重新加载时，CodeMirror 会把新 doc 原样回吐一次，
        // 若不过滤会把刚加载的磁盘版本误标为「未保存」。
        if (f.content === val) return
        f.content = val
        this.store.markDirty(f.path)
      },
    },
    unlockedCount() {
      return this.unlockedVaults.length
    },
    /** 「加密库」分组标题计数：已登记库总数（含锁定与解锁）。 */
    vaultCount() {
      return this.registeredVaults.length
    },
    /** 是否存在可锁定目标：已解锁的库，或处于解锁态的单文件加密页签（.mdl）；都没有则不显示锁定按钮 */
    hasLockables() {
      if (this.unlockedCount > 0) return true
      return this.openFiles.some((f) => f.encrypted && f.vaultId === f.path && !f.locked)
    },
    lineCount() {
      return this.activeFile ? this.activeFile.content.split('\n').length : 1
    },
    renderedPreview() {
      return this.renderMarkdown(this.activeFile?.content ?? '')
    },
    autoLockLabel() {
      const s = this.store.settings.autoLockSecs
      if (s <= 0) return '已关闭'
      const m = Math.floor(s / 60)
      const sec = s % 60
      return m > 0 ? `${m} 分钟` : `${sec} 秒`
    },
    /** 另存为密码强度（4 格） */
    saveAsStrength(): number {
      const p = this.saveAsPwd
      if (p.length >= 12 && /[A-Z]/.test(p) && /[0-9]/.test(p) && /[^A-Za-z0-9]/.test(p)) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
    /** 工作目录新建加密文件/单文件库的密码强度（与另存为口径一致，4 格）。 */
    newFileStrength(): number {
      const p = this.newFilePwd
      if (p.length >= 12 && /[A-Z]/.test(p) && /[0-9]/.test(p) && /[^A-Za-z0-9]/.test(p)) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
    /** 当前另存为目标库的所有文件夹（相对库根路径，递归扁平化）。 */
    saveAsFolders(): string[] {
      const roots = this.vaultTrees[this.saveAsVaultId] || []
      const out: string[] = []
      const walk = (nodes: FsNode[], prefix: string) => {
        for (const n of nodes) {
          if (!n.is_dir) continue
          const rel = prefix ? `${prefix}/${n.name}` : n.name
          out.push(rel)
          if (n.children && n.children.length) walk(n.children, rel)
        }
      }
      walk(roots, '')
      return out
    },
    /** 新建加密库的密码强度（4 格）。 */
    createVaultStrength(): number {
      const p = this.createVaultForm.pwd
      if (p.length >= 12 && /[A-Z]/.test(p) && /[0-9]/.test(p) && /[^A-Za-z0-9]/.test(p)) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
    /** 批量加密弹窗的密码强度（与新建库口径一致）。 */
    beStrength(): number {
      const p = this.bePassword
      if (p.length >= 12 && /[A-Z]/.test(p) && /[0-9]/.test(p) && /[^A-Za-z0-9]/.test(p)) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
    /** 导入目录弹窗（新建库时）的密码强度。 */
    idCreateStrength(): number {
      const p = this.idCreateForm.pwd
      if (p.length >= 12 && /[A-Z]/.test(p) && /[0-9]/.test(p) && /[^A-Za-z0-9]/.test(p)) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
    /** 修改主密码弹窗的新密码强度（与新建库口径一致）。 */
    changePwdStrength(): number {
      const p = this.changePwdForm.next
      if (p.length >= 12 && /[A-Z]/.test(p) && /[0-9]/.test(p) && /[^A-Za-z0-9]/.test(p)) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
    /** 「复制到库」目标库的文件夹列表（相对库根，递归扁平化）。 */
    copyFolders(): string[] {
      const roots = this.vaultTrees[this.copyVaultId] || []
      const out: string[] = []
      const walk = (nodes: FsNode[], prefix: string) => {
        for (const n of nodes) {
          if (!n.is_dir) continue
          const rel = prefix ? `${prefix}/${n.name}` : n.name
          out.push(rel)
          if (n.children && n.children.length) walk(n.children, rel)
        }
      }
      walk(roots, '')
      return out
    },
    /** 当前激活页签是否位于某个库内（vaultId 为库 id 且非单文件 .mdl）。 */
    isActiveInVault(): boolean {
      const f = this.activeFile
      if (!f) return false
      return this.isVaultFile(f)
    },
    /** macOS 原生「视图」菜单勾选态：模式单选 + 显示开关，供 watcher 推送到后端。 */
    nativeMenuChecks(): Record<string, boolean> {
      const s = this.store.settings
      return {
        'menu:mode-edit': this.currentMode === 'edit',
        'menu:mode-split': this.currentMode === 'split',
        'menu:mode-preview': this.currentMode === 'preview',
        'menu:toggle-tabs': !!s.showTabs,
        'menu:toggle-toolbar': !!s.showToolbar,
        'menu:toggle-statusbar': !!s.showStatusbar,
        'menu:toggle-sidebar': !!s.showSidebar,
        'menu:sec-open-files': !!s.showOpenFiles,
        'menu:sec-vaults': !!s.showUnlockedVaults,
        'menu:sec-workdir': !!s.showWorkdir,
        'menu:sec-favorites': !!s.showFavorites,
        'menu:sec-recent': !!s.showRecent,
        'menu:sec-outline': !!s.showOutline,
      }
    },
  },
  watch: {
    // 切到锁定的 .mdl 页签后自动聚焦密码框，可直接输入；
    // 但自动锁定/⌘L 锁定也会触发这里，若用户正在编辑则不抢焦点
    isActiveFileLocked(v: boolean) {
      if (v && !this.isEditingFocused()) this.focusPwdInput('filePwdInput')
    },
    // 页签所属库锁定时出现就地解锁框，自动聚焦主密码框
    isActiveLocked(v: boolean) {
      if (v && !this.isEditingFocused()) this.focusPwdInput('vaultPwdInput')
    },
    // 「保存为新文件」弹窗：改名时同步「保存位置」里的文件名（保留目录与原扩展名），
    // 否则位置残留旧文件名会导致保存出的文件名与输入不符
    saveAsName() {
      this.syncSaveAsPathName()
    },
    // 两个锁定页签之间互切时上面的状态不变，靠 activePath 补一次聚焦；
    // 切到未锁定的页签则聚焦编辑器（实例常驻，撤销历史保留）
    'store.activePath'() {
      this.focusPwdIfLocked()
      const f = this.activeFile
      if (f && !f.locked && !this.isActiveLocked && !this.isEditingFocused()) {
        nextTick(() => this.activeEditor()?.focus?.())
      }
      // 切页签时在侧边栏定位选中文件：工作目录树 / 加密库树展开祖先并滚入可见
      this.revealActiveInSidebar()
      // 预览区滚动同步到新页签：pane-preview 是全局唯一可滚动 div，v-html 换内容时
      // scrollTop 会残留上一个文件的偏移，导致新打开的文件视口停在中间。这里把预览滚到
      // 「新页签编辑器视口顶行对应的位置」：新文件顶行=1 等价于回到顶部，已打开页签切回则
      // 匹配编辑器保留的滚动位置，跟双向同步语义一致。占住 syncingScroll 避免这次程序化
      // 滚动通过 onPreviewScroll 反向 revealLine 到新页签编辑器顶行，破坏源码侧的滚动保留。
      nextTick(() => {
        const pane = this.$refs.previewPane as HTMLElement | null
        if (!pane) return
        this.syncingScroll = true
        const cm = this.activeEditor()
        const topLine = typeof cm?.getTopViewportLine === 'function' ? cm.getTopViewportLine() : 1
        this._previewScrollToLine(topLine)
        setTimeout(() => { this.syncingScroll = false }, 60)
      })
    },
    // 新建文件 / 新建文件夹弹窗：用户显式点击触发，打开后自动聚焦名称输入框
    showNewFile(v: boolean) {
      if (v) this.focusRef('newFileNameInput')
    },
    showNewFolder(v: boolean) {
      if (v) this.focusRef('newFolderNameInput')
    },
    // 重命名弹窗：打开后自动聚焦名称输入框并选中预填内容，便于直接改
    showRename(v: boolean) {
      if (v) this.focusRef('renameNameInput')
    },
    // 外部修改监控开关：开则起轮询，关则停轮询并清掉已显示的提醒角标
    'store.settings.watchExternalChanges'() {
      this.applyExternalWatch()
    },
    // 自动保存间隔变更：重建定时器（间隔为 0 时仅停止）
    'store.settings.autoSaveSecs'() {
      this.applyAutoSave()
    },
    // 字号变更：写 CSS 变量后让全部 CodeMirror 实例重新测量行高（预览区纯 CSS 自动生效）
    'store.settings.editorFontSize'() {
      this.applyEditorFontSize()
      this.$nextTick(() => {
        const refs = this.$refs.cmEditor as any
        const list = Array.isArray(refs) ? refs : refs ? [refs] : []
        for (const cm of list) cm?.requestMeasure?.()
      })
    },
    // 预览内容栏最大宽变更：纯 CSS 生效，重写 --editor-max-width 变量即可
    'store.settings.editorMaxWidth'() {
      this.applyEditorMaxWidth()
    },
    // macOS 原生菜单同步：视图勾选态（nativeMenuChecks）或库/工作目录状态变化时，推送到系统菜单
    // （后端据此改勾选 + 改「文件」菜单里库/工作目录开关项的标题）。非 macOS 为无害空操作。
    nativeMenuChecks: {
      handler() {
        this.pushNativeMenu()
      },
      immediate: true,
    },
    workdirRoot() {
      this.pushNativeMenu()
    },
    registeredVaults() {
      this.pushNativeMenu()
    },
  },
  mounted() {
    // 恢复上次编辑器模式
    this.currentMode = this.store.settings.editorMode || 'split'
    // 应用编辑器/预览字号（CSS 变量挂在 documentElement，编辑器与预览共用）
    this.applyEditorFontSize()
    // 应用预览内容栏最大宽（--editor-max-width，驱动 .md 的 max-width）
    this.applyEditorMaxWidth()
    // 恢复侧边栏宽度
    const savedWidth = localStorage.getItem('marklock.sidebarWidth')
    if (savedWidth) {
      const w = parseInt(savedWidth, 10)
      if (w >= 160 && w <= 500) this.sidebarWidth = w
    }
    // 恢复分屏比例
    const savedRatio = localStorage.getItem('marklock.splitRatio')
    if (savedRatio) {
      const r = parseFloat(savedRatio)
      if (r >= 0.2 && r <= 0.8) this.splitRatio = r
    }
    // 最小化自动锁定：每 500ms 轮询窗口最小化状态（受「窗口最小化时锁定」开关控制）
    let lastMinimized = false
    this._minimizeTimer = setInterval(async () => {
      try {
        const min = await getCurrentWebviewWindow().isMinimized()
        if (min && !lastMinimized && this.store.settings.lockOnMinimize) {
          // 从非最小化变为最小化，触发锁定
          this.store.lockAll().catch(() => {})
        }
        lastMinimized = min
      } catch {}
    }, 500)
    // 系统休眠 / 锁屏锁定：休眠会挂起定时器，唤醒后心跳出现大间隔；页面隐藏后恢复同理
    const SUSPEND_GAP_MS = 5000
    let lastBeat = Date.now()
    this._sleepTimer = setInterval(() => {
      const now = Date.now()
      if (this.store.settings.lockOnSleep && now - lastBeat > SUSPEND_GAP_MS) {
        this.store.lockAll().catch(() => {})
      }
      lastBeat = now
    }, 1000)
    let hiddenAt = 0
    this._onVisChange = () => {
      if (document.visibilityState === 'hidden') {
        hiddenAt = Date.now()
      } else if (hiddenAt) {
        const hidden = Date.now() - hiddenAt
        hiddenAt = 0
        if (this.store.settings.lockOnSleep && hidden > SUSPEND_GAP_MS) this.store.lockAll().catch(() => {})
        // 回到前台：立刻查一轮文件戳记与目录条目，不等下一个轮询间隔（切回本应用时最关心外部改动）
        externalChanges.check().catch(() => {})
        workdirChanges.check().catch(() => {})
      }
    }
    document.addEventListener('visibilitychange', this._onVisChange)
    // 粘贴图片/文件：在 document 捕获阶段拦截（优先于 CodeMirror 默认文本粘贴）。
    // 仅在剪贴板确含文件/图片且当前是可编辑页签时接管，否则放行让 CM 正常粘贴文本。
    this._onPaste = (e: ClipboardEvent) => this.handlePaste(e)
    document.addEventListener('paste', this._onPaste, true)
    // 恢复侧边栏分组/树节点的展开收起状态
    const secSaved = localStorage.getItem('marklock.secClosed')
    if (secSaved) { try { this.secClosed = JSON.parse(secSaved) } catch {} }
    const treeSaved = localStorage.getItem('marklock.treeClosed')
    if (treeSaved) { try { this.treeClosed = JSON.parse(treeSaved) } catch {} }
    // 恢复上次的工作目录（暂存）
    const saved = localStorage.getItem('marklock.workdir')
    if (saved) {
      this.workdirRoot = saved
      this.refreshWorkdir()
    }
    // 恢复已解锁库的树
    this.unlockedVaults.forEach((v) => this.refreshVaultTree(v.id, v.path))
    // 刚从解锁页返回：自动展开该库的根层（仅根目录，内部文件夹收起；解锁时由 store 置位）
    if (this.store.pendingExpandVault) {
      this.openVaultToRoot(this.store.pendingExpandVault)
    }
    // 恢复上次打开的页签（含工作目录外的文件）；记下 Promise，外部打开需等它完成
    this._sessionRestore = this.store.restoreSession()
    // 定时回收过期会话（自动锁定）
    setInterval(() => this.reapExpired(), 30 * 1000)
    // 文件/目录拖放：用 Tauri 内置拖放事件（payload 为 {type,paths,position}，
    // enter/over 显示高亮，drop 路由处理，leave 取消高亮）。
    // 外部拖入只在右侧主面板触发「打开」；侧边栏区域留给列表项拖放排序。
    getCurrentWebviewWindow()
      .onDragDropEvent((evt) => {
        const payload = evt.payload as { type: string; paths?: string[]; position?: { x: number; y: number } }
        const pos = payload.position
        // 侧边栏可见且鼠标落在侧边栏内：外部拖入不响应（交给列表排序）
        const inSidebar = this.store.settings.showSidebar && pos && pos.x < this.sidebarWidth
        if (payload.type === 'enter' || payload.type === 'over') {
          this.dragOver = !inSidebar
        } else if (payload.type === 'drop') {
          this.dragOver = false
          if (!inSidebar && payload.paths && payload.paths.length) this.routeDroppedPaths(payload.paths)
        } else {
          this.dragOver = false
        }
      })
      .then((un) => {
        this._unlistenDrag = un
      })
    // 系统级「打开文件」：macOS Finder 双击/打开方式、拖到 Dock 图标。后端只发信号，
    // 路径统一从 Rust 缓存拉取（drain），避免冷启动丢事件与重复打开。
    listen('marklock://open-paths', () => {
      this.drainPendingOpenPaths()
    }).then((un) => {
      this._unlistenOpen = un
    })
    // 冷启动：拉取后端在监听器就绪前已缓存的待打开路径（双击打开的场景）。
    this.drainPendingOpenPaths()
    // 退出（点红叉 / Cmd+Q）：后端已 prevent_close 并发此事件，由前端判断未保存并确认后退出
    listen('marklock://close-requested', () => {
      this.handleAppClose()
    }).then((un) => {
      this._unlistenClose = un
    })
    // macOS：系统锁屏 / 切换用户 → 后端探测到会话离开控制台后发此事件，
    // 前端按「系统休眠 / 锁屏时锁定」开关决定是否全部锁定（覆盖前端启发式测不到的纯锁屏场景）。
    listen('marklock://screen-locked', () => {
      if (this.store.settings.lockOnSleep) this.store.lockAll().catch(() => {})
    }).then((un) => {
      this._unlistenScreenLock = un
    })
    // macOS 原生菜单：后端把 menu:* 点击统一转发到此事件，前端按 id 分派到既有方法。
    listen<string>('marklock://menu', (e) => {
      this.runMenuAction(e.payload)
    }).then((un) => {
      this._unlistenMenu = un
    })
    // 窗口重新获得焦点：立刻推外部修改与目录增减检查，不等下一个轮询间隔（从其它程序切回来时最关心）
    getCurrentWebviewWindow()
      .onFocusChanged(({ payload }) => {
        if (payload) {
          externalChanges.check().catch(() => {})
          workdirChanges.check().catch(() => {})
        }
      })
      .then((un) => {
        this._unlistenFocus = un
      })
    // 解锁回来后，打开解锁前暂存的文件（拖入库内文件/单文件库的场景）
    if (this.store.pendingOpen) {
      const p = this.store.pendingOpen
      const v = this.store.recent.find((x) => x.id === p.vaultId)
      if (v && v.unlocked) {
        this.store.pendingOpen = null
        if (v.isFileVault && p.path.includes('#')) {
          // 单文件库内文件：path 是 `库路径#相对路径`
          const rel = p.path.split('#').slice(1).join('#')
          this.store
            .openFileVaultFile(p.vaultId, rel, p.name)
            .catch((err) => message.error(String(err)))
        } else {
          this.store
            .openFile(p.vaultId, p.path, p.name)
            .catch((err) => message.error(String(err)))
        }
      }
    }
    // 全局快捷键：Cmd/Ctrl+W 关闭当前页签（而不是关闭整个窗口）；Cmd/Ctrl+F 打开编辑器搜索
    this._onKeydown = (e: KeyboardEvent) => {
      // 新建加密库/设为默认打开程序弹窗：按 Esc 取消。Ant 的 keyboard 仅在焦点位于弹窗内时生效，
      // 这里做兜底，确保焦点不在弹窗内时 Esc 也能取消。
      if (e.key === 'Escape' && (this.showCreateVault || this.showAssocPrompt)) {
        this.showCreateVault = false
        this.showAssocPrompt = false
        return
      }
      // 批量加密 / 导入目录弹窗：busy 时不关闭（避免中途取消造成不一致），完成后（有 result）Esc 直接关。
      if (e.key === 'Escape') {
        if (this.showBatchEncrypt && !this.beBusy) { this.showBatchEncrypt = false; return }
        if (this.showImportDir && !this.idBusy) { this.showImportDir = false; return }
      }
      // Esc 关闭右键上下文菜单
      if (e.key === 'Escape' && this.ctxMenu.visible) {
        this.closeContextMenu()
        return
      }
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'w' || e.key === 'W')) {
        // 有打开页签才拦截，否则放行（允许系统关闭窗口）
        if (this.store.openFiles.length > 0) {
          e.preventDefault()
          const cur = this.store.activePath
          if (cur) this.closeFile(cur)
        }
      }
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && !e.altKey && (e.key === 't' || e.key === 'T')) {
        // Cmd+Shift+T：撤销关闭（重开最近关闭的页签，与主流浏览器一致）
        e.preventDefault()
        this.reopenLastClosed()
      }
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'f' || e.key === 'F')) {
        e.preventDefault()
        // 切换到编辑或分屏模式，确保编辑器可见
        if (this.currentMode === 'preview') this.setMode('split')
        // 等 DOM 更新后再聚焦并打开搜索
        nextTick(() => {
          const cm = this.activeEditor()
          if (cm) {
            const el = cm.editorRef as HTMLElement | undefined
            el?.focus()
            cm.openSearch()
          }
        })
      }
      // Cmd+Shift+F：打开侧边栏并聚焦全局搜索
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && !e.altKey && (e.key === 'f' || e.key === 'F')) {
        e.preventDefault()
        if (!this.store.settings.showSidebar) this.store.updateSettings({ showSidebar: true })
        nextTick(() => {
          const input = document.querySelector('.sb-search .input') as HTMLInputElement
          if (input) {
            input.focus()
            if (this.searchQuery.trim()) this.searchOpen = true
          }
        })
      }
      // Cmd+E：循环切换 编辑 → 分屏 → 预览 → 编辑
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'e' || e.key === 'E')) {
        e.preventDefault()
        const cycle: Record<string, 'edit' | 'split' | 'preview'> = { edit: 'split', split: 'preview', preview: 'edit' }
        this.setMode(cycle[this.currentMode] || 'split')
      }
      // Cmd+L：全部锁定
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'l' || e.key === 'L')) {
        e.preventDefault()
        this.lockAll()
      }
      // Cmd+N：新建文件（同页签 + 号）
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'n' || e.key === 'N')) {
        e.preventDefault()
        this.openNewDraft()
      }
      // Cmd+O：打开文件（md/txt 等文本或加密文件/库，同主菜单「打开文件…」）
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'o' || e.key === 'O')) {
        e.preventDefault()
        this.openFileAction()
      }
      // Cmd+J：切换侧边栏（Cmd+B 保留为编辑器加粗）
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'j' || e.key === 'J')) {
        e.preventDefault()
        this.toggleSidebar()
      }
      // Cmd/Ctrl + = / - / 0：放大、缩小、重置字号（2px 步进，10–28，编辑与预览联动）。
      // macOS 上 Cmd+= 实际产生 '+'（Shift 组合），故两个键新都收；不加 !e.shiftKey 守卫
      if ((e.metaKey || e.ctrlKey) && !e.altKey && (e.key === '=' || e.key === '+' || e.key === '-' || e.key === '_' || e.key === '0')) {
        e.preventDefault()
        if (e.key === '0') this.setEditorFontSize(14)
        else this.setEditorFontSize(this.store.settings.editorFontSize + ((e.key === '-' || e.key === '_') ? -2 : 2))
      }
    }
    window.addEventListener('keydown', this._onKeydown)
    // 点击「更多」菜单或搜索面板外部时自动收起
    this._onDocMousedown = (e: MouseEvent) => {
      // 右键菜单：点击其外部任意处即关闭
      if (this.ctxMenu.visible) {
        const ct = e.target as HTMLElement | null
        if (!(ct && ct.closest && ct.closest('.ctx-menu'))) this.closeContextMenu()
      }
      if (!this.moreOpen && !this.searchOpen) return
      const t = e.target as HTMLElement | null
      if (t && t.closest && t.closest('.menu-wrap, .sb-search')) return
      this.moreOpen = false
      this.searchOpen = false
      this.searchIndex = -1
    }
    document.addEventListener('mousedown', this._onDocMousedown)
    // 窗口尺寸变化 / 滚动时关闭右键菜单，避免位置错位
    this._onWinReposition = () => this.closeContextMenu()
    window.addEventListener('resize', this._onWinReposition)
    window.addEventListener('scroll', this._onWinReposition, true)
    // 首次使用：探测 .mdl/.mdlb 默认打开程序，非本应用时询问是否设为默认
    this.checkFileAssocPrompt()
    // 外部修改监控（按设置开关启动；页签集合每轮动态取，新打开/关闭页签自动纳入）
    this.applyExternalWatch()
    // 工作目录增减监控：别的程序往目录里增删文件/文件夹时及时刷新树（与上面的设置开关无关，始终生效）
    workdirChanges.attach({
      targets: () => this.workdirWatchTargets(),
      onChange: (dirs) => this.onWorkdirLayoutChange(dirs),
    })
    // 自动保存定时（按设置间隔轮巡 dirty 页签静默写盘）
    this.applyAutoSave()
    // 消费暂存的退出请求（在设置/解锁页按 Cmd+Q 或点红叉 → App 兜底跳回编辑器）：
    // 放在 onMounted 末尾确保 close-requested 监听已注册，后续再次关闭不丢事件；
    // handleAppClose 会检查未保存文件，有则弹确认、无则直接退出。
    if (this.store.pendingQuit) {
      this.store.pendingQuit = false
      this.handleAppClose()
    }
  },
  unmounted() {
    if (this._unlistenDrag) this._unlistenDrag()
    // 只摘处理器：定时器与戳记基线跨路由保持，回到编辑器时第一轮比对能补上离开期间的改动
    externalChanges.detach()
    workdirChanges.detach()
    if (this._unlistenOpen) this._unlistenOpen()
    if (this._unlistenClose) this._unlistenClose()
    if (this._unlistenScreenLock) this._unlistenScreenLock()
    if (this._unlistenMenu) this._unlistenMenu()
    if (this._unlistenFocus) this._unlistenFocus()
    if (this._onKeydown) window.removeEventListener('keydown', this._onKeydown)
    if (this._onDocMousedown) document.removeEventListener('mousedown', this._onDocMousedown)
    if (this._onWinReposition) {
      window.removeEventListener('resize', this._onWinReposition)
      window.removeEventListener('scroll', this._onWinReposition, true)
    }
    if (this._minimizeTimer) clearInterval(this._minimizeTimer)
    if (this._sleepTimer) clearInterval(this._sleepTimer)
    if (this._autoSaveTimer) clearInterval(this._autoSaveTimer)
    if (this._onVisChange) document.removeEventListener('visibilitychange', this._onVisChange)
    if (this._onPaste) document.removeEventListener('paste', this._onPaste, true)
    if (this._onSidebarMousemove) document.removeEventListener('mousemove', this._onSidebarMousemove)
    if (this._onSidebarMouseup) document.removeEventListener('mouseup', this._onSidebarMouseup)
    if (this._onSplitMousemove) document.removeEventListener('mousemove', this._onSplitMousemove)
    if (this._onSplitMouseup) document.removeEventListener('mouseup', this._onSplitMouseup)
  },
  methods: {
    /** 聚焦锁定态面板里的密码框（等面板渲染完成） */
    focusPwdInput(refName: string) {
      this.focusRef(refName)
    },
    /** 通用聚焦：弹窗内容经 Portal 异步挂载，用 rAF 有界重试直到输入框就绪后聚焦。 */
    focusRef(refName: string, attempt = 0) {
      this.$nextTick(() => {
        const el = this.$refs[refName] as HTMLInputElement | undefined
        if (el) {
          el.focus()
          // 文本类输入框自动全选预填内容（密码框无 value 不受影响）
          if (typeof el.select === 'function' && el.value) el.select()
          return
        }
        // 弹窗 Portal 可能需多帧才挂载，最多重试 ~12 帧
        if (attempt < 12) requestAnimationFrame(() => this.focusRef(refName, attempt + 1))
      })
    },
    /** 当前页签若在等待密码输入则自动聚焦 */
    focusPwdIfLocked() {
      if (this.isActiveFileLocked) this.focusPwdInput('filePwdInput')
      else if (this.isActiveLocked) this.focusPwdInput('vaultPwdInput')
    },
    // ---------- 格式化工具栏（委托给编辑器组件内的 CodeMirror 实例） ----------
    /** 把字号设置写进 CSS 变量 --editor-font-size（CodeMirror 主题与 .md 预览都引用它） */
    applyEditorFontSize() {
      document.documentElement.style.setProperty('--editor-font-size', `${this.store.settings.editorFontSize}px`)
    },
    /** 把预览内容栏最大宽写进 CSS 变量 --editor-max-width；0（或非法值）表示铺满面板宽度（none） */
    applyEditorMaxWidth() {
      const w = Number(this.store.settings.editorMaxWidth) || 0
      document.documentElement.style.setProperty('--editor-max-width', w > 0 ? `${w}px` : 'none')
    },
    /** 设置字号（限幅 10–28 并取偶数，与 2px 步进对齐）；变更由 watcher 落实到编辑器与预览 */
    setEditorFontSize(size: number) {
      const v = Math.min(28, Math.max(10, Math.round(size / 2) * 2))
      if (v === this.store.settings.editorFontSize) return
      this.store.updateSettings({ editorFontSize: v })
    },
    /** 当前激活页签的编辑器实例（v-for 多实例下 ref 是数组，按 path 对齐 openFiles 顺序取） */
    activeEditor(): any {
      const refs = this.$refs.cmEditor as any
      if (!refs) return null
      if (!Array.isArray(refs)) return refs
      const i = this.openFiles.findIndex((f) => f.path === this.activePath)
      return i >= 0 ? refs[i] ?? null : null
    },
    /** 按路径取页签内容（供每个常驻编辑器实例绑定）；锁定的页签返回空串，解锁后 v-else 才挂载实例 */
    getContentFor(path: string): string {
      const f = this.openFiles.find((x) => x.path === path)
      if (!f || f.locked) return ''
      return f.content ?? ''
    },
    /** 按路径写回页签内容，语义与 activeContent setter 一致（内容未变不记脏） */
    setContentFor(path: string, val: string) {
      const f = this.openFiles.find((x) => x.path === path)
      if (!f) return
      if (f.content === val) return
      f.content = val
      this.store.markDirty(f.path)
    },
    fmtHeading(level: number) {
      this.activeEditor()?.setHeading(level)
    },
    fmtWrap(before: string, after: string, placeholder: string) {
      this.activeEditor()?.wrapSelection(before, after, placeholder)
    },
    fmtCodeBlock() {
      this.activeEditor()?.insertCodeBlock()
    },
    fmtPrefix(prefix: string) {
      this.activeEditor()?.toggleLinePrefix(prefix)
    },
    fmtTable() {
      this.activeEditor()?.insertBlock('| 列 1 | 列 2 | 列 3 |\n| --- | --- | --- |\n|  |  |  |')
    },
    fmtHr() {
      this.activeEditor()?.insertBlock('---')
    },
    fmtLink(image: boolean) {
      this.activeEditor()?.insertLink(image)
    },
    /** 分派 macOS 原生菜单点击（marklock://menu 事件，payload 为 menu:* id）。
     *  全部复用既有方法；菜单项刻意不带 accelerator，故不会与前端全局快捷键 / CodeMirror keymap 冲突。 */
    runMenuAction(id: string) {
      switch (id) {
        case 'menu:prefs': this.$router.push('/settings'); break
        case 'menu:new-file': this.openNewDraft(); break
        case 'menu:new-vault': this.openCreateVault(); break
        case 'menu:open-file': this.openFileAction(); break
        case 'menu:save': this.saveActive(); break
        case 'menu:close-tab': { const p = this.store.activePath; if (p) this.closeFile(p); break }
        case 'menu:batch-encrypt': this.openBatchEncrypt(); break
        case 'menu:import-dir': this.openImportDir(); break
        case 'menu:export-html': this.exportHtml(); break
        case 'menu:find': {
          // 预览模式下无编辑器实例，先切回分屏再定位搜索面板。
          if (this.currentMode === 'preview') this.setMode('split')
          nextTick(() => {
            const cm = this.activeEditor()
            if (cm) {
              const el = cm.editorRef as HTMLElement | undefined
              el?.focus()
              cm.openSearch()
            }
          })
          break
        }
        case 'menu:find-global': {
          if (!this.store.settings.showSidebar) this.store.updateSettings({ showSidebar: true })
          nextTick(() => {
            const input = document.querySelector('.sb-search .input') as HTMLInputElement
            if (input) {
              input.focus()
              if (this.searchQuery.trim()) this.searchOpen = true
            }
          })
          break
        }
        case 'menu:h1': this.fmtHeading(1); break
        case 'menu:h2': this.fmtHeading(2); break
        case 'menu:h3': this.fmtHeading(3); break
        case 'menu:bold': this.fmtWrap('**', '**', '加粗'); break
        case 'menu:italic': this.fmtWrap('*', '*', '斜体'); break
        case 'menu:strike': this.fmtWrap('~~', '~~', '删除'); break
        case 'menu:code': this.fmtWrap('`', '`', 'code'); break
        case 'menu:codeblock': this.fmtCodeBlock(); break
        case 'menu:quote': this.fmtPrefix('> '); break
        case 'menu:ul': this.fmtPrefix('- '); break
        case 'menu:task': this.fmtPrefix('- [ ] '); break
        case 'menu:table': this.fmtTable(); break
        case 'menu:link': this.fmtLink(false); break
        case 'menu:image': this.fmtLink(true); break
        case 'menu:hr': this.fmtHr(); break
        case 'menu:mode-edit': this.setMode('edit'); break
        case 'menu:mode-split': this.setMode('split'); break
        case 'menu:mode-preview': this.setMode('preview'); break
        case 'menu:toggle-tabs': this.store.updateSettings({ showTabs: !this.store.settings.showTabs }); break
        case 'menu:toggle-toolbar': this.store.updateSettings({ showToolbar: !this.store.settings.showToolbar }); break
        case 'menu:toggle-statusbar': this.store.updateSettings({ showStatusbar: !this.store.settings.showStatusbar }); break
        case 'menu:toggle-sidebar': this.toggleSidebar(); break
        case 'menu:sec-open-files': this.store.updateSettings({ showOpenFiles: !this.store.settings.showOpenFiles }); break
        case 'menu:sec-vaults': this.store.updateSettings({ showUnlockedVaults: !this.store.settings.showUnlockedVaults }); break
        case 'menu:sec-workdir': this.store.updateSettings({ showWorkdir: !this.store.settings.showWorkdir }); break
        case 'menu:sec-favorites': this.store.updateSettings({ showFavorites: !this.store.settings.showFavorites }); break
        case 'menu:sec-recent': this.store.updateSettings({ showRecent: !this.store.settings.showRecent }); break
        case 'menu:sec-outline': this.store.updateSettings({ showOutline: !this.store.settings.showOutline }); break
        case 'menu:vault-open': this.openVaultAction(); break
        case 'menu:vault-close': this.closeAllVaults(); break
        case 'menu:workdir-toggle':
          // 工作目录唯一，保持 打开↔关闭 切换。
          if (this.workdirRoot) this.closeWorkdir(); else this.chooseWorkdir()
          break
        case 'menu:lock-all': this.lockAll(); break
      }
    },
    /** 把当前视图勾选态 + 库/工作目录状态推送到 macOS 原生菜单（后端据此回写勾选与开关项标题；非 macOS 为空操作）。 */
    pushNativeMenu() {
      tauri
        .syncNativeMenu(this.nativeMenuChecks, this.registeredVaults.length > 0, !!this.workdirRoot)
        .catch(() => {})
    },
    /** 焦点是否正在可编辑区（编辑器/输入框）：watcher 在重渲染前触发，
     *  此时 activeElement 还是旧面板的元素，可用于判断用户是否正在输入 */
    isEditingFocused() {
      const el = document.activeElement as HTMLElement | null
      if (!el) return false
      return !!el.closest('.cm-editor') || el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable
    },
    setMode(mode: 'edit' | 'split' | 'preview') {
      this.currentMode = mode
      this.store.updateSettings({ editorMode: mode })
    },
    /** 预览区双击切换 split/preview（可在设置中关闭）：落在代码块复制按钮上时跳过，避免误切视图。 */
    onPreviewDblClick(e: MouseEvent) {
      if (!this.store.settings.previewDblClickToggle) return
      if ((e.target as HTMLElement | null)?.closest('.code-copy-btn')) return
      // 连点复选框是连续勾选/取消，不应误触视图切换
      if ((e.target as HTMLElement | null)?.closest('input.task-cb')) return
      this.setMode(this.currentMode === 'split' ? 'preview' : 'split')
    },
    onEditorScroll(line: number) {
      if (this.syncingScroll) return
      this.syncingScroll = true
      this._previewScrollToLine(line)
      this._outlineSyncActive(line)
      setTimeout(() => { this.syncingScroll = false }, 60)
    },
    /** 把大纲高亮同步到视口顶部行所属的最后一个标题。 */
    _outlineSyncActive(line: number) {
      const list = this.outline
      if (!list.length) return
      let cur = list[0].line
      for (const h of list) {
        if (h.line <= line) cur = h.line
        else break
      }
      this.outlineActiveLine = cur
    },
    /** 点击大纲项：光标定位到标题行并滚入视口，同步滚动预览区。 */
    outlineGoto(item: { text: string; line: number }) {
      this.outlineActiveLine = item.line
      const cm = this.activeEditor()
      cm?.setCursorToLine?.(item.line)
      this._previewScrollToLine(item.line)
    },
    onPreviewScroll() {
      if (this.syncingScroll) return
      this.syncingScroll = true
      const pane = this.$refs.previewPane as HTMLElement
      const cm = this.activeEditor()
      const blocks = this._syncBlocks()
      if (pane && cm && blocks.length) {
        const top = pane.scrollTop
        let i = 0
        for (let k = 0; k < blocks.length; k++) { if (blocks[k].top <= top) i = k; else break }
        const cur = blocks[i]
        const next = blocks[i + 1]
        let line = cur.line
        if (next && next.top > cur.top) {
          const frac = Math.min(1, Math.max(0, (top - cur.top) / (next.top - cur.top)))
          line = cur.line + frac * (next.line - cur.line)
        }
        cm.revealLine(Math.round(line))
      }
      setTimeout(() => { this.syncingScroll = false }, 60)
    },
    /** 预览区链接点击拦截：`http/https/mailto` 交给系统默认浏览器；`#锚点` 保留默认行为；
     *  其余相对路径文件链接（如 `./ARCHITECTURE.md`）必须拦截——否则 WKWebView 会拿
     *  `tauri://localhost/ARCHITECTURE.md` 导航，把编辑器整页替换成乱码原文；
     *  拦截后按当前文件所在目录解析为绝对路径，在应用内打开。
     *  用 `getAttribute('href')` 取原始值，因为属性读取会把相对路径预处理成 `tauri://localhost/...`，不利于协议判断。 */
    onPreviewLinkClick(e: MouseEvent) {
      const target = e.target as HTMLElement | null
      if (!target) return
      // 代码块复制按钮优先拦截（button 不在 a 内，后续链接分支不会误捕）
      const copyBtn = target.closest('.code-copy-btn') as HTMLElement | null
      if (copyBtn) {
        e.preventDefault()
        e.stopPropagation()
        this.copyCodeBlock(copyBtn)
        return
      }
      // 任务清单复选框：回改源码对应标记（分屏直接改，预览模式先确认）
      const cb = target.closest('input.task-cb') as HTMLInputElement | null
      if (cb) {
        // 勾选态以源码渲染为准：阻止浏览器默认翻转，避免取消确认时 DOM 与源码不一致
        e.preventDefault()
        this.onPreviewTaskToggle(cb)
        return
      }
      const a = target.closest('a') as HTMLAnchorElement | null
      if (!a) return
      const raw = a.getAttribute('href') || ''
      const lower = raw.toLowerCase()
      if (lower.startsWith('http://') || lower.startsWith('https://') || lower.startsWith('mailto:')) {
        e.preventDefault()
        tauri.openUrl(raw).catch((err) => message.error(String(err)))
      } else if (raw.startsWith('#') || !raw) {
        // 页内锚点：保持默认行为
      } else {
        e.preventDefault()
        this.openRelativeLink(raw)
      }
    },
    /** 预览区任务清单复选框点击：按 DOM 顺序定位源码里第几个任务标记并翻转。
     *  分屏模式直接写回页签正文（编辑器同步、记脏，保存走常规流程）；
     *  预览模式先弹确认，确认后改源码并立即保存。 */
    onPreviewTaskToggle(cb: HTMLInputElement) {
      const f = this.activeFile
      if (!f || f.locked) return
      const md = cb.closest('.md')
      if (!md) return
      const idx = Array.prototype.indexOf.call(md.querySelectorAll('input.task-cb'), cb)
      if (idx < 0) return
      const next = this.toggleTaskMarker(f.content, idx)
      if (next === null) return
      if (this.currentMode === 'preview') {
        this.pendingTaskSrc = next
        // 取任务原文供弹窗展示（过长截断）；浏览器在 click 派发前已把 checked 预置为新态
        // （preventDefault 会回滚），故 cb.checked 即用户意图，不能再取反
        const raw = (cb.closest('li')?.textContent ?? '').replace(/\s+/g, ' ').trim()
        this.pendingTaskText = raw.length > 40 ? raw.slice(0, 40) + '…' : raw
        this.pendingTaskChecked = cb.checked
        this.showTaskConfirm = true
      } else {
        this.setContentFor(f.path, next)
      }
    },
    /** 翻转源码中第 idx 个（0 起、按源码顺序）任务清单标记：`[ ]`→`[x]`、`[x]`/`[X]`→`[ ]`；找不到返回 null。 */
    toggleTaskMarker(src: string, idx: number): string | null {
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
    },
    /** 预览模式勾选确认弹窗的「修改并保存」：写回正文后立即落盘。 */
    async submitTaskConfirm() {
      this.showTaskConfirm = false
      const f = this.activeFile
      if (!f) return
      this.setContentFor(f.path, this.pendingTaskSrc)
      await this.saveActive()
    },
    /** 复制预览代码（代码块或行内代码）到剪贴板（去掉渲染器附带的首尾换行），成功后按钮短暂变对勾「已复制」。 */
    async copyCodeBlock(btn: HTMLElement) {
      const wrap = btn.closest('.codespan-wrap')
      const code = wrap ? wrap.querySelector('code') : btn.closest('pre')?.querySelector('code')
      if (!code) return
      const text = (code.textContent ?? '').replace(/^\n/, '').replace(/\n$/, '')
      try {
        await navigator.clipboard.writeText(text)
      } catch {
        return message.error('复制失败')
      }
      btn.classList.add('copied')
      const txt = btn.querySelector('.cc-txt')
      if (txt) txt.textContent = '已复制'
      window.setTimeout(() => {
        btn.classList.remove('copied')
        if (txt) txt.textContent = '复制'
      }, 1400)
    },
    /** 把预览区相对链接解析成绝对路径并打开（参考基：当前文件所在目录，其次工作目录根）。 */
    async openRelativeLink(href: string) {
      const clean = decodeURI(href.split('#')[0].split('?')[0])
      if (!clean) return
      const win = clean.includes('\\') && !clean.includes('/')
      const sep = win ? '\\' : '/'
      let base = ''
      const f = this.activeFile
      if (f && !f.encrypted && f.path && !f.isNew) base = this.parentDir(f.path)
      if (!base && this.workdirRoot) base = this.workdirRoot
      if (!base) return message.warning('无法确定相对路径的基准目录，请从侧边栏打开该文件')
      // 绝对路径直接使用；相对路径基于 base 解析并折叠 `.` / `..`
      const joined = /^([a-zA-Z]:[\\/]|[\\/])/.test(clean) ? clean : this.joinPath(base, clean)
      const segs: string[] = []
      for (const s of joined.split(/[\\/]+/)) {
        if (!s || s === '.') continue
        if (s === '..') segs.pop()
        else segs.push(s)
      }
      const abs = (joined.startsWith(sep) || /^[a-zA-Z]:[\\/]/.test(joined) ? (joined.match(/^([a-zA-Z]:)?[\\/]/)?.[0] ?? '') : '') + segs.join(sep)
      try {
        const info = await tauri.inspectPath(abs)
        if (info.is_dir) return message.info('暂不支持在预览中打开目录链接')
        if (/\.(mdl|mdlb)$/i.test(abs)) {
          // 加密目标：沿用拖放路由的解锁流程，不能明文打开
          if (/\.mdlb$/i.test(abs)) {
            const existing = this.store.recent.find((x) => x.id === abs)
            if (!existing) {
              this.store.registerVault({ id: abs, name: this.fileName(abs).replace(/\.mdlb$/i, ''), path: abs, isDir: false, isFileVault: true })
            }
            this.store.pendingOpen = null
            this.$router.push('/unlock')
          } else {
            await this.store.openEncryptedFile(abs, this.fileName(abs))
          }
          return
        }
        await this.store.openPlainFile(abs, this.fileName(abs))
      } catch (err) {
        message.error(String(err))
      }
    },
    /** 采集预览区各顶层块的 {行号, 距内容顶部偏移}（以 .md 内 .mb[data-l] 为准，偏移基于滚动内容坐标）。 */
    _syncBlocks(): { line: number; top: number }[] {
      const pane = this.$refs.previewPane as HTMLElement
      if (!pane) return []
      const md = pane.querySelector('.md') as HTMLElement | null
      if (!md) return []
      const paneRect = pane.getBoundingClientRect()
      const out: { line: number; top: number }[] = []
      for (const el of Array.from(md.querySelectorAll('.mb'))) {
        const he = el as HTMLElement
        const line = parseInt(he.dataset.l || '0', 10) || 0
        const top = he.getBoundingClientRect().top - paneRect.top + pane.scrollTop
        out.push({ line, top })
      }
      return out
    },
    /** 根据源码行号在相邻块之间线性插值，得到预览 scrollTop 并应用（把该行所在块顶对齐到视口顶）。 */
    _previewScrollToLine(line: number) {
      const pane = this.$refs.previewPane as HTMLElement
      const blocks = this._syncBlocks()
      if (!pane || !blocks.length) return
      let i = 0
      for (let k = 0; k < blocks.length; k++) { if (blocks[k].line <= line) i = k; else break }
      const cur = blocks[i]
      const next = blocks[i + 1]
      let top = cur.top
      if (next && next.line > cur.line) {
        const frac = Math.min(1, Math.max(0, (line - cur.line) / (next.line - cur.line)))
        top = cur.top + frac * (next.top - cur.top)
      }
      const maxScroll = pane.scrollHeight - pane.clientHeight
      pane.scrollTop = Math.max(0, Math.min(top, maxScroll))
    },
    toggleSidebar() {
      this.store.updateSettings({ showSidebar: !this.store.settings.showSidebar })
    },
    onSidebarResizeStart(e: MouseEvent) {
      e.preventDefault()
      this._sidebarDragging = true
      this._onSidebarMousemove = (ev: MouseEvent) => {
        if (!this._sidebarDragging) return
        const w = Math.max(160, Math.min(500, ev.clientX))
        this.sidebarWidth = w
      }
      this._onSidebarMouseup = () => {
        this._sidebarDragging = false
        localStorage.setItem('marklock.sidebarWidth', String(this.sidebarWidth))
      }
      document.addEventListener('mousemove', this._onSidebarMousemove)
      document.addEventListener('mouseup', this._onSidebarMouseup)
    },
    onSplitResizeStart(e: MouseEvent) {
      e.preventDefault()
      this._splitDragging = true
      const panes = (e.target as HTMLElement).parentElement as HTMLElement
      const panesRect = panes.getBoundingClientRect()
      this._onSplitMousemove = (ev: MouseEvent) => {
        if (!this._splitDragging) return
        const ratio = (ev.clientX - panesRect.left) / panesRect.width
        this.splitRatio = Math.max(0.2, Math.min(0.8, ratio))
      }
      this._onSplitMouseup = () => {
        this._splitDragging = false
        localStorage.setItem('marklock.splitRatio', String(this.splitRatio))
      }
      document.addEventListener('mousemove', this._onSplitMousemove)
      document.addEventListener('mouseup', this._onSplitMouseup)
    },
    resetSplitRatio() {
      this.splitRatio = 0.5
      localStorage.setItem('marklock.splitRatio', '0.5')
    },
    toggleSec(id: string) {
      this.secClosed[id] = !this.secClosed[id]
      localStorage.setItem('marklock.secClosed', JSON.stringify(this.secClosed))
    },
    isSecClosed(id: string) {
      return !!this.secClosed[id]
    },
    toggleTree(id: string) {
      this.treeClosed[id] = !this.treeClosed[id]
      localStorage.setItem('marklock.treeClosed', JSON.stringify(this.treeClosed))
    },
    isTreeClosed(id: string) {
      return !!this.treeClosed[id]
    },
    /**
     * 展开指定库的根层（库行本身），但把库内所有文件夹节点收起——
     * 即“只展开根目录，下一级不展开”；需先加载好库树（vaultTrees）才能遍历到子节点。
     */
    expandVaultTree(vaultId: string) {
      this.treeClosed['v-' + vaultId] = false
      const collapse = (nodes: FsNode[]) => {
        for (const n of nodes) {
          if (n.is_dir) {
            this.treeClosed[n.path] = true
            if (n.children && n.children.length) collapse(n.children)
          }
        }
      }
      collapse((this.vaultTrees[vaultId] as FsNode[]) || [])
      localStorage.setItem('marklock.treeClosed', JSON.stringify(this.treeClosed))
    },
    /** 解锁后打开库：先加载库树，再只展开根层（内部文件夹保持收起）；同时确保「加密库」分组处于展开状态。 */
    async openVaultToRoot(vaultId: string) {
      const v = this.store.recent.find((x) => x.id === vaultId)
      if (v) await this.refreshVaultTree(v.id, v.path)
      this.expandVaultTree(vaultId)
      this.store.pendingExpandVault = ''
      this._expandVaultSection()
    },
    /** 展开侧边栏「加密库」分组（若处于收起态则清除并持久化）。 */
    _expandVaultSection() {
      if (this.secClosed['vaults']) {
        this.secClosed['vaults'] = false
        localStorage.setItem('marklock.secClosed', JSON.stringify(this.secClosed))
      }
    },

    // ---------- 切页签时在侧边栏定位选中文件 ----------
    /** 按激活文件路径分派：工作目录明文文件 → 工作目录树；库内文件 → 库树。 */
    async revealActiveInSidebar() {
      const f = this.activeFile
      if (!f || !this.store.settings.showSidebar) return
      const sep = f.path.includes('\\') && !f.path.includes('/') ? '\\' : '/'
      const inWorkdir = !!this.workdirRoot && (f.path === this.workdirRoot || f.path.startsWith(this.workdirRoot + sep))
      if (f.vaultId && f.path.includes('#')) {
        // 单文件库内文件：relPath 字段即库内相对路径（与树的 '/' 格式一致），比按第一个 '#' 切割更稳；
        // 展开定位库树行，库挂在工作目录下时回退高亮库本体行
        await this.revealInVaultTree(f.vaultId, f.relPath || f.path.slice(f.path.lastIndexOf('#') + 1))
      } else if (inWorkdir) {
        // 工作目录内一律尝试定位其行：明文 / 独立 .mdl（tab 路径即文件路径）/ .mdlb 或目录库本体；
        // 目录库内文件的 tab 路径不等于库本体，命中不了就回退到库树行选择器
        await this.revealInWorkdir(f.path)
      }
    },
    /** 用节点 path 是否为文件 path 的真前缀收集祖先链（同时兼容 / 与 \ 分隔）。 */
    _pathAncestors(filePath: string, nodePath: string): boolean {
      return nodePath !== filePath && (filePath.startsWith(nodePath + '/') || filePath.startsWith(nodePath + '\\'))
    },
    /** 展开工作目录树中包含该文件的折叠目录（按需懒加载子项），再高亮行滚入可见。 */
    async revealInWorkdir(filePath: string) {
      const walk = async (list: WorkNode[]) => {
        for (const n of list) {
          if (!this._pathAncestors(filePath, n.path) || !n.is_dir || n.is_vault) continue
          if (!n.open || !n.loaded) {
            n.open = true
            if (!n.loaded) {
              try {
                n.children = this.toWorkNodes(await tauri.listDir(n.path))
                n.loaded = true
              } catch { return }
            }
            await nextTick()
          }
          // 继续向更深层祖先递归展开
          await walk(n.children as WorkNode[])
        }
      }
      await walk(this.workdir)
      // 主选工作目录行（明文 / 独立 .mdl / 库本体）；目录库内文件的 tab 路径不对应任何工作目录行，回退到库树行
      this._scrollSidebarRowIntoView(`.t-item[data-wpath="${CSS.escape(filePath)}"]`, `.vtn-file[data-vpath="${CSS.escape(filePath)}"]`)
    },
    /** 展开库树及其中包含该文件的折叠文件夹（rel 为单文件库内相对路径），再高亮行滚入可见。 */
    async revealInVaultTree(vaultId: string, rel: string) {
      const key = 'v-' + vaultId
      if (this.isTreeClosed(key)) this.toggleTree(key)
      const tree = this.vaultTrees[vaultId]
      if (tree) {
        const walk = (nodes: FsNode[]): boolean => {
          let touched = false
          for (const n of nodes) {
            if (n.is_dir && this._pathAncestors(rel, n.path)) {
              if (this.treeClosed[n.path]) { this.treeClosed[n.path] = false; touched = true }
              if (n.children && walk(n.children as FsNode[])) touched = true
            }
          }
          return touched
        }
        if (walk(tree)) {
          localStorage.setItem('marklock.treeClosed', JSON.stringify(this.treeClosed))
          await nextTick()
        }
      }
      // 主选库树行；若库挂在工作目录下，行不可见时回退选中库本体行
      this._scrollSidebarRowIntoView(`.vtn-file[data-vpath="${CSS.escape(rel)}"]`, `.t-item[data-wpath="${CSS.escape(vaultId)}"]`)
    },
    /** 把侧边栏中首个匹配行的滚入可视区域；按优先级传多个选择器（如库树行 → 工作目录库本体行）。 */
    _scrollSidebarRowIntoView(...selectors: string[]) {
      nextTick(() => {
        const sb = this.$refs.sbBody as HTMLElement | null
        if (!sb) return
        for (const sel of selectors) {
          const el = sb.querySelector(sel) as HTMLElement | null
          if (el) {
            // scrollIntoView 对已在视口内的元素无副作用
            el.scrollIntoView({ block: 'nearest' })
            return
          }
        }
      })
    },

    // ---------- 工作目录 ----------
    async chooseWorkdir() {
      const dir = await tauri.openDirDialog()
      if (!dir) return
      await this.setWorkdir(dir)
    },
    /** 关闭工作目录：取消挂载并清空本地记录（不动磁盘文件）；未挂载时也无害。 */
    closeWorkdir() {
      this.workdirRoot = ''
      this.workdir = []
      localStorage.removeItem('marklock.workdir')
      workdirChanges.reset([])
      message.info('已关闭工作目录')
    },
    /** 关闭所有库：先全部锁定（清零内存密钥）再从登记列表移除全部库（不删磁盘文件，收藏不受影响）。 */
    async closeAllVaults() {
      await this.store.lockAll()
      this.store.clearRecentVaults()
      this.vaultTrees = {}
      message.info('已关闭所有库')
    },
    /** 把某个系统目录挂载为工作目录（拖放/选择共用入口）；挂载后展开侧边栏与该分组，避免用户误以为无响应。 */
    async setWorkdir(dir: string) {
      this.workdirRoot = dir
      localStorage.setItem('marklock.workdir', dir)
      this.store.updateSettings({ showSidebar: true, showWorkdir: true })
      // 打开工作目录后自动展开该分组（清除可能残留的折叠态）
      this.secClosed['workdir'] = false
      localStorage.setItem('marklock.secClosed', JSON.stringify(this.secClosed))
      await this.refreshWorkdir()
    },
    /** 按「显示隐藏文件」开关过滤目录节点，并补充折叠态字段（工作目录列目录的统一入口）。 */
    toWorkNodes(nodes: FsNode[]): WorkNode[] {
      const visible = this.store.settings.showHiddenFiles
        ? nodes
        : nodes.filter((n) => !n.name.startsWith('.'))
      return visible.map((n) => ({ ...n, open: false, loaded: false }))
    },
    /**
     * 把旧子树的展开态与已列内容按路径移植到新列表上：
     * 自动监控刷新不能把用户展开的层级整体收回去（更深层随 prev.children 一起保留，
     * 那些目录自身的增减由监控轮询各自负责）。
     */
    restoreOpenState(list: WorkNode[], oldList: WorkNode[]) {
      const byPath = new Map(oldList.map((o) => [this.normPath(o.path), o]))
      for (const n of list) {
        if (!n.is_dir) continue
        const prev = byPath.get(this.normPath(n.path))
        if (!prev || !prev.open || !prev.loaded) continue
        n.open = true
        n.loaded = true
        n.children = (prev.children || []) as WorkNode[]
      }
    },
    /**
     * 重列工作目录根。
     * @param keepOpen 自动监控刷新时保留已展开层级；手动刷新（原有语义）整体收起。
     */
    async refreshWorkdir(keepOpen = false) {
      if (!this.workdirRoot) return
      // 手动刷新后把基线交给磁盘最新状态；自动刷新的基线已由轮询本身推进，不另置
      if (!keepOpen) workdirChanges.reset([this.workdirRoot])
      try {
        const nodes = await tauri.listDir(this.workdirRoot)
        const next = this.toWorkNodes(nodes)
        if (keepOpen) this.restoreOpenState(next, this.workdir)
        this.workdir = next
      } catch (e) {
        // 自动刷新失败（目录刚被删等）不打扰用户，下一轮轮询会再试
        if (!keepOpen) message.error(String(e))
      }
    },
    /** 工作目录增减监控目标：挂载了根目录、侧边栏可见且分组未收起时，
     * 取根 + 树里已列过内容的普通目录（`.mdlb` / 目录库是原子节点，内容归库树负责）；
     * 上限兜底，避开用户展开上百个目录时每轮发几百次 invoke。 */
    workdirWatchTargets(): string[] {
      if (!this.workdirRoot || !this.store.settings.showSidebar || this.isSecClosed('workdir')) return []
      const out: string[] = [this.workdirRoot]
      const walk = (list: WorkNode[]) => {
        for (const n of list) {
          // 已列过内容就继续监控，空目录（刚删光文件 / 刚新建）也要盯住
          if (!n.is_dir || n.is_vault || !n.loaded) continue
          out.push(n.path)
          if (n.children && n.children.length) walk(n.children as WorkNode[])
        }
      }
      walk(this.workdir)
      return out.slice(0, 60)
    },
    /** 目录增减回调：静默重列变化的目录并保留展开层级。 */
    async onWorkdirLayoutChange(dirs: string[]) {
      for (const d of dirs) await this.reloadWorkdirDir(d, true)
    },
    async toggleWorkdirFolder(node: WorkNode) {
      node.open = !node.open
      if (node.open && !node.loaded) {
        try {
          const children = await tauri.listDir(node.path)
          node.children = this.toWorkNodes(children)
          node.loaded = true
        } catch (e) {
          message.error(String(e))
        }
      }
    },
    /** 折叠工作目录树里所有已展开的文件夹（含库目录下的文件夹）；只收起显示，已加载的子项与懒加载标记都保留。 */
    collapseWorkdirFolders() {
      const walk = (list: WorkNode[]) => {
        for (const n of list) {
          if (!n.is_dir) continue
          n.open = false
          if (n.children?.length) walk(n.children as WorkNode[])
        }
      }
      walk(this.workdir)
    },
    /** 工作目录标题栏「更多」菜单：除折叠外的全部操作（新建文件 / 新建文件夹 / 刷新 / 换目录 / 关闭）收进这里。 */
    openWorkdirMoreMenu(e: MouseEvent) {
      e.preventDefault()
      if (!this.workdirRoot) return
      this.showCtxMenu(e, [
        { kind: 'item', label: '在根目录新建文件', run: () => this.wdNewFileAt(this.workdirRoot) },
        { kind: 'item', label: '在根目录新建文件夹', run: () => this.wdNewFolderAt(this.workdirRoot) },
        { kind: 'item', label: '刷新目录', run: () => this.refreshWorkdir() },
        { kind: 'item', label: '选择工作目录…', run: () => this.chooseWorkdir() },
        { kind: 'sep' },
        { kind: 'item', label: '关闭工作目录', run: () => this.closeWorkdir() },
      ])
    },
    /** 加密库标题栏「更多」菜单：标题栏只留「锁定所有库」，新建/打开/关闭所有库收进这里（无登记库时不出关闭项）。 */
    openVaultsMoreMenu(e: MouseEvent) {
      e.preventDefault()
      const items: any[] = [
        { kind: 'item', label: '新建库', run: () => this.openCreateVault() },
        { kind: 'item', label: '打开库', run: () => this.openVaultAction() },
      ]
      if (this.registeredVaults.length) {
        items.push({ kind: 'sep' }, { kind: 'item', label: '关闭所有库', run: () => this.closeAllVaults() })
      }
      this.showCtxMenu(e, items)
    },

    // ---------- 库树 ----------
    async refreshVaultTree(vaultId: string, dir: string) {
      try {
        const v = this.store.recent.find((x) => x.id === vaultId)
        const tree = v?.isFileVault
          ? await tauri.listFileVault(vaultId, dir)
          : await tauri.listVault(vaultId, dir)
        this.vaultTrees[vaultId] = tree
      } catch {
        // 列树失败 = 会话已不存在（如进程重启后残留的假解锁标记）。
        // 回落为锁定态，避免出现「已解锁」徽标 + 空树的矛盾状态。
        this.vaultTrees[vaultId] = []
        const v = this.store.recent.find((x) => x.id === vaultId)
        if (v && v.unlocked) {
          v.unlocked = false
          this.store.persistRecent()
        }
      }
    },

    // ---------- 打开文件 ----------
    async openFileNode(node: FsNode, vaultId: string) {
      if (node.is_dir) return
      try {
        const v = this.store.recent.find((x) => x.id === vaultId)
        if (v?.isFileVault) {
          // 单文件库内文件：node.path 是相对路径
          await this.store.openFileVaultFile(vaultId, node.path, node.name)
        } else {
          await this.store.openFile(vaultId, node.path, node.name)
        }
      } catch (e) {
        message.error(String(e))
      }
    },
    /** 工作目录里的普通（未加密）文件：明文读取打开。 */
    async workdirOpenPlain(node: FsNode) {
      if (node.is_dir) return
      try {
        await this.store.openPlainFile(node.path, node.name)
      } catch (e) {
        message.error(String(e))
      }
    },
    async activateFile(path: string) {
      const f = this.openFiles.find((x) => x.path === path)
      if (f) {
        this.store.activePath = path
        this.store.activeVaultId = f.vaultId
      }
    },
    /** 判断页签关闭前是否需要提醒：有未保存修改，且不是「空内容的新建草稿」。 */
    needsClose(f: OpenFile): boolean {
      if (!f.dirty) return false
      if (f.isNew && f.content.trim() === '') return false
      return true
    },
    /** 关闭单个页签：存在未保存修改则先确认。 */
    closeFile(path: string) {
      const f = this.openFiles.find((x) => x.path === path)
      if (!f || !this.needsClose(f)) {
        this.store.closeFile(path)
        return
      }
      this.openCloseConfirm({ kind: 'one', path, files: [f] })
    },
    /** 关闭全部页签：存在未保存修改则先确认。 */
    closeAllFiles() {
      const dirty = this.openFiles.filter((f) => this.needsClose(f))
      if (!dirty.length) {
        this.forceCloseAll()
        return
      }
      this.openCloseConfirm({ kind: 'all', files: dirty })
    },
    /** 关闭其它页签：其它中存在未保存修改则先确认。 */
    closeOthers(path: string) {
      const dirty = this.openFiles.filter((f) => this.needsClose(f) && f.path !== path)
      if (!dirty.length) {
        this.store.closeOthers(path)
        return
      }
      this.openCloseConfirm({ kind: 'others', path, files: dirty })
    },
    /** 撤销关闭：从最近关闭栈取最后一个页签重新打开（草稿直接恢复；文件走统一打开逻辑，含存在性校验与锁库跳转）。 */
    async reopenLastClosed() {
      const t = this.store.takeClosedTab()
      if (!t) {
        message.info('没有最近关闭的页签')
        return
      }
      if (t.isNew) {
        this.store.restoreDraftTab(t)
        return
      }
      await this.recentOpenFile({ path: t.path, name: t.name, vaultId: t.vaultId, encrypted: t.encrypted })
    },
    /** 无条件清空所有页签。 */
    forceCloseAll() {
      this.store.noteClosedTabs(this.store.openFiles)
      this.store.openFiles = []
      this.store.activePath = ''
      this.store.activeVaultId = ''
      this.store.persistOpenFiles()
    },

    // ---------- 关闭/退出未保存确认 ----------
    /** 打开确认弹窗，记录待执行的关闭操作；quitting=true 时为“退出应用”语义。 */
    openCloseConfirm(op: { kind: 'one' | 'others' | 'all'; path?: string; files: OpenFile[] }, quitting = false) {
      this.pendingClose = op
      this.pendingQuit = quitting
      const verb = quitting ? '退出前' : '关闭前'
      this.closeConfirmTitle = quitting ? '退出前保存' : '未保存的修改'
      this.closeConfirmDiscard = quitting ? '不保存并退出' : '不保存'
      this.closeConfirmSave = quitting ? '保存并退出' : '保存'
      this.closeConfirmText =
        op.files.length === 1
          ? `「${op.files[0].name}」有未保存的修改，${verb}是否保存？`
          : `有 ${op.files.length} 个文件存在未保存的修改，${verb}是否保存？`
      this.showCloseConfirm = true
    },
    /** 取消（关闭弹窗；退出场景下中止退出，窗口保持打开）。 */
    cancelClose() {
      this.showCloseConfirm = false
      this.pendingClose = null
      this.pendingQuit = false
    },
    /** 不保存：退出场景直接退出，否则丢弃修改并关闭。 */
    discardClose() {
      const op = this.pendingClose
      const quitting = this.pendingQuit
      this.showCloseConfirm = false
      this.pendingClose = null
      this.pendingQuit = false
      if (quitting) {
        this.doQuit()
        return
      }
      if (op) this.execCloseOp(op)
    },
    /** 保存后关闭/退出；草稿无法静默保存，保存非草稿后保留草稿页签并走「另存为」。 */
    async saveAndClose() {
      const op = this.pendingClose
      if (!op) return
      const quitting = this.pendingQuit
      const drafts = op.files.filter((f) => f.isNew)
      const savable = op.files.filter((f) => !f.isNew)
      for (const f of savable) {
        try {
          await this.store.saveFile(f.path, f.content)
        } catch (e) {
          message.error('保存失败：' + String(e))
          return
        }
      }
      // 批量写盘后刷新监控基线，避开把自己的保存当成外部修改
      externalChanges.sync(savable.map((f) => this.watchTargetOf(f)))
      this.showCloseConfirm = false
      this.pendingClose = null
      this.pendingQuit = false
      // 退出应用：无草稿则直接退出；有草稿则中止退出，转去另存为（应用保持打开）
      if (quitting) {
        if (!drafts.length) {
          this.doQuit()
          return
        }
        message.info('存在未保存的草稿，已保存其余文件；请为草稿另存为后再退出')
        await this.activateFile(drafts[0].path)
        this.openSaveAs()
        return
      }
      if (!drafts.length) {
        this.execCloseOp(op)
        return
      }
      // 含草稿：单个草稿直接弹出「另存为」（保存后转为正式页签，不关闭）
      if (op.kind === 'one') {
        await this.activateFile(drafts[0].path)
        this.openSaveAs()
        return
      }
      // 多个：关闭已保存部分，保留草稿页签并提示手动另存为
      this.closeKeepDirtyDrafts(op)
      message.info(`${drafts.length} 个草稿尚未保存，已保留页签，请另存为`)
      await this.activateFile(drafts[0].path)
    },
    /** 处理后端发来的退出（关窗）请求：有未保存则弹窗确认，否则直接退出。 */
    async handleAppClose() {
      // 已有确认弹窗时忽略重复的关闭请求
      if (this.showCloseConfirm) return
      const dirty = this.openFiles.filter((f) => this.needsClose(f))
      if (!dirty.length) {
        this.doQuit()
        return
      }
      this.openCloseConfirm({ kind: 'all', files: dirty }, true)
    },
    /** 调用后端退出应用（后端会先锁定全部库清零密钥）。 */
    doQuit() {
      tauri.quitApp().catch(() => {})
    },
    /** 执行真正的关闭动作（已确认或无需确认时调用）。 */
    execCloseOp(op: { kind: 'one' | 'others' | 'all'; path?: string }) {
      if (op.kind === 'one' && op.path) this.store.closeFile(op.path)
      else if (op.kind === 'others' && op.path) this.store.closeOthers(op.path)
      else this.forceCloseAll()
    },
    /** 关闭除「保留页签 + 未保存草稿」外的所有页签。 */
    closeKeepDirtyDrafts(op: { kind: 'one' | 'others' | 'all'; path?: string }) {
      // 先入撤销栈：保留条件取反即被关闭的页签
      this.store.noteClosedTabs(
        this.store.openFiles.filter(
          (f) => !((f.isNew && this.needsClose(f)) || (op.kind === 'others' && f.path === op.path)),
        ),
      )
      this.store.openFiles = this.store.openFiles.filter(
        (f) => (f.isNew && this.needsClose(f)) || (op.kind === 'others' && f.path === op.path),
      )
      const kept = this.store.openFiles
      if (!kept.some((f) => f.path === this.store.activePath)) {
        const next = kept[kept.length - 1]
        this.store.activePath = next ? next.path : ''
        this.store.activeVaultId = next ? next.vaultId : ''
      }
      this.store.persistOpenFiles()
    },

    // ---------- 主菜单「打开」 ----------
    /** 打开文件对话框（md/txt 等文本优先，兼顾 .mdl/.mdlb），选中后复用拖放路由。 */
    async openFileAction() {
      const p = await tauri.openFileDialog(false)
      if (p) await this.openDroppedPaths([p])
    },
    /** 打开库：加密文件优先的对话框（.mdlb/.mdl 靠前），选中后复用拖放路由（.mdlb 登记并跳解锁）。 */
    async openVaultAction() {
      const p = await tauri.openFileDialog(true)
      if (p) await this.openDroppedPaths([p])
    },

    // ---------- 右键上下文菜单 ----------
    /** 视图开关菜单项（页签/工具栏/状态栏/侧边栏）。 */
    viewToggleItems() {
      const s = this.store.settings
      return [
        { kind: 'item', label: '页签', checked: s.showTabs, run: () => this.store.updateSettings({ showTabs: !this.store.settings.showTabs }) },
        { kind: 'item', label: '工具栏', checked: s.showToolbar, run: () => this.store.updateSettings({ showToolbar: !this.store.settings.showToolbar }) },
        { kind: 'item', label: '状态栏', checked: s.showStatusbar, run: () => this.store.updateSettings({ showStatusbar: !this.store.settings.showStatusbar }) },
        { kind: 'item', label: '侧边栏', sc: '⌘J', checked: s.showSidebar, run: () => this.store.updateSettings({ showSidebar: !this.store.settings.showSidebar }) },
      ]
    },
    /** 侧边栏分组开关菜单项（与主菜单侧边栏子菜单一致）。 */
    sidebarToggleItems() {
      const s = this.store.settings
      return [
        { kind: 'item', label: '显示侧边栏', sc: '⌘J', checked: s.showSidebar, run: () => this.store.updateSettings({ showSidebar: !this.store.settings.showSidebar }) },
        { kind: 'sep' },
        { kind: 'item', label: '打开的文件', checked: s.showOpenFiles, run: () => this.store.updateSettings({ showOpenFiles: !this.store.settings.showOpenFiles }) },
        { kind: 'item', label: '加密库', checked: s.showUnlockedVaults, run: () => this.store.updateSettings({ showUnlockedVaults: !this.store.settings.showUnlockedVaults }) },
        { kind: 'item', label: '工作目录', checked: s.showWorkdir, run: () => this.store.updateSettings({ showWorkdir: !this.store.settings.showWorkdir }) },
        { kind: 'item', label: '收藏', checked: s.showFavorites, run: () => this.store.updateSettings({ showFavorites: !this.store.settings.showFavorites }) },
        { kind: 'item', label: '最近打开', checked: s.showRecent, run: () => this.store.updateSettings({ showRecent: !this.store.settings.showRecent }) },
        { kind: 'item', label: '大纲', checked: s.showOutline, run: () => this.store.updateSettings({ showOutline: !this.store.settings.showOutline }) },
        { kind: 'sep' },
        // 「打开库」常驻（可同时打开多个库）；仅当有已登记库时才追加「关闭所有库」。
        { kind: 'item', label: '打开库', run: () => this.openVaultAction() },
        ...(this.registeredVaults.length
          ? [{ kind: 'item', label: '关闭所有库', run: () => this.closeAllVaults() }]
          : []),
        // 未挂载工作目录时变为「打开工作目录」；已挂载时为「关闭工作目录」
        this.workdirRoot
          ? { kind: 'item', label: '关闭工作目录', run: () => this.closeWorkdir() }
          : { kind: 'item', label: '打开工作目录', run: () => this.chooseWorkdir() },
      ]
    },
    /** 打开右键菜单：按类型构建菜单项并定位到光标处（夹取到视口内）。 */
    openContextMenu(e: MouseEvent, menu: 'view' | 'tabsbar' | 'tab' | 'sidebar' | 'vault' | 'favfile', path?: string) {
      e.preventDefault()
      let items: any[] = []
      if (menu === 'view') {
        items = this.viewToggleItems()
      } else if (menu === 'tabsbar') {
        items = [
          { kind: 'item', label: '新建文件', sc: '⌘N', run: () => this.openNewDraft() },
          { kind: 'item', label: '打开文件…', sc: '⌘O', run: () => this.openFileAction() },
          { kind: 'item', label: '撤销关闭页签', sc: '⌘⇧T', disabled: !this.store.recentlyClosed.length, run: () => this.reopenLastClosed() },
          { kind: 'sep' },
          ...this.viewToggleItems(),
        ]
      } else if (menu === 'tab' && path) {
        const p = path
        const f = this.openFiles.find((x) => x.path === p)
        const isMdl = p.endsWith('.mdl')
        items = [
          { kind: 'item', label: '关闭', sc: '⌘W', run: () => this.closeFile(p) },
          { kind: 'item', label: '关闭其它', run: () => this.closeOthers(p) },
          { kind: 'item', label: '关闭已保存', run: () => this.store.closeSaved() },
          { kind: 'item', label: '关闭全部', run: () => this.closeAllFiles() },
          { kind: 'item', label: '撤销关闭页签', sc: '⌘⇧T', disabled: !this.store.recentlyClosed.length, run: () => this.reopenLastClosed() },
          { kind: 'sep' },
          { kind: 'item', label: REVEAL_LABEL, disabled: !f || f.isNew, run: () => this.revealTab(p) },
          {
            kind: 'item' as const,
            label: f?.external === 'modified' ? '重新加载磁盘版本（外部已修改）' : '重新加载磁盘版本',
            // 草稿未落盘 / 页签或所属库锁定时无磁盘明文可读
            disabled: !f || f.isNew || f.locked || this.isVaultFileLocked(f),
            run: () => this.reloadTab(p),
          },
          ...(isMdl && f && !f.isNew
            ? [{ kind: 'item' as const, label: '修改密码…', run: () => this.openChangePwd({ id: p, path: p }) }]
            : []),
          { kind: 'sep' },
          {
            kind: 'item' as const,
            // 草稿未落盘没有可收藏的真实路径；收藏按 path 记录，与侧边栏星标同一入口
            label: this.store.isFavoriteFile(p) ? '取消收藏' : '加入收藏',
            disabled: !f || f.isNew,
            run: () => this.store.toggleFavoriteFile(p),
          },
          ...(f && !f.isNew && !f.encrypted && !f.locked
            ? [
                { kind: 'item' as const, label: '转为加密文件…', run: () => this.openConvertDialog(p) },
                { kind: 'item' as const, label: '添加到加密库…', run: () => this.openCopyToVault(p) },
              ]
            : []),
          {
            kind: 'item' as const,
            label: '导出 HTML…',
            // 锁定（本页签或所属库）时无明文可渲染；草稿有内存正文仍可导出
            disabled: !f || f.locked || this.isVaultFileLocked(f),
            run: () => this.exportHtml(p),
          },
        ]
      } else if (menu === 'sidebar') {
        items = this.sidebarToggleItems()
      } else if (menu === 'favfile' && path) {
        // 侧边栏「收藏」的收藏项必定已收藏，因此固定为「取消收藏」（与库行走 vault 菜单、页签菜单的措辞一致）
        const p = path
        const f = this.favoriteFileList.find((x) => x.path === p)
        items = [
          { kind: 'item', label: '打开', disabled: !f, run: () => f && this.recentOpenFile(f) },
          { kind: 'item', label: '取消收藏', run: () => this.store.toggleFavoriteFile(p) },
          { kind: 'sep' },
          { kind: 'item', label: REVEAL_LABEL, run: () => this.revealTab(p) },
        ]
      } else if (menu === 'vault' && path) {
        const id = path
        const v = this.store.recent.find((x) => x.id === id)
        const isMdl = !!v && !(v.isDir || v.isFileVault)
        // 已解锁的库（目录库 / 单文件库）才能新建文件/文件夹（需写入会话）
        const canCreate = !!v && !isMdl && v.unlocked
        items = [
          ...(canCreate
            ? [
                { kind: 'item' as const, label: '新建文件', run: () => this.openNewFile({ id: v!.id, name: v!.name }, null) },
                { kind: 'item' as const, label: '新建文件夹', run: () => this.openNewFolder({ id: v!.id, name: v!.name }, null) },
                { kind: 'sep' as const },
              ]
            : []),
          { kind: 'item', label: this.store.isFavorite(id) ? '取消收藏' : '收藏', run: () => this.store.toggleFavorite(id) },
          { kind: 'item', label: isMdl ? '修改密码…' : '修改主密码…', run: () => this.openChangePwd({ id, path: v?.path || id, name: v?.name, isFileVault: v?.isFileVault }) },
          { kind: 'item', label: REVEAL_LABEL, run: () => this.revealTab(v?.path || id) },
          { kind: 'sep' },
          // 锁定态：提供「解锁并打开」（跳解锁页）；已解锁：提供「锁定该库」
          ...(v?.unlocked
            ? [{ kind: 'item' as const, label: '锁定该库', run: () => this.lockVault(id) }]
            : [{ kind: 'item' as const, label: '解锁并打开', run: () => this.recentOpenVault(id) }]),
          { kind: 'sep' },
          // 关闭库：从侧边栏列表取消登记（不删除磁盘文件）；若已解锁先锁定再移除
          { kind: 'item', label: '关闭库', run: () => this.removeVaultFromList(id) },
        ]
      }
      this.showCtxMenu(e, items)
    },
    /** 库内节点（文件夹/文件）右键菜单：新建（仅文件夹）、重命名、删除。 */
    openVaultNodeMenu(e: MouseEvent, v: { id: string; name: string }, node: FsNode) {
      e.preventDefault()
      const items: any[] = []
      if (node.is_dir) {
        items.push(
          { kind: 'item', label: '新建文件', run: () => this.openNewFile(v, node) },
          { kind: 'item', label: '新建文件夹', run: () => this.openNewFolder(v, node) },
          { kind: 'sep' },
        )
      }
      items.push(
        { kind: 'item', label: '重命名 / 移动', run: () => this.openRename(v.id, node) },
        { kind: 'item', label: '删除', run: () => this.openDelete(v.id, node) },
      )
      this.showCtxMenu(e, items)
    },
    /** 拼接目录与名字为绝对路径（工作目录明文操作用，容错重复斜杠）。 */
    joinPath(dir: string, name: string) {
      // Windows 下后端/拖放路径可能是反斜杠风格，按原风格拼接，避免出现混合分隔符
      const win = dir.includes('\\') && !dir.includes('/')
      const sep = win ? '\\' : '/'
      const base = dir.replace(win ? /[\\/]+$/ : /\/+$/, '')
      return (base ? base + sep : sep) + name.replace(/^[\\/]+/, '')
    },
    /** 取绝对路径的父目录（兼容 Windows 反斜杠；盘符根返回原样，便于与节点 path 命中）。 */
    parentDir(path: string) {
      const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
      if (i <= 0) return path
      return path.slice(0, i)
    },
    /** 路径规范化：统一分隔符并去尾部分隔符，用于跨平台比较。 */
    normPath(p: string) {
      return p.replace(/\\+/g, '/').replace(/\/+$/, '')
    },
    /** 工作目录普通节点（目录/明文文件）右键菜单：新建（仅目录）/ 在访达中显示 / 重命名 / 删除。
     *  库与加密文件节点（is_vault）不再单独出菜单，与普通文件一致（系统级路径操作）。 */
    openWorkdirNodeMenu(e: MouseEvent, node: FsNode) {
      e.preventDefault()
      const items: any[] = []
      if (node.is_dir && !node.is_vault) {
        items.push(
          { kind: 'item', label: '新建文件', run: () => this.wdOpenNewFile(node) },
          { kind: 'item', label: '新建文件夹', run: () => this.wdOpenNewFolder(node) },
        )
      }
      items.push(
        { kind: 'item', label: REVEAL_LABEL, run: () => this.revealTab(node.path) },
        { kind: 'sep' },
        { kind: 'item', label: '重命名', run: () => this.wdOpenRename(node) },
        { kind: 'item', label: '删除', run: () => this.wdOpenDelete(node) },
      )
      this.showCtxMenu(e, items)
    },
    wdOpenNewFile(dirNode: FsNode) {
      this.wdNewFileAt(dirNode.path)
    },
    wdOpenNewFolder(dirNode: FsNode) {
      this.wdNewFolderAt(dirNode.path)
    },
    /** 在工作目录内某个目录（绝对路径）下新建文件：右键菜单与分组标题栏按钮共用，弹窗内可选普通 md / 加密。 */
    wdNewFileAt(dirPath: string) {
      this.wdOp = true
      this.wdDir = dirPath
      this.newFileName = ''
      this.newFileType = 'plain'
      this.newFilePwd = ''
      this.newFileConfirm = ''
      this.showNewFile = true
    },
    /** 在工作目录内某个目录（绝对路径）下新建文件夹。 */
    wdNewFolderAt(dirPath: string) {
      this.wdOp = true
      this.wdDir = dirPath
      this.newFolderName = ''
      this.showNewFolder = true
    },
    wdOpenRename(node: FsNode) {
      this.wdOp = true
      this.wdPath = node.path
      this.renameIsDir = !!node.is_dir
      this.renameName = node.name
      this.showRename = true
    },
    wdOpenDelete(node: FsNode) {
      this.wdOp = true
      this.wdPath = node.path
      this.deleteIsDir = !!node.is_dir
      this.showDelete = true
    },
    /** 刷新工作目录某个目录节点的子项（绝对路径）；命中根目录则整体刷新。 */
    async reloadWorkdirDir(absDir: string, keepOpen = false) {
      if (!this.workdirRoot) return
      // 手动刷新 / 应用自身增删后把基线交给磁盘最新状态，避免监控轮询把同样的变化再刷一次；
      // 自动刷新的基线已由轮询本身推进，再清反而会把紧随其后的真实改动并入基线而漏刷
      if (!keepOpen) workdirChanges.reset([absDir])
      // 大小写不敏感仅限单字母盘符前缀（Windows 的 C:\ 与 c:\ 视为同一路径），其余段保持大小写敏感
      const norm = this.normPath(absDir)
      const normRoot = this.normPath(this.workdirRoot)
      const driveOnly = /^[a-zA-Z]:\/$/
      const hitRoot =
        !absDir ||
        (driveOnly.test(normRoot)
          ? norm.toLowerCase() === normRoot.toLowerCase()
          : norm === normRoot)
      if (hitRoot) {
        await this.refreshWorkdir(keepOpen)
        return
      }
      const reload = async (list: WorkNode[]): Promise<boolean> => {
        for (const n of list) {
          if (n.is_dir && this.normPath(n.path) === norm) {
            try {
              const children = await tauri.listDir(n.path)
              const next = this.toWorkNodes(children)
              if (keepOpen) this.restoreOpenState(next, (n.children || []) as WorkNode[])
              n.children = next
              n.loaded = true
              // 手动刷新（含新建后定位到目标目录）顺带展开；自动监控保持原展开态
              if (!keepOpen) n.open = true
            } catch (e) {
              if (!keepOpen) message.error(String(e))
            }
            return true
          }
          if (n.children && n.children.length && (await reload(n.children as WorkNode[]))) return true
        }
        return false
      }
      await reload(this.workdir)
    },
    /** 定位并展示右键菜单（按估算尺寸夹取到视口内，避免出界）。
     *  above：菜单抬到该元素上方（状态栏等靠底部的条目用），水平与元素左对齐，
     *  右侧出界时整体左移、保证距窗口右缘 20px；title：菜单顶部的标题（可省）。 */
    showCtxMenu(e: MouseEvent, items: any[], above?: HTMLElement, title?: string) {
      const menuW = 208
      // 高度按真实盒模型估算：项 30，分隔线 1 + margin 5×2，容器 padding 5×2 + border 1×2，
      // 标题复用上排分组名的 .m-cap（padding 6+4 + 11px 字 ×1.5715 行高）
      const headH = title ? 28 : 0
      const menuH = items.reduce((h, it) => h + (it.kind === 'sep' ? 11 : 30), 0) + 12 + headH
      let x = e.clientX
      let y = e.clientY
      if (above) {
        const r = above.getBoundingClientRect()
        x = r.left
        if (x + menuW > window.innerWidth - 20) x = window.innerWidth - 20 - menuW
        y = r.top - menuH - 8
      }
      if (x + menuW > window.innerWidth) x = window.innerWidth - menuW - 4
      if (y + menuH > window.innerHeight) y = window.innerHeight - menuH - 4
      this.ctxMenu = { visible: true, x: Math.max(4, x), y: Math.max(4, y), items, title: title || '' }
    },
    closeContextMenu() {
      this.ctxMenu.visible = false
    },
    /** 执行菜单项：禁用项忽略；执行后关闭菜单。 */
    runCtxItem(it: any) {
      if (it.disabled) return
      it.run && it.run()
      this.closeContextMenu()
    },
    // ---------- 系统文件类型关联（首次使用询问） ----------
    /** 启动后探测一次：未询问过且非默认打开程序时弹窗；已是默认或平台不支持则静默标记已问过。 */
    async checkFileAssocPrompt() {
      if (this.store.settings.assocPromptAsked) return
      try {
        const owned = await tauri.isDefaultFileHandler()
        if (owned) {
          this.store.updateSettings({ assocPromptAsked: true })
          return
        }
        if (await tauri.supportsDefaultAppGuide()) this.showAssocPrompt = true
        else this.store.updateSettings({ assocPromptAsked: true })
      } catch {
        // 探测失败不打扰用户，也不标记，下次启动重试
      }
    },
    /** 弹窗内「暂不需要」/ 关闭：不再重复询问。 */
    dismissAssocPrompt() {
      this.showAssocPrompt = false
      this.store.updateSettings({ assocPromptAsked: true })
    },
    /** 接受：一键将本应用设为默认；系统拒绝自动更改时提示手动步骤。 */
    async applySetDefault() {
      try {
        const r = await tauri.trySetDefaultFileHandler()
        if (r === 'set') message.success('已将 MarkLock 设为 .md / .mdl / .mdlb 的默认打开程序')
        else message.warning('自动设置未成功。请右键文件 →「打开方式」→ 选择 MarkLock 并勾选「始终使用此应用」（macOS：显示简介 → 打开方式 → 全部更改…）')
      } catch (e) {
        message.error(String(e))
      }
      this.showAssocPrompt = false
      this.store.updateSettings({ assocPromptAsked: true })
    },

    /** 在访达中定位页签对应的真实文件：.mdlb 库内文件（path 含 #）定位到库文件本身。 */
    async revealTab(path: string) {
      const real = path.includes('#') ? path.split('#')[0] : path
      try {
        await tauri.revealInFinder(real)
      } catch (e) {
        message.error(String(e))
      }
    },
    clearRecent() {
      this.store.clearRecentFiles()
      this.store.clearRecentVaults()
      message.success('已清空最近打开（收藏已保留）')
    },

    // ---------- 全局搜索（已解锁库 + 工作目录明文文件） ----------
    /** 搜索框输入：防抖 300ms 后触发搜索。 */
    onSearchInput() {
      if (this._searchTimer) clearTimeout(this._searchTimer)
      const q = this.searchQuery.trim()
      if (!q) {
        this.searchResults = []
        this.searching = false
        this.searchOpen = false
        this.searchIndex = -1
        return
      }
      this._searchTimer = setTimeout(() => this.runSearch(q), 300)
    },
    async runSearch(q: string) {
      this.searching = true
      this.searchOpen = true
      try {
        const workdirs = this.workdirRoot ? [this.workdirRoot] : []
        const remote = await tauri.search(q, workdirs)
        // 打开的页签（内容在内存）也参与搜索：覆盖打开的明文文件、
        // 已解锁的单文件 .mdl、库内已打开的页签；与 Rust 结果按文件去重
        const local = this.searchOpenTabs(q)
        // 归一化分隔符后再比对：Windows 下 Rust 的 rel_path 用 '\'、页签 path 可能混用，
        // 归一化避免同一文件的本地/进程结果没去重而重复列出来
        const localKeys = new Set(local.map((h) => this.normPath(h.open_path as string)))
        // 已打开文件的命中用内存内容替换 Rust 结果，但插回它在 Rust 结果里的原位置，
        // 保证“文件是否已打开”不影响列表顺序与条数
        const merged: tauri.SearchHit[] = []
        const placed = new Set<string>()
        for (const h of remote) {
          const k = this.normPath(this.hitKey(h))
          if (localKeys.has(k)) {
            if (!placed.has(k)) {
              placed.add(k)
              merged.push(...local.filter((l) => this.normPath(l.open_path as string) === k))
            }
            continue
          }
          merged.push(h)
        }
        // Rust 没覆盖到的页签命中（工作目录外的文件、未落盘草稿、磁盘无匹配但内存有匹配）排到末尾
        merged.push(...local.filter((l) => !placed.has(this.normPath(l.open_path as string))))
        this.searchResults = merged.slice(0, 100) // 对齐 Rust 侧 SEARCH_MAX_HITS
        this.searchIndex = this.searchResults.length ? 0 : -1
      } catch (e) {
        message.error(String(e))
        this.searchResults = []
        this.searchIndex = -1
      } finally {
        this.searching = false
      }
    },
    /** Rust 命中换算成与页签 path 同口径的文件标识（用于和打开页签命中去重）。 */
    hitKey(h: tauri.SearchHit): string {
      if (h.plain || !h.vault_id) {
        return h.vault_id ? `${h.vault_id}/${h.rel_path}` : h.rel_path
      }
      const v = this.store.recent.find((x) => x.id === h.vault_id)
      return v?.isFileVault ? `${h.vault_id}#${h.rel_path}` : `${h.vault_id}/${h.rel_path}`
    },
    /** 搜索当前打开的页签（文件名 + 内存内容），返回命中结果。
     *  口径与 Rust 侧 search_file_unit / search_content 对齐：每文件最多 3 条内容命中（非重叠
     *  往后推），只有内容无命中时才补一条文件名/路径命中，避免“文件是否已打开”导致结果不一致。 */
    searchOpenTabs(q: string): tauri.SearchHit[] {
      const ql = q.toLowerCase()
      if (!ql) return []
      const hits: tauri.SearchHit[] = []
      for (const f of this.store.openFiles) {
        if (hits.length >= 100) break // 对齐 Rust 侧 SEARCH_MAX_HITS
        if (f.locked) {
          // 锁定页签读不到内容，文件名照常匹配
          if (f.name.toLowerCase().includes(ql) || f.path.toLowerCase().includes(ql)) {
            hits.push(this.tabNameHit(f))
          }
          continue
        }
        const content = f.content || ''
        const lower = content.toLowerCase()
        const disp = this.tabDisplay(f)
        let pos = lower.indexOf(ql)
        let perFile = 0
        while (pos >= 0 && perFile < 3) {
          hits.push({
            ...disp,
            snippet: this.makeSnippet(content, pos),
            plain: !f.encrypted,
            open_path: f.path,
          })
          perFile++
          pos = lower.indexOf(ql, pos + ql.length)
        }
        if (!perFile && (f.name.toLowerCase().includes(ql) || f.path.toLowerCase().includes(ql))) {
          hits.push(this.tabNameHit(f))
        }
      }
      return hits
    },
    /** 页签命中的展示口径：工作目录内的明文文件按「工作目录 + 相对路径」显示，与 Rust 侧明文
     *  结果一致（同一文件打开前后不该长出两种样子）；库内文件仍按所属库显示，工作目录外的
     *  明文文件保持「打开的文件 + 绝对路径」。 */
    tabDisplay(f: { vaultId: string; name: string; path: string; relPath?: string; encrypted: boolean }): { vault_id: string; vault_name: string; rel_path: string } {
      if (!f.encrypted && this.workdirRoot) {
        const root = this.normPath(this.workdirRoot)
        const p = this.normPath(f.path)
        if (root && p.startsWith(root + '/')) {
          return { vault_id: this.workdirRoot, vault_name: '工作目录', rel_path: p.slice(root.length + 1) }
        }
      }
      return {
        vault_id: f.vaultId,
        vault_name: this.tabVaultName(f.vaultId),
        rel_path: f.encrypted ? (f.relPath || f.name) : f.path,
      }
    },
    /** 页签所属库显示名。 */
    tabVaultName(vaultId: string): string {
      if (!vaultId) return '打开的文件'
      const v = this.store.recent.find((x) => x.id === vaultId)
      return v ? v.name : '打开的文件'
    },
    /** 页签文件名命中结果（snippet 用展示路径，高亮自然落在文件名上）。 */
    tabNameHit(f: { vaultId: string; name: string; path: string; relPath?: string; encrypted: boolean }): tauri.SearchHit {
      const disp = this.tabDisplay(f)
      return {
        ...disp,
        snippet: disp.rel_path,
        plain: !f.encrypted,
        open_path: f.path,
        name_hit: true,
      }
    },
    /** 前端版命中片段（与 Rust 侧口径一致：匹配点前后各约 30 字符）。 */
    makeSnippet(content: string, pos: number): string {
      const start = Math.max(0, pos - 30)
      const end = Math.min(content.length, pos + 30)
      let s = content.slice(start, end).replace(/[\n\r\t]/g, ' ')
      if (start > 0) s = '…' + s
      if (end < content.length) s += '…'
      return s
    },
    /** 片段内关键字高亮（大小写不敏感，HTML 转义后包裹 <mark>）。 */
    highlight(snippet: string): string {
      const q = this.searchQuery.trim()
      if (!q) return this.escapeHtml(snippet)
      const escaped = this.escapeHtml(snippet)
      const qEsc = this.escapeHtml(q).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
      return escaped.replace(new RegExp(qEsc, 'gi'), (m) => `<mark>${m}</mark>`)
    },
    escapeHtml(s: string): string {
      return s
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;')
    },
    /** 搜索结果显示的文件名（相对/绝对路径的最后一段）。 */
    hitName(h: tauri.SearchHit): string {
      return h.rel_path.split('/').pop() || h.rel_path
    },
    /** 搜索框键盘：上下键移动选中、回车打开、Esc 关闭。 */
    onSearchKeydown(e: KeyboardEvent) {
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        if (!this.searchResults.length) return
        this.searchIndex = (this.searchIndex + 1) % this.searchResults.length
        this.scrollSearchHitIntoView()
      } else if (e.key === 'ArrowUp') {
        e.preventDefault()
        if (!this.searchResults.length) return
        this.searchIndex = (this.searchIndex - 1 + this.searchResults.length) % this.searchResults.length
        this.scrollSearchHitIntoView()
      } else if (e.key === 'Enter') {
        if (this.searchIndex >= 0 && this.searchResults[this.searchIndex]) {
          e.preventDefault()
          this.openSearchHit(this.searchResults[this.searchIndex])
        }
      } else if (e.key === 'Escape') {
        this.searchOpen = false
        this.searchIndex = -1
      }
    },
    /** 让当前选中项滚入可视区域。 */
    scrollSearchHitIntoView() {
      this.$nextTick(() => {
        const el = this.$refs.searchPop as HTMLElement | null
        const active = el?.querySelector('.search-hit.active') as HTMLElement | null
        if (active && typeof active.scrollIntoView === 'function') {
          active.scrollIntoView({ block: 'nearest' })
        }
      })
    },
    /** 点击搜索结果：打开对应文件（打开的页签直接激活；明文命中走明文打开，库命中走解密读取），
     *  打开后再把命中词滚动到视口内。 */
    async openSearchHit(hit: tauri.SearchHit) {
      // 搜索词与命中页签在下面会被清空，先捕获用于定位
      const query = this.searchQuery.trim()
      let targetPath = ''
      try {
        // 打开的页签命中：直接激活对应页签，不重读文件
        if (hit.open_path) {
          const f = this.store.openFiles.find((x) => x.path === hit.open_path)
          if (f) {
            this.store.activePath = f.path
            this.store.activeVaultId = f.vaultId
            targetPath = f.path
          }
        }
        if (!targetPath) {
          if (hit.plain) {
            const abs = `${hit.vault_id}/${hit.rel_path}`
            const name = hit.rel_path.split('/').pop() || hit.rel_path
            // 工作目录里的加密文件（.mdl/.mdlb 只会以文件名命中）：走解锁/打开库流程，不能按明文读取
            if (/\.(mdl|mdlb)$/i.test(name)) {
              const node: FsNode = { name, path: abs, is_dir: false, is_vault: true, children: [] }
              await this.workdirOpenVault(node)
            } else {
              await this.store.openPlainFile(abs, name)
              targetPath = abs
            }
          } else {
            const v = this.store.recent.find((x) => x.id === hit.vault_id)
            if (v?.isFileVault) {
              await this.store.openFileVaultFile(hit.vault_id, hit.rel_path, hit.rel_path.split('/').pop() || hit.rel_path)
              targetPath = `${hit.vault_id}#${hit.rel_path}`
            } else {
              const abs = hit.vault_id ? `${hit.vault_id}/${hit.rel_path}` : hit.rel_path
              await this.store.openFile(hit.vault_id, abs, hit.rel_path.split('/').pop() || hit.rel_path)
              targetPath = abs
            }
          }
        }
        this.searchOpen = false
        this.searchQuery = ''
        this.searchResults = []
        this.searchIndex = -1
      } catch (e) {
        message.error(String(e))
        return
      }
      // 打开失败或走了库解锁/跳转流程（无对应页签正文）时不做定位，保持原有行为
      if (targetPath) await this.locateSearchHit(targetPath, query, hit.name_hit ? '' : hit.snippet)
    },
    /** 打开命中文件后定位到正文里的命中处：编辑器选中命中区间并滚入视口，预览区同步滚动。
     *  位置优先由命中片段在正文里反查（同文件多处命中时能对上被点的那一条），退回正文首个
     *  匹配（与 snippet 同源口径）；文件名/路径命中或正文不可读（锁定页签、跳解锁页）时不动，
     *  停在文件顶部。新打开的文件要等「解密读取 → 内容入库 → 编辑器实例挂载并把正文同步进
     *  doc」，故逐帧重试直到就绪，目标位置记在参数里不会丢失。 */
    async locateSearchHit(path: string, query: string, snippet: string) {
      const ql = (query || '').trim().toLowerCase()
      if (!ql) return
      for (let attempt = 0; attempt < 40; attempt++) {
        // 等一次 DOM 刷新 + 一帧，让刚插入的页签实例完成挂载
        await new Promise<void>((r) => this.$nextTick(() => setTimeout(r, 16)))
        if (this.activePath !== path) return
        const f = this.openFiles.find((x) => x.path === path)
        if (!f) return
        const content = f.content || ''
        if (!content) {
          // 锁定页签 / 跳解锁页 / 正文还未填充：前几帧再等等，之后放弃定位（保持停在文件顶部）
          if (attempt > 5) return
          continue
        }
        const pos = this._resolveHitPos(content, ql, snippet)
        if (pos < 0) return
        // revealRange 返回 false 表示实例还没挂载或正文未同步进 doc，下一帧重试
        if (!this.activeEditor()?.revealRange?.(pos, pos + ql.length)) continue
        // 占住双向滚动同步：否则编辑器定位会触发 onEditorScroll 反过来把预览区按行号插值重滚，
        // 抢掉精确的命中位置（复用大纲跳转 / 分屏同步的标志位；窗口略长以覆盖
        // revealRange 下一帧的重新对齐，避开 display:none → 可见时的一串测量滚事件）
        this.syncingScroll = true
        setTimeout(() => { this.syncingScroll = false }, 200)
        const line = content.slice(0, pos).split('\n').length
        this._previewScrollToHit(ql, line)
        // 跳过了 onEditorScroll（被上面的同步标志位挡下），大纲高亮自己跟着补上
        this._outlineSyncActive(line)
        return
      }
    },
    /** 解析命中在正文中的字符偏移：先用片段原文反查，再退到正文首个匹配；无匹配返回 -1。
     *  片段把换行/制表压成了空格，跨行的片段在正文里对不上，属正常退回首个匹配。 */
    _resolveHitPos(content: string, ql: string, snippet: string): number {
      const lower = content.toLowerCase()
      const core = (snippet || '').replace(/^…+/, '').replace(/…+$/, '').toLowerCase()
      if (core) {
        const at = lower.indexOf(core)
        if (at >= 0) {
          const inCore = core.indexOf(ql)
          if (inCore >= 0) return at + inCore
        }
      }
      return lower.indexOf(ql)
    },
    /** 预览区定位到命中处：先给命中词加临时高亮，再把命中点对齐到视口上三分之一；
     *  找不到（跨文本节点）或面板折叠（纯编辑模式，宽度为 0 测不出位置）则退回按源码行
     *  插值滚动，与大纲跳转同口径。 */
    _previewScrollToHit(ql: string, line: number) {
      const pane = this.$refs.previewPane as HTMLElement | null
      const md = pane?.querySelector('.md') as HTMLElement | null
      if (!pane || !md) {
        this._previewScrollToLine(line)
        return
      }
      const mark = this._markPreviewHit(md, ql, line)
      if (!mark || pane.clientWidth < 2 || pane.clientHeight < 2) {
        this._previewScrollToLine(line)
        return
      }
      // 命中点靠视口上三分之一处，保证命中内容可见且留有余量
      const hitTop = mark.getBoundingClientRect().top - pane.getBoundingClientRect().top + pane.scrollTop
      const maxScroll = pane.scrollHeight - pane.clientHeight
      pane.scrollTop = Math.max(0, Math.min(hitTop - pane.clientHeight * 0.33, maxScroll))
    },
    /** 在预览区给命中词包一层 `<mark class="pv-hit">`：先还原上一次的高亮，再在目标行所属
     *  顶层块（.mb[data-l]）里找查询词首个出现的文本节点；限定在块内才能对上编辑器里选中的
     *  那处。命中被 Markdown 行内语法拆到多个文本节点时不包，返回 null 由调用方退回行号插值。
     *  切换文件 / 正文变化时 v-html 会整体替换 innerHTML，高亮自然消失。 */
    _markPreviewHit(md: HTMLElement, ql: string, line: number): HTMLElement | null {
      for (const old of Array.from(md.querySelectorAll('mark.pv-hit'))) {
        const parent = old.parentNode
        if (parent) parent.replaceChild(document.createTextNode(old.textContent || ''), old)
        else old.remove()
      }
      // 包 mark 会把文本节点拆成三段，先合回来才能保证下一次仍能在单节点内匹配
      md.normalize()
      let scope: HTMLElement = md
      for (const el of Array.from(md.querySelectorAll('.mb'))) {
        const he = el as HTMLElement
        const l = parseInt(he.dataset.l || '0', 10) || 0
        if (l > line) break
        scope = he
      }
      const walker = document.createTreeWalker(scope, NodeFilter.SHOW_TEXT)
      for (let n = walker.nextNode(); n; n = walker.nextNode()) {
        const node = n as Text
        const text = node.nodeValue
        if (!text) continue
        const i = text.toLowerCase().indexOf(ql)
        if (i < 0) continue
        const r = document.createRange()
        r.setStart(node, i)
        r.setEnd(node, Math.min(i + ql.length, text.length))
        const mark = document.createElement('mark')
        mark.className = 'pv-hit'
        try {
          r.surroundContents(mark)
        } catch {
          r.detach()
          return null
        }
        r.detach()
        return mark
      }
      return null
    },

    // ---------- 侧边栏拖放排序（纯鼠标事件，整个条目按住拖动，移动超阈值才判定为拖动） ----------
    /** 条目按下：记录起点，移动超过阈值才进入拖动，否则按普通点击处理。 */
    onItemMouseDown(key: string, e: MouseEvent) {
      // 忽略非左键
      if (e.button !== 0) return
      // 阻止默认行为（拖动时触发文本框选）
      e.preventDefault()
      const startX = e.clientX
      const startY = e.clientY
      let started = false

      const move = (ev: MouseEvent) => {
        const dx = ev.clientX - startX
        const dy = ev.clientY - startY
        if (!started && Math.abs(dx) + Math.abs(dy) < 4) return
        if (!started) {
          started = true
          this.dragKey = key
          this.dragging = true
          this.dragOverKey = key
          this.dragMoveY = ev.clientY
        }
        // 拖动过程中清除可能产生的文本选区
        const sel = window.getSelection()
        if (sel && sel.rangeCount > 0) sel.removeAllRanges()
        this.dragMoveY = ev.clientY
        const el = document.elementFromPoint(ev.clientX, ev.clientY)
        if (!el) return
        const item = el.closest('.t-item[dragable], .tab[dragable]')
        if (!item) return
        const k = item.getAttribute('dragable') || ''
        if (k && k !== this.dragKey) this.dragOverKey = k
      }
      const up = () => {
        window.removeEventListener('mousemove', move)
        window.removeEventListener('mouseup', up)
        if (started) {
          const from = this.dragKey
          const to = this.dragOverKey
          if (from && to && from !== to) {
            const [fromType, fromId] = this.splitDragKey(from)
            const [toType, toId] = this.splitDragKey(to)
            if (fromType === toType) {
              if (fromType === 'open') this.store.reorderOpenFiles(fromId, toId)
              else if (fromType === 'favfile') this.store.reorderFavoriteFiles(fromId, toId)
              else if (fromType === 'favvault') this.store.reorderFavorites(fromId, toId)
              else if (fromType === 'vault') this.store.reorderVaults(fromId, toId)
            }
          }
        }
        this.dragKey = ''
        this.dragOverKey = ''
        this.dragging = false
      }
      window.addEventListener('mousemove', move)
      window.addEventListener('mouseup', up)
    },
    splitDragKey(key: string): [string, string] {
      const i = key.indexOf(':')
      return i < 0 ? ['', key] : [key.slice(0, i), key.slice(i + 1)]
    },
    /** 分组拖拽状态 class。 */
    secClass(id: string) {
      return {
        dragging: this.dragKey === 'sec:' + id,
        dragover: this.dragOverKey === 'sec:' + id,
      }
    },

    // ---------- 侧边栏分组拖放排序（拖动分组标题调整分组先后；不拖动则折叠/展开） ----------
    onSectionMouseDown(key: string, e: MouseEvent) {
      if (e.button !== 0) return
      // 点分组标题内的按钮（全部关闭 / 清除等）不参与折叠或拖动
      const t = e.target as HTMLElement
      if (t.closest('.acts, .icon-btn')) return
      e.preventDefault()
      const startX = e.clientX
      const startY = e.clientY
      let started = false
      const move = (ev: MouseEvent) => {
        const dx = ev.clientX - startX
        const dy = ev.clientY - startY
        if (!started && Math.abs(dx) + Math.abs(dy) < 4) return
        if (!started) {
          started = true
          this.dragKey = key
          this.dragging = true
          this.dragOverKey = key
          this.dragMoveY = ev.clientY
        }
        const sel = window.getSelection()
        if (sel && sel.rangeCount > 0) sel.removeAllRanges()
        this.dragMoveY = ev.clientY
        const el = document.elementFromPoint(ev.clientX, ev.clientY)
        if (!el) return
        const sec = el.closest('.sec[dragable]')
        if (!sec) return
        const k = sec.getAttribute('dragable') || ''
        if (k && k !== this.dragKey) this.dragOverKey = k
      }
      const up = () => {
        window.removeEventListener('mousemove', move)
        window.removeEventListener('mouseup', up)
        if (started) {
          const from = this.dragKey
          const to = this.dragOverKey
          if (from && to && from !== to) {
            const [, fromId] = this.splitDragKey(from)
            const [, toId] = this.splitDragKey(to)
            if (fromId && toId) this.store.reorderSection(fromId, toId)
          }
        } else {
          // 未拖动：按点击折叠/展开
          const [, id] = this.splitDragKey(key)
          if (id) this.toggleSec(id)
        }
        this.dragKey = ''
        this.dragOverKey = ''
        this.dragging = false
      }
      window.addEventListener('mousemove', move)
      window.addEventListener('mouseup', up)
    },

    // ---------- 保存 ----------
    async saveActive() {
      const f = this.activeFile
      if (!f) return
      if (f.isNew) {
        // 草稿页签：弹出「另存为」选择目标
        this.openSaveAs()
        return
      }
      try {
        await this.store.saveFile(f.path, f.content)
        // 把自己的写入计入基线，免得下一轮把刚保存的文件当成被外部改动重读一遍
        externalChanges.sync([this.watchTargetOf(f)])
        message.success(f.encrypted ? '已加密保存' : '已保存')
      } catch (e) {
        message.error(String(e))
      }
    },

    /** 按设置间隔重建自动保存定时器（间隔 ≤ 0 或取不到正间隔时仅停止）。 */
    applyAutoSave() {
      if (this._autoSaveTimer) {
        clearInterval(this._autoSaveTimer)
        this._autoSaveTimer = null
      }
      const secs = this.store.settings.autoSaveSecs
      if (!secs || secs <= 0) return
      this._autoSaveTimer = setInterval(() => this.autoSaveTick(), secs * 1000)
    },
    /**
     * 自动保存一轮：静默写盘全部符合条件的 dirty 页签（加密与明文一视同仁）。
     * 跳过：未落盘草稿（需用户确认目标）、锁定态页签/锁定库内文件（明文已清零不可写）、
     * 外部冲突页签（磁盘已变，静默覆盖会丢外部修改，留给用户手动解决）。
     * 单个失败不阻断其余，仅当前页签报错提醒。
     */
    async autoSaveTick() {
      const targets = this.openFiles.filter((f) => {
        if (!f.dirty || f.isNew || f.locked) return false
        if (f.external === 'modified' || f.external === 'missing') return false
        if (f.encrypted && this.isVaultFileLocked(f)) return false
        return true
      })
      for (const f of targets) {
        try {
          await this.store.saveFile(f.path, f.content)
          // 把自己的写入计入基线，免得下一轮轮询把自动保存当成外部改动
          externalChanges.sync([this.watchTargetOf(f)])
        } catch (e) {
          // 静默保存失败不打扰用户；仅当前页签提醒（非当前页签留 dirty 标记下轮重试）
          if (f.path === this.activePath) message.warning('自动保存失败：' + String(e))
        }
      }
    },

    // ---------- 外部修改监控 ----------
    /**
     * 页签对应的磁盘监控目标：单文件库（`.mdlb`）内文件不是独立磁盘文件，
     * 取容器路径（页签标识 `库路径#相对路径` 的 `#` 前部分）；其余（普通文件 /
     * 单文件 .mdl / 目录库内文件）本身就是一个磁盘文件，直接用页签路径。
     * 只对单文件库剔离 `#`，避免文件名本身含 `#` 的普通文件被剔错。
     */
    watchTargetOf(f: OpenFile): string {
      const v = this.store.recent.find((x) => x.id === f.vaultId)
      if (v?.isFileVault && f.path.includes('#')) return f.path.split('#')[0]
      return f.path
    },
    /**
     * 参与监控的页签：排除未落盘草稿，也排除任何取不到明文会话的加密页签
     * （锁定的库/页签明文已清零，此时重读会把内容灌回内存，破坏锁定的安全语义）。
     */
    watchableTabs(): OpenFile[] {
      return this.openFiles.filter((f) => !f.isNew && !f.locked && !this.isVaultFileLocked(f))
    },
    /** 当前需轮询戳记的磁盘路径。 */
    externalTargets(): string[] {
      return this.watchableTabs().map((f) => this.watchTargetOf(f))
    },
    /** 按设置开关接入 / 停止监控（监控器是模块级单例，组件只负责接入与摘除）。 */
    applyExternalWatch() {
      if (this.store.settings.watchExternalChanges) {
        externalChanges.attach({
          targets: () => this.externalTargets(),
          onChange: (paths) => this.onExternalChange(paths),
          onMissing: (paths) => this.onExternalMissing(paths),
        })
      } else {
        externalChanges.stop()
        this.openFiles.forEach((f) => (f.external = ''))
      }
    },
    /**
     * 磁盘戳记变化：逐页签重读正文比对（一个 `.mdlb` 容器对应多个页签，容器变了不代表
     * 每个页签都变了）。确认是真外部修改后：无未保存修改 → 直接重新加载；有未保存修改
     * → 不顶掉用户内容，挂角标提醒并等用户选择“重新加载”。
     */
    async onExternalChange(paths: string[]) {
      const hit = new Set(paths)
      for (const f of this.watchableTabs()) {
        if (!hit.has(this.watchTargetOf(f))) continue
        let disk = ''
        try {
          disk = await this.store.readDiskContent(f)
        } catch {
          // 重读失败（文件被占用 / 会话刚好失效）：本轮跳过，下一轮再试
          continue
        }
        if (disk === f.content) {
          // 与内存内容一致（多为自己的写入刚好改变了 mtime）：静默，不打扰用户
          if (f.external === 'missing') f.external = ''
          continue
        }
        if (f.dirty) {
          f.external = 'modified'
          message.warning(`「${this.fileDisplayName(f)}」在外部被修改，但页签有未保存内容；可在页签右键选「重新加载」取磁盘版本`, 6)
        } else {
          f.content = disk
          f.external = ''
          message.info(`「${this.fileDisplayName(f)}」已被外部修改，已重新加载最新内容`)
        }
      }
    },
    /** 磁盘文件消失（被删除 / 重命名 / 整个库被移走）：只提醒不改内容，避开静默丢数据。 */
    onExternalMissing(paths: string[]) {
      const hit = new Set(paths)
      for (const f of this.watchableTabs()) {
        if (!hit.has(this.watchTargetOf(f))) continue
        if (f.external === 'missing') continue
        f.external = 'missing'
        message.error(`「${this.fileDisplayName(f)}」在磁盘上已被删除或移动，页签内容已保留（保存会重新写入）`, 6)
      }
    },
    /** 主动重新加载磁盘版本：页签有未保存修改时先确认（此操作会丢弃内存修改）。 */
    reloadTab(path: string) {
      const f = this.openFiles.find((x) => x.path === path)
      if (!f) return
      if (f.dirty) {
        this.reloadTarget = path
        this.showReloadConfirm = true
        return
      }
      this.doReloadTab(path)
    },
    async doReloadTab(path: string) {
      this.showReloadConfirm = false
      this.reloadTarget = ''
      try {
        const f = await this.store.reloadFromDisk(path)
        message.success(`「${this.fileDisplayName(f)}」已重新加载`)
      } catch (e) {
        message.error(String(e))
      }
    },

    // ---------- 单文件 .mdl 页签内解锁 ----------
    async unlockActiveFile() {
      const f = this.activeFile
      if (!f || !f.locked) return
      if (!this.unlockPwd) return message.warning('请输入密码')
      this.unlocking = true
      try {
        await this.store.unlockFile(f.path, this.unlockPwd)
        this.unlockPwd = ''
        message.success('已解锁')
      } catch (e) {
        message.error(String(e))
      } finally {
        this.unlocking = false
      }
    },

    /** 就地解锁当前页签所在的库：输主密码 → 解锁 → 该库所有页签内容自动恢复。 */
    async unlockActiveVault() {
      const f = this.activeFile
      if (!f) return
      if (!this.vaultUnlockPwd) return message.warning('请输入主密码')
      this.vaultUnlocking = true
      this.vaultUnlockError = ''
      try {
        await this.store.unlockVault(f.vaultId, this.vaultUnlockPwd)
        this.vaultUnlockPwd = ''
        message.success('已解锁')
        // 就地解锁不会经过 EditorView mounted 的统一刷新，这里补加载库树并只展开根层
        await this.openVaultToRoot(f.vaultId)
      } catch (e) {
        this.vaultUnlockError = String(e)
      } finally {
        this.vaultUnlocking = false
      }
    },

    // ---------- 修改主密码（库 / 单文件 .mdl） ----------
    /** 打开改密弹窗；target 为库 id（目录库=目录路径，单文件库=.mdlb 路径，.mdl=文件路径）。 */
    openChangePwd(target: { id: string; path?: string; name?: string; isFileVault?: boolean }) {
      const v = this.store.recent.find((x) => x.id === target.id)
      const isFile = !v ? (target.path || '').endsWith('.mdl') : !(v.isDir || v.isFileVault)
      const isFileVault = v ? !!v.isFileVault : !!target.isFileVault
      // 库形态需先解锁；单文件 .mdl 改密只需旧密码，不依赖会话
      if (v && !isFile && !v.unlocked) return message.warning('请先解锁该库再修改密码')
      const title = isFile
        ? `${v?.name || target.name || (this.fileName(target.path || '') || '文件')}（加密文件）`
        : `${v?.name || target.name || this.fileName(target.id) || '库'}`
      this.changePwdTarget = { id: target.id, path: v?.path || target.path || target.id, title, isFileVault, isPlainMdl: isFile }
      this.changePwdForm = { current: '', next: '', confirm: '' }
      this.showChangePwd = true
    },
    async submitChangePwd() {
      const { current, next, confirm } = this.changePwdForm
      if (!current) return message.warning('请输入当前密码')
      if (next.length < 1) return message.warning('请设置新密码（至少 1 位）')
      if (next !== confirm) return message.warning('两次密码不一致')
      const t = this.changePwdTarget
      this.changingPwd = true
      try {
        await this.store.changeVaultPassword(t.id, t.path, t.isFileVault, current, next)
        message.success('密码已修改')
        this.showChangePwd = false
        const v = this.store.recent.find((x) => x.id === t.id)
        if (v) await this.refreshVaultTree(v.id, v.path)
      } catch (e) {
        message.error(String(e))
      } finally {
        this.changingPwd = false
      }
    },

    // ---------- 新建加密库 ----------
    /** 打开「新建加密库」弹窗（右上角菜单入口）。 */
    openCreateVault() {
      this.createVaultForm = { name: '', path: '', pwd: '', confirm: '' }
      this.showCreateVault = true
    },
    /** 空状态「打开库」：弹出加密文件选择框，选中后走拖放路由（登记/解锁），并确保侧边栏可见。 */
    async openVaultFromEmpty() {
      const p = await tauri.openFileDialog(true)
      if (!p) return
      this.store.updateSettings({ showSidebar: true })
      await this.openDroppedPaths([p])
    },
    /** 空状态「创建库」：打开新建加密库弹窗。 */
    createVaultFromEmpty() {
      this.openCreateVault()
    },
    async browseCreateVaultPath() {
      const path = await tauri.saveFileVaultDialog()
      if (path) this.createVaultForm.path = path
    },
    /** 核心创建逻辑：校验 → 创建单文件库 → 登记 → 解锁 → 刷新树，返回新库 id（失败返回 null）。 */
    async doCreateVault(): Promise<string | null> {
      const { name, path, pwd, confirm } = this.createVaultForm
      if (!path) {
        message.warning('请选择保存位置')
        return null
      }
      if (pwd.length < 1) {
        message.warning('请设置密码（至少 1 位）')
        return null
      }
      if (pwd !== confirm) {
        message.warning('两次密码不一致')
        return null
      }
      this.creatingVault = true
      try {
        const filePath = path.endsWith('.mdlb') ? path : `${path}.mdlb`
        const vname = name.trim() || (this.fileName(filePath) || '').replace(/\.mdlb$/, '') || '未命名'
        await tauri.createFileVault(filePath, pwd, undefined)
        this.store.registerVault({ id: filePath, name: vname, path: filePath, isDir: false, isFileVault: true })
        await this.store.unlockVault(filePath, pwd)
        await this.refreshVaultTree(filePath, filePath)
        message.success('单文件库已创建并解锁')
        return filePath
      } catch (e) {
        message.error(String(e))
        return null
      } finally {
        this.creatingVault = false
      }
    },
    /** 菜单入口的独立「新建加密库」弹窗提交：创建成功后关闭弹窗，并确保侧边栏可见以展示新库。 */
    async submitCreateVault() {
      const dir = await this.doCreateVault()
      if (dir) {
        this.showCreateVault = false
        this.store.updateSettings({ showSidebar: true })
      }
    },

    // ---------- 批量加密 ----------
    /** 打开「批量加密文件」弹窗：重置状态。 */
    openBatchEncrypt() {
      this.bePaths = []
      this.bePassword = ''
      this.beConfirm = ''
      this.beMoveOriginals = false
      this.beBusy = false
      this.beResult = null
      this.showBatchEncrypt = true
    },
    /** 多选添加待加密文件，与已有列表去重。 */
    async beBrowse() {
      const picked = await tauri.openMultipleTextFilesDialog()
      if (!picked.length) return
      const set = new Set(this.bePaths)
      for (const p of picked) if (!set.has(p)) this.bePaths.push(p)
    },
    beRemove(p: string) {
      this.bePaths = this.bePaths.filter((x) => x !== p)
    },
    /** 提交批量加密：非空/密码一致校验 → invoke → 展示结果；完成后若工作目录已挂且相关则刷新。 */
    async submitBatchEncrypt() {
      if (!this.bePaths.length) return message.warning('请先添加需加密的文件')
      if (this.bePassword.length < 1) return message.warning('请设置密码（至少 1 位）')
      if (this.bePassword !== this.beConfirm) return message.warning('两次密码不一致')
      this.beBusy = true
      this.beResult = null
      try {
        const r = await tauri.batchEncrypt(this.bePaths, this.bePassword, this.beMoveOriginals)
        this.beResult = r
        message.success(`加密完成：成功 ${r.encrypted.length} / 失败 ${r.failed.length}`)
        // 相关文件若在工作目录树下，刷新展示
        if (this.workdirRoot) {
          const hit = this.bePaths.some((p) => {
            const a = this.normPath(p)
            const b = this.normPath(this.workdirRoot)
            return a === b || a.startsWith(b + '/') || a.startsWith(b + '\\')
          })
          if (hit) await this.reloadWorkdirDir(this.workdirRoot)
        }
      } catch (e) {
        message.error(String(e))
      } finally {
        this.beBusy = false
      }
    },

    // ---------- 导入目录到库 ----------
    /** 打开「导入目录到库」弹窗：默认目标为已解锁库中首个；无解锁库时默认新建。 */
    openImportDir() {
      this.idSrcDir = ''
      this.idBusy = false
      this.idResult = null
      this.idCreateForm = { name: '', path: '', pwd: '', confirm: '' }
      const firstUnlocked = this.unlockedVaults.find((v: any) => v.isDir || v.isFileVault)
      if (firstUnlocked) {
        this.idTargetKind = 'existing'
        this.idVaultId = firstUnlocked.id
      } else {
        this.idTargetKind = 'new'
        this.idVaultId = ''
      }
      this.showImportDir = true
    },
    async idBrowseSrc() {
      const p = await tauri.openDirDialog()
      if (p) this.idSrcDir = p
    },
    async idBrowseVaultPath() {
      const p = await tauri.saveFileVaultDialog()
      if (p) this.idCreateForm.path = p
    },
    /** 提交导入：先确认目标（必要时新建+解锁），再一次性写盘，完成后刷新目标库树。 */
    async submitImportDir() {
      if (!this.idSrcDir) return message.warning('请先选择要导入的目录')
      let vaultId = ''
      let vaultPath = ''
      if (this.idTargetKind === 'existing') {
        if (!this.idVaultId) return message.warning('请选择一个已解锁的库')
        const v = this.store.recent.find((x) => x.id === this.idVaultId)
        if (!v) return message.warning('未找到目标库')
        if (!v.unlocked) return message.warning('目标库尚未解锁，请先解锁后再导入')
        vaultId = v.id
        vaultPath = v.path
      } else {
        const { name, path, pwd, confirm } = this.idCreateForm
        if (!path) return message.warning('请为新库选择保存位置')
        if (pwd.length < 1) return message.warning('请设置密码（至少 1 位）')
        if (pwd !== confirm) return message.warning('两次密码不一致')
        this.idBusy = true
        try {
          const filePath = path.endsWith('.mdlb') ? path : `${path}.mdlb`
          const vname = name.trim() || (this.fileName(filePath) || '').replace(/\.mdlb$/, '') || '未命名'
          await tauri.createFileVault(filePath, pwd, undefined)
          this.store.registerVault({ id: filePath, name: vname, path: filePath, isDir: false, isFileVault: true })
          await this.store.unlockVault(filePath, pwd)
          vaultId = filePath
          vaultPath = filePath
        } catch (e) {
          message.error(String(e))
          this.idBusy = false
          return
        }
      }
      this.idBusy = true
      this.idResult = null
      try {
        const r = await tauri.importDirToVault(vaultId, this.idSrcDir)
        this.idResult = r
        message.success(`导入完成：成功 ${r.imported.length} / 跳过 ${r.skipped.length} / 失败 ${r.failed.length}`)
        try { await this.refreshVaultTree(vaultId, vaultPath) } catch { /* 刷新失败不阻断 */ }
      } catch (e) {
        message.error(String(e))
      } finally {
        this.idBusy = false
      }
    },
    /** 另存为目标库下拉切换：选到「新建库」时清空内联表单，否则重置目标文件夹。 */
    onSaveAsVaultChange() {
      if (this.saveAsVaultId === '__new__') {
        this.createVaultForm = { name: '', path: '', pwd: '', confirm: '' }
      } else {
        this.saveAsDir = ''
      }
    },
    /** 复制到库目标库下拉切换：同上。 */
    onCopyVaultChange() {
      if (this.copyVaultId === '__new__') {
        this.createVaultForm = { name: '', path: '', pwd: '', confirm: '' }
      } else {
        this.copyDir = ''
      }
    },

    // ---------- 复制到加密库 ----------
    /** 打开「复制到加密库」弹窗：主菜单不传路径→当前激活页签；页签右键传入被右键页签路径（可能非激活）。 */
    openCopyToVault(targetPath?: string) {
      const f = targetPath ? this.openFiles.find((x) => x.path === targetPath) : this.activeFile
      if (!f || f.locked || this.isVaultFileLocked(f)) return
      this.copySourcePath = f.path
      this.copyVaultId = this.store.unlockedVaults.find((x) => x.isDir || x.isFileVault)?.id || ''
      this.copyDir = ''
      this.copyName = (f.name || '未命名').replace(/\.md$|\.mdl$|\.markdown$/, '')
      this.createVaultForm = { name: '', path: '', pwd: '', confirm: '' }
      this.showCopyToVault = true
    },
    /** 复制目标文件内容到目标库（保留原页签不变，另存一份到库内）。 */
    async submitCopyToVault() {
      const f = this.openFiles.find((x) => x.path === this.copySourcePath) || this.activeFile
      if (!f) return
      if (!this.copyVaultId) return message.warning('请选择一个已解锁的库')
      // 内联「新建库」：点复制时先创建库，再复制到库根
      let vaultId = this.copyVaultId
      if (vaultId === '__new__') {
        const created = await this.doCreateVault()
        if (!created) return
        vaultId = created
        this.copyVaultId = created
        this.copyDir = ''
      }
      const name = this.copyName.trim() || '未命名'
      const fileName = name.endsWith('.md') ? name : name + '.md'
      const relPath = this.copyDir ? `${this.copyDir}/${fileName}` : fileName
      this.copying = true
      try {
        const v = this.store.recent.find((x) => x.id === vaultId)
        if (v?.isFileVault) {
          await tauri.createInFileVault(vaultId, v.path, relPath, f.content)
        } else {
          await tauri.createInVault(vaultId, relPath, f.content)
        }
        message.success('已复制到加密库')
        this.showCopyToVault = false
        if (v) await this.refreshVaultTree(v.id, v.path)
      } catch (e) {
        message.error(String(e))
      } finally {
        this.copying = false
      }
    },

    // ---------- 新建草稿 / 另存为 ----------
    /** 页签栏空白区（最右侧页签/加号之后）双击新建文件：落在页签/加号等子元素上则忽略。 */
    onToolbarBlankDblclick(e: MouseEvent) {
      const t = e.target as HTMLElement | null
      if (t && t.closest('.tab, .tab-add, .icon-btn, .tb-sep, .tag')) return
      this.openNewDraft()
    },
    openNewDraft() {
      this.store.openNewDraft('未命名')
      // 预览态下源码面板已收起，切到分屏确保编辑器可见
      if (this.currentMode === 'preview') this.setMode('split')
      // 新页签首次渲染才创建实例，等挂载完成后聚焦，可直接输入
      nextTick(() => {
        this.activeEditor()?.focus?.()
      })
    },
    openSaveAs() {
      this.saveAsConvert = false
      this.saveAsSourcePath = ''
      this.saveAsName = (this.activeFile?.name || '未命名').replace(/\.mdl$/, '')
      this.saveAsPwd = ''
      this.saveAsConfirm = ''
      this.saveAsVaultId = this.store.unlockedVaults.find((x) => x.isDir || x.isFileVault)?.id || ''
      this.saveAsDir = ''
      // 默认页签：有已解锁库→直接切到「存入加密库」，否则默认「加密文件 .mdl」；
      // 保存位置默认预填到工作目录根（若已挂载）
      const hasVault = this.store.unlockedVaults.some((x) => x.isDir || x.isFileVault)
      this.saveAsMode = hasVault ? 'vault' : 'file'
      this.saveAsPath = hasVault ? '' : this.defaultSavePath('file')
      this.createVaultForm = { name: '', path: '', pwd: '', confirm: '' }
      this.showSaveAs = true
    },
    /** 工作目录根 + 当前草稿名拼出默认保存路径（未挂载工作目录则返回空；joinPath 兼容 Windows 反斜杠）。 */
    defaultSavePath(mode: 'file' | 'plain'): string {
      if (!this.workdirRoot) return ''
      const name = this.saveAsName.trim() || '未命名'
      const ext = mode === 'plain' ? 'md' : 'mdl'
      return this.joinPath(this.workdirRoot, name + '.' + ext)
    },
    /** 切换另存目标类型：重置为对应默认路径，避免 file/vault/plain 之间残留不同扩展名的旧路径（挂了工作目录时预填该目录）。 */
    setSaveAsMode(m: 'file' | 'vault' | 'plain') {
      this.saveAsMode = m
      this.saveAsPath = m === 'file' || m === 'plain' ? this.defaultSavePath(m) : ''
    },
    /** 用当前「文件名」替换「保存位置」路径中的文件名部分，保留所在目录与原扩展名（joinPath 兼容 Windows 反斜杠）。 */
    syncSaveAsPathName() {
      if (!this.saveAsPath) return
      const base = this.fileName(this.saveAsPath)
      const ext = (base.match(/\.[^.]+$/) || [''])[0]
      const name = (this.saveAsName.trim() || '未命名').replace(/\.[^.]+$/, '')
      const dir = /[\\/]/.test(this.saveAsPath) ? this.parentDir(this.saveAsPath) : ''
      this.saveAsPath = dir ? this.joinPath(dir, name + ext) : name + ext
    },
    async browseSavePath() {
      const name = this.saveAsName.trim() || '未命名'
      if (this.saveAsMode === 'plain') {
        const path = await tauri.saveFileDialog(name + '.md', 'plain')
        if (path) this.saveAsPath = path
        return
      }
      const path = await tauri.saveFileDialog(name + '.mdl')
      if (path) this.saveAsPath = path
    },
    async submitSaveAs() {
      // 转换态针对右键选中的明文页签；普通另存为针对当前激活的草稿页签
      const srcPath = this.saveAsConvert ? this.saveAsSourcePath : (this.activeFile?.path || '')
      const f = this.openFiles.find((x) => x.path === srcPath)
      if (!f) return
      if (this.saveAsConvert) {
        if (f.isNew || f.encrypted || f.locked) return
      } else if (!f.isNew) {
        return
      }
      const name = (this.saveAsName.trim() || '未命名')
      try {
        if (this.saveAsMode === 'plain') {
          // 普通明文 md：选路径后直接落盘，无需密码
          this.syncSaveAsPathName()
          let filePath = this.saveAsPath
          if (!filePath) {
            const picked = await tauri.saveFileDialog(name + '.md', 'plain')
            if (!picked) return
            filePath = picked
          }
          this.saving = true
          await this.store.saveDraft(f.path, f.content, {
            kind: 'plain',
            filePath,
          })
          message.success('已保存为普通 Markdown 文件')
        } else if (this.saveAsMode === 'vault') {
          if (!this.saveAsVaultId) return message.warning('请选择一个已解锁的库')
          // 内联「新建库」：点保存时先创建库，再存到库根
          let vaultId = this.saveAsVaultId
          if (vaultId === '__new__') {
            const created = await this.doCreateVault()
            if (!created) return
            vaultId = created
            this.saveAsVaultId = created
            this.saveAsDir = ''
          }
          const v = this.store.unlockedVaults.find((x) => x.id === vaultId)
          if (!v) return message.warning('该库未解锁')
          const relPath = this.saveAsDir ? `${this.saveAsDir}/${name}.md` : `${name}.md`
          this.saving = true
          await this.store.saveDraft(f.path, f.content, {
            kind: 'vault',
            vaultId,
            relPath,
          })
          message.success('已存入加密库')
        } else {
          const pwd = this.saveAsPwd
          if (pwd.length < 1) return message.warning('请设置密码（至少 1 位）')
          if (pwd !== this.saveAsConfirm) return message.warning('两次密码不一致')
          // 选择保存位置（先同步文件名，确保保存出的文件名与输入一致）
          this.syncSaveAsPathName()
          let filePath = this.saveAsPath
          if (!filePath) {
            const picked = await tauri.saveFileDialog(name + '.mdl')
            if (!picked) return
            filePath = picked
          }
          // 转换前捕获原明文路径（saveDraft 会把页签 path 改为新 .mdl）
          const originalPath = this.saveAsConvert ? f.path : ''
          this.saving = true
          await this.store.saveDraft(f.path, f.content, {
            kind: 'file',
            filePath,
            password: pwd,
          })
          if (this.saveAsConvert) {
            message.success('已转为加密文件')
            this.showSaveAs = false
            if (this.workdirRoot) await this.refreshWorkdir()
            await this.handleConvertedOriginal(originalPath)
          } else {
            message.success('已保存为加密文件')
          }
        }
        this.showSaveAs = false
      } catch (e) {
        message.error(String(e))
      } finally {
        this.saving = false
        this.saveAsConvert = false
        this.saveAsSourcePath = ''
      }
    },

    /** 页签右键「转为加密文件…」：复用另存为弹窗，仅面向独立加密文件 .mdl。 */
    openConvertDialog(p: string) {
      const f = this.openFiles.find((x) => x.path === p)
      if (!f || f.isNew || f.encrypted || f.locked) return
      this.saveAsConvert = true
      this.saveAsSourcePath = p
      this.saveAsName = (f.name || this.fileName(p)).replace(/\.(md|markdown)$/i, '')
      this.saveAsPwd = ''
      this.saveAsConfirm = ''
      this.saveAsVaultId = ''
      this.saveAsDir = ''
      this.saveAsMode = 'file'
      // 默认落在原明文文件所在目录（而非工作目录根）
      const srcDir = this.parentDir(p)
      this.saveAsPath = srcDir ? this.joinPath(srcDir, (this.saveAsName.trim() || '未命名') + '.mdl') : ''
      this.createVaultForm = { name: '', path: '', pwd: '', confirm: '' }
      this.showSaveAs = true
    },
    /** 转换成功后按设置处理原明文文件：删除 / 保留 / 弹窗询问。 */
    async handleConvertedOriginal(originalPath: string) {
      if (!originalPath) return
      const action = this.store.settings.convertMdAction
      if (action === 'delete') {
        await this.deleteConvertedOriginal(originalPath)
      } else if (action === 'ask') {
        this.convertOriginalPath = originalPath
        this.convertRemember = false
        this.showConvertPrompt = true
      }
      // action === 'keep'：什么都不做
    },
    /** 询问弹窗选择：delete / keep；勾选「以后都按此处理」则写回设置。 */
    async onConvertPrompt(choice: 'delete' | 'keep') {
      if (this.convertRemember) this.store.updateSettings({ convertMdAction: choice })
      const path = this.convertOriginalPath
      this.showConvertPrompt = false
      this.convertOriginalPath = ''
      if (choice === 'delete' && path) await this.deleteConvertedOriginal(path)
    },
    /** 删除转换后的原明文文件（失败不阻断，仅提示）。 */
    async deleteConvertedOriginal(path: string) {
      try {
        await tauri.deletePath(path)
        message.success('已将原明文文件移入回收站')
        if (this.workdirRoot) await this.refreshWorkdir()
      } catch (e) {
        message.error(`删除原文件失败：${String(e)}`)
      }
    },

    // ---------- 新建（库内文件 / 文件夹） ----------
    /** 打开「新建文件」弹窗；dirNode 为空表示建在库根。 */
    openNewFile(v: { id: string; name: string }, dirNode: FsNode | null) {
      this.wdOp = false
      this.newFileVaultId = v.id
      this.newFileDir = dirNode ? this.relPathOf(dirNode) : ''
      this.newFileName = ''
      this.newFileType = 'plain' // 库内新建恒为加密，类型选择仅工作目录生效
      this.showNewFile = true
    },
    /** 打开「新建文件夹」弹窗；dirNode 为空表示建在库根。 */
    openNewFolder(v: { id: string; name: string }, dirNode: FsNode | null) {
      this.wdOp = false
      this.newFolderVaultId = v.id
      this.newFolderDir = dirNode ? this.relPathOf(dirNode) : ''
      this.newFolderName = ''
      this.showNewFolder = true
    },
    /** 由节点 path 推导相对库根的路径（节点 path 是绝对路径，去掉库根前缀）。 */
    relPathOf(dirNode: FsNode): string {
      const v = this.store.recent.find((x) => x.id === this.newFileVaultId || x.id === this.newFolderVaultId)
      const root = v?.path || ''
      if (root && dirNode.path.startsWith(root)) {
        return dirNode.path.slice(root.length).replace(/^\/+/, '')
      }
      return dirNode.path
    },
    /** 通用：节点绝对路径 → 相对库根路径（传入库 id）。 */
    relPathInVault(nodePath: string, vaultId: string): string {
      const v = this.store.recent.find((x) => x.id === vaultId)
      const root = v?.path || ''
      if (root && nodePath.startsWith(root)) {
        return nodePath.slice(root.length).replace(/^\/+/, '')
      }
      return nodePath.replace(/^\/+/, '')
    },
    async submitNewFile() {
      const name = this.newFileName.trim()
      if (!name) return
      if (this.wdOp) {
        // 工作目录新建：按所选类型分流（普通 md / 独立加密文件 .mdl / 单文件库 .mdlb）
        if (this.newFileType === 'plain') await this.wdCreatePlain(name)
        else if (this.newFileType === 'encrypted') await this.wdCreateEncrypted(name)
        else await this.wdCreateVaultFile(name)
        return
      }
      if (!this.newFileVaultId) return message.warning('请先解锁一个库')
      const v = this.store.recent.find((x) => x.id === this.newFileVaultId)
      const fileName = name.endsWith('.md') ? name : name + '.md'
      const relPath = this.newFileDir ? `${this.newFileDir}/${fileName}` : fileName
      try {
        if (v?.isFileVault) {
          await tauri.createInFileVault(this.newFileVaultId, v.path, relPath, '# ' + fileName.replace(/\.md$/, ''))
        } else {
          await tauri.createInVault(this.newFileVaultId, relPath, '# ' + fileName.replace(/\.md$/, ''))
        }
        message.success('已创建')
        this.showNewFile = false
        this.newFileName = ''
        if (v) await this.refreshVaultTree(v.id, v.path)
      } catch (e) {
        message.error(String(e))
      }
    },
    /** 工作目录新建普通 md：无扩展名补 .md，创建后刷新父目录并打开。 */
    async wdCreatePlain(name: string) {
      const fileName = /\.(md|markdown|mdown|mkd|txt)$/i.test(name) ? name : name + '.md'
      const abs = this.joinPath(this.wdDir, fileName)
      try {
        await tauri.createPlainFile(abs, `# ${fileName.replace(/\.[^.]+$/, '')}\n`)
        message.success('已创建')
        this.showNewFile = false
        this.newFileName = ''
        await this.reloadWorkdirDir(this.wdDir)
        await this.store.openPlainFile(abs, fileName)
      } catch (e) {
        message.error(String(e))
      }
    },
    /** 工作目录新建独立加密文件 .mdl：自带独立密码，创建后刷新父目录并打开为锁定态页签（输入密码即解锁）。 */
    async wdCreateEncrypted(name: string) {
      const pwd = this.newFilePwd
      if (pwd.length < 1) return message.warning('请设置密码（至少 1 位）')
      if (pwd !== this.newFileConfirm) return message.warning('两次密码不一致')
      const fileName = name.endsWith('.mdl') ? name : name + '.mdl'
      const abs = this.joinPath(this.wdDir, fileName)
      try {
        await tauri.createFile(abs, pwd, `# ${fileName.replace(/\.[^.]+$/, '')}\n`)
        message.success('已创建加密文件，输入密码即可解锁')
        this.showNewFile = false
        this.newFileName = ''
        this.newFilePwd = ''
        this.newFileConfirm = ''
        await this.reloadWorkdirDir(this.wdDir)
        this.store.openEncryptedFile(abs, fileName)
      } catch (e) {
        message.error(String(e))
      }
    },
    /** 工作目录新建单文件库 .mdlb：复用「新建加密库」核心逻辑（登记 + 解锁 + 刷树），并刷新工作目录树。 */
    async wdCreateVaultFile(name: string) {
      // 先关新建弹窗，避免与 doCreateVault 可能弹出的新建库表单叠加
      this.showNewFile = false
      this.createVaultForm = { name, path: this.joinPath(this.wdDir, name), pwd: this.newFilePwd, confirm: this.newFileConfirm }
      const created = await this.doCreateVault()
      if (!created) return
      this.newFileName = ''
      this.newFilePwd = ''
      this.newFileConfirm = ''
      if (this.workdirRoot) await this.refreshWorkdir()
    },
    async submitNewFolder() {
      const name = this.newFolderName.trim()
      if (!name) return
      if (this.wdOp) {
        const abs = this.joinPath(this.wdDir, name)
        try {
          await tauri.createPlainDir(abs)
          message.success('已创建文件夹')
          this.showNewFolder = false
          this.newFolderName = ''
          await this.reloadWorkdirDir(this.wdDir)
        } catch (e) {
          message.error(String(e))
        }
        return
      }
      if (!this.newFolderVaultId) return message.warning('请先解锁一个库')
      const v = this.store.recent.find((x) => x.id === this.newFolderVaultId)
      const relPath = this.newFolderDir ? `${this.newFolderDir}/${name}` : name
      try {
        if (v?.isFileVault) {
          await tauri.createFolderInFileVault(this.newFolderVaultId, v.path, relPath)
        } else {
          await tauri.createFolderInVault(this.newFolderVaultId, relPath)
        }
        message.success('已创建文件夹')
        this.showNewFolder = false
        this.newFolderName = ''
        if (v) await this.refreshVaultTree(v.id, v.path)
      } catch (e) {
        message.error(String(e))
      }
    },

    // ---------- 库内文件管理（重命名/移动、删除） ----------
    /** 打开重命名/移动弹窗；node 为库内文件或目录。 */
    openRename(vaultId: string, node: FsNode) {
      this.wdOp = false
      this.renameVaultId = vaultId
      this.renameNodePath = node.path
      this.renameIsDir = !!node.is_dir
      // 预填当前相对路径（含子目录），支持直接改成带子目录的路径实现移动
      this.renameName = this.relPathInVault(node.path, vaultId)
      this.showRename = true
    },
    async submitRename() {
      if (this.wdOp) {
        // 工作目录明文重命名/移动：在同一父目录下按输入名（可含子路径）拼接目标
        const newName = this.renameName.trim().replace(/^\/+/, '')
        if (!newName) return message.warning('请输入新名称')
        const parent = this.parentDir(this.wdPath)
        const target = this.joinPath(parent, newName)
        if (target === this.wdPath) {
          this.showRename = false
          return
        }
        this.renaming = true
        try {
          await tauri.renamePath(this.wdPath, target)
          message.success('已重命名')
          this.showRename = false
          // 若被重命名的文件正打开，同步其页签 path/name
          const src = this.wdPath
          const f = this.store.openFiles.find((x) => x.path === src)
          if (f) {
            const nm = newName.split('/').pop() || newName
            f.path = target
            f.name = nm
            if (this.store.activePath === src) this.store.activePath = target
            this.store.persistOpenFiles()
          }
          await this.reloadWorkdirDir(parent)
        } catch (e) {
          message.error(String(e))
        } finally {
          this.renaming = false
        }
        return
      }
      const newRel = this.renameName.trim().replace(/^\/+/, '')
      if (!newRel) return message.warning('请输入新名称')
      const oldRel = this.relPathInVault(this.renameNodePath, this.renameVaultId)
      if (newRel === oldRel) {
        this.showRename = false
        return
      }
      this.renaming = true
      try {
        const v = this.store.recent.find((x) => x.id === this.renameVaultId)
        if (v?.isFileVault) {
          await tauri.renameInFileVault(this.renameVaultId, v.path, oldRel, newRel)
        } else {
          await tauri.renameInVault(this.renameVaultId, oldRel, newRel)
        }
        message.success('已重命名')
        this.showRename = false
        // 若被重命名的文件当前已打开，同步更新其 path/name
        this.syncOpenedFileAfterRename(oldRel, newRel)
        if (v) await this.refreshVaultTree(v.id, v.path)
      } catch (e) {
        message.error(String(e))
      } finally {
        this.renaming = false
      }
    },
    /** 重命名/移动后，同步已打开页签的 path/name（若该文件正打开）。 */
    syncOpenedFileAfterRename(oldRel: string, newRel: string) {
      const v = this.store.recent.find((x) => x.id === this.renameVaultId)
      const root = v?.path || ''
      const oldAbs = root ? `${root}/${oldRel}` : oldRel
      const newAbs = root ? `${root}/${newRel}` : newRel
      // 单文件库内文件：path 用 `库路径#相对路径` 标识
      const oldKey = v?.isFileVault ? `${root}#${oldRel}` : oldAbs
      const newKey = v?.isFileVault ? `${root}#${newRel}` : newAbs
      const f = this.store.openFiles.find((x) => x.path === oldKey)
      if (f) {
        f.path = newKey
        f.name = newRel.split('/').pop() || newRel
        f.relPath = newRel
        if (this.store.activePath === oldKey) this.store.activePath = newKey
        this.store.persistOpenFiles()
      }
    },
    /** 打开删除确认弹窗。 */
    openDelete(vaultId: string, node: FsNode) {
      this.wdOp = false
      this.deleteVaultId = vaultId
      this.deleteNodePath = node.path
      this.deleteIsDir = !!node.is_dir
      this.showDelete = true
    },
    async submitDelete() {
      if (this.wdOp) {
        // 工作目录明文删除：移入系统回收站（可恢复），并关闭其已打开页签
        this.deleting = true
        const target = this.wdPath
        try {
          await tauri.deletePath(target)
          message.success('已移入回收站')
          this.showDelete = false
          this.store.closeFile(target)
          this.store.dropClosedTab(target)
          await this.reloadWorkdirDir(this.parentDir(target))
        } catch (e) {
          message.error(String(e))
        } finally {
          this.deleting = false
        }
        return
      }
      const rel = this.relPathInVault(this.deleteNodePath, this.deleteVaultId)
      this.deleting = true
      try {
        const v = this.store.recent.find((x) => x.id === this.deleteVaultId)
        if (v?.isFileVault) {
          await tauri.deleteInFileVault(this.deleteVaultId, v.path, rel)
        } else {
          await tauri.deleteInVault(this.deleteVaultId, rel)
        }
        message.success('已删除')
        this.showDelete = false
        // 若删除的是已打开的文件，关闭其页签
        const root = v?.path || ''
        const key = v?.isFileVault ? `${root}#${rel}` : (root ? `${root}/${rel}` : rel)
        this.store.closeFile(key)
        this.store.dropClosedTab(key)
        if (v) await this.refreshVaultTree(v.id, v.path)
      } catch (e) {
        message.error(String(e))
      } finally {
        this.deleting = false
      }
    },

    // ---------- 锁定 ----------
    async lockVault(id: string) {
      await this.store.lockVault(id)
      message.info('已锁定')
    },
    /**
     * 加密库行点击：已解锁则展开/折叠库内树；锁定态则走解锁流程（跳解锁页输密码），
     * 支持锁定后不移除、二次点击快速重开。
     */
    onVaultRowClick(v: { id: string; unlocked: boolean }) {
      if (v.unlocked) this.toggleTree('v-' + v.id)
      else this.recentOpenVault(v.id)
    },
    /**
     * 从「加密库」列表取消登记（不删除磁盘文件）：若处于解锁态先锁定以清零内存密钥，
     * 再移出 recent 并清理本地树缓存；收藏为独立快照不受影响，仍可在收藏区重新登记。
     */
    async removeVaultFromList(id: string) {
      const v = this.store.recent.find((x) => x.id === id)
      if (v?.unlocked) {
        try {
          await this.store.lockVault(id)
        } catch {
          // 锁定失败不阻断移除（会话可能已自然过期）
        }
      }
      this.store.removeVault(id)
      delete this.vaultTrees[id]
      message.info('已关闭库（磁盘文件未删除）')
    },
    async lockAll() {
      await this.store.lockAll()
      message.info('已全部锁定')
    },
    async reapExpired() {
      const ids = await this.store.reapExpired()
      if (ids.length) {
        message.warning(`${ids.length} 个库已自动锁定`)
        // 刷新树
        ids.forEach((id) => {
          const v = this.store.recent.find((x) => x.id === id)
          if (v) this.refreshVaultTree(id, v.path)
        })
      }
    },
    /** 状态栏「自动锁定」点击弹档位菜单（复用右键菜单的定位与渲染）；档位与设置页「无操作自动锁定」保持一致。 */
    openAutoLockMenu(e: MouseEvent) {
      const cur = this.store.settings.autoLockSecs
      const items = [
        { label: '1 分钟', v: 60 },
        { label: '5 分钟', v: 300 },
        { label: '15 分钟', v: 900 },
        { label: '30 分钟', v: 1800 },
        { label: '不自动锁定', v: 0 },
      ].map((o) => ({
        kind: 'item',
        label: o.label,
        checked: cur === o.v,
        hint: o.v === DEFAULT_SETTINGS.autoLockSecs ? '默认' : '',
        run: () => this.store.updateSettings({ autoLockSecs: o.v }),
      }))
      // 垂直抬到状态栏上方、水平与本项左对齐（具体夹取规则见 showCtxMenu 的 above 分支）
      this.showCtxMenu(e, items, e.currentTarget as HTMLElement, '自动锁定间隔')
    },
    /** 状态栏「字号」点击弹字号菜单；档位与设置页「编辑器字体大小」一致（10–28，2px 步进，对应 ⌘+/⌘-）。 */
    openFontSizeMenu(e: MouseEvent) {
      const cur = this.store.settings.editorFontSize
      const items = Array.from({ length: 10 }, (_, i) => 10 + i * 2).map((v) => ({
        kind: 'item',
        label: `${v} px`,
        checked: cur === v,
        hint: v === DEFAULT_SETTINGS.editorFontSize ? '默认' : '',
        run: () => this.store.updateSettings({ editorFontSize: v }),
      }))
      // 垂直抬到状态栏上方、水平与本项左对齐（与自动锁定菜单同一套定位规则）
      this.showCtxMenu(e, items, e.currentTarget as HTMLElement, '字体大小')
    },

    // ---------- 打开库（从最近/工作目录点击） ----------
    async recentOpenVault(id: string) {
      let v = this.store.recent.find((x) => x.id === id)
      if (!v) {
        // 库已被移出登记（如清空最近打开）：若其被收藏，则用收藏快照重新登记
        const fav = this.store.favorites.find((x) => x.id === id)
        if (!fav) return
        v = this.store.registerVault({ id: fav.id, name: fav.name, path: fav.path, isDir: fav.isDir, isFileVault: fav.isFileVault })
      }
      // 单文件 .mdl（非目录、非单文件库）：不作为库处理，直接开加密文件页签（内容区密码框）
      if (!v.isDir && !v.isFileVault) {
        // 收藏/最近是快照，文件可能已被移动或删除：打开前先校验，失效则清理登记
        if (!(await tauri.pathExists(v.path))) {
          this.store.removeVault(v.id)
          message.warning(`文件不存在（可能已被移动或删除），已从列表移除：${v.name}`)
          return
        }
        this.store.openEncryptedFile(v.path, v.name)
        return
      }
      if (!v.unlocked) {
        this.$router.push({ path: '/unlock', query: { id } })
        return
      }
      await this.refreshVaultTree(id, v.path)
    },
    async workdirOpenVault(node: FsNode) {
      // 工作目录里点击库节点：登记并跳转解锁（或已解锁则展开/打开）
      const isDir = node.is_dir
      // 单文件 .mdl：node.is_vault 为 true 且非目录、非 .mdlb 库
      const isFileVault = !isDir && node.name.endsWith('.mdlb')
      if (!isDir && !isFileVault) {
        // 单文件 .mdl：直接开加密文件页签（内容区密码框）
        this.store.openEncryptedFile(node.path, node.name.replace(/\.mdl$/, ''))
        return
      }
      const existing = this.store.recent.find((v) => v.path === node.path)
      if (existing && existing.unlocked) {
        await this.refreshVaultTree(existing.id, node.path)
        return
      }
      this.store.registerVault({
        id: node.path,
        name: node.name.replace(/\.mdlb$/, ''),
        path: node.path,
        isDir,
        isFileVault,
      })
      this.$router.push({ path: '/unlock', query: { id: node.path } })
    },

    // ---------- 打开文件（从最近/收藏点击） ----------
    async recentOpenFile(f: { path: string; name: string; vaultId: string; encrypted: boolean }) {
      // 收藏/最近是磁盘快照，文件可能已被移动或删除：
      // 直接按路径打开的两类（单文件 .mdl / 明文）先校验存在性，失效则清理条目而不是报错；
      // 库内文件的存在性由解锁页校验所属库，此处 path 可能是「库路径#相对路径」合成键，不能直接 stat。
      if (!f.encrypted || f.vaultId === f.path) {
        if (!(await tauri.pathExists(f.path))) {
          const wasFav = this.store.isFavoriteFile(f.path)
          if (wasFav) this.store.toggleFavoriteFile(f.path)
          this.store.forgetFile(f.path)
          message.warning(`文件不存在（可能已被移动或删除），已从${wasFav ? '收藏与最近' : '最近'}中移除：${f.name}`)
          return
        }
      }
      if (f.encrypted) {
        // 单文件 .mdl（vaultId 即文件路径）：直接开页签，内容区显示密码框
        if (f.vaultId === f.path) {
          this.store.openEncryptedFile(f.path, f.name)
          return
        }
        // 库内加密文件：检查所属库是否已解锁（若已被移出登记，则用收藏快照重建登记，保证解锁页能列出该库）
        let v = this.store.recent.find((x) => x.id === f.vaultId)
        if (!v) {
          const fav = this.store.favorites.find((x) => x.id === f.vaultId)
          if (fav) {
            v = this.store.registerVault({ id: fav.id, name: fav.name, path: fav.path, isDir: fav.isDir, isFileVault: fav.isFileVault })
          } else if (f.vaultId) {
            // 库本身未被收藏：vaultId 即库根路径（目录库为目录路径，单文件库为 .mdlb 路径），据此重建
            const isFileVault = /\.mdlb$/i.test(f.vaultId)
            const base = f.vaultId.split(/[\\/]/).pop() || f.vaultId
            const name = isFileVault ? base.replace(/\.mdlb$/i, '') : base
            v = this.store.registerVault({ id: f.vaultId, name, path: f.vaultId, isDir: !isFileVault, isFileVault })
          }
        }
        if (!v || !v.unlocked) {
          // 解锁后自动打开该文件
          this.store.pendingOpen = { vaultId: f.vaultId, path: f.path, name: f.name }
          this.$router.push('/unlock')
          return
        }
        try {
          if (v.isFileVault) {
            // 单文件库内文件：path 是 `库路径#相对路径`
            const rel = f.path.includes('#') ? f.path.split('#').slice(1).join('#') : f.path
            await this.store.openFileVaultFile(f.vaultId, rel, f.name)
          } else {
            await this.store.openFile(f.vaultId, f.path, f.name)
          }
        } catch (e) {
          message.error(String(e))
        }
      } else {
        // 普通文件：直接明文打开
        try {
          await this.store.openPlainFile(f.path, f.name)
        } catch (e) {
          message.error(String(e))
        }
      }
    },

    // ---------- 拖入 / 系统打开 ----------
    /** 从路径取文件名。兼容 Windows 绝对路径（`\`）与 POSIX/相对路径（`/`）：
     *  仅按 `/` 硬切会让 `C:\...\a.md` 整串落到页签/侧边栏当作显示名。 */
    fileName(path: string) {
      return path.split(/[\\/]/).pop() || path
    },
    /** 从后端拉取并清空待打开路径，再走统一的拖放路由（冷启动与运行中二次打开共用）。 */
    async drainPendingOpenPaths() {
      // 先等冷启动的「恢复页签」跑完：两者并发对同一文件各跑一次 openPlainFile 时，
      // 查重都发生在读盘之前，会建出两个同 path 的页签；且激活态会被恢复流程
      // 最后一次赋值抢走，双击打开的文件反而不靠前。
      try {
        await this._sessionRestore
      } catch {
        // 恢复失败不阻塞外部打开
      }
      try {
        const paths = await tauri.takePendingOpenPaths()
        if (paths && paths.length) await this.openDroppedPaths(paths)
      } catch {
        // 忽略：无缓存或命令不可用
      }
    },
    /**
     * 处理拖入或系统打开的一组路径（可以来自工作目录以外的任何位置）：
     * 普通文件→明文直接打开；目录→挂载为工作目录；
     * 加密库/.mdl→解锁；库内文件→解锁所属库后自动打开。
     */
    async openDroppedPaths(paths: string[]) {
      let workdirMounted = false
      let unlockTarget: string | null = null
      for (const p of paths) {
        try {
          const info = await tauri.inspectPath(p)
          if (info.is_dir) {
            if (info.is_vault) {
              // 目录库：登记并解锁（已解锁则展开树）
              const existing = this.store.recent.find((v) => v.id === p)
              if (existing && existing.unlocked) {
                await this.refreshVaultTree(existing.id, p)
                this._expandVaultSection()
                continue
              }
              if (!unlockTarget) {
                this.store.registerVault({ id: p, name: this.fileName(p), path: p, isDir: true, isFileVault: false })
                unlockTarget = p
              }
            } else if (!workdirMounted) {
              // 普通目录：挂载为工作目录
              await this.setWorkdir(p)
              workdirMounted = true
            }
          } else if (info.is_vault) {
            if (info.is_file_vault) {
              // 单文件库（.mdlb）：登记为库并解锁（已解锁则展开树）
              const existing = this.store.recent.find((v) => v.id === p)
              if (existing && existing.unlocked) {
                await this.refreshVaultTree(existing.id, p)
                this._expandVaultSection()
                continue
              }
              if (!unlockTarget) {
                this.store.registerVault({ id: p, name: this.fileName(p).replace(/\.mdlb$/, ''), path: p, isDir: false, isFileVault: true })
                unlockTarget = p
              }
            } else {
              // 单文件加密文件（.mdl）：直接开页签，内容区显示密码框
              this.store.openEncryptedFile(p, this.fileName(p))
            }
          } else if (info.parent_vault) {
            // 库内文件：所属库已解锁直接打开，否则登记库 + 暂存待打开
            const pv = info.parent_vault
            const existing = this.store.recent.find((v) => v.id === pv)
            if (existing && existing.unlocked) {
              await this.store.openFile(pv, p, this.fileName(p))
              continue
            }
            this.store.registerVault({ id: pv, name: this.fileName(pv), path: pv, isDir: true, isFileVault: false })
            if (!unlockTarget) {
              this.store.pendingOpen = { vaultId: pv, path: p, name: this.fileName(p) }
              unlockTarget = pv
            }
          } else {
            // 普通文件：明文直接打开
            await this.store.openPlainFile(p, this.fileName(p))
          }
        } catch (e) {
          message.error(String(e))
        }
      }
      // 带上目标库 id，解锁页据此默认选中刚拖入的库（多库时不会漏选）
      if (unlockTarget) {
        // 跳转解锁页前展开「加密库」分组，解锁回来后状态已持久化
        this._expandVaultSection()
        this.$router.push({ path: '/unlock', query: { id: unlockTarget } })
      }
    },

    // ---------- 图片/文件资源：粘贴与拖入（落进同级 `.dat` 目录） ----------
    /** 是否可接收资源：已落盘、未锁定的页签（草稿尚无落点，锁定页无明文可写）。 */
    isAssetTarget(f: OpenFile | undefined): f is OpenFile {
      return !!f && !f.locked && !f.isNew && !!f.path
    },
    /** 计算当前页签的资源目录名与锚点目录（绝对）：
     *  - 普通/.mdl：`文件名.dat`，锚点 = 文件所在目录
     *  - 库内文件：`库名.dat`，锚点 = 库容器所在目录（与 .mdlb 同级） */
    assetAnchorInfo(f: OpenFile): { datDirName: string; anchorDir: string } {
      const isVaultFile = !!f.vaultId && f.path.includes('#')
      if (isVaultFile) {
        const vaultPath = f.path.split('#')[0]
        const base = this.fileName(vaultPath).replace(/\.mdlb$/i, '')
        return { datDirName: base + '.dat', anchorDir: this.parentDir(vaultPath) }
      }
      const base = this.fileName(f.path).replace(/\.[^.]+$/, '')
      return { datDirName: base + '.dat', anchorDir: this.parentDir(f.path) }
    },
    /** 文件名是否为图片（决定插入 `![..]` 还是 `[..]`）。 */
    isImageName(name: string): boolean {
      return /\.(png|jpe?g|gif|webp|bmp|svg|avif|tiff?|ico)$/i.test(name)
    },
    /** 把资源链接插入正文光标处；路径含空格/括号用 `<…>` 包裹保证 markdown 合法。
     *  copyMode=true 写相对 `名.dat/文件名`；false（仅链接）写传入的绝对路径。 */
    insertAssetLink(datDirName: string, storedName: string, absSrc: string, copyMode: boolean) {
      const link = copyMode ? `${datDirName}/${storedName}` : absSrc
      const safe = /[\s()]/.test(link) ? `<${link}>` : link
      const alt = storedName.replace(/\.[^.]+$/, '') || storedName
      const text = this.isImageName(storedName) ? `![${alt}](${safe})` : `[${storedName}](${safe})`
      this.activeEditor()?.insertText('\n' + text + '\n')
    },
    /** 读取剪贴板 File 为 data URL（base64 内联）。 */
    readAsDataUrl(file: File): Promise<string> {
      return new Promise((resolve, reject) => {
        const r = new FileReader()
        r.onload = () => resolve(String(r.result || ''))
        r.onerror = () => reject(r.error)
        r.readAsDataURL(file)
      })
    },
    /** 为无名剪贴板图片按 MIME 生成带扩展名的文件名（截图常无 name）。 */
    genPastedName(file: File): string {
      const extByMime: Record<string, string> = {
        'image/png': 'png',
        'image/jpeg': 'jpg',
        'image/gif': 'gif',
        'image/webp': 'webp',
        'image/bmp': 'bmp',
        'image/svg+xml': 'svg',
        'image/avif': 'avif',
      }
      if (file.name) return file.name
      const ext = extByMime[file.type] || 'png'
      const ts = new Date().toISOString().replace(/[:.]/g, '-').replace('T', '_').slice(0, 19)
      return `image_${ts}.${ext}`
    },
    /** 粘贴处理：仅当有可编辑页签且剪贴板确含文件/图片时接管，否则放行给 CodeMirror 粘贴文本。 */
    async handlePaste(e: ClipboardEvent) {
      const f = this.activeFile
      const cd = e.clipboardData
      if (!cd) return
      const files = cd.files ? Array.from(cd.files) : []
      const types = cd.types ? Array.from(cd.types) : []
      const hasFileObj = files.length > 0
      // WKWebView 从访达拷贝文件时 files 为空，但 types 里留有 'Files' 标记
      const hasFilesType = types.includes('Files') || types.some((t) => /^image\//i.test(t))
      const hasAsset = hasFileObj || hasFilesType
      // 未保存的新建草稿：资源要落进「文件同级 .dat」，必须先落盘确定位置——提醒保存并弹出保存框。
      if (f && f.isNew) {
        if (!hasAsset) return // 纯文本粘贴：放行给 CodeMirror 正常处理
        e.preventDefault()
        e.stopPropagation()
        message.info('请先保存文件，再粘贴图片或文件')
        this.openSaveAs()
        return
      }
      if (!this.isAssetTarget(f)) return
      if (!hasAsset) return
      e.preventDefault()
      e.stopPropagation()
      const { datDirName, anchorDir } = this.assetAnchorInfo(f)
      const destDir = this.joinPath(anchorDir, datDirName)
      try {
        if (hasFileObj) {
          // 有文件对象（截图/图片字节）：读出 base64 落盘进 .dat
          for (const file of files) {
            const dataUrl = await this.readAsDataUrl(file)
            const finalAbs = await tauri.writeAssetBytes(destDir, this.genPastedName(file), dataUrl)
            this.insertAssetLink(datDirName, this.fileName(finalAbs), finalAbs, true)
          }
          message.success(`已粘贴 ${files.length} 个资源到 ${datDirName}`)
          return
        }
        // 有 Files 类型但无对象：macOS 走原生剪贴板取路径兜底
        let paths: string[] = []
        try {
          paths = await tauri.clipboardFilePaths()
        } catch {
          paths = []
        }
        if (!paths.length) {
          message.info('剪贴板中的文件暂不可读取，请改用拖入')
          return
        }
        for (const p of paths) {
          const finalAbs = await tauri.copyAsset(destDir, p)
          this.insertAssetLink(datDirName, this.fileName(finalAbs), finalAbs, true)
        }
        message.success(`已粘贴 ${paths.length} 个资源到 ${datDirName}`)
      } catch (err) {
        message.error(String(err))
      }
    },
    /** 从拖入路径里挑出「资源文件」（有扩展名且非文档/库类型）；目录与文档留给原有打开逻辑。 */
    assetCandidates(paths: string[]): string[] {
      const doc = /^(md|markdown|mdown|mkd|txt|mdl|mdlb)$/i
      return paths.filter((p) => {
        const m = p.match(/\.([^.\\/]+)$/)
        return m ? !doc.test(m[1]) : false
      })
    },
    /** 拖入统一入口：正在编辑且有非文档文件拖入 → 询问复制/仅链接；否则走原有打开路由。 */
    routeDroppedPaths(paths: string[]) {
      const f = this.activeFile
      if (this.isAssetTarget(f)) {
        const assets = this.assetCandidates(paths)
        if (assets.length) {
          this.assetPromptFiles = assets
          this.showAssetPrompt = true
          return
        }
      }
      this.openDroppedPaths(paths)
    },
    /** 关闭询问弹窗（不处理，也不打开）。 */
    cancelAssetPrompt() {
      this.showAssetPrompt = false
      this.assetPromptFiles = []
    },
    /** 询问弹窗选择：copy=复制进 .dat 并写相对链接；link=仅写绝对路径链接。 */
    async resolveAssetPrompt(mode: 'copy' | 'link') {
      const f = this.activeFile
      if (!this.isAssetTarget(f) || !this.assetPromptFiles.length) return this.cancelAssetPrompt()
      this.assetBusy = true
      const { datDirName, anchorDir } = this.assetAnchorInfo(f)
      const destDir = this.joinPath(anchorDir, datDirName)
      const list = this.assetPromptFiles.slice()
      try {
        for (const p of list) {
          if (mode === 'copy') {
            const finalAbs = await tauri.copyAsset(destDir, p)
            this.insertAssetLink(datDirName, this.fileName(finalAbs), finalAbs, true)
          } else {
            this.insertAssetLink(datDirName, this.fileName(p), p, false)
          }
        }
        message.success(
          mode === 'copy' ? `已复制 ${list.length} 个资源到 ${datDirName}` : `已链接 ${list.length} 个资源`,
        )
        this.showAssetPrompt = false
        this.assetPromptFiles = []
      } catch (err) {
        message.error(String(err))
      } finally {
        this.assetBusy = false
      }
    },

    // ---------- Markdown 渲染（marked + highlight.js，全局配置见模块顶部 marked.use） ----------
    renderMarkdown(src: string) {
      if (!src) return ''
      // 解析相对图片路径的基准目录 = 当前文件（库内文件取所属库）所在目录；导出/无页签时置空
      const af = this.activeFile
      __assetBaseDir = af && af.path ? this.assetAnchorInfo(af).anchorDir : ''
      // 逐顶层块渲染并标注起始源码行 data-l，供分屏行级滚动同步定位。
      // 包以 div.mb（无 margin/padding/border，子元素上下边距仍与包裹前同样外边距高度叠加，不改变排版）。
      const tokens = marked.lexer(src)
      let off = 0
      let html = ''
      for (const t of tokens) {
        const raw = (t as any).raw || ''
        if (t.type !== 'space') {
          const line = src.slice(0, off).split('\n').length
          html += `<div class="mb" data-l="${line}">${marked.parser([t])}</div>`
        }
        off += raw.length
      }
      return html
    },

    // ---------- 导出 HTML ----------
    /**
     * 把当前页签的 Markdown 渲染为独立可打开的 HTML（内嵌样式与代码高亮配色），
     * 经保存对话框选路径后明文写盘。内容取自内存正文（未保存的编辑也会被导出）；
     * 锁定/无内容时提示。主题与代码高亮跟随当前设置，导出后离线用浏览器打开即可还原观感。
     * 正文里链接到 `.dat` 目录等本地资源会复制到导出目录旁的 `.dat` 并改写为相对链接
     * （与文件同策略，见 inlineExportAssets）；复制失败时图片退回内联 data URL。
     */
    async exportHtml(targetPath?: string) {
      // 主菜单入口不传路径→当前激活页签；页签右键菜单传入被右键页签的路径（可能非激活）。
      const f = targetPath ? this.openFiles.find((x) => x.path === targetPath) : this.activeFile
      if (!f) {
        message.warning('请先打开要导出的文件')
        return
      }
      if (f.locked) {
        message.warning('文件已锁定，请先解锁再导出')
        return
      }
      if (!f.content) {
        message.warning('文件内容为空，无内容可导出')
        return
      }
      // 取展示名的扩展名替换为 .html（草稿名无扩展名时直接追加）
      const base = f.name.replace(/\.[^.]+$/, '') || f.name || '未命名'
      const target = await tauri.saveHtmlDialog(base + '.html')
      if (!target) return
      // 复用预览渲染，但去掉仅在应用内有效的代码块复制按钮（离线 HTML 里无脚本）。
      // 基准目录临时指向被导出文件（可能非激活页签），保证相对图片链接解析正确，渲完恢复。
      const savedBase = __assetBaseDir
      __assetBaseDir = this.assetAnchorInfo(f).anchorDir
      let body: string
      try {
        body = this.renderMarkdown(f.content)
      } finally {
        __assetBaseDir = savedBase
      }
      body = body.replace(/<button[^>]*class="code-copy-btn"[\s\S]*?<\/button>/g, '')
      // 预览里可点的任务复选框在离线页面无回写能力，补回 disabled 保持只读观感
      body = body.replace(/<input type="checkbox" class="task-cb"/g, '<input type="checkbox" disabled="" class="task-cb"')
      // 本地资源（asset 协议 URL）：图片内联 data URL，其它文件复制到导出目录旁
      body = await this.inlineExportAssets(body, target)
      const html = this.buildStandaloneHtml(f.name, body)
      try {
        await tauri.writePlainFile(target, html)
        message.success('已导出 HTML：' + target.split(/[\\/]/).pop())
      } catch (e) {
        message.error(String(e))
      }
    },
    /** 从 asset 协议 URL 反解本地绝对路径。主机写法与编码形态各平台/版本不一，逐一探测：
     *  - 编码形态（如 macOS wry：`asset://localhost/%2FUsers%2F…`）：整串 decodeURIComponent；
     *  - 未编码形态（`asset://localhost//Users/…`、`asset://localhost/Users/a%20b…`）：
     *    decodeURI 只还原 %XX 转义、保留字面 '/'；
     *  - 盘符开头（C:/…）补回前导 '/'。解错只会导致复制失败，后续仍有内联兜底。 */
    assetUrlToLocalPath(ref: string): string {
      const i = ref.indexOf('://')
      if (i < 0) return ''
      const after = ref.slice(i + 3)
      const j = after.indexOf('/')
      const segA = j < 0 ? after : after.slice(0, j) // 主机位（编码形态可能整串路径都在这）
      const rem = j < 0 ? '' : after.slice(j + 1) // 首斜杠之后的剩余段
      const dec = (s: string) => {
        try {
          return decodeURIComponent(s)
        } catch {
          return null
        }
      }
      const decURI = (s: string) => {
        try {
          return decodeURI(s)
        } catch {
          return s
        }
      }
      const looksPath = (s: string) => !!s && (s.startsWith('/') || /^[a-zA-Z]:[\\/]/.test(s))
      const fix = (s: string) => {
        if (!s) return s
        if (/^[a-zA-Z]:[\\/]/.test(s)) return '/' + s
        if (!s.startsWith('/')) return '/' + s
        return s
      }
      // 1) 主机位整串解码即得合法本地路径 → 编码形态（asset://localhost/%2F…）
      const pA = dec(segA)
      if (pA && looksPath(pA)) return fix(pA)
      // 2) 主机位是转义过的且无剩余段（asset://<编码路径> 无主机写法）
      if (pA !== null && pA !== segA && !rem) return fix(pA)
      // 3) 剩余段含字面 '/' → 未编码形态；注意不用 decURI 解 '%2F'（可能是真实目录名）
      if (rem.includes('/')) return fix(decURI(rem))
      // 4) 剩余段无 '/'：整串解码；非法 '%' 则按 %2F 切段逐段解码拼回
      let pR = dec(rem)
      if (pR === null && rem.includes('%2F')) {
        pR = rem
          .split(/%2F/i)
          .map((s) => {
            try {
              return decodeURIComponent(s)
            } catch {
              return s
            }
          })
          .join('/')
      }
      if (pR && looksPath(pR)) return fix(pR)
      // 5) 兜底：原样保留
      return fix(decURI(rem || segA))
    },
    /** 导出 HTML 的后处理：把渲染产物里指向本地磁盘（asset 协议）的资源变成离线可看：
     *  - 统一复制到导出文件同级的 `目录名.dat/` 下，src/href 改写为相对链接（与文件一致，
     *    图片无需再内联，浏览器双击打开即可显示）；含空格括号则 `<…>` 包裹；
     *  - 复制失败时图片退回读文件内联 data URL；仍失败则原样保留不阻断导出。
     *  同一 src 多次出现一并替换；http/data 等外部链接不处理。 */
    async inlineExportAssets(body: string, target: string): Promise<string> {
      const refs = new Set<string>()
      const re = /(?:src|href)="([^"]*)"/g
      let m: RegExpExecArray | null
      while ((m = re.exec(body))) {
        if (/^assets?:\/\//i.test(m[1])) refs.add(m[1])
      }
      const exportDir = this.parentDir(target)
      const exportBase = this.fileName(exportDir) || '导出'
      for (const ref of refs) {
        const abs = this.assetUrlToLocalPath(ref)
        if (!abs) continue
        // 与文件同策略：复制到导出目录旁的 `.dat`，改写为相对路径
        try {
          const destDir = this.joinPath(exportDir, exportBase + '.dat')
          const finalAbs = await tauri.copyAsset(destDir, abs)
          const rel = `${exportBase}.dat/${this.fileName(finalAbs)}`
          const href = /[\s()]/.test(rel) ? `<${rel}>` : rel
          body = body.split(ref).join(href)
          continue
        } catch {}
        // 失败兜底：图片内联 data URL（自包含），其它原样保留
        if (this.isImageName(abs)) {
          try {
            const dataUrl = await tauri.readAssetDataUrl(abs)
            body = body.split(ref).join(dataUrl)
          } catch {}
        }
      }
      return body
    },
    /** 组装独立 HTML 文档：内嵌 Markdown 版式（内容栏宽跟随「预览内容最大宽度」设置）+ 当前代码高亮主题的 token 配色（具体色值，不依赖 CSS 变量）。 */
    buildStandaloneHtml(title: string, bodyHtml: string): string {
      const dark = document.documentElement.getAttribute('data-theme') === 'dark'
      const codeTheme = this.store.settings.codeTheme
      const indent = this.store.settings.codeIndent === 2 ? 2 : 4
      // 各主题 token 配色（github 随明暗切换，其余为应用内一致的暗色系）
      const HL: Record<string, string> = {
        comment: '', keyword: '', string: '', number: '', title: '', attr: '', built: '', meta: '',
      }
      const palettes: Record<string, { light?: Record<string, string>; dark?: Record<string, string>; fixed?: Record<string, string> }> = {
        github: {
          light: { comment: '#6a737d', keyword: '#cf222e', string: '#0a3069', number: '#0550ae', title: '#8250df', attr: '#953800', built: '#953800', meta: '#57606a' },
          dark: { comment: '#8b949e', keyword: '#ff7b72', string: '#a5d6ff', number: '#79c0ff', title: '#d2a8ff', attr: '#ffa657', built: '#ffa657', meta: '#8b949e' },
        },
        monokai: { fixed: { comment: '#75715e', keyword: '#f92672', string: '#e6db74', number: '#ae81ff', title: '#a6e22e', attr: '#66d9ef', built: '#66d9ef', meta: '#75715e' } },
        dracula: { fixed: { comment: '#6272a4', keyword: '#ff79c6', string: '#f1fa8c', number: '#bd93f9', title: '#50fa7b', attr: '#8be9fd', built: '#8be9fd', meta: '#6272a4' } },
        'atom-one-dark': { fixed: { comment: '#5c6370', keyword: '#c678dd', string: '#98c379', number: '#d19a66', title: '#61afef', attr: '#e06c75', built: '#e5c07b', meta: '#5c6370' } },
      }
      const p = palettes[codeTheme] || palettes.github
      const pal = p.fixed || (dark ? p.dark! : p.light!) || p.light!
      Object.assign(HL, pal)
      // 版式基础色（对齐应用亮/暗令牌的具体值）
      const C = dark
        ? { text: '#e5e7eb', text2: '#a3aab5', border: '#2a2e35', border2: '#23262c', bg: '#141619', panel: '#1d2025', hover: '#262a30', primary: '#1677ff' }
        : { text: '#1f2329', text2: '#4e5563', border: '#e3e6eb', border2: '#eef0f4', bg: '#f5f6f8', panel: '#ffffff', hover: '#f2f3f6', primary: '#1677ff' }
      const checkSvg = dark
        ? "%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='64 64 896 896' fill='%23141619'%3E%3Cpath d='M912 190h-69.9c-9.8 0-19.1 4.5-25.3 12.2L472.4 614.7a31.8 31.8 0 0 1-50.6.2L219.2 341.8a32 32 0 0 0-25.3-12.2H112c-14.5 0-22.1 17.6-12.6 28.7l295.9 335.4a64.2 64.2 0 0 0 99.1.3l411.9-467.9C934.1 207.6 926.5 190 912 190z'/%3E%3C/svg%3E"
        : "%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='64 64 896 896' fill='%23fff'%3E%3Cpath d='M912 190h-69.9c-9.8 0-19.1 4.5-25.3 12.2L472.4 614.7a31.8 31.8 0 0 1-50.6.2L219.2 341.8a32 32 0 0 0-25.3-12.2H112c-14.5 0-22.1 17.6-12.6 28.7l295.9 335.4a64.2 64.2 0 0 0 99.1.3l411.9-467.9C934.1 207.6 926.5 190 912 190z'/%3E%3C/svg%3E"
      // 导出文档的内容栏宽跟随「预览内容最大宽度」设置；选「铺满」（0）时不限宽（margin:0 auto 对满宽无影响）
      const exportMaxW = Number(this.store.settings.editorMaxWidth) || 0
      const mdMaxWidth = exportMaxW > 0 ? `${exportMaxW}px` : 'none'
      const css = `
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; }
body { font-family: -apple-system, BlinkMacSystemFont, "PingFang SC", "Segoe UI", "Helvetica Neue", "Microsoft YaHei", Arial, sans-serif; font-size: 15px; line-height: 1.7; color: ${C.text}; background: ${C.panel}; -webkit-font-smoothing: antialiased; }
.md { max-width: ${mdMaxWidth}; margin: 0 auto; padding: 32px 26px 60px; }
.md h1, .md h2, .md h3, .md h4, .md h5, .md h6 { margin: 1.2em 0 0.6em; font-weight: 600; }
.md h1 { font-size: 1.8em; border-bottom: 1px solid ${C.border}; padding-bottom: 0.3em; }
.md h2 { font-size: 1.5em; border-bottom: 1px solid ${C.border}; padding-bottom: 0.3em; }
.md h3 { font-size: 1.25em; }
.md p { margin: 0.8em 0; }
.md a { color: ${C.primary}; text-decoration: none; }
.md a:hover { text-decoration: underline; }
.md code { background: ${C.hover}; padding: 2px 6px; border-radius: 4px; font-size: 0.9em; font-family: "SF Mono", "JetBrains Mono", Menlo, Consolas, "Courier New", monospace; }
.md pre { background: ${C.bg}; border: 1px solid ${C.border}; border-radius: 6px; padding: 12px 16px; overflow-x: auto; margin: 1em 0; }
.md pre code { background: transparent; padding: 0; font-size: 0.85em; tab-size: ${indent}; }
.md blockquote { border-left: 4px solid ${C.primary}; padding: 0.5em 1em; margin: 1em 0; background: ${C.hover}; color: ${C.text2}; }
.md ul, .md ol { padding-left: 2em; margin: 0.8em 0; }
.md li { margin: 0.3em 0; }
.md li:has(> input[type="checkbox"]) { list-style: none; margin-left: -1.5em; }
.md input[type="checkbox"] { appearance: none; -webkit-appearance: none; width: 16px; height: 16px; margin: 0 8px 0 0; vertical-align: -3px; border: 1px solid ${C.border}; border-radius: 4px; background: ${C.panel}; }
.md input[type="checkbox"]:checked { border-color: ${C.primary}; background: url("data:image/svg+xml,${checkSvg}") center / 11px no-repeat ${C.primary}; }
.md li:has(> input[type="checkbox"]:checked) > p { color: ${C.text2}; text-decoration: line-through; }
.md table { border-collapse: collapse; width: 100%; margin: 1em 0; }
.md th, .md td { border: 1px solid ${C.border}; padding: 8px 12px; text-align: left; }
.md th { background: ${C.bg}; font-weight: 600; }
.md tr:nth-child(even) { background: ${C.hover}; }
.md img { max-width: 100%; border-radius: 6px; margin: 1em 0; }
.md hr { border: none; border-top: 1px solid ${C.border}; margin: 2em 0; }
.md .mb > :first-child { margin-top: 0; }
.hljs-comment, .hljs-quote { color: ${HL.comment}; font-style: italic; }
.hljs-keyword, .hljs-selector-tag, .hljs-tag, .hljs-template-tag { color: ${HL.keyword}; }
.hljs-string, .hljs-regexp, .hljs-addition { color: ${HL.string}; }
.hljs-number, .hljs-literal, .hljs-variable, .hljs-template-variable, .hljs-symbol { color: ${HL.number}; }
.hljs-title, .hljs-title.function_, .hljs-title.class_ { color: ${HL.title}; }
.hljs-attr, .hljs-attribute, .hljs-name, .hljs-selector-attr, .hljs-selector-class { color: ${HL.attr}; }
.hljs-built_in, .hljs-type, .hljs-class { color: ${HL.built}; }
.hljs-meta, .hljs-section, .hljs-deletion { color: ${HL.meta}; }
.hljs-emphasis { font-style: italic; }
.hljs-strong { font-weight: 600; }`
      const safeTitle = (title || 'MarkLock 导出').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c] as string))
      return `<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${safeTitle}</title>
<style>${css}
</style>
</head>
<body>
<div class="md">
${bodyHtml}
</div>
</body>
</html>`
    },

    // ---------- 工具 ----------
    vaultName(vaultId: string) {
      const v = this.store.recent.find((x) => x.id === vaultId)
      return v ? v.name : ''
    },
    vaultColor(vaultId: string) {
      const v = this.store.recent.find((x) => x.id === vaultId)
      return v ? v.color : '#8a919e'
    },
    /** 文件图标取色：库内文件用库色；独立 .mdl（无库）用固定加密色；明文保持灰。 */
    fileIconColor(f: { vaultId: string; encrypted: boolean }) {
      const v = this.store.recent.find((x) => x.id === f.vaultId)
      if (v) return v.color
      return f.encrypted ? '#1677ff' : '#8a919e'
    },
    /** 判断一个打开页签是否是「库内加密文件」（vaultId 是库 id，且不是单文件 .mdl）。 */
    isVaultFile(f: { vaultId: string; encrypted: boolean }) {
      return !!(f.encrypted && f.vaultId && f.vaultId !== (f as any).path)
    },
    /** 判断一个库内文件所属库当前是否已锁定。 */
    isVaultFileLocked(f: { vaultId: string; encrypted: boolean }) {
      if (!this.isVaultFile(f)) return false
      const v = this.store.recent.find((x) => x.id === f.vaultId)
      return v ? !v.unlocked : false
    },
    /** 页签/条目的显示名：库内文件锁定后显示「xx 库文件」，不暴露具体文件名。 */
    fileDisplayName(f: { name: string; vaultId: string; encrypted: boolean }) {
      if (this.isVaultFileLocked(f)) {
        const vn = this.vaultName(f.vaultId) || '库'
        return `${vn} 库文件`
      }
      return f.name
    },
    /** 库显示名（用于「xx 库文件」）。 */
    vaultDisplayName(vaultId: string) {
      return this.vaultName(vaultId) || '库'
    },
    /**
     * 文件条目的 tooltip 文案：库内文件锁定后不暴露真实路径，仅提示「xx 库文件（已锁定）」。
     * f 需带 path 字段（OpenFile / recentFile 均有）。
     */
    fileTip(f: { path: string; name: string; vaultId: string; encrypted: boolean }) {
      if (this.isVaultFileLocked(f)) {
        const vn = this.vaultName(f.vaultId) || '库'
        return `${vn} 库文件（已锁定，解锁后可见）`
      }
      return this.externalTip((f as any).external) + ((f as any).path || f.name)
    },
    /** 外部修改状态的 tooltip 文案（无异常时为空串）。 */
    externalTip(ext?: string): string {
      if (ext === 'missing') return '⚠ 磁盘上已被删除或移动\n'
      if (ext === 'modified') return '⚠ 磁盘版本已变（页签有未保存内容），可右键重新加载\n'
      return ''
    },
    /** 页签 tooltip 文案：锁定文件同样不暴露真实路径，未锁定显示完整路径。 */
    tabTip(f: { path: string; name: string; vaultId: string; encrypted: boolean }) {
      return this.fileTip(f)
    },
    timeAgo(ts: number) {
      if (!ts) return ''
      const diff = Date.now() - ts
      const min = Math.floor(diff / 60000)
      if (min < 1) return '刚刚'
      if (min < 60) return `${min}分钟前`
      const hr = Math.floor(min / 60)
      if (hr < 24) return `${hr}小时前`
      return `${Math.floor(hr / 24)}天前`
    },
  },
})
</script>

<template>
  <div class="screen">
    <!-- 拖放高亮遮罩 -->
    <transition name="fade">
      <div v-if="dragOver" class="drop-mask">
        <div class="drop-card">
          <svg width="34" height="34" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"><path d="M4 7V6a2 2 0 0 1 2-2h2l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" /><path d="M12 11v6M9 14l3-3 3 3" /></svg>
          <div class="drop-t">松开以打开</div>
          <div class="drop-d">普通文件直接打开 · 目录挂载为工作目录 · 加密文件/库走解锁</div>
        </div>
      </div>
    </transition>

    <!-- ======== 标题栏（macOS 红绿灯叠加在左侧空白处） ======== -->
    <div class="titlebar" data-tauri-drag-region="deep" @contextmenu="openContextMenu($event, 'view')">
      <span class="icon-btn" data-tauri-drag-region="false" :title="store.settings.showSidebar ? '隐藏侧边栏' : '显示侧边栏'" @click="toggleSidebar">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M9.5 4v16" /></svg>
      </span>
      <div class="tb-title">
        <span class="doc">{{ activeFileName }}</span>
        <span class="dot" title="已保存"></span>
        <span class="vault">{{ vaultName(store.activeVaultId || activeFile?.vaultId || '') }}</span>
        <span v-if="activeFile && activeFile.encrypted" class="tag tag-gold">
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
          已加密
        </span>
        <span v-else-if="activeFile && activeFile.isNew" class="tag tag-gray">
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" /><path d="M14 3v6h6" /></svg>
          未保存
        </span>
        <span v-else-if="activeFile" class="tag tag-gray">
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" /><path d="M14 3v6h6" /></svg>
          明文
        </span>
      </div>
      <div class="tb-right" data-tauri-drag-region="false">
        <span v-if="activeFile?.dirty || currentMode !== 'preview'" class="icon-btn" title="保存 ⌘S" @click="saveActive">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z" /><path d="M17 21v-8H7v8M7 3v5h8" /></svg>
        </span>
        <span v-if="hasLockables" class="icon-btn" title="立即锁定 ⌘L" style="color: #d48806;" @click="lockAll">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
        </span>
        <div class="seg">
          <span :class="{ on: currentMode === 'edit' }" title="切换显示 ⌘E" @click="setMode('edit')">编辑</span>
          <span :class="{ on: currentMode === 'split' }" title="切换显示 ⌘E" @click="setMode('split')">分屏</span>
          <span :class="{ on: currentMode === 'preview' }" title="切换显示 ⌘E" @click="setMode('preview')">预览</span>
        </div>
        <span class="menu-wrap">
          <span class="icon-btn" @click="moreOpen = !moreOpen">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.6" /><circle cx="12" cy="12" r="1.6" /><circle cx="19" cy="12" r="1.6" /></svg>
          </span>
          <div class="menu" :class="{ open: moreOpen }">
            <div class="m-cap">新建</div>
            <div class="m-item" @click="openNewDraft(); moreOpen = false"><span class="ck"></span><span class="m-label">新建文件</span><span class="kbd-hint">⌘N</span></div>
            <div class="m-item" @click="openCreateVault(); moreOpen = false"><span class="ck"></span>新建加密库</div>
            <div class="m-item" v-if="activeFile && !isActiveInVault" @click="openCopyToVault(); moreOpen = false"><span class="ck"></span>复制到加密库</div>
            <div class="m-sep"></div>
            <div class="m-cap">打开</div>
            <div class="m-item" @click="openFileAction(); moreOpen = false"><span class="ck"></span><span class="m-label">打开文件…</span><span class="kbd-hint">⌘O</span></div>
            <div class="m-item" @click="chooseWorkdir(); moreOpen = false"><span class="ck"></span>打开工作目录…</div>
            <div class="m-item" :class="{ disabled: !store.recentlyClosed.length }" @click="store.recentlyClosed.length && (reopenLastClosed(), moreOpen = false)"><span class="ck"></span><span class="m-label">撤销关闭页签</span><span class="kbd-hint">⌘⇧T</span></div>
            <div class="m-sep"></div>
            <div class="m-item has-sub">
              <span class="ck"></span>导入导出<span class="sub-arrow">◂</span>
              <div class="sub-menu">
                <div class="m-item" @click="openBatchEncrypt(); moreOpen = false"><span class="ck"></span>批量加密文件…</div>
                <div class="m-item" @click="openImportDir(); moreOpen = false"><span class="ck"></span>导入目录到库…</div>
                <div class="m-sep"></div>
                <div class="m-item" :class="{ disabled: !activeFile || activeFile.locked }" @click="exportHtml(); moreOpen = false"><span class="ck"></span>导出 HTML…</div>
              </div>
            </div>
            <div class="m-sep"></div>
            <div class="m-cap">显示设置</div>
            <div class="m-item" @click="store.updateSettings({ showTabs: !store.settings.showTabs })"><span class="ck">{{ store.settings.showTabs ? '✓' : '' }}</span>顶部页签</div>
            <div class="m-item" @click="store.updateSettings({ showToolbar: !store.settings.showToolbar })"><span class="ck">{{ store.settings.showToolbar ? '✓' : '' }}</span>格式工具栏</div>
            <div class="m-item" @click="store.updateSettings({ showStatusbar: !store.settings.showStatusbar })"><span class="ck">{{ store.settings.showStatusbar ? '✓' : '' }}</span>状态栏</div>
            <div class="m-item has-sub">
              <span class="ck">{{ store.settings.showSidebar ? '✓' : '' }}</span>侧边栏<span class="sub-arrow">◂</span>
              <div class="sub-menu">
                <div class="m-item" @click="store.updateSettings({ showSidebar: !store.settings.showSidebar })"><span class="ck">{{ store.settings.showSidebar ? '✓' : '' }}</span><span class="m-label">显示侧边栏</span><span class="kbd-hint">⌘J</span></div>
                <div class="m-sep"></div>
                <div class="m-item" @click="store.updateSettings({ showOpenFiles: !store.settings.showOpenFiles })"><span class="ck">{{ store.settings.showOpenFiles ? '✓' : '' }}</span>打开的文件</div>
                <div class="m-item" @click="store.updateSettings({ showUnlockedVaults: !store.settings.showUnlockedVaults })"><span class="ck">{{ store.settings.showUnlockedVaults ? '✓' : '' }}</span>加密库</div>
                <div class="m-item" @click="store.updateSettings({ showWorkdir: !store.settings.showWorkdir })"><span class="ck">{{ store.settings.showWorkdir ? '✓' : '' }}</span>工作目录（主目录）</div>
                <div class="m-item" @click="store.updateSettings({ showFavorites: !store.settings.showFavorites })"><span class="ck">{{ store.settings.showFavorites ? '✓' : '' }}</span>收藏</div>
                <div class="m-item" @click="store.updateSettings({ showRecent: !store.settings.showRecent })"><span class="ck">{{ store.settings.showRecent ? '✓' : '' }}</span>最近打开</div>
                <div class="m-item" @click="store.updateSettings({ showOutline: !store.settings.showOutline })"><span class="ck">{{ store.settings.showOutline ? '✓' : '' }}</span>大纲</div>
                <div class="m-sep"></div>
                <div class="m-item" @click="openVaultAction(); moreOpen = false"><span class="ck"></span>打开库</div>
                <div class="m-item" v-if="registeredVaults.length" @click="closeAllVaults(); moreOpen = false"><span class="ck"></span>关闭所有库</div>
                <div class="m-item" v-if="workdirRoot" @click="closeWorkdir(); moreOpen = false"><span class="ck"></span>关闭工作目录</div>
                <div class="m-item" v-else @click="chooseWorkdir(); moreOpen = false"><span class="ck"></span>打开工作目录</div>
              </div>
            </div>
            <div class="m-sep"></div>
            <div class="m-item" @click="$router.push('/settings'); moreOpen = false"><span class="ck"></span>偏好设置…</div>
          </div>
        </span>
      </div>
    </div>

    <!-- ======== 主体：侧边栏贯穿页签栏与状态栏之间的全高，页签/状态栏仅占右列 ======== -->
    <div class="main">
      <!-- 侧边栏 -->
      <aside class="sidebar" v-show="store.settings.showSidebar" :style="{ width: sidebarWidth + 'px', flex: '0 0 ' + sidebarWidth + 'px' }">
        <div class="sb-head">
          <div class="sb-search">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#b6bcc7" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><path d="M21 21l-4.3-4.3" /></svg>
            <input class="input" placeholder="搜索文件名与内容…" v-model="searchQuery" @input="onSearchInput" @focus="searchQuery && (searchOpen = true)" @keydown="onSearchKeydown" autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false" />
            <!-- 搜索结果下拉 -->
            <div v-if="searchOpen" class="search-pop" ref="searchPop">
              <div v-if="searching" class="search-empty">搜索中…</div>
              <div v-else-if="!searchResults.length" class="search-empty">未找到匹配的文件名或内容</div>
              <template v-else>
                <div class="search-hit" :class="{ active: i === searchIndex }" v-for="(h, i) in searchResults" :key="i" @click="openSearchHit(h)" @mouseenter="searchIndex = i">
                  <span class="sh-dot" :style="{ background: vaultColor(h.vault_id) }"></span>
                  <div class="sh-body">
                    <div class="sh-title"><span class="sh-name">{{ hitName(h) }}</span><span v-if="h.name_hit" class="sh-name-tag">文件名</span><span class="sh-vault">{{ h.vault_name }}</span></div>
                    <div class="sh-snippet" :class="{ 'path-hit': h.name_hit }" v-html="highlight(h.snippet)"></div>
                  </div>
                </div>
              </template>
            </div>
          </div>
        </div>

        <div class="sb-body" ref="sbBody" @contextmenu="openContextMenu($event, 'sidebar')">
          <template v-for="secId in visibleSections" :key="secId">
          <!-- 打开的文件 -->
          <div v-if="secId === 'open'" class="sec" :dragable="'sec:open'" :class="secClass('open')">
            <div class="sec-head" @mousedown="onSectionMouseDown('sec:open', $event)">
              <span class="chev" :style="isSecClosed('open') ? 'transform:rotate(-90deg)' : ''"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 9l6 6 6-6" /></svg></span>
              打开的文件 <span class="cnt">{{ openFiles.length }}</span>
              <span class="acts">
                <span class="icon-btn" title="全部关闭" @click.stop="closeAllFiles()"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6L6 18M6 6l12 12" /></svg></span>
              </span>
            </div>
            <div class="sec-list" v-show="!isSecClosed('open')">
              <div
                v-for="f in openFiles"
                :key="f.path"
                class="t-item"
                :dragable="'open:' + f.path"
                :class="{ on: activePath === f.path, dragging: dragKey === 'open:' + f.path, dragover: dragOverKey === 'open:' + f.path }"
                v-tip="fileTip(f)"
                @mousedown="onItemMouseDown('open:' + f.path, $event)"
                @click="activateFile(f.path)"
                @contextmenu.stop="openContextMenu($event, 'tab', f.path)"
              >
                <EncIcon v-if="f.encrypted" class="lead" type="file" :color="fileIconColor(f)" :size="14" />
                <EncIcon v-else class="lead" type="plain" :color="fileIconColor(f)" :size="14" />
                <span class="fname">{{ fileDisplayName(f) }}</span>
                <span v-if="f.external" class="ext" :class="f.external" :title="f.external === 'missing' ? '磁盘上已被删除或移动' : '磁盘版本已变，页签有未保存内容'"></span>
                <span v-if="f.dirty" class="dirty" title="未保存"></span>
                <span class="star" :class="{ on: store.isFavoriteFile(f.path) }" title="收藏" @click.stop="store.toggleFavoriteFile(f.path)"><svg width="13" height="13" viewBox="0 0 24 24" :fill="store.isFavoriteFile(f.path) ? 'currentColor' : 'none'" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"><path d="M12 2l2.9 6.3 6.9.7-5.1 4.6 1.4 6.8-6.1-3.5-6.1 3.5 1.4-6.8L2.2 9l6.9-.7z" /></svg></span>
                <span class="x" @click.stop="closeFile(f.path)">✕</span>
              </div>
            </div>
          </div>

          <!-- 加密库（锁定后仍保留，便于二次点击解锁） -->
          <div v-else-if="secId === 'vaults'" class="sec" :dragable="'sec:vaults'" :class="secClass('vaults')">
            <div class="sec-head" @mousedown="onSectionMouseDown('sec:vaults', $event)">
              <span class="chev" :style="isSecClosed('vaults') ? 'transform:rotate(-90deg)' : ''"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 9l6 6 6-6" /></svg></span>
              加密库 <span class="cnt">{{ vaultCount }}</span>
              <span class="acts">
                <span class="icon-btn" title="更多操作" @click.stop="openVaultsMoreMenu($event)"><svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                <span class="icon-btn" title="锁定所有库" @click.stop="lockAll()"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg></span>
              </span>
            </div>
            <div class="sec-list" v-show="!isSecClosed('vaults')">
              <div v-for="v in registeredVaults" :key="v.id">
                <div
                  class="t-item vault-row"
                  :dragable="'vault:' + v.id"
                  :class="{ dragging: dragKey === 'vault:' + v.id, dragover: dragOverKey === 'vault:' + v.id }"
                  v-tip="v.path"
                  @mousedown="onItemMouseDown('vault:' + v.id, $event)"
                  @click="onVaultRowClick(v)"
                  @contextmenu.stop="openContextMenu($event, 'vault', v.id)"
                >
                  <svg v-if="v.unlocked" class="lead chev-i" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#8a919e" stroke-width="2" stroke-linecap="round" :style="isTreeClosed('v-' + v.id) ? 'transform:rotate(-90deg)' : ''"><path d="M6 9l6 6 6-6" /></svg>
                  <span v-else class="lead" style="width:14px;flex:0 0 auto"></span>
                  <EncIcon class="lead" type="vault" :color="v.color" :size="15" />
                  <span class="fname">{{ v.name }}</span>
                  <span class="acts">
                    <span class="icon-btn" title="更多操作" @click.stop="openContextMenu($event, 'vault', v.id)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                  </span>
                  <span v-if="v.unlocked" class="icon-btn" title="锁定该库" @click.stop="lockVault(v.id)"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg></span>
                  <span v-else class="icon-btn locked" title="点击解锁并打开" @click.stop="onVaultRowClick(v)"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg></span>
                </div>
                <div class="t-sub" v-show="v.unlocked && !isTreeClosed('v-' + v.id)">
                  <VaultTreeNode
                    v-if="vaultTrees[v.id] && vaultTrees[v.id].length"
                    :nodes="vaultTrees[v.id]"
                    :color="v.color"
                    :active-path="activePath"
                    :closed="treeClosed"
                    :base="17"
                    @open-file="(n) => openFileNode(n, v.id)"
                    @new-file="(n) => openNewFile(v, n)"
                    @new-folder="(n) => openNewFolder(v, n)"
                    @rename-node="(n) => openRename(v.id, n)"
                    @delete-node="(n) => openDelete(v.id, n)"
                    @ctx-node="(p: any) => openVaultNodeMenu(p.e, v, p.node)"
                  />
                  <div v-else class="empty-hint" @click.stop="openContextMenu($event, 'vault', v.id)">库内暂无文件，可在「更多」菜单中新建</div>
                </div>
              </div>
            </div>
          </div>

          <!-- 收藏（文件 + 库） -->
          <div v-else-if="secId === 'favorites'" class="sec" :dragable="'sec:favorites'" :class="secClass('favorites')">
            <div class="sec-head" @mousedown="onSectionMouseDown('sec:favorites', $event)">
              <span class="chev" :style="isSecClosed('favorites') ? 'transform:rotate(-90deg)' : ''"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 9l6 6 6-6" /></svg></span>
              收藏 <span class="cnt">{{ favoriteVaults.length + favoriteFileList.length }}</span>
            </div>
            <div class="sec-list" v-show="!isSecClosed('favorites')">
              <!-- 收藏的文件 -->
              <div
                v-for="f in favoriteFileList"
                :key="f.path"
                class="t-item"
                :dragable="'favfile:' + f.path"
                :class="{ dragging: dragKey === 'favfile:' + f.path, dragover: dragOverKey === 'favfile:' + f.path }"
                v-tip="fileTip(f)"
                @mousedown="onItemMouseDown('favfile:' + f.path, $event)"
                @click="recentOpenFile(f)"
                @contextmenu.stop="openContextMenu($event, 'favfile', f.path)"
              >
                <EncIcon class="lead" :type="f.encrypted ? 'file' : 'plain'" :color="fileIconColor(f)" :size="14" />
                <span class="fname">{{ fileDisplayName(f) }}</span>
                <span class="star on" title="取消收藏" @click.stop="store.toggleFavoriteFile(f.path)"><svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor"><path d="M12 2l2.9 6.3 6.9.7-5.1 4.6 1.4 6.8-6.1-3.5-6.1 3.5 1.4-6.8L2.2 9l6.9-.7z" /></svg></span>
              </div>

              <!-- 收藏的库 -->
              <div
                v-for="v in favoriteVaults.filter((x) => x.isDir || x.isFileVault)"
                :key="v.id"
                class="t-item"
                :dragable="'favvault:' + v.id"
                :class="{ dragging: dragKey === 'favvault:' + v.id, dragover: dragOverKey === 'favvault:' + v.id }"
                v-tip="v.path"
                @mousedown="onItemMouseDown('favvault:' + v.id, $event)"
                @click="recentOpenVault(v.id)"
                @contextmenu.stop="openContextMenu($event, 'vault', v.id)"
              >
                <EncIcon class="lead" type="vault" :color="v.color" :size="14" />
                <span class="fname">{{ v.name }}</span>
                <span class="st-ico" :class="v.unlocked ? 'unlocked' : 'locked'" v-tip="v.unlocked ? '已解锁' : '已锁定'">
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
                </span>
                <span class="star on" title="取消收藏" @click.stop="store.toggleFavorite(v.id)"><svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor"><path d="M12 2l2.9 6.3 6.9.7-5.1 4.6 1.4 6.8-6.1-3.5-6.1 3.5 1.4-6.8L2.2 9l6.9-.7z" /></svg></span>
              </div>
            </div>
          </div>

          <!-- 最近打开（文件 + 库） -->
          <div v-else-if="secId === 'recent'" class="sec" :dragable="'sec:recent'" :class="secClass('recent')">
            <div class="sec-head" @mousedown="onSectionMouseDown('sec:recent', $event)">
              <span class="chev" :style="isSecClosed('recent') ? 'transform:rotate(-90deg)' : ''"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 9l6 6 6-6" /></svg></span>
              最近打开
              <span class="acts">
                <span class="icon-btn" title="清除最近打开" @click.stop="clearRecent()"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M3 6h18M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6M10 11v6M14 11v6" /></svg></span>
              </span>
            </div>
            <div class="sec-list" v-show="!isSecClosed('recent')">
              <!-- 最近打开的文件（记录 50 条，仅显示前 10 条） -->
              <div v-for="f in store.recentFiles.slice(0, 10)" :key="'f-' + f.path" class="t-item" v-tip="fileTip(f) + '\n' + timeAgo(f.lastOpened)" @click="recentOpenFile(f)">
                <EncIcon class="lead" :type="f.encrypted ? 'file' : 'plain'" :color="fileIconColor(f)" :size="14" />
                <span class="fname">{{ fileDisplayName(f) }}</span>
                <span class="star" :class="{ on: store.isFavoriteFile(f.path) }" title="收藏" @click.stop="store.toggleFavoriteFile(f.path)"><svg width="13" height="13" viewBox="0 0 24 24" :fill="store.isFavoriteFile(f.path) ? 'currentColor' : 'none'" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"><path d="M12 2l2.9 6.3 6.9.7-5.1 4.6 1.4 6.8-6.1-3.5-6.1 3.5 1.4-6.8L2.2 9l6.9-.7z" /></svg></span>
              </div>

              <!-- 最近打开的库（仅显示前 10 条；登记的库在「加密库」分组完整可见） -->
              <div v-for="v in store.recent.filter((x) => x.isDir || x.isFileVault).slice(0, 10)" :key="'v-' + v.id" class="t-item" v-tip="v.path + '\n' + timeAgo(v.lastOpened)" @click="recentOpenVault(v.id)" @contextmenu.stop="openContextMenu($event, 'vault', v.id)">
                <EncIcon class="lead" type="vault" :color="v.color" :size="14" />
                <span class="fname">{{ v.name }}</span>
                <span class="st-ico" :class="v.unlocked ? 'unlocked' : 'locked'" v-tip="v.unlocked ? '已解锁' : '已锁定'">
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
                </span>
                <span class="star" :class="{ on: store.isFavorite(v.id) }" title="收藏" @click.stop="store.toggleFavorite(v.id)"><svg width="13" height="13" viewBox="0 0 24 24" :fill="store.isFavorite(v.id) ? 'currentColor' : 'none'" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"><path d="M12 2l2.9 6.3 6.9.7-5.1 4.6 1.4 6.8-6.1-3.5-6.1 3.5 1.4-6.8L2.2 9l6.9-.7z" /></svg></span>
              </div>
            </div>
          </div>

          <!-- 大纲（当前文件的 Markdown 标题结构） -->
          <div v-else-if="secId === 'outline'" class="sec" :dragable="'sec:outline'" :class="secClass('outline')">
            <div class="sec-head" @mousedown="onSectionMouseDown('sec:outline', $event)">
              <span class="chev" :style="isSecClosed('outline') ? 'transform:rotate(-90deg)' : ''"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 9l6 6 6-6" /></svg></span>
              大纲 <span class="cnt">{{ outline.length }}</span>
            </div>
            <div class="sec-list" v-show="!isSecClosed('outline')">
              <div v-if="!outline.length" class="empty-hint">当前文件暂无标题</div>
              <div
                v-else
                v-for="(h, i) in outline"
                :key="h.line + '-' + i"
                class="ol-item"
                :class="{ on: h.line === outlineActiveLine }"
                :style="{ paddingLeft: 12 + (h.level - 1) * 13 + 'px' }"
                v-tip="h.text + '\n第 ' + h.line + ' 行'"
                @click="outlineGoto(h)"
              >
                <span class="ol-h">H{{ h.level }}</span>
                <span class="ol-text">{{ h.text }}</span>
              </div>
            </div>
          </div>

          <!-- 工作目录 -->
          <div v-else-if="secId === 'workdir'" class="sec" :dragable="'sec:workdir'" :class="secClass('workdir')">
            <div class="sec-head" @mousedown="onSectionMouseDown('sec:workdir', $event)">
              <span class="chev" :style="isSecClosed('workdir') ? 'transform:rotate(-90deg)' : ''"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 9l6 6 6-6" /></svg></span>
              <span class="wd-title" v-tip="workdirRoot">{{ workdirTitle }}</span>
              <span class="acts">
                <template v-if="workdirRoot">
                  <span class="icon-btn" title="更多操作" @click.stop="openWorkdirMoreMenu($event)"><svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                  <span class="icon-btn" title="折叠全部文件夹" @click.stop="collapseWorkdirFolders()"><svg width="16" height="16" viewBox="0 0 1024 1024" fill="currentColor" xmlns="http://www.w3.org/2000/svg"><path d="M85.184 149.312h469.376v85.376H85.184V149.312z m755.584 191.68a42.688 42.688 0 0 1-60.416 0l-128-128 60.416-60.352 97.792 97.792 97.792-97.792 60.416 60.352-128 128z m-755.52 21.632h469.312V448H85.248V362.624z m-0.064 213.312h469.376v85.376H85.184V575.936z m567.168 216.704l128-128 6.72-5.44a42.752 42.752 0 0 1 53.696 5.44l128 128-60.416 60.352-97.792-97.792-97.792 97.792-60.416-60.352z m-567.104-3.328h469.312v85.376H85.248v-85.376z" /></svg></span>
                </template>
              </span>
            </div>
            <div class="sec-list" v-show="!isSecClosed('workdir')">
              <div v-if="!workdirRoot" class="empty-hint" @click="chooseWorkdir()">
                点击选择要挂载的系统目录
              </div>
              <div v-else-if="!workdir.length" class="empty-hint" @click="wdNewFileAt(workdirRoot)">
                目录为空，点击新建文件<br>（或在「更多」菜单里新建）
              </div>
              <template v-else v-for="node in workdir" :key="node.path">
                <template v-if="node.is_dir && !node.is_vault">
                  <div class="t-item" v-tip="node.path" @click="toggleWorkdirFolder(node)" @contextmenu.stop="openWorkdirNodeMenu($event, node)">
                    <svg class="lead chev-i" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#8a919e" stroke-width="2" stroke-linecap="round" :style="!node.open ? 'transform:rotate(-90deg)' : ''"><path d="M6 9l6 6 6-6" /></svg>
                    <svg class="lead" style="width:16px;height:16px" viewBox="0 0 24 24" fill="none" stroke="#faad14" stroke-width="1.8" stroke-linejoin="round"><path d="M4 7V6a2 2 0 0 1 2-2h2l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" /></svg>
                    <span class="fname">{{ node.name }}</span>
                    <span class="acts">
                      <span class="icon-btn" title="更多操作" @click.stop="openWorkdirNodeMenu($event, node)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                    </span>
                  </div>
                  <div class="t-sub" v-show="node.open">
                    <template v-for="c in node.children" :key="c.path">
                      <div v-if="c.is_vault" class="t-item" :class="{ on: activePath === c.path }" :data-wpath="c.path" v-tip="c.path" @click="workdirOpenVault(c)" @contextmenu.stop="openWorkdirNodeMenu($event, c)">
                        <span style="width:13px;flex:0 0 13px;"></span>
                        <EncIcon v-if="c.is_dir" class="lead" type="vault" color="#1677ff" :size="14" />
                        <EncIcon v-else-if="c.name.endsWith('.mdlb')" class="lead" type="vault" color="#1677ff" :size="14" />
                        <EncIcon v-else class="lead" type="file" color="#1677ff" :size="14" />
                        <span class="fname">{{ c.name }}</span>
                        <span class="acts">
                          <span class="icon-btn" title="更多操作" @click.stop="openWorkdirNodeMenu($event, c)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                        </span>
                      </div>
                      <div v-else-if="c.is_dir" class="t-item" v-tip="c.path" @click="toggleWorkdirFolder(c)" @contextmenu.stop="openWorkdirNodeMenu($event, c)">
                        <span style="width:13px;flex:0 0 13px;"></span>
                        <svg class="lead" style="width:16px;height:16px" viewBox="0 0 24 24" fill="none" stroke="#faad14" stroke-width="1.8" stroke-linejoin="round"><path d="M4 7V6a2 2 0 0 1 2-2h2l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" /></svg>
                        <span class="fname">{{ c.name }}</span>
                        <span class="acts">
                          <span class="icon-btn" title="更多操作" @click.stop="openWorkdirNodeMenu($event, c)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                        </span>
                      </div>
                      <div v-else class="t-item" :class="{ on: activePath === c.path }" :data-wpath="c.path" v-tip="c.path" @click="workdirOpenPlain(c)" @contextmenu.stop="openWorkdirNodeMenu($event, c)">
                        <span style="width:13px;flex:0 0 13px;"></span>
                        <EncIcon class="lead" type="plain" color="#8a919e" :size="14" />
                        <span class="fname">{{ c.name }}</span>
                        <span class="acts">
                          <span class="icon-btn" title="更多操作" @click.stop="openWorkdirNodeMenu($event, c)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                        </span>
                      </div>
                    </template>
                  </div>
                </template>
                <div v-else-if="node.is_vault" class="t-item" :class="{ on: activePath === node.path }" :data-wpath="node.path" v-tip="node.path" @click="workdirOpenVault(node)" @contextmenu.stop="openWorkdirNodeMenu($event, node)">
                  <span style="width:14px;flex:0 0 14px;"></span>
                  <EncIcon v-if="node.is_dir" class="lead" type="vault" color="#1677ff" :size="14" />
                  <EncIcon v-else-if="node.name.endsWith('.mdlb')" class="lead" type="vault" color="#1677ff" :size="14" />
                  <EncIcon v-else class="lead" type="file" color="#1677ff" :size="14" />
                  <span class="fname">{{ node.name }}</span>
                  <span class="acts">
                    <span class="icon-btn" title="更多操作" @click.stop="openWorkdirNodeMenu($event, node)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                  </span>
                </div>
                <div v-else class="t-item" :class="{ on: activePath === node.path }" :data-wpath="node.path" v-tip="node.path" @click="workdirOpenPlain(node)" @contextmenu.stop="openWorkdirNodeMenu($event, node)">
                  <span style="width:14px;flex:0 0 14px;"></span>
                  <EncIcon class="lead" type="plain" color="#8a919e" :size="14" />
                  <span class="fname">{{ node.name }}</span>
                  <span class="acts">
                    <span class="icon-btn" title="更多操作" @click.stop="openWorkdirNodeMenu($event, node)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
                  </span>
                </div>
              </template>
            </div>
          </div>
          </template>
          <!-- 空态引导：未打开文件/库/工作目录时，侧边栏不再空荡荡，给出两个入口 -->
          <div v-if="sidebarEmpty" class="sb-empty">
            <svg width="36" height="36" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7V6a2 2 0 0 1 2-2h2l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" /><path d="M12 11.5v5M9.5 14h5" /></svg>
            <div class="sb-empty-title">侧边栏暂无内容</div>
            <div class="sb-empty-desc">打开一个加密库，或选择一个工作目录开始</div>
            <div class="sb-empty-btn" @click="openVaultAction()">打开库</div>
            <div class="sb-empty-btn" @click="chooseWorkdir()">选择工作目录</div>
          </div>
        </div>
      </aside>
      <!-- 侧边栏拖拽手柄 -->
      <div class="sidebar-resize" v-show="store.settings.showSidebar" @mousedown="onSidebarResizeStart"></div>

      <div class="right-col">
      <!-- 页签栏（仅侧边栏右侧） -->
      <div class="tabsbar" v-show="store.settings.showTabs" @contextmenu="openContextMenu($event, 'tabsbar')" @dblclick="onToolbarBlankDblclick">
        <div
          v-for="f in openFiles"
          :key="f.path"
          class="tab"
          :dragable="'open:' + f.path"
          :class="{ on: activePath === f.path, dragging: dragKey === 'open:' + f.path, dragover: dragOverKey === 'open:' + f.path }"
          @mousedown="onItemMouseDown('open:' + f.path, $event)"
          @click="activateFile(f.path)"
          @contextmenu.stop="activateFile(f.path); openContextMenu($event, 'tab', f.path)"
        >
          <EncIcon v-if="f.encrypted" type="file" :color="fileIconColor(f)" :size="13" />
          <EncIcon v-else type="plain" :color="fileIconColor(f)" :size="13" />
          <span class="tab-name" v-tip="{ text: tabTip(f), place: 'bottom' }">{{ fileDisplayName(f) }}</span>
          <span class="vt">{{ vaultName(f.vaultId) }}</span>
          <span v-if="f.external" class="ext" :class="f.external" :title="f.external === 'missing' ? '磁盘上已被删除或移动' : '磁盘版本已变，页签有未保存内容'"></span>
          <span v-if="f.dirty" class="dirty" title="未保存"></span>
          <span class="x" @click.stop="closeFile(f.path)">✕</span>
        </div>
        <div class="tab-add" title="新建文件" @click="openNewDraft">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14" /></svg>
        </div>
      </div>

      <!-- 工作台 -->
      <section class="workbench">
        <!-- 格式化工具栏 -->
        <div v-show="store.settings.showToolbar && currentMode !== 'preview' && activeFile" class="toolbar">
          <span class="icon-btn" v-tip="{ text: '标题 1', place: 'bottom' }" @click="fmtHeading(1)"><b style="font-size:13px;">H1</b></span>
          <span class="icon-btn" v-tip="{ text: '标题 2', place: 'bottom' }" @click="fmtHeading(2)"><b style="font-size:13px;">H2</b></span>
          <span class="icon-btn" v-tip="{ text: '标题 3', place: 'bottom' }" @click="fmtHeading(3)"><b style="font-size:13px;">H3</b></span>
          <span class="tb-sep"></span>
          <span class="icon-btn" v-tip="{ text: '加粗 ⌘B', place: 'bottom' }" @click="fmtWrap('**', '**', '加粗')"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M7 4h6a3.5 3.5 0 0 1 0 7H7zM7 11h7a3.5 3.5 0 0 1 0 7H7z" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '斜体 ⌘I', place: 'bottom' }" @click="fmtWrap('*', '*', '斜体')"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M19 4h-9M14 20H5M15 4L9 20" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '删除线', place: 'bottom' }" @click="fmtWrap('~~', '~~', '删除')"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 12h16" /><path d="M17.3 7.5C16.9 5.6 14.8 4 12 4c-3 0-5 1.6-5 3.5 0 .5.1 1 .4 1.5M6.7 16.5C7.1 18.4 9.2 20 12 20c3 0 5-1.6 5-3.5 0-.5-.1-1-.4-1.5" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '行内代码', place: 'bottom' }" @click="fmtWrap('`', '`', 'code')"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M8 6l-6 6 6 6M16 6l6 6-6 6" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '代码块', place: 'bottom' }" @click="fmtCodeBlock"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M10 9.5 7.5 12l2.5 2.5M14 9.5l2.5 2.5-2.5 2.5" /></svg></span>
          <span class="tb-sep"></span>
          <span class="icon-btn" v-tip="{ text: '引用', place: 'bottom' }" @click="fmtPrefix('> ')"><svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor"><path d="M10 7H6a3 3 0 0 0-3 3v4a3 3 0 0 0 3 3h2a3 3 0 0 0 3-3v-7zm11 0h-4a3 3 0 0 0-3 3v4a3 3 0 0 0 3 3h2a3 3 0 0 0 3-3v-7z" opacity="0.9" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '无序列表', place: 'bottom' }" @click="fmtPrefix('- ')"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M9 6h12M9 12h12M9 18h12" /><circle cx="4.5" cy="6" r="1.3" fill="currentColor" stroke="none" /><circle cx="4.5" cy="12" r="1.3" fill="currentColor" stroke="none" /><circle cx="4.5" cy="18" r="1.3" fill="currentColor" stroke="none" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '任务列表', place: 'bottom' }" @click="fmtPrefix('- [ ] ')"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="7" height="7" rx="1.5" /><path d="M13.5 7.5h7M13.5 16.5h7M5 15.5l1.8 1.8 3.2-3.6" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '表格', place: 'bottom' }" @click="fmtTable"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="5" width="18" height="14" rx="2" /><path d="M3 11h18M10 11v8" /></svg></span>
          <span class="tb-sep"></span>
          <span class="icon-btn" v-tip="{ text: '链接 ⌘K', place: 'bottom' }" @click="fmtLink(false)"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7" /><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '图片', place: 'bottom' }" @click="fmtLink(true)"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="5" width="18" height="14" rx="2" /><circle cx="8.5" cy="10" r="1.5" /><path d="M21 16l-5-5-9 9" /></svg></span>
          <span class="icon-btn" v-tip="{ text: '分割线', place: 'bottom' }" @click="fmtHr"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 12h16" /></svg></span>
          <span class="tb-sep"></span>
          <span class="icon-btn" :class="{ on: wordWrap }" v-tip="{ text: wordWrap ? '关闭自动换行' : '开启自动换行', place: 'bottom' }" @click="wordWrap = !wordWrap">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M3 12h15a3 3 0 0 1 0 6h-9" /><path d="M12 18l-3-3 3-3" /></svg>
          </span>
          <span class="tb-spacer"></span>
          <span class="tag tag-gray" style="flex:0 0 auto;">Markdown</span>
        </div>

        <!-- 编辑 / 预览 双栏（mode-* 类驱动切换滑移动画；拖拽时分屏比例即时跟手，禁用过渡） -->
        <div class="panes" :class="[`mode-${currentMode}`, { 'no-anim': _splitDragging }]">
          <!-- 未打开文件时的空态引导 -->
          <div v-if="!activeFile" class="pane-locked">
            <div class="locked-inner">
              <div class="lk-ico">
                <svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="#8a919e" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" /><path d="M14 3v6h6" /></svg>
              </div>
              <div class="lk-t">未打开文件</div>
              <div class="lk-d">从侧边栏选择文件打开，或新建一个文件开始编辑。</div>
              <div class="lk-actions">
                <button class="btn" @click="openNewDraft">新建文件</button>
                <button class="btn btn-primary" @click="openFileAction">打开文件…</button>
                <!-- 未挂载工作目录时才出现：侧边栏此时没有文件树可点，给一个直接挂载本地目录的入口 -->
                <button v-if="!workdirRoot" class="btn" @click="chooseWorkdir">选择工作目录…</button>
              </div>
            </div>
          </div>

          <!-- 单文件 .mdl 锁定态：内容区显示密码框 -->
          <div v-else-if="isActiveFileLocked" class="pane-locked">
            <div class="locked-inner">
              <div class="lk-ico">
                <svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="#1677ff" stroke-width="1.6" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
              </div>
              <div class="lk-t">{{ activeFileName }}</div>
              <div class="lk-d">这是一个加密文件（.mdl），输入密码解锁后即可查看与编辑。</div>
              <div class="unlock-row">
                <input
                  ref="filePwdInput"
                  class="input input-lg"
                  type="password"
                  placeholder="输入该文件的密码"
                  v-model="unlockPwd"
                  :disabled="unlocking"
                  @keyup.enter="unlockActiveFile"
                />
                <button class="btn btn-primary btn-lg" style="flex:0 0 96px;" :disabled="unlocking" @click="unlockActiveFile">
                  {{ unlocking ? '解锁中…' : '解锁' }}
                </button>
              </div>
              <div class="lk-hint">密码仅用于本地解密，不会上传到任何服务器</div>
            </div>
          </div>

          <!-- 库锁定占位：就地输入主密码解锁 -->
          <div v-else-if="isActiveLocked" class="pane-locked">
            <div class="locked-inner">
              <div class="lk-ico">
                <svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="#d48806" stroke-width="1.6" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
              </div>
              <div class="lk-t">「{{ vaultDisplayName(activeFile?.vaultId || '') }}」已锁定</div>
              <div class="unlock-row">
                <input
                  ref="vaultPwdInput"
                  class="input input-lg"
                  type="password"
                  placeholder="输入库主密码"
                  v-model="vaultUnlockPwd"
                  :disabled="vaultUnlocking"
                  @keyup.enter="unlockActiveVault"
                />
                <button class="btn btn-primary btn-lg" style="flex:0 0 96px;" :disabled="vaultUnlocking" @click="unlockActiveVault">
                  {{ vaultUnlocking ? '解锁中…' : '解锁' }}
                </button>
              </div>
              <div class="lk-err" v-if="vaultUnlockError">{{ vaultUnlockError }}</div>
              <div class="lk-hint">解锁后该库所有页签的内容将原样恢复</div>
            </div>
          </div>

          <template v-else>
            <!-- 源码：面板常驻布局流（不用 v-show，WKWebView 下 display 切换会中断 flex-basis 过渡），收起靠 width 滑到 0 -->
            <div class="pane pane-editor" :class="{ collapsed: currentMode === 'preview' }" :style="currentMode === 'split' ? { flex: '0 0 ' + (splitRatio * 100) + '%' } : {}">
              <!-- 每个页签一个常驻实例：切换只靠 display 隐藏，实例不销毁才能保住各自撤销历史（⌘Z）；:key=path 确保实例与文件一一对应
                   预览模式下源码面板只是宽度收到 0、实例仍可编辑，故置 readOnly 兜底：
                   避免定位/大纲跳转等程序化操作把焦点带过去后，用户随手敲的键把内容改掉 -->
              <CodeMirrorEditor
                v-for="f in openFiles"
                :key="f.path"
                ref="cmEditor"
                class="cm-tab-instance"
                :class="{ 'cm-hidden': activePath !== f.path }"
                :model-value="getContentFor(f.path)"
                :on-save="saveActive"
                :word-wrap="wordWrap"
                :readonly="currentMode === 'preview'"
                :on-scroll="activePath === f.path ? onEditorScroll : undefined"
                @update:model-value="(v: string) => setContentFor(f.path, v)"
              />
            </div>
            <!-- 分屏拖拽手柄 -->
            <div v-show="currentMode === 'split'" class="split-resize" @mousedown="onSplitResizeStart" @dblclick="resetSplitRatio"></div>

            <!-- 预览 -->
            <div class="pane pane-preview" :class="{ collapsed: currentMode === 'edit' }" ref="previewPane" :style="currentMode === 'split' ? { flex: '0 0 ' + ((1 - splitRatio) * 100) + '%' } : {}" @scroll="onPreviewScroll" @click="onPreviewLinkClick" @dblclick="onPreviewDblClick">
              <div class="md" :class="`hljs-theme-${store.settings.codeTheme}`" :style="{ '--code-indent': store.settings.codeIndent }" v-html="renderedPreview"></div>
            </div>
          </template>
        </div>

      </section>

      <!-- 状态栏（仅侧边栏右侧，与页签栏同列） -->
      <div v-show="store.settings.showStatusbar" class="statusbar" @contextmenu="openContextMenu($event, 'view')">
        <span class="st">{{ activeFile ? activeFile.content.split('\n').length + ' 行' : '—' }}</span>
      <span class="st">{{ activeFile ? activeFile.content.length + ' 字' : '—' }}</span>
      <span class="st"><span class="dot" :style="activeFile && activeFile.dirty ? 'background:var(--gold)' : ''"></span> {{ activeFile && activeFile.dirty ? '未保存' : '已保存' }}</span>
      <span
        v-if="activeFile && activeFile.external"
        class="st st-ext"
        :class="activeFile.external"
        v-tip="externalTip(activeFile.external) || ''"
        @click="activeFile.external === 'modified' && reloadTab(activeFile.path)"
      >{{ activeFile.external === 'missing' ? '磁盘文件已丢失' : '外部已修改，点击重新加载' }}</span>
      <div class="right">
        <span v-if="activeFile && activeFile.encrypted" class="st"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#d48806" stroke-width="2.2" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>已加密</span>
        <span v-else-if="activeFile" class="st"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#8a919e" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" /><path d="M14 3v6h6" /></svg>明文</span>
        <span class="st st-menu" :title="`字体大小 ${store.settings.editorFontSize} px（点击修改，⌘+ / ⌘- 可实时调节）`" @click="openFontSizeMenu($event)"><svg width="12" height="12" viewBox="0 0 1024 1024" fill="currentColor"><path d="M448 234.688v640H362.688v-640h-256V149.312H704v85.376H448zM789.312 576v298.688H704V576H576V490.688h341.312V576h-128z" /></svg>{{ store.settings.editorFontSize }} px</span>
        <span class="st st-menu" title="点击修改自动锁定间隔" @click="openAutoLockMenu($event)"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></svg>自动锁定 {{ autoLockLabel }}</span>
        <span class="st">{{ unlockedCount }} 库</span>
      </div>
      </div>
      </div>
    </div>

    <!-- 另存为弹窗（保存草稿页签 / 普通 md 转加密文件） -->
    <a-modal v-model:open="showSaveAs" :title="saveAsConvert ? '转为加密文件' : '保存为新文件'" :width="440" :footer="null">
      <div class="saveas">
        <!-- 目标类型切换（转换态仅支持独立加密文件 .mdl，隐藏切换） -->
        <div class="seg saveas-seg" v-if="!saveAsConvert">
          <span :class="{ on: saveAsMode === 'file' }" @click="setSaveAsMode('file')">加密文件 .mdl</span>
          <span :class="{ on: saveAsMode === 'vault' }" @click="setSaveAsMode('vault')">存入加密库</span>
          <span :class="{ on: saveAsMode === 'plain' }" @click="setSaveAsMode('plain')">普通 md</span>
        </div>

        <div class="saveas-field">
          <label>文件名</label>
          <input class="input" v-model="saveAsName" placeholder="如 会议纪要" />
        </div>

        <template v-if="saveAsMode === 'file'">
          <div class="saveas-field">
            <label>保存位置</label>
            <div class="saveas-path">
              <input class="input" v-model="saveAsPath" :placeholder="'如 ~/Documents/' + saveAsName + '.mdl'" readonly />
              <button class="btn" @click="browseSavePath">浏览…</button>
            </div>
          </div>
          <div class="saveas-field">
            <label>主密码</label>
            <input class="input" type="password" v-model="saveAsPwd" placeholder="至少 1 位，用于加密该文件" />
            <div class="strength">
              <i v-for="n in 4" :key="n" :class="'lv' + (saveAsStrength >= n ? saveAsStrength : '')"></i>
              <em v-if="saveAsStrength >= 4">强 · 建议使用密码管理器生成</em>
              <em v-else-if="saveAsStrength >= 3">中 · 建议混合符号</em>
              <em v-else>弱 · 建议 12 位以上并混合符号</em>
            </div>
          </div>
          <div class="saveas-field">
            <label>确认密码</label>
            <input class="input" type="password" v-model="saveAsConfirm" placeholder="再次输入主密码" />
          </div>
          <div class="saveas-hint">保存为独立的加密文件，拥有独立密码，可单独移动/分享。</div>
        </template>

        <template v-else-if="saveAsMode === 'vault'">
          <div class="saveas-field">
            <label>目标库</label>
            <select class="input" v-model="saveAsVaultId" @change="onSaveAsVaultChange">
              <option v-for="v in unlockedVaults.filter((x) => x.isDir || x.isFileVault)" :key="v.id" :value="v.id">{{ v.name }}</option>
              <option value="__new__">＋ 新建库…</option>
            </select>
          </div>
          <!-- 内联新建库表单 -->
          <template v-if="saveAsVaultId === '__new__'">
            <div class="saveas-field">
              <label>保存位置</label>
              <div class="saveas-path">
                <input class="input" v-model="createVaultForm.path" placeholder="选择 .mdlb 文件的保存路径" readonly />
                <button class="btn" @click="browseCreateVaultPath">浏览…</button>
              </div>
            </div>
            <div class="saveas-field">
              <label>主密码</label>
              <input class="input" type="password" v-model="createVaultForm.pwd" placeholder="至少 1 位，用于加密库内所有文件" />
              <div class="strength">
                <i v-for="n in 4" :key="n" :class="'lv' + (createVaultStrength >= n ? createVaultStrength : '')"></i>
                <em v-if="createVaultStrength >= 4">强 · 建议使用密码管理器生成</em>
                <em v-else-if="createVaultStrength >= 3">中 · 建议混合符号</em>
                <em v-else>弱 · 建议 12 位以上并混合符号</em>
              </div>
            </div>
            <div class="saveas-field">
              <label>确认密码</label>
              <input class="input" type="password" v-model="createVaultForm.confirm" placeholder="再次输入主密码" />
            </div>
            <div class="saveas-hint">点击下方「保存」将创建该单文件库，并把文件保存到库根目录。</div>
          </template>
          <template v-else>
            <div class="saveas-field" v-if="saveAsFolders.length">
              <label>目标文件夹</label>
              <select class="input" v-model="saveAsDir">
                <option value="">库根目录</option>
                <option v-for="d in saveAsFolders" :key="d" :value="d">{{ d }}</option>
              </select>
            </div>
            <div class="saveas-hint" v-if="!unlockedVaults.some((x) => x.isDir || x.isFileVault)">没有已解锁的库，可在下拉里选「＋ 新建库…」创建一个。</div>
            <div class="saveas-hint" v-else>存入已解锁库，复用库的主密码，无需再次填写密码。</div>
          </template>
        </template>

        <template v-else>
          <div class="saveas-field">
            <label>保存位置</label>
            <div class="saveas-path">
              <input class="input" v-model="saveAsPath" :placeholder="'如 ~/Documents/' + saveAsName + '.md'" readonly />
              <button class="btn" @click="browseSavePath">浏览…</button>
            </div>
          </div>
          <div class="saveas-hint">保存为普通 Markdown 文件（明文，不加密），可被其他编辑器直接打开；在 MarkLock 中重新打开时按明文阅读/编辑。</div>
        </template>
      </div>

      <div class="modal-foot">
        <button class="btn" @click="showSaveAs = false">取消</button>
        <button class="btn btn-primary" :disabled="saving" @click="submitSaveAs">{{ saving ? '处理中…' : (saveAsConvert ? '转换' : '保存') }}</button>
      </div>
    </a-modal>

    <!-- 转为加密文件完成：原明文文件处理询问 -->
    <a-modal v-model:open="showConvertPrompt" title="转为加密文件完成" :width="440" :footer="null" :closable="false" :mask-closable="false">
      <div class="saveas">
        <div class="convert-msg">已将该文件转为加密文件（.mdl）。是否删除原来的明文文件？删除后不可恢复。</div>
        <div class="convert-path" v-tip="convertOriginalPath">{{ convertOriginalPath }}</div>
        <label class="convert-remember"><input type="checkbox" v-model="convertRemember" /> 以后都按此处理，不再询问</label>
        <div class="modal-foot">
          <button class="btn" @click="onConvertPrompt('keep')">保留原文件</button>
          <button class="btn btn-danger" @click="onConvertPrompt('delete')">删除原文件</button>
        </div>
      </div>
    </a-modal>

    <!-- 新建文件（库内 / 工作目录，工作目录可选普通 md / 加密） -->
    <a-modal v-model:open="showNewFile" title="新建文件" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-field">
          <label>目标位置</label>
          <div class="saveas-hint">{{ wdOp ? (wdDir || '工作目录') : (newFileDir ? '库内 / ' + newFileDir : '库根目录') }}</div>
        </div>
        <!-- 类型选择仅工作目录生效：库内新建天然跟随库主密码加密 -->
        <div class="seg saveas-seg" v-if="wdOp">
          <span :class="{ on: newFileType === 'plain' }" @click="newFileType = 'plain'">普通 md</span>
          <span :class="{ on: newFileType === 'encrypted' }" @click="newFileType = 'encrypted'">加密文件 .mdl</span>
          <span :class="{ on: newFileType === 'vault' }" @click="newFileType = 'vault'">加密库文件 .mdlb</span>
        </div>
        <div class="saveas-field">
          <label>文件名</label>
          <input class="input" ref="newFileNameInput" v-model="newFileName" placeholder="如 会议纪要" @keyup.enter="submitNewFile" />
        </div>
        <template v-if="wdOp && newFileType !== 'plain'">
          <div class="saveas-field">
            <label>主密码</label>
            <input class="input" type="password" v-model="newFilePwd" :placeholder="newFileType === 'vault' ? '至少 1 位，用于加密库内所有文件' : '至少 1 位，用于加密该文件'" />
            <div class="strength">
              <i v-for="n in 4" :key="n" :class="'lv' + (newFileStrength >= n ? newFileStrength : '')"></i>
              <em v-if="newFileStrength >= 4">强 · 建议使用密码管理器生成</em>
              <em v-else-if="newFileStrength >= 3">中 · 建议混合符号</em>
              <em v-else>弱 · 建议 12 位以上并混合符号</em>
            </div>
          </div>
          <div class="saveas-field">
            <label>确认密码</label>
            <input class="input" type="password" v-model="newFileConfirm" placeholder="再次输入主密码" @keyup.enter="submitNewFile" />
          </div>
        </template>
        <div class="saveas-hint" v-if="!wdOp">文件将以库主密码加密存储，打开时无需再次输入密码。</div>
        <div class="saveas-hint" v-else-if="newFileType === 'plain'">将在工作目录创建明文文件（不加密），无扩展名时自动补 .md。</div>
        <div class="saveas-hint" v-else-if="newFileType === 'encrypted'">创建独立的加密文件（.mdl），拥有独立密码，可单独移动/分享。</div>
        <div class="saveas-hint" v-else>在当前目录创建单文件加密库（.mdlb）并自动解锁，创建后显示在侧边栏「加密库」分组。</div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showNewFile = false">取消</button>
        <button class="btn btn-primary" @click="submitNewFile">创建</button>
      </div>
    </a-modal>

    <!-- 新建文件夹（库内） -->
    <a-modal v-model:open="showNewFolder" title="新建文件夹" :width="420" :footer="null">
      <div class="saveas">
        <div class="saveas-field">
          <label>目标位置</label>
          <div class="saveas-hint">{{ wdOp ? (wdDir || '工作目录') : (newFolderDir ? '库内 / ' + newFolderDir : '库根目录') }}</div>
        </div>
        <div class="saveas-field">
          <label>文件夹名</label>
          <input class="input" ref="newFolderNameInput" v-model="newFolderName" placeholder="如 项目文档" @keyup.enter="submitNewFolder" />
        </div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showNewFolder = false">取消</button>
        <button class="btn btn-primary" @click="submitNewFolder">创建</button>
      </div>
    </a-modal>

    <!-- 新建加密库 -->
    <a-modal v-model:open="showCreateVault" title="新建加密库" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-field">
          <label>保存位置</label>
          <div class="saveas-path">
            <input class="input" v-model="createVaultForm.path" placeholder="选择 .mdlb 文件的保存路径" readonly />
            <button class="btn" @click="browseCreateVaultPath">浏览…</button>
          </div>
        </div>
        <div class="saveas-field">
          <label>主密码</label>
          <input class="input" type="password" v-model="createVaultForm.pwd" placeholder="至少 1 位，用于加密库内所有文件" />
          <div class="strength">
            <i v-for="n in 4" :key="n" :class="'lv' + (createVaultStrength >= n ? createVaultStrength : '')"></i>
            <em v-if="createVaultStrength >= 4">强 · 建议使用密码管理器生成</em>
            <em v-else-if="createVaultStrength >= 3">中 · 建议混合符号</em>
            <em v-else>弱 · 建议 12 位以上并混合符号</em>
          </div>
        </div>
        <div class="saveas-field">
          <label>确认密码</label>
          <input class="input" type="password" v-model="createVaultForm.confirm" placeholder="再次输入主密码" @keyup.enter="submitCreateVault" />
        </div>
        <div class="saveas-hint">单文件库（.mdlb）：文件名、目录结构、正文全部密文，可直接放进 iCloud / Dropbox 同步。</div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showCreateVault = false">取消</button>
        <button class="btn btn-primary" :disabled="creatingVault" @click="submitCreateVault">{{ creatingVault ? '创建中…' : '创建并解锁' }}</button>
      </div>
    </a-modal>

    <!-- 批量加密文件 -->
    <a-modal v-model:open="showBatchEncrypt" title="批量加密文件" :width="520" :footer="null" :mask-closable="!beBusy" :closable="!beBusy">
      <div class="saveas">
        <div class="saveas-field">
          <label>待加密文件 <span class="be-cnt">（已选 {{ bePaths.length }} 个）</span></label>
          <div class="be-list">
            <div v-if="!bePaths.length" class="empty-hint">尚未选择文件，点击下方「添加文件…」。</div>
            <div v-for="p in bePaths" :key="p" class="be-row">
              <span class="be-path" v-tip="p">{{ p }}</span>
              <span class="be-del" @click="beRemove(p)">×</span>
            </div>
          </div>
          <div class="be-actions">
            <button class="btn" :disabled="beBusy" @click="beBrowse">添加文件…</button>
            <button class="btn" :disabled="beBusy || !bePaths.length" @click="bePaths = []">清空</button>
          </div>
        </div>
        <div class="saveas-field">
          <label>主密码</label>
          <input class="input" type="password" v-model="bePassword" :readonly="beBusy" placeholder="至少 1 位，本次批次内所有文件共用" />
          <div class="strength">
            <i v-for="n in 4" :key="n" :class="'lv' + (beStrength >= n ? beStrength : '')"></i>
            <em v-if="beStrength >= 4">强 · 建议使用密码管理器生成</em>
            <em v-else-if="beStrength >= 3">中 · 建议混合符号</em>
            <em v-else>弱 · 建议 12 位以上并混合符号</em>
          </div>
        </div>
        <div class="saveas-field">
          <label>确认密码</label>
          <input class="input" type="password" v-model="beConfirm" :readonly="beBusy" placeholder="再次输入主密码" @keyup.enter="submitBatchEncrypt" />
        </div>
        <div class="saveas-field">
          <label>原文件处理</label>
          <label class="radio"><input type="radio" :value="false" v-model="beMoveOriginals" :disabled="beBusy" /> 保留原文件，同目录生成 .mdl 副本</label>
          <label class="radio"><input type="radio" :value="true" v-model="beMoveOriginals" :disabled="beBusy" /> 将原文件移到同目录下的「原文件」子文件夹</label>
        </div>
        <div v-if="beResult" class="be-result">
          <div>✓ 加密成功：<b>{{ beResult.encrypted.length }}</b> 个文件<template v-if="beMoveOriginals">，已移动 {{ beResult.moved.length }} 个原件</template></div>
          <div v-if="beResult.failed.length" class="be-failed">
            ⚠ 失败 {{ beResult.failed.length }} 个：
            <ul>
              <li v-for="(f, i) in beResult.failed.slice(0, 10)" :key="i"><span class="be-fpath">{{ f[0] }}</span> — {{ f[1] }}</li>
              <li v-if="beResult.failed.length > 10">… 另外 {{ beResult.failed.length - 10 }} 项</li>
            </ul>
          </div>
        </div>
        <div class="saveas-hint">同一个主密码，但每个文件都是独立的 .mdl（与目录库内文件同构）；Argon2id 仅执行一次，多文件不会逐个耗时。</div>
      </div>
      <div class="modal-foot">
        <button class="btn" :disabled="beBusy" @click="showBatchEncrypt = false">{{ beResult ? '关闭' : '取消' }}</button>
        <button class="btn btn-primary" :disabled="beBusy" @click="submitBatchEncrypt">{{ beBusy ? '加密中…' : '开始加密' }}</button>
      </div>
    </a-modal>

    <!-- 导入目录到库 -->
    <a-modal v-model:open="showImportDir" title="导入目录到库" :width="520" :footer="null" :mask-closable="!idBusy" :closable="!idBusy">
      <div class="saveas">
        <div class="saveas-field">
          <label>源目录</label>
          <div class="saveas-path">
            <input class="input" v-model="idSrcDir" :readonly="true" placeholder="选择包含 md/txt 的目录（含子目录）" />
            <button class="btn" :disabled="idBusy" @click="idBrowseSrc">浏览…</button>
          </div>
        </div>
        <div class="saveas-field">
          <label>目标库</label>
          <label class="radio"><input type="radio" value="existing" v-model="idTargetKind" :disabled="idBusy || !unlockedVaults.length" /> 导入到已解锁的库</label>
          <label class="radio"><input type="radio" value="new" v-model="idTargetKind" :disabled="idBusy" /> 新建单文件库（.mdlb）后导入</label>
        </div>
        <template v-if="idTargetKind === 'existing'">
          <div class="saveas-field">
            <label>选择库</label>
            <select class="input" v-model="idVaultId" :disabled="idBusy">
              <option v-for="v in unlockedVaults.filter((x) => x.isDir || x.isFileVault)" :key="v.id" :value="v.id">{{ v.name }}</option>
            </select>
            <div v-if="!unlockedVaults.length" class="saveas-hint">当前无已解锁库，请选「新建库」或先去解锁页解锁一个库。</div>
          </div>
        </template>
        <template v-else>
          <div class="saveas-field">
            <label>新库保存位置</label>
            <div class="saveas-path">
              <input class="input" v-model="idCreateForm.path" placeholder="选择 .mdlb 文件的保存路径" readonly />
              <button class="btn" :disabled="idBusy" @click="idBrowseVaultPath">浏览…</button>
            </div>
          </div>
          <div class="saveas-field">
            <label>主密码</label>
            <input class="input" type="password" v-model="idCreateForm.pwd" :readonly="idBusy" placeholder="至少 1 位" />
            <div class="strength">
              <i v-for="n in 4" :key="n" :class="'lv' + (idCreateStrength >= n ? idCreateStrength : '')"></i>
              <em v-if="idCreateStrength >= 4">强</em>
              <em v-else-if="idCreateStrength >= 3">中</em>
              <em v-else>弱</em>
            </div>
          </div>
          <div class="saveas-field">
            <label>确认密码</label>
            <input class="input" type="password" v-model="idCreateForm.confirm" :readonly="idBusy" placeholder="再次输入主密码" />
          </div>
        </template>
        <div v-if="idResult" class="be-result">
          <div>✓ 导入：<b>{{ idResult.imported.length }}</b> 个文件 / 跳过 {{ idResult.skipped.length }} / 失败 {{ idResult.failed.length }}</div>
          <div v-if="idResult.skipped.length" class="be-failed">
            → 跳过示例（最多10 项）：
            <ul><li v-for="(s, i) in idResult.skipped.slice(0, 10)" :key="i">{{ s }}</li></ul>
          </div>
          <div v-if="idResult.failed.length" class="be-failed">
            ⚠ 失败明细：
            <ul><li v-for="(f, i) in idResult.failed.slice(0, 10)" :key="i"><span class="be-fpath">{{ f[0] }}</span> — {{ f[1] }}</li></ul>
          </div>
        </div>
        <div class="saveas-hint">仅导入 .md/.markdown/.mdown/.mkd/.txt，自动保留子目录结构；目标已存在同名文件时不覆盖。</div>
      </div>
      <div class="modal-foot">
        <button class="btn" :disabled="idBusy" @click="showImportDir = false">{{ idResult ? '关闭' : '取消' }}</button>
        <button class="btn btn-primary" :disabled="idBusy" @click="submitImportDir">{{ idBusy ? '导入中…' : '开始导入' }}</button>
      </div>
    </a-modal>

    <!-- 复制到加密库 -->
    <a-modal v-model:open="showCopyToVault" title="复制到加密库" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-field">
          <label>目标库</label>
          <select class="input" v-model="copyVaultId" @change="onCopyVaultChange">
            <option v-for="v in unlockedVaults.filter((x) => x.isDir || x.isFileVault)" :key="v.id" :value="v.id">{{ v.name }}</option>
            <option value="__new__">＋ 新建库…</option>
          </select>
        </div>
        <!-- 内联新建库表单 -->
        <template v-if="copyVaultId === '__new__'">
          <div class="saveas-field">
            <label>保存位置</label>
            <div class="saveas-path">
              <input class="input" v-model="createVaultForm.path" placeholder="选择 .mdlb 文件的保存路径" readonly />
              <button class="btn" @click="browseCreateVaultPath">浏览…</button>
            </div>
          </div>
          <div class="saveas-field">
            <label>主密码</label>
            <input class="input" type="password" v-model="createVaultForm.pwd" placeholder="至少 1 位，用于加密库内所有文件" />
            <div class="strength">
              <i v-for="n in 4" :key="n" :class="'lv' + (createVaultStrength >= n ? createVaultStrength : '')"></i>
              <em v-if="createVaultStrength >= 4">强 · 建议使用密码管理器生成</em>
              <em v-else-if="createVaultStrength >= 3">中 · 建议混合符号</em>
              <em v-else>弱 · 建议 12 位以上并混合符号</em>
            </div>
          </div>
          <div class="saveas-field">
            <label>确认密码</label>
            <input class="input" type="password" v-model="createVaultForm.confirm" placeholder="再次输入主密码" />
          </div>
          <div class="saveas-hint">点击下方「复制」将创建该单文件库，并把文件复制到库根目录。</div>
        </template>
        <template v-else>
          <div class="saveas-field" v-if="copyFolders.length">
            <label>目标文件夹</label>
            <select class="input" v-model="copyDir">
              <option value="">库根目录</option>
              <option v-for="d in copyFolders" :key="d" :value="d">{{ d }}</option>
            </select>
          </div>
          <div class="saveas-field">
            <label>文件名</label>
            <input class="input" v-model="copyName" placeholder="如 会议纪要" />
          </div>
          <div class="saveas-hint" v-if="!unlockedVaults.some((x) => x.isDir || x.isFileVault)">没有已解锁的库，可在下拉里选「＋ 新建库…」创建一个。</div>
          <div class="saveas-hint" v-else>将当前文件内容另存一份到库内，原文件保持不变。</div>
        </template>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showCopyToVault = false">取消</button>
        <button class="btn btn-primary" :disabled="copying" @click="submitCopyToVault">{{ copying ? '复制中…' : '复制' }}</button>
      </div>
    </a-modal>

    <!-- 修改主密码（库 / 单文件 .mdl） -->
    <a-modal v-model:open="showChangePwd" :title="`修改密码 · ${changePwdTarget.title}`" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-hint">仅以新密码重新加密密钥，不重加密正文；旧密码即刻失效且不可找回。</div>
        <div class="saveas-field">
          <label>当前密码</label>
          <input class="input" type="password" v-model="changePwdForm.current" placeholder="输入现有主密码" />
        </div>
        <div class="saveas-field">
          <label>新密码</label>
          <input class="input" type="password" v-model="changePwdForm.next" placeholder="至少 1 位" />
          <div class="strength">
            <i v-for="n in 4" :key="n" :class="'lv' + (changePwdStrength >= n ? changePwdStrength : '')"></i>
            <em v-if="changePwdStrength >= 4">强 · 建议使用密码管理器生成</em>
            <em v-else-if="changePwdStrength >= 3">中 · 建议混合符号</em>
            <em v-else>弱 · 建议 12 位以上并混合符号</em>
          </div>
        </div>
        <div class="saveas-field">
          <label>确认新密码</label>
          <input class="input" type="password" v-model="changePwdForm.confirm" placeholder="再次输入新密码" @keyup.enter="submitChangePwd" />
        </div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showChangePwd = false">取消</button>
        <button class="btn btn-primary" :disabled="changingPwd" @click="submitChangePwd">{{ changingPwd ? '修改中…' : '确认修改' }}</button>
      </div>
    </a-modal>

    <!-- 重命名 / 移动（库内） -->
    <a-modal v-model:open="showRename" :title="renameIsDir ? '重命名文件夹 / 移动' : '重命名文件 / 移动'" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-field">
          <label>新名称 / 路径</label>
          <input class="input" ref="renameNameInput" v-model="renameName" placeholder="如 项目/新名字.md" @keyup.enter="submitRename" />
        </div>
        <div class="saveas-hint">可直接输入新名称；若输入含「/」的路径（如「子目录/新名字.md」），则把该项移动/重命名到对应位置。</div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showRename = false">取消</button>
        <button class="btn btn-primary" :disabled="renaming" @click="submitRename">{{ renaming ? '处理中…' : '确定' }}</button>
      </div>
    </a-modal>

    <!-- 删除（库内） -->
    <a-modal v-model:open="showDelete" :title="(wdOp ? '移入回收站：' : '') + (deleteIsDir ? '文件夹' : '文件')" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-hint" :style="wdOp ? 'color:var(--text-2);' : 'color:#cf1322;'">
          {{ wdOp
            ? (deleteIsDir ? '将把该文件夹移入系统回收站，可从回收站恢复。' : '将把该文件移入系统回收站，可从回收站恢复。')
            : (deleteIsDir ? '将删除该文件夹及其内部所有内容（不可恢复）。' : '将删除该文件（不可恢复）。') }}
        </div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showDelete = false">取消</button>
        <button :class="['btn', wdOp ? 'btn-primary' : 'btn-danger']" :disabled="deleting" @click="submitDelete">{{ wdOp ? (deleting ? '移入中…' : '移入回收站') : (deleting ? '删除中…' : '删除') }}</button>
      </div>
    </a-modal>

    <!-- 预览模式勾选任务清单确认 -->
    <a-modal v-model:open="showTaskConfirm" :title="pendingTaskChecked ? '勾选任务' : '取消勾选'" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-hint" style="color:var(--text-2);">「{{ pendingTaskText }}」{{ pendingTaskChecked ? '该任务已完成' : '该任务设置为未完成' }}。</div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showTaskConfirm = false">取消</button>
        <button class="btn btn-primary" @click="submitTaskConfirm">确定</button>
      </div>
    </a-modal>

    <!-- 关闭未保存文件确认 -->
    <a-modal v-model:open="showCloseConfirm" :title="closeConfirmTitle" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-hint">{{ closeConfirmText }}</div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="cancelClose">取消</button>
        <button class="btn btn-danger" @click="discardClose">{{ closeConfirmDiscard }}</button>
        <button class="btn btn-primary" @click="saveAndClose">{{ closeConfirmSave }}</button>
      </div>
    </a-modal>

    <!-- 外部修改：丢弃内存修改、重新加载磁盘版本确认 -->
    <a-modal v-model:open="showReloadConfirm" title="重新加载磁盘版本" :width="440" :footer="null">
      <div class="saveas">
        <div class="saveas-hint">页签有未保存的修改，重新加载将丢弃这些修改并换回磁盘上的内容。是否继续？</div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="showReloadConfirm = false">取消</button>
        <button class="btn btn-danger" @click="doReloadTab(reloadTarget)">丢弃并重载</button>
      </div>
    </a-modal>

    <!-- 拖入资源（图片/文件）：询问复制到 .dat 资源目录还是仅链接原路径 -->
    <a-modal v-model:open="showAssetPrompt" title="拖入文件" :width="480" :footer="null" :mask-closable="false">
      <div class="saveas">
        <div class="saveas-hint">检测到拖入的非文档文件。复制到资源目录会将其存入当前文件同名的 .dat 目录并插入相对链接（随文件一起移动/加密）；仅链接则保留原位置，插入绝对路径链接。</div>
        <div class="saveas-hint" style="max-height:160px;overflow:auto;word-break:break-all;">
          <div v-for="p in assetPromptFiles" :key="p" style="opacity:.85;">{{ fileName(p) }}</div>
        </div>
      </div>
      <div class="modal-foot">
        <button class="btn" :disabled="assetBusy" @click="cancelAssetPrompt">取消</button>
        <button class="btn" :disabled="assetBusy" @click="resolveAssetPrompt('link')">仅链接</button>
        <button class="btn btn-primary" :disabled="assetBusy" @click="resolveAssetPrompt('copy')">复制到资源目录</button>
      </div>
    </a-modal>

    <!-- 首次使用：询问是否设为 .mdl/.mdlb 默认打开程序 -->
    <a-modal v-model:open="showAssocPrompt" title="设为默认打开程序" :width="460" :footer="null" :mask-closable="false">
      <div class="saveas">
        <div class="saveas-hint">
          检测到系统当前不是以 MarkLock 打开 .md / .mdl / .mdlb 文件。是否将 MarkLock 设为这三种文件类型的默认打开程序？
          设为默认后，在访达 / 资源管理器中双击这些文件即可直接由 MarkLock 打开。
        </div>
        <div class="saveas-hint" style="opacity:.75;">点击后将自动完成设置，无需进入系统设置；极少数被系统锁定的情况会提示手动步骤。</div>
      </div>
      <div class="modal-foot">
        <button class="btn" @click="dismissAssocPrompt">暂不需要</button>
        <button class="btn btn-primary" @click="applySetDefault">设为默认打开程序</button>
      </div>
    </a-modal>

    <!-- ======== 右键上下文菜单（自绘） ======== -->
    <div
      v-if="ctxMenu.visible"
      class="ctx-menu"
      :style="{ left: ctxMenu.x + 'px', top: ctxMenu.y + 'px' }"
      @contextmenu.prevent
    >
      <div v-if="ctxMenu.title" class="m-cap">{{ ctxMenu.title }}</div>
      <template v-for="(it, i) in ctxMenu.items" :key="i">
        <div v-if="it.kind === 'sep'" class="m-sep"></div>
        <div v-else class="m-item" :class="{ disabled: it.disabled }" @click="runCtxItem(it)">
          <span class="ck">{{ it.checked ? '✓' : '' }}</span><span class="m-label">{{ it.label }}<span v-if="it.hint" class="m-sub">{{ it.hint }}</span></span><span v-if="it.sc" class="kbd-hint">{{ it.sc }}</span>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 拖放遮罩 */
.drop-mask {
  position: fixed; inset: 0; z-index: 200;
  display: flex; align-items: center; justify-content: center;
  background: rgba(22, 119, 255, 0.06);
  backdrop-filter: blur(1px);
  pointer-events: none;
}
.drop-card {
  display: flex; flex-direction: column; align-items: center; gap: 14px;
  padding: 28px 44px;
  background: var(--panel);
  border: 2px dashed var(--primary);
  border-radius: var(--radius-lg);
  color: var(--primary);
  box-shadow: var(--shadow-modal);
}
.drop-card svg { color: var(--primary); }
.drop-t { font-size: 15px; font-weight: 600; color: var(--primary-active); }
.drop-d { font-size: 12px; color: var(--text-3); margin-top: -6px; }
.fade-enter-active, .fade-leave-active { transition: opacity 0.15s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }

/* 标题栏 */
.tb-title { display: flex; align-items: center; gap: 8px; min-width: 0; cursor: default; -webkit-user-select: none; user-select: none; }
.tb-title .doc { font-size: 13.5px; font-weight: 600; }
.tb-title .vault { font-size: 12px; color: var(--text-3); }
.tb-right { margin-left: auto; display: flex; align-items: center; gap: 6px; }
.dot { width: 8px; height: 8px; border-radius: 50%; background: var(--green); display: inline-block; }

/* 下拉菜单 */
.menu-wrap { position: relative; }
.menu {
  position: absolute; right: 0; top: 32px; z-index: 60;
  width: 216px;
  background: var(--panel);
  border: 1px solid var(--border-pop);
  border-radius: var(--radius);
  box-shadow: var(--shadow-pop);
  padding: 5px;
  display: none;
}
.menu.open { display: block; }
.menu .m-cap { font-size: 11px; color: var(--text-4); padding: 6px 10px 4px; }
/* 弹层菜单顶部标题：视觉与右上角主菜单的分组名称（.menu .m-cap）保持一致，仅加不可选中 */
.ctx-menu .m-cap { font-size: 11px; color: var(--text-4); padding: 6px 10px 4px; user-select: none; }
.m-item {
  display: flex; align-items: center; gap: 8px;
  height: 30px; padding: 0 10px;
  border-radius: var(--radius-sm);
  font-size: 13px; color: var(--text-2);
  cursor: pointer;
}
.m-item:hover { background: var(--hover); }
.m-item .ck { width: 15px; color: var(--primary); font-weight: 700; flex: 0 0 15px; }
.m-item .m-label { flex: 1 1 auto; }
/* 档位项尾注（如「默认」）：跟在本项文字后、比正文更小更淡，不占用快捷键列 */
.m-item .m-sub { margin-left: 6px; font-size: 11px; color: var(--text-4); }
.m-item .kbd-hint { flex: 0 0 auto; margin-left: 16px; font-size: 12px; color: var(--text-4); letter-spacing: .5px; }
.m-sep { height: 1px; background: var(--border-2); margin: 5px 4px; }

/* 子菜单 */
.m-item.has-sub { position: relative; }
.m-item.has-sub .sub-arrow { margin-left: auto; font-size: 10px; color: var(--text-4); }
.m-item.has-sub > .sub-menu {
  display: none;
  position: absolute; right: 100%; top: -6px;
  width: 216px;
  background: var(--panel);
  border: 1px solid var(--border-pop);
  border-radius: var(--radius-sm);
  box-shadow: var(--shadow-modal);
  padding: 4px;
  z-index: 70;
}
.m-item.has-sub:hover > .sub-menu { display: block; }

/* 右键上下文菜单（复用 .m-item/.m-cap/.m-sep/.ck） */
.ctx-menu {
  position: fixed; z-index: 300;
  min-width: 200px;
  background: var(--panel);
  border: 1px solid var(--border-pop);
  border-radius: var(--radius);
  box-shadow: var(--shadow-pop);
  padding: 5px;
}
.m-item.disabled { opacity: 0.45; pointer-events: none; }

/* 分段选择器 */
.seg { display: inline-flex; padding: 2px; background: var(--hover); border-radius: var(--radius-sm); gap: 2px; }
.seg span { padding: 2px 12px; font-size: 13px; color: var(--text-2); border-radius: 5px; cursor: pointer; transition: all 0.2s; }
.seg span.on { background: var(--panel); color: var(--text); font-weight: 500; box-shadow: 0 2px 4px rgba(0, 0, 0, 0.06); }

/* 页签栏 */
.tabsbar {
  flex: 0 0 36px;
  display: flex; align-items: stretch;
  background: var(--bg);
  border-bottom: 1px solid var(--border-2);
  overflow-x: auto;
  scrollbar-width: none;
  /* 双击空白新建时不要出现文本选区（.tab 已单独禁用，容器兼顾空白区） */
  -webkit-user-select: none; user-select: none;
}
.tabsbar::-webkit-scrollbar { display: none; }
.tab {
  display: flex; align-items: center; gap: 7px;
  padding: 0 6px 0 12px;
  font-size: 12.5px; color: var(--text-3);
  border-right: 1px solid var(--border-2);
  cursor: pointer; -webkit-user-select: none; user-select: none;
  white-space: nowrap;
  position: relative;
  flex: 1 1 auto;
  min-width: 0;
  max-width: 240px;
}
.tab:hover { color: var(--text-2); background: var(--hover); }
.tab.on { background: var(--panel); color: var(--text); font-weight: 500; }
.tab.dragging { opacity: 0.45; }
.tab.dragover { box-shadow: inset 2px 0 0 var(--primary); background: var(--primary-bg); }
.tab.on::after { content: ""; position: absolute; left: 0; right: 0; bottom: -1px; height: 2px; background: var(--primary); }
.tab .x {
  position: absolute; right: 3px; top: 50%; transform: translateY(-50%);
  width: 20px; height: 20px;
  display: inline-flex; align-items: center; justify-content: center;
  border-radius: 4px; color: var(--text-4); font-size: 12px; opacity: 0;
  z-index: 1;
}
/* .tab:hover .x, .tab.on .x { opacity: 1; } */
.tab:hover .x { opacity: 1; background-color: var(--panel);}
.tab .x:hover { background: var(--border); color: var(--text); }
.tab .dirty { width: 7px; height: 7px; border-radius: 50%; background: var(--gold); flex: 0 0 auto; }
/* 外部修改角标：空心环区别于实心未保存小圆点，避免两种状态同现时看不出区别 */
.tab .ext, .t-item .ext {
  width: 7px; height: 7px; border-radius: 50%; flex: 0 0 auto;
  border: 1.5px solid var(--orange, #fa8c16); background: transparent;
}
.tab .ext.missing, .t-item .ext.missing { border-color: #cf1322; }
.tab .tab-name { max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tab .vt { font-size: 10px; padding: 0 5px; border-radius: 3px; background: var(--hover); color: #722ed1; flex: 0 0 auto; 
  display: none;
}
.tab:hover .vt {
  /* display: block; */
}
.tab.locked { color: var(--text-4); }
.tab.locked.on { color: var(--text-2); }
.tab-add { flex: 0 0 34px; display: flex; align-items: center; justify-content: center; color: var(--text-3); cursor: pointer; }
.tab-add:hover { color: var(--primary); background: var(--hover); }

/* 主体：左侧为全高侧边栏，右侧 right-col 纵向堆叠页签栏/工作台/状态栏 */
.main { flex: 1; display: flex; min-height: 0; }
.right-col { flex: 1; display: flex; flex-direction: column; min-width: 0; min-height: 0; }

/* 侧边栏 */
.sidebar {
  width: 264px; flex: 0 0 264px;
  display: flex; flex-direction: column;
  background: var(--sidebar-bg);
  /* 侧边栏与右列的分隔线：用弹层边框色，比 --border 再深一档 */
  border-right: 1px solid var(--border-pop);
  overflow: hidden;
}
.sidebar-resize {
  flex: 0 0 3px;
  cursor: col-resize;
  background: var(--sidebar-bg);
  position: relative;
  z-index: 10;
  transition: background 0.15s;
}
.sidebar-resize:hover,
.sidebar-resize:active {
  background: var(--primary);
}
.sb-head { padding: 10px 12px 6px; }
.sb-search { position: relative; }
.sb-search .input { height: 30px; font-size: 13px; padding-left: 30px; background: var(--panel); }
.sb-search svg { position: absolute; left: 9px; top: 8px; }

/* 搜索结果下拉 */
.search-pop {
  position: absolute;
  left: 0; right: 0; top: 36px;
  max-height: 520px;
  overflow-y: auto;
  background: var(--panel);
  border: 1px solid var(--border-pop);
  border-radius: var(--radius-sm);
  box-shadow: var(--shadow-pop);
  z-index: 50;
  padding: 4px;
}
.search-empty { padding: 18px 12px; text-align: center; font-size: 12.5px; color: var(--text-4); }
.search-hit {
  display: flex; gap: 8px; align-items: flex-start;
  padding: 7px 8px;
  border-radius: 5px;
  cursor: pointer;
}
.search-hit:hover { background: var(--hover); }
.search-hit.active { background: var(--hover); }
.sh-dot { width: 8px; height: 8px; border-radius: 50%; margin-top: 5px; flex: 0 0 auto; }
.sh-body { flex: 1; min-width: 0; }
.sh-title { display: flex; align-items: baseline; gap: 6px; }
.sh-name { font-size: 12.5px; font-weight: 500; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.sh-vault { font-size: 10.5px; color: var(--text-3); flex: 0 0 auto; }
/* 文件名命中徽章：与结果行本身显示的文件名区分，避免误认为内容匹配 */
.sh-name-tag {
  flex: 0 0 auto;
  font-size: 10px; line-height: 14px;
  padding: 0 5px;
  border-radius: 3px;
  color: var(--primary-active);
  background: var(--primary-bg);
  border: 1px solid rgba(22, 119, 255, 0.2);
}
.sh-snippet { font-size: 12px; color: var(--text-3); margin-top: 2px; line-height: 1.5; display: -webkit-box; -webkit-line-clamp: 4; -webkit-box-orient: vertical; overflow: hidden; word-break: break-all; }
.sh-snippet.path-hit { color: var(--text-4); font-family: var(--mono); font-size: 11.5px; }
.sh-snippet mark { background: transparent; color: var(--primary); font-weight: 600; }

.sb-body { flex: 1; overflow-y: auto; padding-bottom: 12px; }

/* 分组 */
.sec + .sec { 
  border-top: 1px solid var(--border);
  /* margin-top: 6px; */
  /* padding-top: 4px; */
}
.sec.dragging { opacity: 0.45; }
.sec.dragover { box-shadow: inset 0 2px 0 var(--primary); }
.sec.dragover .sec-head { background: var(--primary-bg); }
.sec-head {
  display: flex; align-items: center; gap: 4px;
  height: 28px; padding: 0 8px 0 4px;
  font-size: 11px; font-weight: bold;
  color: var(--text-2);
  letter-spacing: 0.3px;
  white-space: nowrap;
  cursor: pointer; user-select: none;
  position: sticky; top: 0;
  background: var(--bg);
  z-index: 2;
}
.sec-head .chev { width: 18px; height: 18px; display: inline-flex; align-items: center; justify-content: center; color: var(--text-3); transition: transform 0.15s; }
.sec-head .cnt { color: var(--text-4); font-weight: 400; margin-left: 2px; }
.sec-head .acts { margin-left: auto; display: none; gap: 2px; flex: 0 0 auto; }
/* 工作目录标题为目录名，可能很长：允许收缩省略，把右侧按钮组留给固定宽度 */
.sec-head .wd-title { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.sec-head:hover .acts { display: flex; }
.sec-head .acts .icon-btn { width: 20px; height: 20px; }
/* 悬停背景：侧边栏底色与 --hover 太接近几乎看不出，改用 --border 保证可辨识 */
.sec-head .acts .icon-btn:hover { background: var(--border); }

.sec-list { background-color: var(--panel); }

/* 大纲条目：按标题层级缩进，H1~H6 小字号徽章标识 */
.ol-item {
  display: flex; align-items: center; gap: 6px;
  height: 26px; padding-right: 10px;
  font-size: 12.5px; color: var(--text-2);
  cursor: pointer; white-space: nowrap; overflow: hidden;
}
.ol-item:hover { background: var(--hover); }
.ol-item.on { background: var(--primary-bg); color: var(--primary); }
.ol-item .ol-h { flex: 0 0 auto; font-size: 9.5px; font-weight: 600; color: var(--text-4); opacity: 0.75; letter-spacing: 0.2px; }
.ol-item.on .ol-h { color: var(--primary); opacity: 1; }
.ol-item .ol-text { overflow: hidden; text-overflow: ellipsis; }

.t-item {
  display: flex; align-items: center; gap: 5px;
  height: 29px; padding: 0 10px 0 12px;
  font-size: 13px; color: var(--text-2);
  cursor: pointer;
  -webkit-user-select: none; user-select: none;
  position: relative;
}
.t-item:hover { background: var(--hover); }
.t-item.on { background: var(--primary-bg); color: var(--primary-active); font-weight: 500; }
.t-item.dragging { opacity: 0.4; }
.t-item.dragover { box-shadow: inset 0 2px 0 var(--primary); background: var(--primary-bg); }
.t-item .fname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.t-item svg.lead { flex: 0 0 auto; }
.chev-i { transition: transform 0.15s; }
.t-item .meta { font-size: 11px; color: var(--text-4); flex: 0 0 auto; }
/* 工作目录树行的「更多操作」：与加密库行 / 库内树同款三点图标，悬停行才显示；
   行悬停底色已是 --hover，按钮悬停改用 --border 才能看出反馈 */
.t-item .acts { flex: 0 0 auto; display: none; gap: 2px; align-items: center; }
.t-item:hover .acts { display: flex; }
.t-item .acts .icon-btn { width: 18px; height: 18px; color: var(--text-4); }
.t-item .acts .icon-btn:hover { background: var(--border); color: var(--text); }
.t-item .x {
  width: 17px; height: 17px;
  display: none; align-items: center; justify-content: center;
  border-radius: 4px; color: var(--text-3); font-size: 12px; flex: 0 0 auto;
}
.t-item:hover .x { display: inline-flex; }
.t-item .x:hover { background: var(--border); color: var(--text); }
.t-item .dirty { width: 7px; height: 7px; border-radius: 50%; background: var(--gold); flex: 0 0 auto; }
/* 状态栏外部修改提示：modified 可点击重载，missing 仅展示 */
.statusbar .st-ext { color: #d46b08; cursor: pointer; }
.statusbar .st-ext.missing { color: #cf1322; cursor: default; }
.t-item .star {
  width: 18px; height: 18px;
  display: inline-flex; align-items: center; justify-content: center;
  border-radius: 4px; color: var(--text-4); flex: 0 0 auto;
  opacity: 0;
}
.t-item:hover .star { opacity: 1; }
.t-item .star.on { opacity: 1; color: var(--gold); }
.t-item .star:hover { background: var(--border); color: var(--gold); }
.t-sub { padding-left: 28px; }
.t-sub2 { padding-left: 49px; }
.vault-row .fname { font-weight: 600; color: var(--text); }
.vault-row .icon-btn { width: 18px; height: 18px; }
/* 锁定态锁形图标：与解锁态的 .icon-btn 同框（18px）以保证两行右对齐，仅用橙色区分锁定状态 */
.vault-row .icon-btn.locked { color: #d48806; }
.vault-row .acts { flex: 0 0 auto; display: none; gap: 2px; align-items: center; }
.vault-row:hover .acts { display: flex; }
.vault-row .acts .icon-btn { width: 18px; height: 18px; }

.btn {
  display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  height: 32px; padding: 0 15px; font-size: 14px; border-radius: var(--radius-sm);
  border: 1px solid var(--border); background: var(--panel); color: var(--text);
  cursor: pointer; transition: all 0.2s; white-space: nowrap;
}
.btn:hover { color: var(--primary-hover); border-color: var(--primary-hover); }
.btn-primary { background: var(--primary); border-color: var(--primary); color: #fff; box-shadow: 0 2px 0 rgba(5, 145, 255, 0.1); }
.btn-primary:hover { background: var(--primary-hover); border-color: var(--primary-hover); color: #fff; }
.btn-danger { color: #cf1322; border-color: #cf1322; }
.btn-danger:hover { color: #fff; background: #cf1322; border-color: #cf1322; }
.btn-sm { height: 24px; padding: 0 8px; font-size: 12px; }

/* 另存为弹窗 */
.modal-foot { display: flex; justify-content: flex-end; gap: 8px; padding-top: 8px; }
.saveas { display: flex; flex-direction: column; gap: 14px; }
.saveas-seg { width: 100%; }
.saveas-seg span { flex: 1; text-align: center; }
.saveas-field { display: flex; flex-direction: column; gap: 6px; }
.saveas-field label { font-size: 13px; color: var(--text-2); }
.saveas-hint { font-size: 12px; color: var(--text-3); line-height: 1.6; }
.saveas-field select.input { width: 100%; }
.saveas-path { display: flex; gap: 8px; }
/* 转为加密文件：原明文文件处理询问弹窗 */
.convert-msg { font-size: 13.5px; color: var(--text); line-height: 1.6; }
.convert-path {
  font-size: 12px; color: var(--text-3); font-family: var(--mono);
  padding: 8px 10px; border-radius: var(--radius-sm);
  background: var(--hover); word-break: break-all;
}
.convert-remember { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--text-2); cursor: pointer; }
.saveas-path .input { flex: 1; font-family: var(--mono); font-size: 12.5px; }

/* 密码强度条 */
.strength { display: flex; gap: 4px; align-items: center; }
.strength i { height: 4px; flex: 1; border-radius: 2px; background: var(--border-2); }
.strength i.lv1 { background: var(--red); }
.strength i.lv2 { background: var(--gold); }
.strength i.lv3 { background: #a0d911; }
.strength i.lv4 { background: var(--green); }
.strength em { font-style: normal; font-size: 12px; color: var(--text-3); margin-left: 4px; }

/* 批量加密 / 导入弹窗专有样式 */
.be-cnt { color: var(--text-3); font-weight: normal; font-size: 12px; }
.be-list {
  max-height: 180px; overflow-y: auto;
  border: 1px solid var(--border-2); border-radius: 6px;
  padding: 6px 8px; background: var(--bg);
  display: flex; flex-direction: column; gap: 4px;
}
.be-list .empty-hint { padding: 10px; text-align: center; color: var(--text-3); font-size: 12px; }
.be-row { display: flex; align-items: center; gap: 8px; font-size: 12.5px; }
.be-path {
  flex: 1; min-width: 0;
  font-family: var(--mono);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  color: var(--text-1);
}
.be-del {
  flex: 0 0 20px; text-align: center; cursor: pointer;
  color: var(--text-3); border-radius: 4px; line-height: 18px;
}
.be-del:hover { color: var(--red); background: rgba(207, 19, 34, 0.08); }
.be-actions { display: flex; gap: 8px; }
.be-result {
  border: 1px solid var(--border-2); border-radius: 6px;
  padding: 8px 10px; background: var(--bg);
  font-size: 12.5px; line-height: 1.7; color: var(--text-1);
}
.be-result .be-failed { margin-top: 6px; color: var(--text-2); }
.be-result .be-failed ul { margin: 4px 0 0 18px; padding: 0; }
.be-result .be-failed li { font-size: 12px; }
.be-fpath { font-family: var(--mono); color: var(--text-2); }
label.radio {
  display: flex; align-items: center; gap: 6px;
  font-size: 12.5px; color: var(--text-1); cursor: pointer;
}
label.radio input[type="radio"] { margin: 0; }

/* 编辑区 */
.workbench { flex: 1; display: flex; flex-direction: column; min-width: 0; min-height: 0; }
.toolbar {
  flex: 0 0 40px;
  display: flex; align-items: center; gap: 2px;
  padding: 0 12px;
  border-bottom: 1px solid var(--border-2);
  overflow-x: auto;
}
.toolbar .icon-btn { flex: 0 0 auto; }
.toolbar .icon-btn.on { background: var(--primary-bg); color: var(--primary-active); }
.tb-sep { width: 1px; height: 18px; background: var(--border); margin: 0 6px; flex: 0 0 auto; }
.tb-spacer { flex: 1; }

.panes { flex: 1; display: flex; min-height: 0; overflow: hidden; }
.pane { flex: 1 1 0%; min-width: 0; overflow-y: auto; }
.pane-editor { border-right: 1px solid var(--border-2); display: flex; }
/* 非激活页签的编辑器实例：保留在 DOM 中（撤销历史不丢），仅视觉隐藏 */
.pane-editor .cm-tab-instance.cm-hidden { display: none; }
/* 收起态：宽度滑到 0 后禁止绘制内容与滚动条，并去掉分隔边框避免在边缘留一条竖线 */
.pane.collapsed {
  overflow: hidden;
  border-right-color: transparent;
}

/* 编辑 / 分屏 / 预览 切换滑动动画：用 flex-basis 过渡让面板从边上滑入滑出，
   过渡常驻（仅拖拽时关闭）；每个模式状态都显式声明两侧面板的 flex-basis，
   保证 编辑↔分屏、分屏↔预览、编辑↔预览 任意方向的切换都产生过渡 */
.panes .pane {
  transition: flex-basis 0.24s cubic-bezier(0.45, 0, 0.2, 1);
}
.panes .split-resize {
  transition: flex-basis 0.24s cubic-bezier(0.45, 0, 0.2, 1), background 0.15s;
}
.panes.no-anim .pane,
.panes.no-anim .split-resize {
  transition: none;
}
/* 编辑：编辑器满宽、预览收起 */
.panes.mode-edit .pane-editor {
  flex: 1 0 100%;
}
.panes.mode-edit .pane-preview {
  flex: 1 0 0%;
}
/* 预览：预览满宽、编辑器收起 */
.panes.mode-preview .pane-editor {
  flex: 1 0 0%;
}
.panes.mode-preview .pane-preview {
  flex: 1 0 100%;
}
/* 分屏：两侧按 splitRatio 占比（由内联样式给出，此处仅把 grow 归零避免中间帧溢出） */
.panes.mode-split .pane {
  flex-grow: 0;
}
.empty-hint {
  padding: 14px 12px;
  font-size: 12.5px;
  color: var(--text-4);
  text-align: center;
  cursor: pointer;
}
.empty-hint:hover { color: var(--primary); }

/* 侧边栏空态引导：图标 + 说明 + 两个入口按钮（淡边框描边），居中占位 */
.sb-empty {
  display: flex; flex-direction: column; align-items: center; gap: 6px;
  padding: 48px 16px 0; color: var(--text-4); text-align: center;
}
.sb-empty-title { font-size: 13px; font-weight: 600; color: var(--text-3); }
.sb-empty-desc { font-size: 12px; line-height: 1.6; }
.sb-empty-btn {
  margin-top: 6px; width: 100%; max-width: 180px;
  display: flex; align-items: center; justify-content: center; gap: 8px;
  padding: 6px 10px; border: 1px solid var(--border-pop); border-radius: 8px;
  background: var(--bg); font-size: 12.5px; color: var(--text-2); cursor: pointer;
  transition: border-color 0.15s, color 0.15s;
}
.sb-empty-btn:hover { border-color: var(--primary); color: var(--primary); }

.pane-preview { background: var(--panel); }
/* 全局搜索命中在预览区的临时高亮（由 _markPreviewHit 注入）：用琥珀色底而非浏览器默认的
   黄底黑字，保证亮/暗主题下都不抢正文颜色；首次出现闪一下以便定位后一眼看到 */
.pane-preview .md :deep(mark.pv-hit) {
  background: rgba(250, 173, 20, 0.3);
  color: inherit;
  border-radius: 2px;
  padding: 0 1px;
  box-shadow: 0 0 0 1px rgba(250, 173, 20, 0.45);
  animation: pv-hit-in 0.9s ease-out;
}
html[data-theme='dark'] .pane-preview .md :deep(mark.pv-hit) {
  background: rgba(250, 173, 20, 0.24);
  box-shadow: 0 0 0 1px rgba(250, 173, 20, 0.36);
}
@keyframes pv-hit-in {
  from { background: rgba(250, 173, 20, 0.72); }
  to { background: rgba(250, 173, 20, 0.3); }
}
.split-resize {
  flex: 0 0 4px;
  cursor: col-resize;
  background: var(--panel);
}
/* 非分屏模式下拖拽手柄随面板一起收缩滑出（flex-basis 可过渡，width 不可） */
.panes.mode-edit .split-resize,
.panes.mode-preview .split-resize {
  flex-basis: 0;
}
.split-resize:hover,
.split-resize:active {
  background: var(--primary);
}

/* 锁定占位 */
.pane-locked { display: flex; align-items: center; justify-content: center; background: var(--panel); flex: 1; }
.locked-inner { text-align: center; max-width: 380px; padding-bottom: 40px; }
.lk-ico {
  width: 64px; height: 64px; margin: 0 auto 18px;
  border-radius: 16px;
  background: var(--gold-bg);
  border: 1px solid #ffe58f;
  display: flex; align-items: center; justify-content: center;
}
.lk-t { font-size: 16px; font-weight: 600; }
.lk-d { font-size: 13px; color: var(--text-3); margin: 8px 0 20px; line-height: 1.7; }
.lk-actions { display: flex; gap: 8px; justify-content: center; flex-wrap: wrap; }
.lk-hint { font-size: 12px; color: var(--text-4); margin-top: 14px; }
.lk-err { font-size: 12.5px; color: #cf1322; margin-top: 8px; }
.unlock-row { display: flex; gap: 8px; }
.unlock-row .input { flex: 1; }

/* 预览 markdown：限定最大宽时靠 margin 居中于画布（选「铺满」时 max-width 为 none，margin 自然无效果） */
.md { padding: 26px 34px 60px; max-width: var(--editor-max-width, 1280px); margin: 0 auto; }
.md h1 { font-size: 24px; margin: 4px 0 14px; padding-bottom: 8px; border-bottom: 1px solid var(--border-2); }
.md h2 { font-size: 18px; margin: 22px 0 10px; }
.md p { margin: 10px 0; color: var(--text); }
.md ul, .md ol { margin: 10px 0 10px 22px; }
.md li { margin: 4px 0; }
.md blockquote {
  margin: 12px 0; padding: 8px 14px;
  border-left: 3px solid var(--primary);
  background: var(--primary-bg);
  color: var(--text-2);
  border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
  font-size: 13.5px;
}
.md code { font-family: var(--mono); font-size: 12.5px; background: var(--hover); color: #c2185b; padding: 1px 6px; border-radius: 4px; }
.md pre {
  margin: 12px 0; padding: 14px 16px;
  background: var(--bg); border: 1px solid var(--border-2);
  border-radius: var(--radius-sm);
  font-family: var(--mono); font-size: 12.5px; line-height: 1.7;
  overflow-x: auto;
}
.md pre code { background: none; color: #531dab; padding: 0; }
.md table { border-collapse: collapse; margin: 12px 0; font-size: 13.5px; }
.md th, .md td { border: 1px solid var(--border); padding: 6px 14px; }
.md th { background: var(--hover); font-weight: 600; }
.md .chk { color: var(--green); font-weight: 700; }
.md .unchk { color: var(--text-4); }

/* 状态栏（位于 right-col 底部，仅占侧边栏右侧） */
.statusbar {
  flex: 0 0 28px;
  display: flex; align-items: center; gap: 16px;
  padding: 0 14px;
  font-size: 12px; color: var(--text-3);
  background: var(--panel);
  border-top: 1px solid var(--border-2);
  user-select: none;
}
.statusbar .st { display: inline-flex; align-items: center; gap: 5px; }
/* 状态栏可点击弹菜单的项（字号 / 自动锁定）：与 .st-ext 一样给出手型与悬停反馈 */
.statusbar .st-menu { cursor: pointer; }
.statusbar .st-menu:hover { color: var(--primary); }
.statusbar .right { margin-left: auto; display: flex; align-items: center; gap: 16px; }

/* 输入框（侧边栏搜索） */
.input {
  width: 100%; height: 30px; padding: 4px 11px; font-size: 14px;
  border: 1px solid var(--border); border-radius: var(--radius-sm);
  background: var(--panel); color: var(--text); outline: none; transition: all 0.2s;
}
.input::placeholder { color: var(--text-4); }
.input:hover { border-color: var(--primary-hover); }
.input:focus { border-color: var(--primary); box-shadow: 0 0 0 2px rgba(5, 145, 255, 0.1); }
</style>
