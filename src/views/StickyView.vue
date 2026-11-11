<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import { emitTo, listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { PhysicalSize } from '@tauri-apps/api/dpi'
import { useVaultStore } from '../stores/vault'
import type { QuickLaunchItem, StickyNoteTarget } from '../stores/vault'
import * as tauri from '../lib/tauri'
import { readNote, writeNote } from '../lib/sticky'
import { renderMarkdown, setAssetBaseDir } from '../lib/preview'
import { domToMarkdown, preserveTrailing, enterInTaskItem } from '../lib/wysiwyg'
import { lang } from '../locales'
import { isMac } from '../lib/platform'
import { appIcons, ensureAppIcons } from '../lib/appinfo'
import { splitLaunchArgs } from '../lib/launchArgs'
import appIcon from '../assets/about-icon.png'
import CodeMirrorEditor from '../components/CodeMirrorEditor.vue'
import MarkdownPreview from '../components/MarkdownPreview.vue'

/**
 * 便携便签视图：运行在 Rust 标签为 `sticky` 的独立置顶小窗里（路由守卫保证只在此窗出现）。
 * 单一文件、轻量交互：编辑 / 分屏 / 预览三态 + TODO 点勾 + 底部快速启动；
 * Esc 或窗口失焦即保存并隐藏（窗口常驻不销毁，下次弹出瞬时）。
 */
export default defineComponent({
  name: 'StickyView',
  components: { CodeMirrorEditor, MarkdownPreview },
  data() {
    return {
      content: '',
      dirty: false,
      loading: false,
      needUnlock: false,
      // 读取失败且与「库未解锁」无关（明文笔记 IO 失败等）：展示原因 + 重试，
      // 不能落进解锁表单（明文目标无 vaultId，解锁按钮按下也无事发生）
      loadErr: '',
      picking: false,
      syncingScroll: false,
      saving: false,
      // 便签窗是否获得焦点 + 鼠标是否在正文（编辑器/预览）区内：两者共同决定模式切换段是否浮现
      focused: false,
      overBody: false,
      // 自动换行（与主编辑器同名同义）：便签窗窄，恒开，不再给开关
      wordWrap: true,
      // ---------- 预览就地编辑（与主编辑器同一套语义，见 EditorView 的同名成员） ----------
      // 注入预览的 HTML：编辑期间冻结，任何一次赋值都会重建 DOM、清掉光标
      previewHtml: '',
      // DOM 由用户持有、尚未序列化回源码
      previewDirty: false,
      previewFrozen: false,
      // 自己回写后的源码快照：用于区分「改动来自就地编辑」与「磁盘/外部写入」
      previewSelfContent: null as string | null,
      previewComposing: false,
      _previewFlushTimer: 0 as any,
      // 便签自己的只读 ⇄ 编辑开关：不写回全局设置项（两窗各持一份 store，互相覆盖会打架），
      // 与 wordWrap 一样是会话内状态
      pvEdit: true,
      // 主应用图标（与关于对话框同源）：标题栏「打开 MarkLock」按钮用
      appIcon,
      // 预览模式勾选任务清单的确认弹窗（与主编辑器同语义）：pendingTaskSrc 为改好勾选的源码，
      // text/checked 仅供展示（哪条任务、要勾上还是取消）
      showTaskConfirm: false,
      pendingTaskSrc: '',
      pendingTaskText: '',
      pendingTaskChecked: false,
      pwd: '',
      unlockErr: false,
      // macOS 原生可拖边缩放且 tao 不支持 drag_resize_window，手柄仅在 Windows/Linux 显示
      showResizeGrip: !isMac,
    }
  },
  computed: {
    store() {
      return useVaultStore()
    },
    target(): StickyNoteTarget | null {
      return this.store.settings.stickyNote
    },
    quickLaunch() {
      return this.store.settings.quickLaunch
    },
    /** 快速启动项是否显示文字（设置页「启动项显示」：仅图标 / 图标及文字）。 */
    launchShowText(): boolean {
      return this.store.settings.stickyLaunchDisplay === 'iconText'
    },
    /** 视图模式读写到 store，跨会话记忆（编辑 / 分屏 / 预览）。 */
    mode: {
      get(): 'edit' | 'split' | 'preview' {
        return this.store.settings.stickyMode
      },
      set(v: 'edit' | 'split' | 'preview') {
        this.store.updateSettings({ stickyMode: v })
      },
    },
    /** 便签文件名（标题栏展示） */
    fileName(): string {
      if (!this.target) return ''
      const base = this.target.kind === 'mdlb' && this.target.rel ? this.target.rel : this.target.path
      return base.split(/[\\/]/).pop() || base
    },
    /** 相对图片解析基准目录 */
    baseDir(): string {
      if (!this.target) return ''
      return this.target.path.replace(/[\\/][^\\/]*$/, '')
    },
    /** 快速启动图标的内存缓存（path → data URL），由 lib/appinfo 异步填充。 */
    iconMap(): Record<string, string> {
      return appIcons
    },
    /** 标题栏「锁定」按钮是否显示：便签指向加密文件（.mdl / .mdlb）且当前已成功解密读取时。 */
    canLock(): boolean {
      const t = this.target
      return !!t && t.kind !== 'plain' && !this.needUnlock && !this.loading
    },
    /** 渲染结果：基准目录取便签文件自己在磁盘上的目录（与主编辑器同规则，不接附件缓存）。 */
    renderedPreview(): string {
      void lang.value
      setAssetBaseDir(this.baseDir)
      return renderMarkdown(this.content)
    },
    /** 能不能就地编辑：纯预览态 + 正文已读到（解锁表单 / 读取失败态没有正文可改）。
     *  分屏不开——与主编辑器同口径，两栏同屏时改渲染区会让光标在栏间跳。 */
    pvCanEdit(): boolean {
      return this.mode === 'preview' && !!this.target && !this.needUnlock && !this.loadErr && !this.loading
    },
    /** 预览区当前是否真的可打字 = 能开 且 这颗开关开着（便签自己的会话内状态） */
    previewEditable(): boolean {
      return this.pvCanEdit && this.pvEdit
    },
  },
  watch: {
    // 渲染结果注入预览：编辑期间 DOM 才是真源，除了自己回写源码触发的那一次重渲染，
    // 其它来源（弹出时重新读盘、外部改动、任务清单确认写回）都必须解冻重注入，
    // 否则用户对着旧 DOM 打字会把刚落盘的改动盖掉。
    renderedPreview: {
      immediate: true,
      handler(v: string) {
        if (this.previewFrozen && this.content === this.previewSelfContent) return
        this.previewFrozen = false
        this.previewDirty = false
        this.previewHtml = v
      },
    },
    // 离开编辑态（切出预览 / 收起便签前关开关）时把 DOM 序列化回源码；
    // 进入编辑态只需告诉浏览器回车要分段成 <p>（WebKit 默认插 <div>，会被当成原始 HTML 整块保留）
    previewEditable(v: boolean, old: boolean) {
      if (v && !old) {
        try {
          document.execCommand('defaultParagraphSeparator', false, 'p')
        } catch {}
        return
      }
      if (!v && old) this.commitPreview()
    },
    // 快速启动列表变化（含弹出时 reloadSettings 拉到主窗口新增项）时预取图标；已缓存的路径会自行跳过。
    // immediate：首帧就拉一次，不等列表变化（图标到位前不显示图占位）。
    quickLaunch: {
      immediate: true,
      handler(list: { path: string }[]) {
        ensureAppIcons(list.map((i) => i.path))
      },
    },
  },
  async mounted() {
    await this.load()
    // 每次被弹出（托盘左键 / 热角 / 命令）都重新读取，覆盖隐藏期间的外部改动
    listen('marklock://sticky-show', async () => {
      await this.save(true)
      // 先按旧目标落盘编辑内容，再从 localStorage 刷新设置：主窗口改的快速启动 / 便签目标等即时生效
      this.store.reloadSettings()
      await this.load()
    }).catch(() => {})
    // 托盘菜单键点击：后端已立即隐藏窗口（避免盖住菜单），这里补一次静默落盘，未保存内容不丢
    listen('marklock://sticky-hide', () => {
      this.save(true)
    }).catch(() => {})
    window.addEventListener('keydown', this.onKey)
    window.addEventListener('blur', this.onBlur)
    // 初始焦点态：弹出时若已获焦则 focused=true（热角路径不夺焦，靠 focus 事件补后续切换）。
    this.focused = document.hasFocus()
    window.addEventListener('focus', this.onFocus)
    // 用户点进便签（pointerdown）才视为「要用它」：解除后端「移开即隐藏」，此后交回 blur/Esc 收起。
    // 仅 pointerdown 触发 stickyEngage：热角路径 win.show() 不夺焦，但 Windows 上系统可能自动激活
    // 窗口，此时用户并未真正交互，不应解除 AUTOHIDE（否则「移开即隐藏」失效）。
    window.addEventListener('pointerdown', this.onEngage)
  },
  beforeUnmount() {
    window.removeEventListener('keydown', this.onKey)
    window.removeEventListener('blur', this.onBlur)
    window.removeEventListener('focus', this.onFocus)
    window.removeEventListener('pointerdown', this.onEngage)
  },
  methods: {
    async load() {
      const t = this.target
      if (!t) {
        this.content = ''
        this.needUnlock = false
        this.loadErr = ''
        return
      }
      this.loading = true
      try {
        this.content = await readNote(t)
        this.dirty = false
        this.needUnlock = false
        this.loadErr = ''
      } catch (e) {
        // 读失败最常见原因是库未解锁：交由解锁表单处理。
        // 其余失败（明文笔记 IO 失败、库文件被移动等）展示原因 + 重试：
        // 明文目标根本没有库可解，误显示密码表单会卡死在无意义的解锁页
        const msg = String(e)
        if (t.kind !== 'plain' && msg.includes('尚未解锁')) {
          this.needUnlock = true
          this.loadErr = ''
        } else {
          this.needUnlock = false
          this.loadErr = msg || this.$t('sticky.loadFail')
          this.content = ''
          this.dirty = false
        }
      } finally {
        this.loading = false
      }
    },
    async doUnlock() {
      const t = this.target
      if (!t || !t.vaultId) return
      this.unlockErr = false
      try {
        if (t.kind === 'mdlb') await tauri.unlockFileVault(t.vaultId, t.path, this.pwd)
        else await tauri.unlock(t.vaultId, t.path, this.pwd)
        this.pwd = ''
        await this.load()
      } catch {
        this.unlockErr = true
      }
    },
    /** 锁定便签所在的加密库：先静默落盘避免丢数据，再清零内存密钥并回到就地解锁表单。 */
    async lockNote() {
      const t = this.target
      const vaultId = t?.vaultId
      if (!vaultId) return
      // 锁定前先静默落盘（与 hide 收起同语义），未保存内容不随密钥清零丢失
      await this.save(true)
      try {
        await tauri.lock(vaultId)
      } catch (e) {
        message.error(String(e))
        return
      }
      // 回到锁定态：清空已解密的明文正文，显示就地解锁表单（下次弹出 / 输入密码重新解密）
      this.content = ''
      this.dirty = false
      this.pwd = ''
      this.unlockErr = false
      this.needUnlock = true
      this.loadErr = ''
      message.info(this.$t('toast.locked'))
    },
    onInput(v: string) {
      // 内容未变则不记脏：外部修改重载后 CodeMirror 会把新 doc 原样回吐一次，
      // 若不过滤会把刚加载的磁盘版本误标为「未保存」（与主编辑器 setContentFor 同语义）。
      if (this.content === v) return
      this.content = v
      this.dirty = true
    },

    // ---------- 预览就地编辑（与主编辑器同一套语义） ----------
    /** contenteditable 的那块 `.md` 是 MarkdownPreview 的根元素（单根组件，直接取 $el）。 */
    previewMdEl(): HTMLElement | null {
      const el = (this.$refs.previewMd as unknown as { $el?: Element } | null)?.$el
      return el instanceof HTMLElement ? el : null
    },
    /** 编辑期每次输入后防抖回写：只更新正文（记脏、供保存），不动 DOM，光标不受影响。 */
    onPreviewEditInput() {
      if (!this.previewEditable || this.previewComposing) return
      this.previewFrozen = true
      this.previewDirty = true
      if (this._previewFlushTimer) clearTimeout(this._previewFlushTimer)
      this._previewFlushTimer = setTimeout(() => {
        this._previewFlushTimer = 0
        this.flushPreviewDom()
      }, 220)
    },
    /** 把 DOM 序列化回源码，保持冻结（DOM 仍是真源）：保存、收起、锁定、重新读盘前调用。 */
    flushPreviewDom() {
      if (this._previewFlushTimer) {
        clearTimeout(this._previewFlushTimer)
        this._previewFlushTimer = 0
      }
      if (!this.previewDirty) return
      this.previewDirty = false
      const el = this.previewMdEl()
      if (!el) return
      const md = preserveTrailing(this.content, domToMarkdown(el))
      if (md === this.content) return
      this.previewSelfContent = md
      this.content = md
      this.dirty = true
    },
    /** 结束编辑态：先回写，再解冻并注入渲染结果（未回写的改动会被源码覆盖，与「源码为准」一致）。 */
    commitPreview() {
      this.flushPreviewDom()
      this.previewFrozen = false
      this.previewSelfContent = null
      this.previewHtml = this.renderedPreview
    },
    onPreviewCompositionStart() {
      this.previewComposing = true
    },
    /** 中文输入法上屏后才回写：合成过程中 DOM 里是未确定的拼音串，写进源码就是脏数据。 */
    onPreviewCompositionEnd() {
      this.previewComposing = false
      this.onPreviewEditInput()
    },
    /** 粘贴一律降级为纯文本：富文本会带进 HTML 标签，反解析只能原样保留，源码会被污染。 */
    onPreviewEditPaste(e: ClipboardEvent) {
      if (!this.previewEditable) return
      e.preventDefault()
      const text = e.clipboardData?.getData('text/plain') ?? ''
      if (!text) return
      document.execCommand('insertText', false, text)
      this.onPreviewEditInput()
    },
    /** 代码块内 Enter 必须插入裸换行：浏览器在 `<pre>` 里插 `<div>`，`textContent` 会丢掉换行，
     *  回写后两行代码被并成一行。其余块交给原生 insertParagraph 分段。 */
    onPreviewEditKeydown(e: KeyboardEvent) {
      if (!this.previewEditable) return
      // 便签是整篇渲染、没有主编辑器那套 ⌘S 之外的键位冲突：Ctrl/Cmd+S 仍交给窗口级 onKey 落盘
      if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return
      const node = window.getSelection()?.anchorNode?.parentElement ?? null
      if (node?.closest('pre')) {
        e.preventDefault()
        document.execCommand('insertText', false, '\n')
        this.onPreviewEditInput()
        return
      }
      const root = this.previewMdEl()
      if (root && enterInTaskItem(e, root)) this.onPreviewEditInput()
    },
    /** 只读 ⇄ 编辑开关（便签自己的会话内状态，不写全局设置项）。 */
    togglePvEdit() {
      this.pvEdit = !this.pvEdit
    },
    onTaskToggle(payload: { next: string; text: string; checked: boolean }) {
      // 预览模式：弹确认（与主编辑器一致），确认后才写回并保存
      if (this.mode === 'preview') {
        this.pendingTaskSrc = payload.next
        const raw = payload.text
        this.pendingTaskText = raw.length > 40 ? raw.slice(0, 40) + '…' : raw
        this.pendingTaskChecked = payload.checked
        this.showTaskConfirm = true
        return
      }
      // 分屏模式：直接翻转源码标记并静默落盘（属行内快改，不弹 toast）
      this.content = payload.next
      this.dirty = true
      this.save(true)
    },
    /** 预览模式勾选确认弹窗的「修改并保存」：写回正文后立即落盘（显式操作，弹成功提示）。 */
    async submitTaskConfirm() {
      this.showTaskConfirm = false
      this.content = this.pendingTaskSrc
      this.dirty = true
      await this.save()
    },
    async save(silent = false) {
      const t = this.target
      // 预览里未防抖回写的就地编辑先落到正文：否则写盘的是旧源码，DOM 里刚打的字丢失
      this.flushPreviewDom()
      // saving 互斥：⌘S 会同时命中窗口级 keydown 与 CodeMirror 内部 Mod-s keymap，
      // 两者并发调用本方法时 dirty 尚未在 await 后置位，会各弹一次提示；用 in-flight 标志只放行一次。
      if (!t || !this.dirty || this.needUnlock || this.saving) return
      this.saving = true
      try {
        await writeNote(t, this.content)
        this.dirty = false
        if (!silent) message.success(this.$t(t.kind === 'mdl' ? 'toast.savedEnc' : 'toast.saved'))
      } catch (e) {
        message.error(String(e))
      } finally {
        this.saving = false
      }
    },
    async hide() {
      await this.save(true)
      tauri.hideStickyNote().catch(() => {})
    },
    onKey(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        // 编辑器持有焦点（光标在正文里、正在打字）时 Esc 不做操作，避免误触收起；
        // 焦点不在编辑器（预览态、或编辑态但焦点已移开）时，Esc 照常保存并收起。
        const cm = this.$refs.editorCm as unknown as { isFocused?: () => boolean } | null
        if (cm?.isFocused?.()) return
        e.preventDefault()
        this.hide()
        return
      }
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') {
        e.preventDefault()
        this.save()
        return
      }
      // Cmd/ Ctrl + E：循环切换 编辑 → 分屏 → 预览 → 编辑（与主编辑器同键同语义）
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && (e.key === 'e' || e.key === 'E')) {
        e.preventDefault()
        const cycle: Record<string, 'edit' | 'split' | 'preview'> = { edit: 'split', split: 'preview', preview: 'edit' }
        this.mode = cycle[this.mode] || 'split'
      }
    },
    /** 双击预览区在分屏 / 预览间切换（受设置项 previewDblClickToggle 控制，与主编辑器一致）：
     *  落在代码块复制按钮 / 任务清单复选框上时跳过，避免误切视图。
     *  就地编辑开启时让位给浏览器的双击选词——预览已是编辑面，双击应当选中单词而不是换视图。 */
    onPreviewDblClick(e: MouseEvent) {
      if (this.previewEditable) return
      if (!this.store.settings.previewDblClickToggle) return
      const t = e.target as HTMLElement | null
      if (t?.closest('.code-copy-btn')) return
      if (t?.closest('input.task-cb')) return
      this.mode = this.mode === 'split' ? 'preview' : 'split'
    },
    /** 分屏比例滚动同步：编辑器滚动 → 预览按相同百分比滚动。
     *  （便签预览用整篇渲染、无源码块 data-l 锚点，故采用比例近似同步，与主编辑器的行锚点同步不同） */
    onEditorScroll() {
      if (this.syncingScroll || this.mode !== 'split') return
      this.syncingScroll = true
      const cm = this.$refs.editorCm as unknown as { getScrollRatio?: () => number } | null
      const pane = this.$refs.previewPane as HTMLElement | null
      if (cm?.getScrollRatio && pane) {
        const max = pane.scrollHeight - pane.clientHeight
        if (max > 0) pane.scrollTop = Math.max(0, Math.min(cm.getScrollRatio() * max, max))
      }
      setTimeout(() => { this.syncingScroll = false }, 60)
    },
    /** 预览区滚动 → 编辑器按相同百分比滚动（反向同步）。 */
    onPreviewScroll() {
      if (this.syncingScroll || this.mode !== 'split') return
      this.syncingScroll = true
      const cm = this.$refs.editorCm as unknown as { scrollToRatio?: (r: number) => void } | null
      const pane = this.$refs.previewPane as HTMLElement | null
      if (cm?.scrollToRatio && pane) {
        const max = pane.scrollHeight - pane.clientHeight
        if (max > 0) cm.scrollToRatio(Math.max(0, Math.min(pane.scrollTop / max, 1)))
      }
      setTimeout(() => { this.syncingScroll = false }, 60)
    },
    async onBlur() {
      this.focused = false
      // 打开系统文件选择框会让本窗短暂失焦，此时不能收起（否则选完文件回来窗已隐藏）。
      if (this.picking) return
      // WebView 失焦 ≠ 窗口真正失去激活：窗口进入某些系统交互（如 Windows 的
      // 系统 sizing 循环）时会先切换一次焦点，WebView2 随即触发 blur，但窗口仍是前台窗口。
      // 若据此直接收起，就会出现「按住缩放手柄一移动鼠标便签就消失」。
      // 故以系统前台窗口为准，只在窗口真被别的窗口或桌面抢走激活时才收起
      //（macOS 无缩放手柄，保持失焦即收起的原行为）。
      if (!isMac) {
        const active = await tauri.stickyIsActive().catch(() => false)
        if (active) return
      }
      // 失焦即收起：先落盘再隐藏，保证便签内容不丢
      this.hide()
    },
    /**
     * 右下角缩放手柄：按住拖动调整窗口大小。
     *
     * 不使用系统原生缩放（startResizeDragging）：Windows 上它是 ReleaseCapture +
     * WM_NCLBUTTONDOWN 进入系统 sizing 循环，开始瞬间会切换一次焦点、WebView2 触发 blur，
     * 与「失焦即收起」冲突（表现为按住手柄一移动鼠标便签就消失）。
     * 这里改用 pointer capture + setSize 自行跟手：不进入原生循环、不产生焦点切换，
     * 鼠标移出窗口后仍能继续收到 pointermove。
     */
    async startResize(e: PointerEvent) {
      if (e.button !== 0) return
      e.preventDefault()
      const grip = e.currentTarget as HTMLElement
      const win = getCurrentWindow()
      // screenX/Y 是 CSS 像素，窗口尺寸是物理像素：用缩放比换算，避免高 DPI 下拖动幅度不对
      const scale = await win.scaleFactor().catch(() => window.devicePixelRatio || 1)
      const inner = await win.innerSize().catch(() => null)
      if (!inner) return
      try {
        // 指针捕获：拖出窗口范围后仍持续收到 pointermove
        grip.setPointerCapture(e.pointerId)
      } catch {}
      const startX = e.screenX
      const startY = e.screenY
      const startW = inner.width
      const startH = inner.height
      // 与建窗时 min_inner_size(300, 220) 对齐（换算为物理像素）
      const minW = 300 * scale
      const minH = 220 * scale
      let raf = 0
      let nextW = startW
      let nextH = startH
      const flush = () => {
        raf = 0
        win.setSize(new PhysicalSize(Math.round(nextW), Math.round(nextH))).catch(() => {})
      }
      const onMove = (ev: PointerEvent) => {
        nextW = Math.max(minW, startW + (ev.screenX - startX) * scale)
        nextH = Math.max(minH, startH + (ev.screenY - startY) * scale)
        // 每帧最多下发一次，避免 pointermove 高频调用 IPC
        if (!raf) raf = requestAnimationFrame(flush)
      }
      const onEnd = () => {
        grip.removeEventListener('pointermove', onMove)
        grip.removeEventListener('pointerup', onEnd)
        grip.removeEventListener('pointercancel', onEnd)
        if (raf) {
          cancelAnimationFrame(raf)
          flush()
        }
        try {
          grip.releasePointerCapture(e.pointerId)
        } catch {}
      }
      grip.addEventListener('pointermove', onMove)
      grip.addEventListener('pointerup', onEnd)
      grip.addEventListener('pointercancel', onEnd)
    },
    /** 未设置文件时直接在此选源文件：与设置页同规则（明文 md / 加密 mdl，.mdlb 容器除外）。 */
    async pickFile() {
      if (this.picking) return
      this.picking = true
      try {
        const path = await tauri.openStickyFileDialog()
        if (!path) return
        if (/\.mdlb$/i.test(path)) {
          message.warning(this.$t('settings.sticky.noMdlb'))
          return
        }
        const kind = /\.mdl$/i.test(path) ? 'mdl' : 'plain'
        this.store.updateSettings({
          stickyNote: { vaultId: kind === 'mdl' ? path : null, path, rel: null, kind },
        })
        await this.load()
      } finally {
        this.picking = false
      }
    },
    /** 窗口获得焦点：仅更新焦点态（模式段浮现条件之一），不解除后端 AUTOHIDE。 */
    onFocus() {
      this.focused = true
    },
    /** 用户真正点进便签（pointerdown）：解除后端 AUTOHIDE，此后由 blur/Esc/关闭按钮收起。 */
    onEngage() {
      tauri.stickyEngage().catch(() => {})
    },
    /** 打开主窗口设置页：由 Rust 唤回主窗（含托盘隐藏态）并通知跳转。 */
    openSettings() {
      tauri.openSettings().catch(() => {})
    },
    /** 打开 MarkLock：由 Rust 唤回主窗（取消最小化 / 托盘隐藏态后显示并聚焦）。 */
    openMain() {
      tauri.showMainWindow().catch(() => {})
    },
    /** 在主界面中打开这篇便签：先静默落盘（否则主窗从磁盘读到的是旧内容，两边还会互相覆盖），
     *  再由 Rust 唤回主窗，最后把目标原样发给主窗——便签是独立 webview，改不了主窗路由，只能靠事件递。 */
    async openNoteInMain() {
      const t = this.target
      if (!t) return
      await this.save(true)
      tauri.showMainWindow().catch(() => {})
      try {
        await emitTo('main', 'marklock://sticky-open-note', t)
      } catch {
        // 主窗不存在时事件发不出去：窗已经唤回了，用户自己点开即可，不额外报错
      }
    },
    /** 快速启动：把参数原始串按引号感知拆分后交给 Rust（macOS 经 open --args 透传，带参时后端自动 open -n 另起进程）。 */
    launch(item: QuickLaunchItem) {
      tauri.launchLocal(item.path, splitLaunchArgs(item.args)).catch((e) => message.error(String(e)))
    },
  },
})
</script>

<template>
  <div class="sticky">
    <!-- 右下角缩放热区：不画图标也不带提示气泡（便签不固定从某个角落弹出，角落标记显得突兀），
         只保留一块不可见拖拽区，鼠标移上去变斜向双箭头提示可调大小（见 startResize） -->
    <div v-if="showResizeGrip" class="resize-grip" @pointerdown="startResize"></div>
    <!-- 自绘标题栏：整条可拖拽移动窗口（无边框窗靠它移动）；设置 / 关闭集中在右上角 -->
    <div class="sticky-bar" data-tauri-drag-region>
      <span class="grip" aria-hidden="true">⠿</span>
      <span class="title" data-tauri-drag-region>{{ fileName || $t('sticky.title') }}</span>
      <span v-if="dirty" class="dot" :title="$t('sticky.unsaved')"></span>
      <button class="x lock-btn" v-if="canLock" v-tip="{ text: $t('sticky.lock'), place: 'bottom' }" @click="lockNote"><svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="4.5" y="10.5" width="15" height="10" rx="2" /><path d="M8 10.5V7a4 4 0 0 1 8 0v3.5" /></svg></button>
      <button class="x main-btn" v-tip="{ text: $t('sticky.main'), place: 'bottom' }" @click="openMain"><img :src="appIcon" alt="" /><span>{{ $t('sticky.mainBtn') }}</span></button>
      <button class="x to-main" v-tip="{ text: $t('sticky.openInMain'), place: 'bottom' }" @click="openNoteInMain"><svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 4h6v6" /><path d="M20 4l-8.5 8.5" /><path d="M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4" /></svg></button>
      <button class="x" v-tip="{ text: $t('sticky.settings'), place: 'bottom' }" @click="openSettings">⚙</button>
      <button class="x" v-tip="{ text: $t('sticky.close'), place: 'bottom' }" @click="hide">×</button>
    </div>

    <div class="sticky-body" @mouseenter="overBody = true" @mouseleave="overBody = false">
      <!-- 未设置便签文件：可直接在此选文件 -->
      <div v-if="!target" class="empty">
        <p>{{ $t('sticky.notSet') }}</p>
        <button class="primary" @click="pickFile">{{ $t('sticky.chooseFile') }}</button>
        <p class="hint">{{ $t('sticky.notSetHint') }}</p>
      </div>

      <!-- 库已锁定：就地解锁（本次会话保持） -->
      <div v-else-if="needUnlock" class="unlock">
        <p>{{ $t('sticky.locked') }}</p>
        <input
          v-model="pwd"
          type="password"
          :placeholder="$t('sticky.pwdPh')"
          @keyup.enter="doUnlock"
        />
        <p v-if="unlockErr" class="err">{{ $t('sticky.unlockFail') }}</p>
        <button class="primary" @click="doUnlock">{{ $t('sticky.unlock') }}</button>
      </div>

      <!-- 读取失败（与解锁无关）：展示后端原因 + 重试按钮 -->
      <div v-else-if="loadErr" class="unlock">
        <p class="err">{{ loadErr }}</p>
        <button class="primary" @click="load">{{ $t('sticky.retry') }}</button>
      </div>

      <!-- 正常三态：面板常驻布局流 + flex-basis 滑动过渡（对齐主编辑器 .panes 切换动画；不用 v-show，
           因 WKWebView 下 display 切换会中断 flex-basis 过渡） -->
      <template v-else>
        <!-- 顶部浮层只留两样东西（同一行、靠右）：只读 ⇄ 编辑那颗 + 模式切换段。
             整条格式化工具栏在 400 逻辑点的便签里放不下，而且它改的是源码 / 渲染 DOM 这类主界面才需要的操作，
             便签里要改格式直接进编辑态打字即可，故不再摆一排水印按钮。
             浮现规则不变：获得焦点且鼠标在正文内才出现，平时半透明、悬停转不透明；容器不吃鼠标。 -->
        <div class="sticky-float" v-show="focused && overBody">
          <button v-if="pvCanEdit" type="button" class="pv-toggle" :class="{ on: previewEditable }" v-tip="{ text: previewEditable ? $t('editor.pvReadTip') : $t('editor.pvEditTip'), place: 'bottom' }" @click="togglePvEdit">
            <svg v-if="previewEditable" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M4 20h4l10.5-10.5a2.1 2.1 0 0 0-3-3L5 17v3z" /><path d="M14.5 6.5l3 3" /></svg>
            <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 12S6 6.5 12 6.5 21.5 12 21.5 12 18 17.5 12 17.5 2.5 12 2.5 12z" /><circle cx="12" cy="12" r="2.6" /></svg>
          </button>
          <div class="seg modes">
            <span :class="{ on: mode === 'edit' }" @click="mode = 'edit'">{{ $t('sticky.edit') }}</span>
            <span :class="{ on: mode === 'split' }" @click="mode = 'split'">{{ $t('sticky.split') }}</span>
            <span :class="{ on: mode === 'preview' }" @click="mode = 'preview'">{{ $t('sticky.preview') }}</span>
          </div>
        </div>
        <div class="panes" :class="`mode-${mode}`">
          <div class="pane pane-editor" :class="{ collapsed: mode === 'preview' }">
            <CodeMirrorEditor ref="editorCm" :model-value="content" :word-wrap="wordWrap" @update:model-value="onInput" :on-save="save" :on-scroll="onEditorScroll" />
          </div>
          <div ref="previewPane" class="pane pane-preview" :class="{ collapsed: mode === 'edit' }" @dblclick="onPreviewDblClick" @scroll="onPreviewScroll">
            <!-- html 绑的是被冻结的 previewHtml（编辑期间不再重设 innerHTML，光标才保得住），
                 source 仍传当前正文，供只读态的任务清单回改按序号定位源码标记 -->
            <MarkdownPreview
              ref="previewMd"
              :html="previewHtml"
              :source="content"
              :editable="previewEditable"
              @task-toggle="onTaskToggle"
              @edit-input="onPreviewEditInput"
              @edit-keydown="onPreviewEditKeydown"
              @edit-paste="onPreviewEditPaste"
              @edit-compositionstart="onPreviewCompositionStart"
              @edit-compositionend="onPreviewCompositionEnd"
            />
          </div>
        </div>
      </template>
    </div>

    <div v-if="quickLaunch.length" class="sticky-launch" :class="{ 'icon-only': !launchShowText }">
      <button v-for="(item, i) in quickLaunch" :key="`${item.path}#${i}`" :title="item.args ? `${item.path} ${item.args}` : item.path" @click="launch(item)">
        <img v-if="iconMap[item.path]" :src="iconMap[item.path]" class="ql-ico" alt="" />
        <span v-if="launchShowText">{{ item.name }}</span>
      </button>
    </div>

    <!-- 预览模式勾选任务清单确认（与主编辑器同语义） -->
    <a-modal v-model:open="showTaskConfirm" :title="pendingTaskChecked ? $t('editor.taskCheck') : $t('editor.taskUncheck')" :width="320" :footer="null">
      <p class="task-confirm-hint">「{{ pendingTaskText }}」{{ pendingTaskChecked ? $t('editor.taskDone') : $t('editor.taskUndone') }}。</p>
      <div class="task-confirm-foot">
        <button class="ghost" @click="showTaskConfirm = false">{{ $t('common.cancel') }}</button>
        <button class="primary" @click="submitTaskConfirm">{{ $t('common.confirm') }}</button>
      </div>
    </a-modal>
  </div>
</template>

<style scoped>
.sticky {
  display: flex;
  flex-direction: column;
  height: 100vh;
  background: var(--panel);
  color: var(--text);
  overflow: hidden;
  /* 圆角便签：配合窗口透明（见 lib.rs 的 transparent）+ global.css 的窗口背景透出，
     overflow:hidden 把标题栏/正文/启动栏一并裁成圆角矩形 */
  border-radius: 12px;
  border: 1px solid var(--border-2);
  /* 缩放热区用绝对定位钉在右下角 */
  position: relative;
}
/* 右下角缩放热区：无可见图标，只靠斜向双箭头光标提示；比可视尺寸略大，方便命中 */
.resize-grip {
  position: absolute;
  right: 0;
  bottom: 0;
  width: 16px;
  height: 16px;
  z-index: 20;
  cursor: nwse-resize;
  /* 拖动期间不选中文字、不让触摸手势抢走指针 */
  user-select: none;
  touch-action: none;
}
.sticky-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  background: var(--bg);
  border-bottom: 1px solid var(--border);
  user-select: none;
}
.grip { color: var(--text-4); font-size: 12px; }
.title {
  flex: 1;
  font-size: 12px;
  color: var(--text-2);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--orange, #fa8c16);
}
.x {
  border: none;
  background: transparent;
  color: var(--text-3);
  font-size: 16px;
  line-height: 1;
  padding: 3px 7px;
  border-radius: 5px;
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.x:hover { background: var(--border-pop); color: var(--text); }
.x:active { background: var(--border-2); }
/* 锁定按钮：内联 SVG 挂锁图标需 flex 居中（.x 默认按行高排布文本，SVG 会偏上） */
.lock-btn { display: inline-flex; align-items: center; }
.lock-btn svg { display: block; }
/* 「在主界面中打开」：与锁定按钮同规格（同样是内联 SVG，需要 flex 居中） */
.to-main { display: inline-flex; align-items: center; }
.to-main svg { display: block; }
/* 「打开 MarkLock」主图标按钮：常驻浅底 + 描边 + 短文字标签，图标与文字水平居中；PNG 自带留白取 18px */
.main-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 2px 8px;
  background: var(--panel);
  border: 1px solid var(--border-2);
  border-radius: 6px;
  font-size: 12px;
}
.main-btn img { display: block; width: 18px; height: 18px; }
.main-btn:hover { background: var(--hover); border-color: var(--border-pop); }
/* 顶部浮层：一颗只读 ⇄ 编辑 + 「编辑 / 分屏 / 预览」切换段，同一行靠右。
   整条格式化工具栏撤掉了——400 逻辑点放不下，而便签要改格式直接进编辑态打字即可。
   容器不吃鼠标事件，两颗控件自己恢复吃点击；整层平时半透明，指针压到任一控件转不透明
   （:hover 沿 DOM 祖先链匹配，pointer-events:none 不影响父层拿到子级的 hover）。 */
.sticky-float {
  position: absolute;
  top: 8px;
  right: 10px;
  z-index: 5;
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 6px;
  pointer-events: none;
  opacity: 0.35;
  transition: opacity 0.18s ease;
}
.sticky-float:hover {
  opacity: 1;
}
/* 模式切换分段控件：样式对齐主编辑器工具栏的 .seg（见 EditorView.vue） */
.seg.modes {
  pointer-events: auto;
}
/* 只读 ⇄ 编辑：与切换段同高的一颗小按钮，只在纯预览态出现（pvCanEdit） */
.pv-toggle {
  pointer-events: auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 25px;
  padding: 0;
  border: 1px solid var(--border-2);
  border-radius: var(--radius-sm);
  background: var(--panel);
  color: var(--text-2);
  cursor: pointer;
}
.pv-toggle:hover {
  color: var(--text);
  background: var(--hover);
}
.pv-toggle.on {
  color: var(--primary-active);
}
.seg {
  display: inline-flex;
  padding: 1px;
  background: var(--hover);
  border-radius: var(--radius-sm);
  gap: 2px;
}
.seg span {
  padding: 1px 9px;
  font-size: 12px;
  line-height: 1.5;
  color: var(--text-2);
  border-radius: 4px;
  cursor: pointer;
  transition: all 0.2s;
}
.seg span.on {
  background: var(--panel);
  color: var(--text);
  font-weight: 500;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.06);
}
.sticky-body {
  flex: 1;
  display: flex;
  min-height: 0;
  position: relative;
}
/* 编辑 / 分屏 / 预览 切换滑动动画：面板常驻，靠 flex-basis 过渡从边上滑入滑出（对齐主编辑器 .panes） */
.panes {
  flex: 1;
  display: flex;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
}
.pane {
  flex: 1 1 0%;
  min-width: 0;
  overflow: hidden;
  transition: flex-basis 0.24s cubic-bezier(0.45, 0, 0.2, 1);
}
.pane-editor { border-right: 1px solid var(--border); display: flex; }
.pane-editor :deep(.cm-editor) { flex: 1; }
.pane-preview { overflow-y: auto; }
.pane-preview :deep(.md) { padding: 12px 14px 24px; font-size: 13px; }
/* 收起态：宽度滑到 0 后禁止绘制内容，并去掉分隔边框避免边缘留竖线 */
.pane.collapsed { overflow: hidden; border-right-color: transparent; }
/* 编辑：源码满宽、预览收起 */
.panes.mode-edit .pane-editor { flex: 1 0 100%; }
.panes.mode-edit .pane-preview { flex: 1 0 0%; }
/* 预览：预览满宽、源码收起 */
.panes.mode-preview .pane-editor { flex: 1 0 0%; }
.panes.mode-preview .pane-preview { flex: 1 0 100%; }
/* 分屏：两侧各半 */
.panes.mode-split .pane { flex: 1 1 50%; }
.empty, .unlock {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 20px;
  text-align: center;
  color: var(--text-2);
  font-size: 13px;
}
.hint { color: var(--text-4); font-size: 12px; }
.unlock input {
  width: 70%;
  padding: 6px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--bg);
  color: var(--text);
}
.err { color: var(--red, #f5222d); font-size: 12px; }
.primary {
  border: none;
  background: var(--primary);
  color: #fff;
  padding: 5px 16px;
  border-radius: 6px;
  cursor: pointer;
}
/* 预览勾选确认弹窗 */
.task-confirm-hint { color: var(--text-2); font-size: 13px; line-height: 1.6; margin: 0 0 16px; }
.task-confirm-foot { display: flex; justify-content: flex-end; gap: 8px; }
.ghost {
  border: 1px solid var(--border-pop);
  background: var(--panel);
  color: var(--text-2);
  padding: 5px 16px;
  border-radius: 6px;
  cursor: pointer;
}
.ghost:hover { color: var(--text); }
.sticky-launch {
  display: flex;
  gap: 6px;
  padding: 6px 8px;
  border-top: 1px solid var(--border);
  background: var(--bg);
  overflow-x: auto;
}
.sticky-launch button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  border-radius: 8px;
  padding: 4px 8px;
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}
.sticky-launch button:hover { color: var(--primary); border-color: var(--primary); }
.ql-ico { width: 28px; height: 28px; object-fit: contain; flex: none; }
/* 纯图标：去掉文字后收紧为方形内边距，一排更紧凑 */
.sticky-launch.icon-only button { padding: 4px 6px; gap: 0; }
</style>
