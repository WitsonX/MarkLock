<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import TitleBar from '../components/TitleBar.vue'
import AboutCard from '../components/AboutCard.vue'
import { useVaultStore } from '../stores/vault'
import type { Settings } from '../stores/vault'
import { lang } from '../locales'
import { isMac } from '../lib/platform'
import { PREVIEW_FONTS, fontLabel, resolveFontStack } from '../lib/fonts'
import * as tauri from '../lib/tauri'
import { appIcons, ensureAppIcons } from '../lib/appinfo'
import { runUpdateCheck } from '../lib/update'
import { BUILTIN_SKINS, SKIN_TOKEN_GROUPS, newCustomSkin, forkSkin, previewCustomSkin, skinSwatch } from '../lib/skins'
import type { CustomSkin } from '../lib/skins'
import hljs from 'highlight.js'

interface NavItem {
  key: string
  label: string
  core?: boolean
  icon: string
}

/** 「加密方案」组里默认折叠的四行；搜索结果命中它们时先展开再定位 */
const SCHEME_ROWS = ['e2e', 'tamper', 'perFileKey', 'bruteForce']

/** 「界面元素」勾选清单能切换的显示/隐藏项，值即 settings 字段名 */
type AreaKey =
  | 'showTabs' | 'showToolbar' | 'showStatusbar'
  | 'showOpenFiles' | 'showUnlockedVaults' | 'showFavorites' | 'showRecent' | 'showWorkdir' | 'showOutline'

export default defineComponent({
  name: 'SettingsView',
  components: { TitleBar, AboutCard },
  data() {
    return {
      active: 'general',
      // 通用页「立即检查更新」进行中标志（禁用按钮、防重复点击）；与 AboutCard 内部同名标志彼此独立。
      checking: false,
      // 设置搜索关键词
      searchQuery: '',
      // 「重置所有配置」确认弹窗
      resetModalOpen: false,
      resetting: false,
      // 「加密方案」四项说明的展开态（默认折成一行，避免只读信息挤占设置项）
      schemeOpen: false,
      // 编辑器字体大小选项（px，与 ⌘+/⌘- 的 2px 步进对齐，10–28）
      fontSizeOptions: Array.from({ length: 10 }, (_, i) => ({ label: `${10 + i * 2} px`, value: 10 + i * 2 })),
      // 快速启动项内联重命名：当前编辑行的下标（-1 表示无）与草稿名
      editingLaunch: -1,
      editingName: '',
      // 自定义皮肤编辑器：草稿（null=关闭）、正在编辑的明暗档位、删除确认目标
      skinDraft: null as CustomSkin | null,
      skinDraftMode: 'light' as 'light' | 'dark',
      skinDeleteId: '',
      // 色板分组（静态清单，直接引用 lib/skins 的定义）
      skinGroups: SKIN_TOKEN_GROUPS,
    }
  },
  computed: {
    store() {
      return useVaultStore()
    },
    // 以下选项含界面文案，随当前语言重算（放入 computed 而非 data）
    nav(): NavItem[] {
      return [
        { key: 'general', label: this.$t('settings.nav.general'), icon: 'gear' },
        { key: 'appear', label: this.$t('settings.nav.appear'), icon: 'appear' },
        { key: 'security', label: this.$t('settings.nav.security'), icon: 'shield', core: true },
        { key: 'sticky', label: this.$t('settings.nav.sticky'), icon: 'sticky' },
        { key: 'keys', label: this.$t('settings.nav.keys'), icon: 'keys' },
        { key: 'about', label: this.$t('settings.nav.about'), icon: 'about' },
      ]
    },
    autoLockOptions() {
      return [
        { label: this.$t('settings.options.min1'), value: 60 },
        { label: this.$t('settings.options.min5'), value: 300 },
        { label: this.$t('settings.options.min15'), value: 900 },
        { label: this.$t('settings.options.min30'), value: 1800 },
        { label: this.$t('settings.options.neverLock'), value: 0 },
      ]
    },
    autoSaveOptions() {
      return [
        { label: this.$t('settings.options.sec10'), value: 10 },
        { label: this.$t('settings.options.sec30'), value: 30 },
        { label: this.$t('settings.options.min1'), value: 60 },
        { label: this.$t('settings.options.min2'), value: 120 },
        { label: this.$t('settings.options.min5'), value: 300 },
        { label: this.$t('settings.options.min10'), value: 600 },
        { label: this.$t('settings.options.off'), value: 0 },
      ]
    },
    /** 便签文件展示名（未设置时给占位文案） */
    stickyNoteLabel(): string {
      const t = this.store.settings.stickyNote
      if (!t) return this.$t('settings.sticky.none')
      const base = t.kind === 'mdlb' && t.rel ? t.rel : t.path
      return base.split(/[\\/]/).pop() || base
    },
    cornerOptions() {
      return [
        { label: this.$t('settings.sticky.topLeft'), value: 'topLeft' },
        { label: this.$t('settings.sticky.bottomLeft'), value: 'bottomLeft' },
        { label: this.$t('settings.sticky.topRight'), value: 'topRight' },
        { label: this.$t('settings.sticky.bottomRight'), value: 'bottomRight' },
      ]
    },
    modifierOptions() {
      return [
        { label: this.$t('settings.sticky.modCommand'), value: 'command' },
        { label: this.$t('settings.sticky.modShift'), value: 'shift' },
        { label: this.$t('settings.sticky.modControl'), value: 'control' },
        { label: this.$t('settings.sticky.modAlt'), value: 'alt' },
      ]
    },
    dwellOptions() {
      return [
        { label: '0.3 s', value: 300 },
        { label: '0.5 s', value: 500 },
        { label: '1 s', value: 1000 },
      ]
    },
    maxWidthOptions() {
      return [
        { label: this.$t('settings.appear.fill'), value: 0 },
        { label: '640 px', value: 640 },
        { label: '780 px', value: 780 },
        { label: '900 px', value: 900 },
        { label: '1080 px', value: 1080 },
        { label: '1280 px', value: 1280 },
      ]
    },
    // 预览字体族选项：来自内置跨平台清单（src/lib/fonts.ts），名称随当前语言重算
    previewFontOptions() {
      void lang.value
      return PREVIEW_FONTS.map((f) => ({ label: fontLabel(f), value: f.id }))
    },
    // 预览行间距选项（无单位 line-height 倍数；1.7 为默认）
    lineHeightOptions() {
      return [
        { label: `${this.$t('settings.appear.lhTight')} (1.4)`, value: 1.4 },
        { label: `${this.$t('settings.appear.lhStandard')} (1.7)`, value: 1.7 },
        { label: `${this.$t('settings.appear.lhLoose')} (1.9)`, value: 1.9 },
        { label: `${this.$t('settings.appear.lhExtra')} (2.1)`, value: 2.1 },
      ]
    },
    // 检查更新频率选项
    updateCheckOptions() {
      return [
        { label: this.$t('settings.options.updateDaily'), value: 'daily' },
        { label: this.$t('settings.options.updateWeekly'), value: 'weekly' },
        { label: this.$t('settings.options.updateMonthly'), value: 'monthly' },
        { label: this.$t('settings.options.updateManual'), value: 'manual' },
        { label: this.$t('settings.options.updateOff'), value: 'off' },
      ]
    },
    // 当前实际语言（供 watch 触发搜索索引重建）
    currentLang() {
      return lang.value
    },
    /** 快捷键符号：macOS 用 ⌘，其它平台用 Ctrl（与快捷键页展示一致） */
    modKey() {
      return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent) ? '⌘' : 'Ctrl'
    },
    /** Option / Alt 键符号（与 modKey 同一判定，供页签切换等组合快捷键展示） */
    altKey() {
      return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent) ? '⌥' : 'Alt'
    },
    /** 搜索候选列表（分类+分组+标题） */
    searchResults(): any[] {
      const q = this.searchQuery.trim().toLowerCase()
      if (!q) return []
      const index: any[] = (this as any)._searchIndex ?? []
      return index.filter((it) => it.text.includes(q)).slice(0, 40)
    },
    /** 未保存内容页签数（含未落盘草稿），重置前用于提醒 */
    unsavedCount() {
      return this.store.openFiles.filter((f) => f.dirty || f.isNew).length
    },
    /** 已解锁的库数（重置后失效，需重新输密码） */
    unlockedVaultCount() {
      return this.store.recent.filter((v) => v.unlocked).length
    },
    /** 预览代码高亮 HTML（用制表符缩进，tab-size 才能生效） */
    previewCodeHtml() {
      const code = `function greet(name) {
\tconst msg = \`Hello, \${name}!\`;
\tconsole.log(msg);
\treturn msg;
}

// 调用示例
greet('MarkLock');`
      return hljs.highlight(code, { language: 'javascript' }).value
    },
    /** 预览字体族当前 CSS 值（与 EditorView 同源 src/lib/fonts.ts），供排版实时示例使用 */
    previewFontStack() {
      return resolveFontStack(this.store.settings.previewFontFamily)
    },
    /** 快速启动图标的内存缓存（path → data URL），由 lib/appinfo 异步填充。 */
    iconMap(): Record<string, string> {
      return appIcons
    },
    /** 当前实际生效的明暗档位（皮肤色卡按此取色；「跟随系统」时读媒体查询） */
    resolvedTheme(): 'light' | 'dark' {
      const th = this.store.settings.theme
      if (th === 'light' || th === 'dark') return th
      return window.matchMedia?.('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
    },
    /** 皮肤色卡列表：内置在前、自定义在后；三色为底色 / 面板 / 主色 */
    skinCards() {
      const mode = this.resolvedTheme
      const customs = this.store.settings.customSkins
      return [
        ...BUILTIN_SKINS.map((s) => ({ id: s.id, name: this.$t(s.nameKey), builtin: true, swatch: skinSwatch(s.id, mode, customs) })),
        ...customs.map((s) => ({ id: s.id, name: s.name, builtin: false, swatch: skinSwatch(s.id, mode, customs) })),
      ]
    },
    /** 自定义皮肤编辑弹窗：有草稿即打开 */
    skinModalOpen(): boolean {
      return !!this.skinDraft
    },
    /** 待删除的自定义皮肤（删除确认弹窗用） */
    skinDeleteTarget(): CustomSkin | null {
      return this.store.settings.customSkins.find((s) => s.id === this.skinDeleteId) || null
    },
    /** 草稿当前档位（浅色/深色）的令牌表 */
    skinDraftTokens(): Record<string, string> {
      return this.skinDraft ? this.skinDraft.tokens[this.skinDraftMode] : {}
    },
  },
  mounted() {
    // 便签窗（独立 webview）可能刚改过设置（如在便签内选定文件），本窗内存副本是启动时的旧值；
    // 进入设置页先从 localStorage 刷新，保证显示与后续写盘都基于最新状态（否则会显示为空并覆盖掉便签目标）。
    this.store.reloadSettings()
    this.buildSearchIndex()
    // 支持通过路由 query（例如外部/书签链接、便签窗「设置」按钮）直达指定页签；
    // 菜单「关于」不再走此路径，改由 App.vue 弹独立对话框（src/lib/aboutDialog.ts）。
    this.applyTabQuery()
    // 预取已有快速启动项的图标（新增项在 addLaunch 里单独预取）
    ensureAppIcons(this.store.settings.quickLaunch.map((i) => i.path))
  },
  beforeUnmount() {
    ;(this as any)._searchIndex = null
    // 编辑到一半直接离开设置页：撤掉预览样式，避免 data-skin 停在 __preview__ 上
    if (this.skinDraft) previewCustomSkin(null, this.store.settings.skin)
  },
  watch: {
    // 草稿任何改动（令牌、名称、高级 CSS）即时预览，所见即所得
    skinDraft: {
      deep: true,
      handler(v: CustomSkin | null) {
        if (v) previewCustomSkin(v)
      },
    },
    // 主窗可能已停在设置页（组件复用、mounted 不再触发），故监听 query 变化重新定位分区。
    '$route.query.tab'() {
      this.applyTabQuery()
    },
    searchQuery() {
      this.applySearch()
    },
    active() {
      // 切换分类时重新应用高亮与「无结果」提示
      this.applySearch()
    },
    currentLang() {
      // 语言切换后标题/描述变化，重新采集搜索索引
      this.$nextTick(() => {
        this.buildSearchIndex()
        this.applySearch()
      })
    },
  },
  methods: {
    /** 托盘相关文案取词：macOS 无「托盘/通知区域」概念，按 Mac 后缀取专属词条（菜单栏 / Dock 语义），其他平台用原键。 */
    trayT(key: string) {
      return this.$t(`settings.general.${key}${isMac ? 'Mac' : ''}`)
    },

    // ---------- 界面元素勾选清单 ----------
    /** 切换一个显示/隐藏项 */
    toggleArea(key: AreaKey) {
      this.store.updateSettings({ [key]: !this.store.settings[key] } as Pick<Settings, AreaKey>)
    },

    // ---------- 皮肤（配色方案） ----------
    /** 应用某个皮肤（内置或自定义） */
    applySkin(id: string) {
      this.store.updateSettings({ skin: id })
    },
    /** 打开自定义皮肤编辑器：传 id 编辑现有皮肤（拷一份草稿改），不传则新建。
     *  默认停在当前明暗档位上编辑，另一档位的令牌互不影响。 */
    openSkinEditor(id?: string) {
      const existing = id ? this.store.settings.customSkins.find((s) => s.id === id) : null
      this.skinDraft = existing
        ? { ...existing, tokens: { light: { ...existing.tokens.light }, dark: { ...existing.tokens.dark } } }
        : newCustomSkin(this.$t('skin.defaultName'))
      this.skinDraftMode = this.resolvedTheme
      previewCustomSkin(this.skinDraft)
    },
    /** 复制某个皮肤（内置或自定义）为新草稿并打开编辑器，在其基础上改；不立即落盘，保存后才新增。 */
    duplicateSkin(id: string) {
      const src = this.store.settings.customSkins.find((s) => s.id === id)
      const builtin = BUILTIN_SKINS.find((s) => s.id === id)
      const baseName = src ? src.name : builtin ? this.$t(builtin.nameKey) : id
      this.skinDraft = forkSkin(id, this.$t('skin.duplicateName', { name: baseName }), this.store.settings.customSkins)
      this.skinDraftMode = this.resolvedTheme
      previewCustomSkin(this.skinDraft)
    },
    /** 关闭编辑器且不保存：撤预览样式、恢复原皮肤 */
    cancelSkinDraft() {
      const restore = this.store.settings.skin
      this.skinDraft = null
      previewCustomSkin(null, restore)
    },
    /** 保存草稿：写入 customSkins 并立即应用。
     *  先落设置（App.vue 的 watcher 会注入正式样式），再撤预览样式，避免出现无规则的空档。 */
    saveSkinDraft() {
      const d = this.skinDraft
      if (!d) return
      const name = (d.name || '').trim() || this.$t('skin.defaultName')
      const list = this.store.settings.customSkins.filter((s) => s.id !== d.id)
      list.push({ ...d, name, tokens: { light: { ...d.tokens.light }, dark: { ...d.tokens.dark } } })
      this.store.updateSettings({ customSkins: list, skin: d.id })
      this.skinDraft = null
      previewCustomSkin(null, d.id)
      message.success(this.$t('skin.saved', { name }))
    },
    /** 把当前档位的令牌整套复制到另一档位（浅深同色的皮肤不必填两遍） */
    copyTokensToOtherMode() {
      const d = this.skinDraft
      if (!d) return
      const to = this.skinDraftMode === 'light' ? 'dark' : 'light'
      d.tokens[to] = { ...d.tokens[this.skinDraftMode] }
    },
    /** 写入一个令牌值；空串表示清除该覆盖、继承基线 */
    setSkinToken(key: string, value: string) {
      const d = this.skinDraft
      if (!d) return
      const v = (value || '').trim()
      if (v) d.tokens[this.skinDraftMode][key] = v
      else delete d.tokens[this.skinDraftMode][key]
    },
    onSkinTokenPick(key: string, e: Event) {
      this.setSkinToken(key, (e.target as HTMLInputElement).value)
    },
    /** 文本框写入令牌值（可填 rgba() / 渐变 / var() 等取色器表达不了的值） */
    onSkinTokenText(key: string, e: Event) {
      this.setSkinToken(key, (e.target as HTMLInputElement).value)
    },
    /** 令牌的十六进制值，供 <input type="color"> 显示；非十六进制（rgba / 渐变等）给中性灰占位，
     *  真实值以旁边的文本框为准——原生取色器只能表达 #rrggbb。 */
    skinTokenHex(key: string): string {
      const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec((this.skinDraftTokens[key] || '').trim())
      if (!m) return '#808080'
      const h = m[1].toLowerCase()
      return '#' + (h.length === 3 ? h.split('').map((c) => c + c).join('') : h)
    },
    /** 令牌的中文/英文说明（词条键去掉 `--` 前缀） */
    skinTokenLabel(key: string): string {
      return this.$t(`skin.tokens.${key.slice(2)}`)
    },
    askDeleteSkin(id: string) {
      this.skinDeleteId = id
    },
    /** 确认删除自定义皮肤；若正在使用则回落默认皮肤 */
    confirmDeleteSkin() {
      const id = this.skinDeleteId
      const name = this.skinDeleteTarget?.name || ''
      if (!id) return
      const customSkins = this.store.settings.customSkins.filter((s) => s.id !== id)
      this.store.updateSettings(this.store.settings.skin === id ? { customSkins, skin: 'default' } : { customSkins })
      this.skinDeleteId = ''
      message.success(this.$t('skin.deleted', { name }))
    },
    /** 根据路由 query.tab 定位到对应分区（无效值忽略，保持当前分区）。 */
    applyTabQuery() {
      const tab = this.$route.query.tab
      if (typeof tab === 'string' && this.nav.some((n) => n.key === tab)) this.active = tab
    },
    /** 选择便签源文件：支持明文 md / 单文件加密 .mdl；.mdlb 是容器（内含多文件）不作为单便签。 */
    async pickStickyFile() {
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
    },
    clearStickyFile() {
      this.store.updateSettings({ stickyNote: null })
      // 便签窗口是独立 webview，store 不共享；清除文件后直接隐藏它，避免还显示旧内容
      tauri.hideStickyNote().catch(() => {})
    },
    /** 添加快速启动项：无过滤器选任意本地应用 / 路径，名称取文件名去扩展名。 */
    async addLaunch() {
      const path = await tauri.openAnyFileDialog()
      if (!path) return
      const cur = this.store.settings.quickLaunch
      if (cur.some((x) => x.path === path)) return
      // 优先用系统层面的本地化应用名（macOS / Windows）；读不到时回退到文件名（去扩展名）
      const fallback = (path.split(/[\\/]/).pop() || path).replace(/\.[^.]+$/, '')
      const name = (await tauri.appDisplayName(path)) || fallback
      this.store.updateSettings({ quickLaunch: [...cur, { name, path }] })
      ensureAppIcons([path])
    },
    removeLaunch(i: number) {
      const cur = [...this.store.settings.quickLaunch]
      cur.splice(i, 1)
      this.store.updateSettings({ quickLaunch: cur })
    },
    /** 进入快速启动项的内联重命名（记下当前名为草稿）。 */
    startEditLaunch(i: number) {
      this.editingLaunch = i
      this.editingName = this.store.settings.quickLaunch[i]?.name ?? ''
      // 下一帧自动聚焦并全选，便于一改版名
      this.$nextTick(() => {
        const el = this.$refs.launchNameInput as HTMLInputElement | undefined
        if (el) { el.focus(); el.select() }
      })
    },
    cancelLaunchEdit() {
      this.editingLaunch = -1
    },
    /** 提交重命名：非空才写回并持久化（仅改 name，path 不变）；取消/重复 blur 需先判定仍在编辑态。 */
    commitLaunchEdit(i: number) {
      if (this.editingLaunch !== i) return
      const name = this.editingName.trim()
      if (name) {
        const cur = [...this.store.settings.quickLaunch]
        if (cur[i] && cur[i].name !== name) {
          cur[i] = { ...cur[i], name }
          this.store.updateSettings({ quickLaunch: cur })
        }
      }
      this.editingLaunch = -1
    },
    /** 「开机自启」开关：调用 Rust 写入 / 移除系统启动项；失败时 toast 提示并回退开关。
     *  与 store.settings.autoStart 双向绑定：先乐观更新 UI，后端拒绝时再回写。 */
    async toggleAutoStart() {
      const next = !this.store.settings.autoStart
      this.store.updateSettings({ autoStart: next })
      try {
        await tauri.setAutoStart(next)
      } catch (e) {
        // 后端写入失败（权限 / 注册表 / plist 异常）：回写设置并提示，避免 UI 与系统状态脱节
        this.store.updateSettings({ autoStart: !next })
        message.error(this.$t('settings.toast.autoStartFailed', { err: String(e) }))
      }
    },
    /** 「显示托盘图标」开关：关闭时连带关闭「关闭最小化到托盘」，避免窗口隐藏后没有任何恢复入口。 */
    toggleTrayIcon() {
      const on = !this.store.settings.showTrayIcon
      this.store.updateSettings(on ? { showTrayIcon: true } : { showTrayIcon: false, closeToTray: false })
    },
    /** 「关闭最小化到托盘」开关：开启时若托盘图标被隐藏则连带开启，保证隐藏后有托盘入口可恢复。 */
    toggleCloseToTray() {
      const on = !this.store.settings.closeToTray
      if (on && !this.store.settings.showTrayIcon) {
        this.store.updateSettings({ closeToTray: true, showTrayIcon: true })
      } else {
        this.store.updateSettings({ closeToTray: on })
      }
    },
    /** 从 DOM 收集全部设置行，构建搜索索引（标题/描述/关键词/分组/分类） */
    buildSearchIndex() {
      const rows = Array.from(document.querySelectorAll('.s-pane .row[data-key]'))
      const index = rows.map((el) => {
        const pane = el.closest('.s-pane') as HTMLElement
        // 分组标题是行的兄弟节点；「界面元素」这类行嵌在 .area 里，所以要沿着父级往上找
        let groupLabel = ''
        let node: HTMLElement | null = el as HTMLElement
        const groupEl = el.closest('.group')
        while (node && groupEl && !groupLabel) {
          let p = node.previousElementSibling
          while (p) {
            if (p.classList && p.classList.contains('group-head')) { groupLabel = p.textContent!.trim(); break }
            p = p.previousElementSibling
          }
          if (node === groupEl) break
          node = node.parentElement
        }
        const t = el.querySelector('.t')?.textContent?.trim() ?? ''
        const d = el.querySelector('.d')?.textContent?.trim() ?? ''
        const kw = el.getAttribute('data-keywords') ?? ''
        return {
          key: el.getAttribute('data-key')!,
          pane: pane.dataset.pane ?? '',
          paneLabel: pane.querySelector('h2')?.textContent?.trim() ?? '',
          groupLabel,
          title: t,
          text: `${t} ${d} ${kw} ${groupLabel}`.toLowerCase(),
        }
      })
      // 结果顺序跟左侧导航一致（模板里 pane 的先后可能与会调整，故不依赖 DOM 顺序）；sort 稳定，pane 内保持原序
      const paneOrder = this.nav.map((n) => n.key)
      index.sort((a, b) => paneOrder.indexOf(a.pane) - paneOrder.indexOf(b.pane))
      ;(this as any)._searchIndex = index
    },
    /** 按关键词过滤：命中行加 .hit 高亮，未命中行加 .dim 淡化 */
    applySearch() {
      const q = this.searchQuery.trim().toLowerCase()
      const index: any[] = (this as any)._searchIndex ?? []
      const matched = new Set<string>()
      if (q) {
        for (const it of index) {
          if (it.text.includes(q)) matched.add(it.key)
        }
      }
      document.querySelectorAll<HTMLElement>('.s-pane .row[data-key]').forEach((el) => {
        const key = el.getAttribute('data-key')!
        if (!q) {
          el.classList.remove('hit', 'dim')
        } else if (matched.has(key)) {
          el.classList.add('hit')
          el.classList.remove('dim')
        } else {
          el.classList.add('dim')
          el.classList.remove('hit')
        }
      })
    },
    clearSearch() {
      this.searchQuery = ''
    },
    /** 点击搜索结果：跳到对应分类并滚动定位、闪烁提示 */
    gotoResult(key: string) {
      const index: any[] = (this as any)._searchIndex ?? []
      const hit = index.find((it) => it.key === key)
      if (!hit) return
      this.active = hit.pane
      if (SCHEME_ROWS.includes(key)) this.schemeOpen = true
      this.$nextTick(() => {
        const el = document.querySelector<HTMLElement>(`.s-pane[data-pane="${hit.pane}"] .row[data-key="${key}"]`)
        if (!el) return
        el.scrollIntoView({ block: 'center', behavior: 'smooth' })
        el.classList.remove('flash')
        // 强制 reflow 以便重复触发闪烁动画
        void el.offsetWidth
        el.classList.add('flash')
        window.setTimeout(() => el.classList.remove('flash'), 1200)
      })
    },
    /** 一键将 MarkLock 设为 .md / .mdl / .mdlb 的默认打开程序；系统拒绝时提示手动步骤。 */
    async applySetDefault() {
      try {
        const r = await tauri.trySetDefaultFileHandler()
        if (r === 'set') message.success(this.$t('settings.toast.setDefaultSuccess'))
        else message.warning(this.$t('settings.toast.setDefaultManual'))
      } catch (e) {
        message.error(String(e))
      }
    },
    /** 立即手动检查一次更新（不受频率门控）；结果展示（弹窗/轻提示）由 runUpdateCheck 统一处理。 */
    async runManualCheck() {
      if (this.checking) return
      this.checking = true
      try {
        await runUpdateCheck(lang.value === 'zh')
      } catch (e) {
        message.error(String(e))
      } finally {
        this.checking = false
      }
    },
    /** 执行「重置所有配置」；store 锁定全部库并清空本地记录，随后切回编辑器并重新载入窗口。 */
    async submitReset() {
      if (this.resetting) return
      this.resetting = true
      try {
        await this.store.resetAll()
      } catch (e) {
        this.resetting = false
        this.resetModalOpen = false
        message.error(this.$t('settings.toast.resetFailed', { err: String(e) }))
        return
      }
      // 先把 hash 改到编辑器（replaceState 不触发路由，避免中间挂载又写回 localStorage），再整页重载：
      // 所有组件按重置后的默认值重新初始化
      window.history.replaceState(null, '', `${window.location.pathname}${window.location.search}#/editor`)
      location.reload()
    },
  },
})
</script>

<template>
  <div class="screen">
    <TitleBar :title="$t('settings.title')" subtitle="MarkLock">
      <span class="back" data-tauri-drag-region="false" :title="$t('settings.back')" @click="$router.push('/editor')">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 12H5M12 19l-7-7 7-7" /></svg>
        <span class="back-txt">{{ $t('settings.back') }}</span>
      </span>
    </TitleBar>

    <div class="settings">
      <!-- 左侧菜单 -->
      <nav class="s-nav">
        <!-- 设置搜索：按标题/描述/关键词过滤，命中行高亮，可点击跳转定位 -->
        <div class="s-search">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><path d="M21 21l-4.35-4.35" /></svg>
          <input v-model="searchQuery" type="text" :placeholder="$t('settings.searchPlaceholder')" spellcheck="false" />
          <span v-if="searchQuery" class="clear" :title="$t('settings.searchClear')" @click="clearSearch">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M18 6L6 18M6 6l12 12" /></svg>
          </span>
        </div>
        <div v-show="searchQuery.trim()" class="s-results">
          <div v-for="r in searchResults" :key="r.key" class="s-result" @click="gotoResult(r.key)">
            <div class="rt">{{ r.title }}</div>
            <div class="rp">{{ r.paneLabel }} · {{ r.groupLabel }}</div>
          </div>
          <div v-if="!searchResults.length" class="s-result-empty">{{ $t('settings.searchNoResult') }}</div>
        </div>
        <template v-if="!searchQuery.trim()">
          <div class="cap">{{ $t('settings.prefCap') }}</div>
          <div
            v-for="item in nav"
            :key="item.key"
            class="s-item"
            :class="{ on: active === item.key }"
            @click="active = item.key"
          >
            <svg v-if="item.icon === 'gear'" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9c.14.6.7 1 1.51 1H21a2 2 0 1 1 0 4h-.09c-.8 0-1.37.41-1.51 1z" /></svg>
            <svg v-else-if="item.icon === 'appear'" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="9" /><path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor" opacity="0.25" /></svg>
            <svg v-else-if="item.icon === 'shield'" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 2l8 4v5c0 5-3.5 9.4-8 11-4.5-1.6-8-6-8-11V6z" /><path d="M9 12l2 2 4-4" /></svg>
            <svg v-else-if="item.icon === 'keys'" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="2" y="6" width="20" height="12" rx="2" /><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10" /></svg>
            <svg v-else-if="item.icon === 'sticky'" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 4h16v10l-6 6H4z" /><path d="M14 20v-6h6" /></svg>
            <svg v-else width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="9" /><path d="M12 11v5M12 8h.01" /></svg>
            {{ item.label }}
            <span v-if="item.core" class="tag tag-gold" style="margin-left:auto;height:18px;font-size:10px;">{{ $t('settings.coreTag') }}</span>
          </div>
        </template>
      </nav>

      <!-- 内容区 -->
      <div class="s-body">
        <!-- 通用 -->
        <div v-show="active === 'general'" class="s-pane" data-pane="general">
          <h2>{{ $t('settings.general.h2') }}</h2>
          <div class="desc">{{ $t('settings.general.desc') }}</div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gLaunch') }}</div>
            <div class="row" data-key="restoreLastSession" data-keywords="会话 页签 恢复 启动 session restore">
              <div class="info"><div class="t">{{ $t('settings.general.restore') }}</div><div class="d">{{ $t('settings.general.restoreD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.restoreLastSession }" @click="store.updateSettings({ restoreLastSession: !store.settings.restoreLastSession })"></div>
            </div>
            <div class="row" data-key="autoStart" data-keywords="开机 自启 自动启动 登录 启动项 开机启动 auto start startup login launch boot">
              <div class="info"><div class="t">{{ $t('settings.general.autoStart') }}</div><div class="d">{{ $t('settings.general.autoStartD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.autoStart }" @click="toggleAutoStart"></div>
            </div>
            <div v-if="store.settings.autoStart" class="row" data-key="autoStartShowWindow" data-keywords="自启 显示 主窗 弹出 静默 后台 托盘 show window autostart hidden silent background">
              <div class="info"><div class="t">{{ $t('settings.general.autoStartShowWindow') }}</div><div class="d">{{ $t('settings.general.autoStartShowWindowD') }}</div></div>
              <!-- 托盘关闭时强制弹窗（避免不可达），开关视觉为 ON 且不可点；重新开启托盘后恢复为用户偏好 -->
              <div
                class="switch"
                :class="{ on: store.settings.autoStartShowWindow || !store.settings.showTrayIcon, disabled: !store.settings.showTrayIcon }"
                @click="store.settings.showTrayIcon && store.updateSettings({ autoStartShowWindow: !store.settings.autoStartShowWindow })"
              ></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ trayT('gTray') }}</div>
            <div class="row" data-key="showTrayIcon" data-keywords="托盘 图标 菜单栏 显示 驻留 tray icon menu bar 系统托盘 dock">
              <div class="info"><div class="t">{{ trayT('showTrayIcon') }}</div><div class="d">{{ trayT('showTrayIconD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showTrayIcon }" @click="toggleTrayIcon"></div>
            </div>
            <div class="row" data-key="closeToTray" data-keywords="托盘 后台 常驻 关闭 最小化 隐藏 驻留 tray 系统托盘 background minimize hide dock">
              <div class="info"><div class="t">{{ trayT('closeToTray') }}</div><div class="d">{{ trayT('closeToTrayD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.closeToTray }" @click="toggleCloseToTray"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gFiles') }}</div>
            <div class="row" data-key="watchExternalChanges" data-keywords="监控 外部修改 刷新 重载 文件 watch external reload">
              <div class="info"><div class="t">{{ $t('settings.general.watchExternal') }}</div><div class="d">{{ $t('settings.general.watchExternalD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.watchExternalChanges }" @click="store.updateSettings({ watchExternalChanges: !store.settings.watchExternalChanges })"></div>
            </div>
            <div class="row" data-key="autoSaveSecs" data-keywords="自动保存 保存 静默 文件 auto save">
              <div class="info"><div class="t">{{ $t('settings.general.autoSave') }}</div><div class="d">{{ $t('settings.general.autoSaveD') }}</div></div>
              <a-select
                :value="store.settings.autoSaveSecs"
                style="width: 150px;"
                :options="autoSaveOptions"
                @change="(v: number) => store.updateSettings({ autoSaveSecs: v })"
              />
            </div>
            <div class="row" data-key="showHiddenFiles" data-keywords="隐藏文件 文件浏览 目录 hidden files">
              <div class="info"><div class="t">{{ $t('settings.general.hiddenFiles') }}</div><div class="d">{{ $t('settings.general.hiddenFilesD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showHiddenFiles }" @click="store.updateSettings({ showHiddenFiles: !store.settings.showHiddenFiles })"></div>
            </div>
            <div class="row" data-key="convertMdAction" data-keywords="转为加密 mdl 明文 删除 保留 convert encrypt delete keep">
              <div class="info"><div class="t">{{ $t('settings.general.convertMd') }}</div><div class="d">{{ $t('settings.general.convertMdD') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.convertMdAction === 'ask' }" @click="store.updateSettings({ convertMdAction: 'ask' })">{{ $t('settings.general.convertAsk') }}</span>
                <span :class="{ on: store.settings.convertMdAction === 'delete' }" @click="store.updateSettings({ convertMdAction: 'delete' })">{{ $t('settings.general.convertDelete') }}</span>
                <span :class="{ on: store.settings.convertMdAction === 'keep' }" @click="store.updateSettings({ convertMdAction: 'keep' })">{{ $t('settings.general.convertKeep') }}</span>
              </div>
            </div>
            <div class="row" data-key="setDefaultHandler" data-keywords="默认 打开方式 文件关联 md mdl mdlb default handler">
              <div class="info"><div class="t">{{ $t('settings.general.setDefault') }}</div><div class="d">{{ $t('settings.general.setDefaultD') }}</div></div>
              <button class="btn" @click="applySetDefault">{{ $t('settings.general.setDefaultBtn') }}</button>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gUpdate') }}</div>
            <div class="row" data-key="updateCheckFreq" data-keywords="检查 更新 版本 自动 频率 每天 每周 update check version daily weekly">
              <div class="info"><div class="t">{{ $t('settings.general.autoUpdate') }}</div><div class="d">{{ $t('settings.general.autoUpdateD') }}</div></div>
              <a-select
                :value="store.settings.updateCheckFreq"
                style="width: 150px;"
                :options="updateCheckOptions"
                @change="(v: string) => store.updateSettings({ updateCheckFreq: v as 'daily' | 'weekly' | 'monthly' | 'manual' | 'off' })"
              />
            </div>
            <div class="row" data-key="checkUpdateNow" data-keywords="立即 现在 检查 更新 手动 网络 check now manual network">
              <div class="info"><div class="t">{{ $t('settings.general.checkNow') }}</div><div class="d">{{ $t('settings.general.checkNowD') }}</div></div>
              <button class="btn btn-sm" :disabled="checking" @click="runManualCheck">{{ checking ? $t('settings.update.checking') : $t('settings.general.checkNowBtn') }}</button>
            </div>
          </div>
        </div>

        <!-- 外观 -->
        <div v-show="active === 'appear'" class="s-pane" data-pane="appear">
          <h2>{{ $t('settings.appear.h2') }}</h2>
          <div class="desc">{{ $t('settings.appear.desc') }}</div>
          <div class="group">
            <div class="group-head">{{ $t('settings.appear.gUi') }}</div>
            <div class="row" data-key="theme" data-keywords="主题 浅色 深色 暗黑 dark light 跟随系统 theme">
              <div class="info"><div class="t">{{ $t('settings.appear.appearanceMode') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.theme === 'light' }" @click="store.updateSettings({ theme: 'light' })">{{ $t('settings.appear.light') }}</span>
                <span :class="{ on: store.settings.theme === 'dark' }" @click="store.updateSettings({ theme: 'dark' })">{{ $t('settings.appear.dark') }}</span>
                <span :class="{ on: store.settings.theme === 'system' }" @click="store.updateSettings({ theme: 'system' })">{{ $t('settings.appear.followSystem') }}</span>
              </div>
            </div>
            <div class="row" data-key="skin" data-keywords="皮肤 配色 换肤 颜色 外观 自定义 css sepia nord solarized gruvbox skin color theme">
              <div class="info">
                <div class="t">{{ $t('skin.pick') }}</div>
                <div class="d">{{ $t('skin.pickD') }}</div>
              </div>
              <button class="btn btn-sm" @click="openSkinEditor()">{{ $t('skin.new') }}</button>
            </div>
            <div class="skin-grid">
              <div
                v-for="c in skinCards"
                :key="c.id"
                class="skin-card"
                :class="{ on: store.settings.skin === c.id }"
                @click="applySkin(c.id)"
              >
                <span class="sw" :style="{ background: c.swatch[0] }">
                  <i :style="{ background: c.swatch[1] }"></i>
                  <b :style="{ background: c.swatch[2] }"></b>
                </span>
                <span class="nm">{{ c.name }}</span>
                <span v-if="store.settings.skin === c.id" class="skin-ck">✓</span>
                <span class="skin-acts">
                  <button class="icon-btn" :title="$t('skin.duplicate')" @click.stop="duplicateSkin(c.id)"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a2 2 0 0 1 2-2h10" /></svg></button>
                  <button v-if="!c.builtin" class="icon-btn" :title="$t('skin.edit')" @click.stop="openSkinEditor(c.id)"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9" /><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" /></svg></button>
                  <button v-if="!c.builtin" class="icon-btn" :title="$t('skin.delete')" @click.stop="askDeleteSkin(c.id)"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14" /></svg></button>
                </span>
              </div>
            </div>
            <div class="row" data-key="language" data-keywords="语言 language 英文 中文 english 简体中文 切换">
              <div class="info"><div class="t">{{ $t('settings.appear.language') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.language === 'system' }" @click="store.updateSettings({ language: 'system' })">{{ $t('settings.appear.langSystem') }}</span>
                <span :class="{ on: store.settings.language === 'zh' }" @click="store.updateSettings({ language: 'zh' })">{{ $t('settings.appear.langZh') }}</span>
                <span :class="{ on: store.settings.language === 'en' }" @click="store.updateSettings({ language: 'en' })">{{ $t('settings.appear.langEn') }}</span>
              </div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.appear.gElements') }}</div>
            <div class="area">
              <div class="area-label">{{ $t('settings.appear.windowAreas') }}</div>
              <div class="area-rows">
                <label class="row row-check" data-key="showTabs" data-keywords="页签 tab 标签栏 顶部 窗口 tabs bar">
                  <input type="checkbox" :checked="store.settings.showTabs" @change="toggleArea('showTabs')" />
                  <span class="t">{{ $t('settings.appear.showTabs') }}</span>
                  <span class="d">{{ $t('settings.appear.showTabsD') }}</span>
                </label>
                <label class="row row-check" data-key="showToolbar" data-keywords="工具栏 格式化 按钮 toolbar format">
                  <input type="checkbox" :checked="store.settings.showToolbar" @change="toggleArea('showToolbar')" />
                  <span class="t">{{ $t('settings.appear.toolbar') }}</span>
                  <span class="d">{{ $t('settings.appear.toolbarD') }}</span>
                </label>
                <label class="row row-check" data-key="showStatusbar" data-keywords="状态栏 行数 字数 保存状态 底部 statusbar lines words">
                  <input type="checkbox" :checked="store.settings.showStatusbar" @change="toggleArea('showStatusbar')" />
                  <span class="t">{{ $t('settings.appear.statusbar') }}</span>
                  <span class="d">{{ $t('settings.appear.statusbarD') }}</span>
                </label>
              </div>
            </div>
            <div class="area">
              <div class="area-label">{{ $t('settings.appear.sidebarPanels') }}</div>
              <div class="area-rows">
                <label class="row row-check" data-key="showOpenFiles" data-keywords="侧边栏 打开的文件 open files">
                  <input type="checkbox" :checked="store.settings.showOpenFiles" @change="toggleArea('showOpenFiles')" />
                  <span class="t">{{ $t('settings.appear.openFiles') }}</span>
                  <span class="d">{{ $t('settings.appear.openFilesD') }}</span>
                </label>
                <label class="row row-check" data-key="showUnlockedVaults" data-keywords="侧边栏 加密库 库 vaults">
                  <input type="checkbox" :checked="store.settings.showUnlockedVaults" @change="toggleArea('showUnlockedVaults')" />
                  <span class="t">{{ $t('settings.appear.vaults') }}</span>
                  <span class="d">{{ $t('settings.appear.vaultsD') }}</span>
                </label>
                <label class="row row-check" data-key="showFavorites" data-keywords="侧边栏 收藏 favorites">
                  <input type="checkbox" :checked="store.settings.showFavorites" @change="toggleArea('showFavorites')" />
                  <span class="t">{{ $t('settings.appear.favorites') }}</span>
                  <span class="d">{{ $t('settings.appear.favoritesD') }}</span>
                </label>
                <label class="row row-check" data-key="showRecent" data-keywords="侧边栏 最近打开 recent">
                  <input type="checkbox" :checked="store.settings.showRecent" @change="toggleArea('showRecent')" />
                  <span class="t">{{ $t('settings.appear.recent') }}</span>
                  <span class="d">{{ $t('settings.appear.recentD') }}</span>
                </label>
                <label class="row row-check" data-key="showWorkdir" data-keywords="侧边栏 工作目录 文件夹 workdir folder">
                  <input type="checkbox" :checked="store.settings.showWorkdir" @change="toggleArea('showWorkdir')" />
                  <span class="t">{{ $t('settings.appear.workdir') }}</span>
                  <span class="d">{{ $t('settings.appear.workdirD') }}</span>
                </label>
                <label class="row row-check" data-key="showOutline" data-keywords="侧边栏 大纲 标题 目录 outline heading">
                  <input type="checkbox" :checked="store.settings.showOutline" @change="toggleArea('showOutline')" />
                  <span class="t">{{ $t('settings.appear.outline') }}</span>
                  <span class="d">{{ $t('settings.appear.outlineD') }}</span>
                </label>
              </div>
            </div>
            <div class="row" data-key="previewDblClickToggle" data-keywords="双击 预览 分屏 切换 视图 double click preview split toggle">
              <div class="info"><div class="t">{{ $t('settings.appear.dblToggle') }}</div><div class="d">{{ $t('settings.appear.dblToggleD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.previewDblClickToggle }" @click="store.updateSettings({ previewDblClickToggle: !store.settings.previewDblClickToggle })"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.appear.gTypography') }}</div>
            <div class="row" data-key="editorFontSize" data-keywords="字体 字号 font size 放大 缩小 缩放 zoom">
              <div class="info"><div class="t">{{ $t('settings.appear.fontSize') }}</div><div class="d">{{ $t('settings.appear.fontSizeD', { mod: modKey }) }}</div></div>
              <a-select
                :value="store.settings.editorFontSize"
                style="width: 120px;"
                :options="fontSizeOptions"
                @change="(v: number) => store.updateSettings({ editorFontSize: v })"
              />
            </div>
            <div class="row" data-key="editorMaxWidth" data-keywords="预览 内容栏 宽度 排版 限宽 max width 铺满 preview width">
              <div class="info"><div class="t">{{ $t('settings.appear.maxWidth') }}</div><div class="d">{{ $t('settings.appear.maxWidthD') }}</div></div>
              <a-select
                :value="store.settings.editorMaxWidth"
                style="width: 120px;"
                :options="maxWidthOptions"
                @change="(v: number) => store.updateSettings({ editorMaxWidth: v })"
              />
            </div>
            <div class="row" data-key="previewFontFamily" data-keywords="预览 字体 字形 font 衬线 无衬线 等宽 serif sans mono family">
              <div class="info"><div class="t">{{ $t('settings.appear.previewFont') }}</div><div class="d">{{ $t('settings.appear.previewFontD') }}</div></div>
              <a-select
                :value="store.settings.previewFontFamily"
                style="width: 200px;"
                :options="previewFontOptions"
                @change="(v: string) => store.updateSettings({ previewFontFamily: v })"
              />
            </div>
            <div class="row" data-key="previewLineHeight" data-keywords="预览 行间距 行高 排版 间距 line height leading spacing">
              <div class="info"><div class="t">{{ $t('settings.appear.lineHeight') }}</div><div class="d">{{ $t('settings.appear.lineHeightD') }}</div></div>
              <a-select
                :value="store.settings.previewLineHeight"
                style="width: 140px;"
                :options="lineHeightOptions"
                @change="(v: number) => store.updateSettings({ previewLineHeight: v })"
              />
            </div>
            <div class="type-preview-wrap">
              <div class="type-preview-label">{{ $t('settings.appear.previewEffect') }}</div>
              <div class="md type-preview" :style="{ '--preview-font-family': previewFontStack, '--preview-line-height': String(store.settings.previewLineHeight), 'font-size': (store.settings.editorFontSize + 1) + 'px' }">
                <h3>{{ $t('settings.appear.typeSampleTitle') }}</h3>
                <p>{{ $t('settings.appear.typeSampleBody') }}</p>
                <ul>
                  <li>{{ $t('settings.appear.typeSampleLi1') }}</li>
                  <li>{{ $t('settings.appear.typeSampleLi2') }}</li>
                </ul>
              </div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.appear.gCode') }}</div>
            <div class="row" data-key="codeTheme" data-keywords="代码块 高亮 配色 预览 github monokai dracula code highlight">
              <div class="info"><div class="t">{{ $t('settings.appear.codeTheme') }}</div><div class="d">{{ $t('settings.appear.codeThemeD') }}</div></div>
              <a-select
                :value="store.settings.codeTheme"
                style="width: 160px;"
                :options="[
                  { label: 'GitHub Light', value: 'github' },
                  { label: 'Monokai', value: 'monokai' },
                  { label: 'Dracula', value: 'dracula' },
                  { label: 'Atom One Dark', value: 'atom-one-dark' },
                ]"
                @change="(v: string) => store.updateSettings({ codeTheme: v as 'github' | 'monokai' | 'dracula' | 'atom-one-dark' })"
              />
            </div>
            <div class="row" data-key="codeIndent" data-keywords="代码块 tab 缩进 空格 tab indent">
              <div class="info"><div class="t">{{ $t('settings.appear.codeIndent') }}</div><div class="d">{{ $t('settings.appear.codeIndentD') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.codeIndent === 2 }" @click="store.updateSettings({ codeIndent: 2 })">{{ $t('settings.appear.indent2') }}</span>
                <span :class="{ on: store.settings.codeIndent === 4 }" @click="store.updateSettings({ codeIndent: 4 })">{{ $t('settings.appear.indent4') }}</span>
              </div>
            </div>
            <div class="code-preview-wrap">
              <div class="code-preview-label">{{ $t('settings.appear.previewEffect') }}</div>
              <pre class="code-preview" :class="`hljs-theme-${store.settings.codeTheme}`" :style="{ '--code-indent': store.settings.codeIndent }"><code class="hljs language-javascript" v-html="previewCodeHtml"></code></pre>
            </div>
          </div>
        </div>

        <!-- 安全与加密 -->
        <div v-show="active === 'security'" class="s-pane" data-pane="security">
          <h2>{{ $t('settings.security.h2') }}</h2>
          <div class="desc">{{ $t('settings.security.desc') }}</div>

          <div class="group">
            <div class="group-head">{{ $t('settings.security.gScheme') }}</div>
            <div class="row" data-key="encScheme" data-keywords="加密 机制 说明 常开 不可关闭 端到端 防篡改 scheme detail">
              <div class="info"><div class="t">{{ $t('settings.security.schemeSummary') }}</div><div class="d">{{ $t('settings.security.schemeSummaryD') }}</div></div>
              <button class="btn btn-sm" @click="schemeOpen = !schemeOpen">{{ schemeOpen ? $t('settings.security.schemeHide') : $t('settings.security.schemeShow') }}</button>
            </div>
            <div v-show="schemeOpen" class="row" data-key="e2e" data-keywords="端到端 加密 密文 上传 end-to-end encrypt upload">
              <div class="info"><div class="t">{{ $t('settings.security.e2e') }}</div><div class="d">{{ $t('settings.security.e2eD') }}</div></div>
              <span class="tag tag-green">{{ $t('settings.security.enabledTag') }}</span>
            </div>
            <div v-show="schemeOpen" class="row" data-key="tamper" data-keywords="防篡改 校验 损坏 篡改 tamper check corrupt">
              <div class="info"><div class="t">{{ $t('settings.security.tamper') }}</div><div class="d">{{ $t('settings.security.tamperD') }}</div></div>
              <span class="tag tag-green">{{ $t('settings.security.enabledTag') }}</span>
            </div>
            <div v-show="schemeOpen" class="row" data-key="perFileKey" data-keywords="独立 密钥 文件密钥 泄露 per-file key leak">
              <div class="info"><div class="t">{{ $t('settings.security.perFileKey') }}</div><div class="d">{{ $t('settings.security.perFileKeyD') }}</div></div>
              <span class="tag tag-green">{{ $t('settings.security.enabledTag') }}</span>
            </div>
            <div v-show="schemeOpen" class="row" data-key="bruteForce" data-keywords="抗暴力 破解 派生 密码猜测 brute force derive guess">
              <div class="info"><div class="t">{{ $t('settings.security.bruteForce') }}</div><div class="d">{{ $t('settings.security.bruteForceD') }}</div></div>
              <span class="tag tag-green">{{ $t('settings.security.enabledTag') }}</span>
            </div>
          </div>

          <div class="group">
            <div class="group-head">{{ $t('settings.security.gLock') }}</div>
            <div class="row" data-key="autoLockSecs" data-keywords="自动锁定 无操作 闲置 超时 auto lock idle">
              <div class="info"><div class="t">{{ $t('settings.security.autoLock') }}</div><div class="d">{{ $t('settings.security.autoLockD') }}</div></div>
              <a-select
                :value="store.settings.autoLockSecs"
                style="width: 150px;"
                :options="autoLockOptions"
                @change="(v: number) => store.updateSettings({ autoLockSecs: v })"
              />
            </div>
            <div class="row" data-key="lockOnMinimize" data-keywords="最小化 锁定 窗口 minimize lock window">
              <div class="info"><div class="t">{{ $t('settings.security.lockOnMinimize') }}</div></div>
              <div class="switch" :class="{ on: store.settings.lockOnMinimize }" @click="store.updateSettings({ lockOnMinimize: !store.settings.lockOnMinimize })"></div>
            </div>
            <div class="row" data-key="lockOnSleep" data-keywords="休眠 锁屏 睡眠 锁定 sleep screen lock">
              <div class="info"><div class="t">{{ $t('settings.security.lockOnSleep') }}</div></div>
              <div class="switch" :class="{ on: store.settings.lockOnSleep }" @click="store.updateSettings({ lockOnSleep: !store.settings.lockOnSleep })"></div>
            </div>
            <div class="row" data-key="clearClipboardOnLock" data-keywords="剪贴板 复制 清除 粘贴 clipboard copy clear paste">
              <div class="info"><div class="t">{{ $t('settings.security.clearClipboard') }}</div><div class="d">{{ $t('settings.security.clearClipboardD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.clearClipboardOnLock }" @click="store.updateSettings({ clearClipboardOnLock: !store.settings.clearClipboardOnLock })"></div>
            </div>
          </div>

          <div class="group">
            <div class="group-head">{{ $t('settings.security.gData') }}</div>
            <div class="row" data-key="backupEnabled" data-keywords="备份 上一版 恢复 崩溃 半写 backup restore recover crash atomic">
              <div class="info"><div class="t">{{ $t('settings.security.backup') }}</div><div class="d">{{ $t('settings.security.backupD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.backupEnabled }" @click="store.updateSettings({ backupEnabled: !store.settings.backupEnabled })"></div>
            </div>
          </div>

          <div class="group danger-zone">
            <div class="group-head">{{ $t('settings.security.gDanger') }}</div>
            <div class="row" data-key="resetAll" data-keywords="重置 清空 恢复默认 配置 reset clear default">
              <div class="info"><div class="t">{{ $t('settings.security.reset') }}</div><div class="d">{{ $t('settings.security.resetD') }}</div></div>
              <button class="btn btn-danger btn-sm" @click="resetModalOpen = true">{{ $t('settings.security.resetBtn') }}</button>
            </div>
          </div>
        </div>

        <!-- 快捷键 -->
        <div v-show="active === 'keys'" class="s-pane" data-pane="keys">
          <h2>{{ $t('settings.keys.h2') }}</h2>
          <div class="desc">{{ $t('settings.keys.desc') }}</div>
          <div class="group">
            <div class="group-head">{{ $t('settings.keys.gFile') }}</div>
            <div class="row" data-key="kb-save" data-keywords="保存 save 文件">
              <div class="info"><div class="t">{{ $t('settings.keys.save') }}</div><div class="d">{{ modKey }} + S</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">S</span></span>
            </div>
            <div class="row" data-key="kb-new" data-keywords="新建 new 文件 草稿">
              <div class="info"><div class="t">{{ $t('settings.keys.new') }}</div><div class="d">{{ modKey }} + N</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">N</span></span>
            </div>
            <div class="row" data-key="kb-open" data-keywords="打开 open 文件">
              <div class="info"><div class="t">{{ $t('settings.keys.open') }}</div><div class="d">{{ modKey }} + O</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">O</span></span>
            </div>
            <div class="row" data-key="kb-close" data-keywords="关闭页签 close tab">
              <div class="info"><div class="t">{{ $t('settings.keys.closeTab') }}</div><div class="d">{{ modKey }} + W</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">W</span></span>
            </div>
            <div class="row" data-key="kb-reopen" data-keywords="撤销关闭 重新打开 reopen undo 页签">
              <div class="info"><div class="t">{{ $t('settings.keys.reopenTab') }}</div><div class="d">{{ $t('settings.keys.reopenTabD', { mod: modKey }) }}</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">⇧</span> <span class="kbd">T</span></span>
            </div>
            <div class="row" data-key="kb-tabnav" data-keywords="切换页签 上一个 下一个 向前 向后 tab next previous switch">
              <div class="info"><div class="t">{{ $t('settings.keys.tabNav') }}</div><div class="d">{{ $t('settings.keys.tabNavD', { mod: modKey, alt: altKey }) }}</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">{{ altKey }}</span> <span class="kbd">←</span> / <span class="kbd">→</span></span>
            </div>
            <div class="row" data-key="kb-tabnth" data-keywords="第 N 个页签 定位 数字 tab number jump">
              <div class="info"><div class="t">{{ $t('settings.keys.nthTab') }}</div><div class="d">{{ $t('settings.keys.nthTabD', { mod: modKey }) }}</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">1</span> … <span class="kbd">9</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.keys.gSearch') }}</div>
            <div class="row" data-key="kb-find" data-keywords="搜索 替换 find replace">
              <div class="info"><div class="t">{{ $t('settings.keys.find') }}</div><div class="d">{{ modKey }} + F</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">F</span></span>
            </div>
            <div class="row" data-key="kb-gsearch" data-keywords="全局搜索 库 工作目录 search">
              <div class="info"><div class="t">{{ $t('settings.keys.globalSearch') }}</div><div class="d">{{ modKey }} + ⇧ + F</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">⇧</span> <span class="kbd">F</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.keys.gView') }}</div>
            <div class="row" data-key="kb-view" data-keywords="切换 编辑 分屏 预览 视图">
              <div class="info"><div class="t">{{ $t('settings.keys.toggleView') }}</div><div class="d">{{ modKey }} + E</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">E</span></span>
            </div>
            <div class="row" data-key="kb-sidebar" data-keywords="侧边栏 显示 隐藏 栏">
              <div class="info"><div class="t">{{ $t('settings.keys.toggleSidebar') }}</div><div class="d">{{ modKey }} + J</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">J</span></span>
            </div>
            <div class="row" data-key="kb-fontsize" data-keywords="字号 字体 放大 缩小 重置 zoom font">
              <div class="info"><div class="t">{{ $t('settings.keys.fontSizeKeys') }}</div><div class="d">{{ $t('settings.keys.fontSizeKeysD', { mod: modKey }) }}</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">=</span> / <span class="kbd">−</span> / <span class="kbd">0</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.keys.gFormat') }}</div>
            <div class="row" data-key="kb-bold" data-keywords="加粗 bold 粗体">
              <div class="info"><div class="t">{{ $t('settings.keys.bold') }}</div><div class="d">{{ modKey }} + B</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">B</span></span>
            </div>
            <div class="row" data-key="kb-italic" data-keywords="斜体 italic">
              <div class="info"><div class="t">{{ $t('settings.keys.italic') }}</div><div class="d">{{ modKey }} + I</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">I</span></span>
            </div>
            <div class="row" data-key="kb-link" data-keywords="链接 link 插入">
              <div class="info"><div class="t">{{ $t('settings.keys.insertLink') }}</div><div class="d">{{ modKey }} + K</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">K</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.keys.gSecurity') }}</div>
            <div class="row" data-key="kb-lock" data-keywords="锁定 全部锁 安全 lock">
              <div class="info"><div class="t">{{ $t('settings.keys.lockAll') }}</div><div class="d">{{ modKey }} + L</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">L</span></span>
            </div>
          </div>
        </div>

        <!-- 便携便签 -->
        <div v-show="active === 'sticky'" class="s-pane" data-pane="sticky">
          <h2>{{ $t('settings.sticky.h2') }}</h2>
          <div class="desc">{{ $t('settings.sticky.desc') }}</div>
          <div class="group">
            <div class="group-head">{{ $t('settings.sticky.gWindow') }}</div>
            <div class="row" data-key="stickyNote" data-keywords="便签 文件 选择 sticky note file">
              <div class="info"><div class="t">{{ $t('settings.sticky.file') }}</div><div class="d">{{ stickyNoteLabel }}</div></div>
              <div style="display:flex;gap:8px;">
                <a-button size="small" @click="pickStickyFile">{{ $t('settings.sticky.choose') }}</a-button>
                <a-button v-if="store.settings.stickyNote" size="small" danger @click="clearStickyFile">{{ $t('settings.sticky.clear') }}</a-button>
              </div>
            </div>
            <div class="row" data-key="trayStickyButton" data-keywords="托盘 菜单栏 点击 不弹出 左键 右键 便签 弹出 菜单 tray click off left right sticky menu">
              <div class="info"><div class="t">{{ trayT('traySticky') }}</div><div class="d">{{ trayT('trayStickyD') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.trayStickyButton === 'off' }" @click="store.updateSettings({ trayStickyButton: 'off' })">{{ $t('settings.general.trayOff') }}</span>
                <span :class="{ on: store.settings.trayStickyButton === 'left' }" @click="store.updateSettings({ trayStickyButton: 'left' })">{{ $t('settings.general.trayLeft') }}</span>
                <span :class="{ on: store.settings.trayStickyButton === 'right' }" @click="store.updateSettings({ trayStickyButton: 'right' })">{{ $t('settings.general.trayRight') }}</span>
              </div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.sticky.gCorner') }}</div>
            <div class="row" data-key="hotCornerEnabled" data-keywords="热角 角落 唤起 hot corner">
              <div class="info"><div class="t">{{ $t('settings.sticky.cornerOn') }}</div><div class="d">{{ $t('settings.sticky.cornerOnD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.hotCorner.enabled }" @click="store.updateSettings({ hotCorner: { ...store.settings.hotCorner, enabled: !store.settings.hotCorner.enabled } })"></div>
            </div>
            <div class="row" data-key="hotCornerPos" data-keywords="角落 位置 corner">
              <div class="info"><div class="t">{{ $t('settings.sticky.cornerPos') }}</div><div class="d">{{ $t('settings.sticky.cornerPosD') }}</div></div>
              <a-select
                :value="store.settings.hotCorner.corner"
                style="width: 150px;"
                :options="cornerOptions"
                @change="(v: string) => store.updateSettings({ hotCorner: { ...store.settings.hotCorner, corner: v as 'topLeft' | 'bottomLeft' | 'topRight' | 'bottomRight' } })"
              />
            </div>
            <div class="row" data-key="hotCornerDwell" data-keywords="停留 时长 dwell">
              <div class="info"><div class="t">{{ $t('settings.sticky.dwell') }}</div><div class="d">{{ $t('settings.sticky.dwellD') }}</div></div>
              <a-select
                :value="store.settings.hotCorner.dwellMs"
                style="width: 150px;"
                :options="dwellOptions"
                @change="(v: number) => store.updateSettings({ hotCorner: { ...store.settings.hotCorner, dwellMs: v } })"
              />
            </div>
            <div class="row" data-key="hotCornerRequireMod" data-keywords="修饰键 按住 快捷键 modifier cmd">
              <div class="info"><div class="t">{{ $t('settings.sticky.modOn') }}</div><div class="d">{{ $t('settings.sticky.modOnD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.hotCorner.requireModifier }" @click="store.updateSettings({ hotCorner: { ...store.settings.hotCorner, requireModifier: !store.settings.hotCorner.requireModifier } })"></div>
            </div>
            <div v-if="store.settings.hotCorner.requireModifier" class="row" data-key="hotCornerModifier" data-keywords="修饰键 选择 modifier">
              <div class="info"><div class="t">{{ $t('settings.sticky.modKey') }}</div><div class="d">{{ $t('settings.sticky.modKeyD') }}</div></div>
              <a-select
                :value="store.settings.hotCorner.modifier"
                style="width: 150px;"
                :options="modifierOptions"
                @change="(v: string) => store.updateSettings({ hotCorner: { ...store.settings.hotCorner, modifier: v as 'command' | 'shift' | 'control' | 'alt' } })"
              />
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.sticky.gLaunch') }}</div>
            <div class="row" data-key="stickyLaunchDisplay" data-keywords="快速启动 显示 图标 文字 launch display icon text">
              <div class="info"><div class="t">{{ $t('settings.sticky.launchDisplay') }}</div><div class="d">{{ $t('settings.sticky.launchDisplayD') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.stickyLaunchDisplay === 'icon' }" @click="store.updateSettings({ stickyLaunchDisplay: 'icon' })">{{ $t('settings.sticky.launchDisplayIcon') }}</span>
                <span :class="{ on: store.settings.stickyLaunchDisplay === 'iconText' }" @click="store.updateSettings({ stickyLaunchDisplay: 'iconText' })">{{ $t('settings.sticky.launchDisplayIconText') }}</span>
              </div>
            </div>
            <div v-for="(item, i) in store.settings.quickLaunch" :key="item.path" class="row">
              <img v-if="iconMap[item.path]" :src="iconMap[item.path]" class="ql-ico" alt="" />
              <div v-else class="ql-ico ql-ico-ph" aria-hidden="true"></div>
              <div class="info">
                <input
                  v-if="editingLaunch === i"
                  ref="launchNameInput"
                  class="ql-rename"
                  :value="editingName"
                  @input="editingName = ($event.target as HTMLInputElement).value"
                  @keydown.enter="commitLaunchEdit(i)"
                  @keydown.esc="cancelLaunchEdit()"
                  @blur="commitLaunchEdit(i)"
                />
                <div v-else class="t" :title="$t('settings.sticky.renameD')" @dblclick="startEditLaunch(i)">{{ item.name }}</div>
                <div class="d">{{ item.path }}</div>
              </div>
              <a-button size="small" @click="startEditLaunch(i)">{{ $t('settings.sticky.rename') }}</a-button>
              <a-button size="small" danger @click="removeLaunch(i)">{{ $t('settings.sticky.remove') }}</a-button>
            </div>
            <div class="row" data-key="quickLaunch" data-keywords="快速启动 应用 launch">
              <div class="info"><div class="t">{{ $t('settings.sticky.addLaunch') }}</div><div class="d">{{ $t('settings.sticky.addLaunchD') }}</div></div>
              <a-button size="small" @click="addLaunch">{{ $t('settings.sticky.choose') }}</a-button>
            </div>
          </div>
        </div>

        <!-- 关于 -->
        <div v-show="active === 'about'" class="s-pane" data-pane="about">
          <AboutCard />
        </div>
      </div>
    </div>

    <!-- 重置所有配置：二次确认 -->
    <a-modal
      v-model:open="resetModalOpen"
      :title="$t('settings.resetModal.title')"
      :width="460"
      :footer="null"
      :mask-closable="!resetting"
      :closable="!resetting"
    >
      <div class="reset-box">
        <div class="reset-lead">{{ $t('settings.resetModal.lead') }}</div>
        <ul class="reset-list">
          <li>{{ $t('settings.resetModal.li1') }}</li>
          <li>{{ $t('settings.resetModal.li2') }}</li>
          <li>{{ $t('settings.resetModal.li3') }}</li>
          <li>{{ $t('settings.resetModal.li4') }}</li>
        </ul>
        <div class="reset-note">{{ $t('settings.resetModal.note') }}</div>
        <div v-if="store.recent.length" class="reset-warn">{{ $t('settings.resetModal.warnUnlocked', { count: unlockedVaultCount }) }}</div>
        <div v-if="unsavedCount" class="reset-warn">{{ $t('settings.resetModal.warnUnsaved', { count: unsavedCount }) }}</div>
        <div class="modal-foot">
          <button class="btn" :disabled="resetting" @click="resetModalOpen = false">{{ $t('settings.resetModal.cancel') }}</button>
          <button class="btn btn-danger" :disabled="resetting" @click="submitReset">{{ resetting ? $t('settings.resetModal.resetting') : $t('settings.resetModal.confirm') }}</button>
        </div>
      </div>
    </a-modal>

    <!-- 自定义皮肤编辑器：色板改令牌 + 高级 CSS 兜底，改动即时预览（见 lib/skins.ts previewCustomSkin） -->
    <a-modal
      :open="skinModalOpen"
      :title="$t('skin.editorTitle')"
      :width="780"
      :footer="null"
      :mask-closable="false"
      @cancel="cancelSkinDraft"
    >
      <div class="skin-edit" v-if="skinDraft">
        <div class="se-row">
          <label>{{ $t('skin.name') }}</label>
          <input class="se-in se-name" v-model="skinDraft.name" :placeholder="$t('skin.defaultName')" />
          <span class="se-spacer"></span>
          <label>{{ $t('skin.editMode') }}</label>
          <div class="seg">
            <span :class="{ on: skinDraftMode === 'light' }" @click="skinDraftMode = 'light'">{{ $t('settings.appear.light') }}</span>
            <span :class="{ on: skinDraftMode === 'dark' }" @click="skinDraftMode = 'dark'">{{ $t('settings.appear.dark') }}</span>
          </div>
          <button class="btn btn-sm" :title="$t('skin.copyToOtherD')" @click="copyTokensToOtherMode">{{ $t('skin.copyToOther') }}</button>
        </div>
        <div class="se-hint">{{ $t('skin.tokenHint') }}</div>
        <div class="se-tokens">
          <div v-for="g in skinGroups" :key="g.id" class="se-group">
            <div class="se-cap">{{ $t('skin.group.' + g.id) }}</div>
            <div class="se-list">
              <div v-for="k in g.tokens" :key="k" class="se-tk">
                <input type="color" class="se-pick" :value="skinTokenHex(k)" @input="onSkinTokenPick(k, $event)" />
                <span class="se-lb"><b>{{ k }}</b><em>{{ skinTokenLabel(k) }}</em></span>
                <input class="se-in se-val" :value="skinDraftTokens[k] || ''" :placeholder="$t('skin.inherit')" @change="onSkinTokenText(k, $event)" />
                <button v-if="skinDraftTokens[k]" class="icon-btn se-clr" :title="$t('skin.clearToken')" @click="setSkinToken(k, '')">✕</button>
                <span v-else class="se-clr-ph"></span>
              </div>
            </div>
          </div>
        </div>
        <div class="se-row se-css-row">
          <label>{{ $t('skin.advCss') }}</label>
          <textarea class="se-in se-css" v-model="skinDraft.css" spellcheck="false" :placeholder="$t('skin.advCssPh')"></textarea>
        </div>
        <div class="se-note">{{ $t('skin.advCssD') }}</div>
        <div class="modal-foot">
          <button class="btn" @click="cancelSkinDraft">{{ $t('common.cancel') }}</button>
          <button class="btn btn-primary" @click="saveSkinDraft">{{ $t('skin.save') }}</button>
        </div>
      </div>
    </a-modal>

    <!-- 删除自定义皮肤确认 -->
    <a-modal
      :open="!!skinDeleteTarget"
      :title="$t('skin.deleteTitle')"
      :width="420"
      :footer="null"
      @cancel="skinDeleteId = ''"
    >
      <div class="reset-box">
        <div class="reset-lead">{{ $t('skin.deleteLead', { name: skinDeleteTarget ? skinDeleteTarget.name : '' }) }}</div>
        <div class="modal-foot">
          <button class="btn" @click="skinDeleteId = ''">{{ $t('common.cancel') }}</button>
          <button class="btn btn-danger" @click="confirmDeleteSkin">{{ $t('skin.deleteBtn') }}</button>
        </div>
      </div>
    </a-modal>
  </div>
</template>

<style scoped>
.settings { flex: 1; display: flex; min-height: 0; }

.back {
  display: inline-flex; align-items: center; gap: 5px;
  height: 26px; padding: 0 10px 0 8px; border-radius: 7px;
  border: 1px solid var(--border); background: var(--bg);
  color: var(--text-2); font-size: 12.5px; font-weight: 500;
  cursor: pointer; transition: all 0.2s;
}
.back:hover { background: var(--primary-bg); border-color: var(--primary-hover); color: var(--primary-active); }
.back .back-txt { line-height: 1; }

.s-nav {
  width: 255px; flex: 0 0 255px;
  background: var(--bg);
  border-right: 1px solid var(--border-2);
  padding: 14px 10px;
  position: relative;
}
.s-nav .cap { font-size: 11px; color: var(--text-4); letter-spacing: 0.5px; padding: 0 10px 8px; }

/* 设置搜索框 */
.s-search {
  display: flex; align-items: center; gap: 7px;
  height: 30px; padding: 0 9px; margin-bottom: 10px;
  border: 1px solid var(--border); border-radius: var(--radius-sm);
  background: var(--panel); color: var(--text-4);
}
.s-search:focus-within { border-color: var(--primary-hover); }
.s-search input {
  flex: 1; min-width: 0; border: none; outline: none; background: transparent;
  font-size: 13px; color: var(--text);
}
.s-search input::placeholder { color: var(--text-4); }
.s-search .clear { display: inline-flex; align-items: center; cursor: pointer; color: var(--text-4); padding: 2px; }
.s-search .clear:hover { color: var(--text-2); }

/* 搜索候选面板：覆盖菜单项列表 */
.s-results {
  position: absolute; left: 10px; right: 4px; top: 56px; bottom: 12px;
  overflow-y: auto;
  background: var(--panel);
  display: flex; flex-direction: column; gap: 2px;
}
.s-result {
  padding: 7px 10px; border-radius: var(--radius-sm); cursor: pointer;
}
.s-result:hover { background: var(--hover); }
.s-result .rt { font-size: 13px; color: var(--text); }
.s-result .rp { font-size: 11px; color: var(--text-4); margin-top: 1px; }
.s-result-empty { padding: 10px; font-size: 12.5px; color: var(--text-4); }
.s-item {
  display: flex; align-items: center; gap: 9px;
  height: 34px; padding: 0 10px;
  white-space: nowrap;
  border-radius: var(--radius-sm);
  font-size: 13.5px; color: var(--text-2);
  cursor: pointer; margin-bottom: 2px;
}
.s-item:hover { background: var(--hover); }
.s-item.on { background: var(--primary-bg); color: var(--primary-active); font-weight: 500; }

.s-body { flex: 1; overflow-y: auto; padding: 26px 38px 60px; }
.s-pane { max-width: 720px; }
.s-pane h2 { font-size: 18px; font-weight: 600; margin-bottom: 4px; }
.s-pane .desc { font-size: 13px; color: var(--text-3); margin-bottom: 20px; }

.group {
  border: 1px solid var(--border-2);
  border-radius: var(--radius);
  margin-bottom: 18px;
  overflow: hidden;
}
.group-head {
  padding: 8px 16px;
  font-size: 12px; font-weight: 600; color: var(--text-3);
  background: var(--hover);
  border-bottom: 1px solid var(--border-2);
  letter-spacing: 0.5px;
}
.row {
  display: flex; align-items: center; gap: 16px;
  padding: 8px 16px;
  border-bottom: 1px solid var(--border-2);
}
.row:last-child { border-bottom: none; }
.row .info { flex: 1; min-width: 0; }
.row .info .t { font-size: 14px; }
.row .info .d { font-size: 12px; color: var(--text-3); margin-top: 2px; }
/* 「界面元素」：左侧小标题 + 右侧一行一项的勾选清单（勾选框与名称同色，说明作次要信息跟在后面） */
.area { display: flex; align-items: flex-start; gap: 10px; border-bottom: 1px solid var(--border-2); }
.area-label {
  flex: none; width: 82px; padding: 10px 0 8px 16px;
  font-size: 12px; font-weight: 600; color: var(--text-3);
}
.area-rows { flex: 1; min-width: 0; padding: 2px 0 4px; }
.row-check { cursor: pointer; gap: 10px; padding-left: 6px; }
.row-check:hover { background: var(--hover); }
.row-check input {
  width: 15px; height: 15px; margin: 0; flex: none;
  accent-color: var(--primary); cursor: pointer;
}
.row-check .t { font-size: 14px; flex: none; width: 132px; }
.row-check .d { font-size: 12px; color: var(--text-3); flex: 1; min-width: 0; margin-top: 0; }
/* 快速启动项图标缩略图；无图标时用同尺寸空占位保持行对齐 */
.ql-ico { width: 32px; height: 32px; object-fit: contain; flex: none; }
.ql-ico-ph { border-radius: 7px; background: var(--hover); }
/* 内联重命名输入框：贴合原 .t 字号，复用主题变量 */
.ql-rename {
  width: 100%; max-width: 360px; font-size: 14px;
  padding: 2px 8px; border: 1px solid var(--border-pop); border-radius: 6px;
  background: var(--panel); color: var(--text); outline: none;
}
.ql-rename:focus { border-color: var(--primary); }
.row .info .t { cursor: text; }
.row .info .t:hover { color: var(--primary); }

.fake-select {
  display: inline-flex; align-items: center; justify-content: space-between; gap: 10px;
  min-width: 180px; height: 30px; padding: 0 10px;
  border: 1px solid var(--border); border-radius: var(--radius-sm);
  font-size: 13px; cursor: pointer; background: var(--panel);
}
.fake-select:hover { border-color: var(--primary-hover); }

.danger-zone { border-color: #ffccc7; }
.danger-zone .group-head { background: var(--red-bg); color: #cf1322; }

/* 关于页内容已抽至 src/components/AboutCard.vue（与 App.vue 弹出的独立「关于」对话框共用） */

/* 搜索命中行高亮 / 未命中行淡化 / 跳转定位闪烁 */
.row.hit { background: var(--primary-bg); }
.row.dim { opacity: 0.35; }
.row.flash { animation: row-flash 1.2s ease; }
@keyframes row-flash {
  0% { background: var(--primary-bg); }
  70% { background: var(--primary-bg); }
  100% { background: transparent; }
}

/* 开关 */
.switch {
  position: relative;
  width: 44px; height: 22px;
  background: rgba(0, 0, 0, 0.25);
  border-radius: 11px;
  cursor: pointer;
  transition: background 0.2s;
  flex: 0 0 auto;
}
.switch::after {
  content: "";
  position: absolute;
  top: 2px; left: 2px;
  width: 18px; height: 18px;
  background: #fff;
  border-radius: 50%;
  transition: transform 0.2s;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.2);
}
.switch.on { background: var(--primary); }
.switch.on::after { transform: translateX(22px); }
.switch.disabled { cursor: not-allowed; opacity: 0.45; }

/* 「即将推出」标签 */
.soon {
  flex: 0 0 auto;
  font-size: 10px; color: var(--text-4);
  padding: 1px 6px; border-radius: 4px;
  background: var(--hover);
  margin-right: 8px;
}

/* 分段选择器 */
.seg { display: inline-flex; padding: 2px; background: var(--hover); border-radius: var(--radius-sm); gap: 2px; }
.seg span { padding: 2px 12px; font-size: 13px; color: var(--text-2); border-radius: 5px; cursor: pointer; transition: all 0.2s; }
.seg span.on { background: var(--panel); color: var(--text); font-weight: 500; box-shadow: 0 2px 4px rgba(0, 0, 0, 0.06); }

/* 按钮 */
.btn {
  display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  height: 32px; padding: 0 15px; font-size: 14px; border-radius: var(--radius-sm);
  border: 1px solid var(--border); background: var(--panel); color: var(--text);
  cursor: pointer; transition: all 0.2s; white-space: nowrap;
}
.btn:hover { color: var(--primary-hover); border-color: var(--primary-hover); }
.btn-danger { color: var(--red); border-color: var(--red); }
.btn-danger:hover { color: #fff; background: var(--red); border-color: var(--red); }
.btn-sm { height: 24px; padding: 0 8px; font-size: 12px; }
.btn-primary { background: var(--primary); border-color: var(--primary); color: #fff; }
.btn-primary:hover { background: var(--primary-hover); border-color: var(--primary-hover); color: #fff; }

/* 重置确认弹窗 */
.modal-foot { display: flex; justify-content: flex-end; gap: 8px; padding-top: 14px; }
.reset-box { padding-top: 4px; }
.reset-lead { font-size: 13px; color: var(--text-2); margin-bottom: 8px; }
.reset-list {
  margin: 0 0 12px; padding-left: 18px;
  font-size: 12.5px; color: var(--text-3); line-height: 1.9;
}
.reset-note {
  font-size: 12px; color: var(--text-3);
  padding: 8px 10px; border-radius: var(--radius-sm);
  background: var(--hover);
}
.reset-warn {
  margin-top: 8px;
  font-size: 12px; color: var(--red);
  padding: 8px 10px; border-radius: var(--radius-sm);
  background: var(--red-bg);
}

/* 代码主题预览 */
.code-preview-wrap { 
  /* margin-top: 16px; */
  padding: 8px 16px;
}
.code-preview-label { font-size: 12px; color: var(--text-3); margin-bottom: 8px; }
.code-preview {
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 12px 16px;
  overflow-x: auto;
  font-family: var(--mono);
  font-size: 13px;
  line-height: 1.6;
  tab-size: var(--code-indent, 4);
  margin: 0;
}
.code-preview code {
  background: transparent;
  padding: 0;
  font-family: inherit;
  font-size: inherit;
}

/* 排版实时示例：复用全局 .md 样式，绑定与预览区相同的 CSS 变量，改字体/行距/字号即时反映 */
.type-preview-wrap { padding: 8px 16px; }
.type-preview-label { font-size: 12px; color: var(--text-3); margin-bottom: 8px; }
.type-preview {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--editor-bg);
  /* 覆盖全局 .md 的大内边距（特异性 0,2,0 > .md 的 0,1,0，无需 !important） */
  padding: 4px 16px 10px;
  max-height: 280px;
  overflow-y: auto;
}

/* ---------- 皮肤（配色方案） ---------- */
/* 色卡网格不是 .row，它下面那行（语言）会少一条分隔线，这里补上 */
.skin-grid + .row { border-top: 1px solid var(--border-2); }
.skin-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 8px;
  padding: 12px 16px 16px;
}
.skin-card {
  position: relative;
  padding: 7px;
  border: 1px solid var(--border-2);
  border-radius: var(--radius);
  background: var(--panel);
  cursor: pointer;
  transition: border-color 0.2s, box-shadow 0.2s;
}
.skin-card:hover { border-color: var(--border-pop); }
.skin-card.on { border-color: var(--primary); box-shadow: 0 0 0 2px var(--primary-bg); }
/* 色卡：外框=页面底色，左块=面板色，右下条=主色，一眼看出整套配色倾向 */
.skin-card .sw {
  display: block;
  position: relative;
  height: 36px;
  overflow: hidden;
  border: 1px solid var(--border-2);
  border-radius: var(--radius-sm);
}
.skin-card .sw i { position: absolute; left: 6px; top: 6px; bottom: 6px; width: 36%; border-radius: 3px; }
.skin-card .sw b { position: absolute; right: 8px; bottom: 8px; width: 36%; height: 6px; border-radius: 3px; }
.skin-card .nm {
  display: block;
  margin-top: 6px;
  font-size: 12.5px;
  color: var(--text-2);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.skin-card.on .nm { color: var(--text); font-weight: 500; }
/* 勾选标记压在色卡右上角，名称整行可用 */
.skin-ck { position: absolute; right: 13px; top: 13px; font-size: 12px; color: var(--primary); }
/* 色卡悬浮操作按钮（复制 / 编辑 / 删除）：悬停才显现，垫底色以免压住名称 */
.skin-acts { position: absolute; right: 6px; bottom: 4px; display: none; gap: 2px; padding: 0 2px; background: var(--panel); border-radius: 6px; }
.skin-card:hover .skin-acts { display: inline-flex; }

/* 自定义皮肤编辑器 */
.skin-edit { padding-top: 4px; }
.se-row { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; }
.se-row > label { flex: none; font-size: 13px; color: var(--text-2); }
.se-spacer { flex: 1; }
.se-in {
  padding: 3px 8px;
  border: 1px solid var(--border-pop);
  border-radius: var(--radius-sm);
  background: var(--panel);
  color: var(--text);
  font-size: 13px;
  outline: none;
}
.se-in:focus { border-color: var(--primary); }
.se-in::placeholder { color: var(--text-4); }
.se-name { width: 190px; }
.se-hint { font-size: 12px; color: var(--text-3); margin-bottom: 8px; }
.se-tokens {
  max-height: 44vh;
  overflow-y: auto;
  padding: 2px 12px 10px;
  border: 1px solid var(--border-2);
  border-radius: var(--radius);
}
.se-cap { padding: 9px 0 6px; font-size: 12px; font-weight: 600; color: var(--text-3); }
.se-list { display: grid; grid-template-columns: 1fr 1fr; gap: 6px 18px; }
.se-tk { display: grid; grid-template-columns: 26px minmax(0, 1fr) 112px 22px; align-items: center; gap: 6px; }
.se-pick {
  width: 26px; height: 24px; padding: 0;
  border: 1px solid var(--border); border-radius: 5px;
  background: none; cursor: pointer;
}
.se-lb { display: flex; flex-direction: column; min-width: 0; line-height: 1.3; }
.se-lb b { font-family: var(--mono); font-size: 11px; font-weight: 500; color: var(--text-2); }
.se-lb em {
  font-style: normal; font-size: 11px; color: var(--text-4);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.se-val { height: 24px; padding: 0 6px; font-family: var(--mono); font-size: 12px; }
.se-clr { width: 20px; height: 20px; font-size: 11px; color: var(--text-4); }
.se-clr-ph { width: 20px; }
.se-css-row { align-items: flex-start; margin-top: 12px; }
.se-css-row > label { padding-top: 5px; }
.se-css {
  flex: 1;
  min-height: 110px;
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 12px;
  line-height: 1.6;
  resize: vertical;
}
.se-note { font-size: 12px; color: var(--text-3); margin-top: 6px; }
</style>
