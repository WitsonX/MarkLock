<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getVersion } from '@tauri-apps/api/app'
import TitleBar from '../components/TitleBar.vue'
import { useVaultStore } from '../stores/vault'
import { lang } from '../locales'
import * as tauri from '../lib/tauri'
import { LINKS } from '../lib/links'
import hljs from 'highlight.js'
// 关于页品牌 logo：与 Dock 图标同一光栅化流程生成的白底版（主图标源文件保持 #EAF2FE 不动）
import aboutIcon from '../assets/about-icon.png'

interface NavItem {
  key: string
  label: string
  core?: boolean
  icon: string
}

export default defineComponent({
  name: 'SettingsView',
  components: { TitleBar },
  data() {
    return {
      aboutIcon,
      active: 'general',
      // 关于页版本号：运行时从 Tauri 读取（源自 tauri.conf.json），避免在文案里写死
      appVersion: '',
      // 原生菜单「关于」事件解绑句柄
      _unlistenMenu: null as UnlistenFn | null,
      // 设置搜索关键词
      searchQuery: '',
      // 「重置所有配置」确认弹窗
      resetModalOpen: false,
      resetting: false,
      // 编辑器字体大小选项（px，与 ⌘+/⌘- 的 2px 步进对齐，10–28）
      fontSizeOptions: Array.from({ length: 10 }, (_, i) => ({ label: `${10 + i * 2} px`, value: 10 + i * 2 })),
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
    /** 关于页展示与跳转的项目链接 */
    links() {
      return LINKS
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
  },
  mounted() {
    this.buildSearchIndex()
    // 读取真实应用版本用于「关于」页展示；非 Tauri 环境（纯 web dev）下静默降级
    getVersion().then((v) => (this.appVersion = v)).catch(() => {})
    // 支持通过路由 query（如原生菜单「关于」跳入）直达指定页签
    const tab = this.$route.query.tab
    if (typeof tab === 'string' && this.nav.some((n) => n.key === tab)) this.active = tab
    // 已在设置页时，原生菜单「关于」直接切到关于页签（App.vue 此时不再重复 push）
    listen<string>('marklock://menu', (e) => {
      if (e.payload === 'menu:about') this.active = 'about'
    }).then((un) => {
      this._unlistenMenu = un
    })
  },
  beforeUnmount() {
    ;(this as any)._searchIndex = null
    if (this._unlistenMenu) this._unlistenMenu()
  },
  watch: {
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
    /** 关于页外部链接：用系统浏览器打开（链接集中定义于 src/lib/links.ts）。 */
    openLink(url: string) {
      tauri.openUrl(url).catch((err) => message.error(String(err)))
    },
    /** 从 DOM 收集全部设置行，构建搜索索引（标题/描述/关键词/分组/分类） */
    buildSearchIndex() {
      const rows = Array.from(document.querySelectorAll('.s-pane .row[data-key]'))
      const index = rows.map((el) => {
        const pane = el.closest('.s-pane') as HTMLElement
        let groupLabel = ''
        let p = el.previousElementSibling
        while (p) {
          if (p.classList && p.classList.contains('group-head')) { groupLabel = p.textContent!.trim(); break }
          p = p.previousElementSibling
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
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gTabs') }}</div>
            <div class="row" data-key="showTabs" data-keywords="页签 tab 标签栏 顶部 tabs bar">
              <div class="info"><div class="t">{{ $t('settings.general.showTabs') }}</div><div class="d">{{ $t('settings.general.showTabsD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showTabs }" @click="store.updateSettings({ showTabs: !store.settings.showTabs })"></div>
            </div>
            <div class="row" data-key="watchExternalChanges" data-keywords="监控 外部修改 刷新 重载 watch external reload">
              <div class="info"><div class="t">{{ $t('settings.general.watchExternal') }}</div><div class="d">{{ $t('settings.general.watchExternalD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.watchExternalChanges }" @click="store.updateSettings({ watchExternalChanges: !store.settings.watchExternalChanges })"></div>
            </div>
            <div class="row" data-key="autoSaveSecs" data-keywords="自动保存 保存 静默 auto save">
              <div class="info"><div class="t">{{ $t('settings.general.autoSave') }}</div><div class="d">{{ $t('settings.general.autoSaveD') }}</div></div>
              <a-select
                :value="store.settings.autoSaveSecs"
                style="width: 150px;"
                :options="autoSaveOptions"
                @change="(v: number) => store.updateSettings({ autoSaveSecs: v })"
              />
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gSidebar') }}</div>
            <div class="row" data-key="showOpenFiles" data-keywords="侧边栏 打开的文件 open files">
              <div class="info"><div class="t">{{ $t('settings.general.openFiles') }}</div><div class="d">{{ $t('settings.general.openFilesD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showOpenFiles }" @click="store.updateSettings({ showOpenFiles: !store.settings.showOpenFiles })"></div>
            </div>
            <div class="row" data-key="showUnlockedVaults" data-keywords="侧边栏 加密库 库 vaults">
              <div class="info"><div class="t">{{ $t('settings.general.vaults') }}</div><div class="d">{{ $t('settings.general.vaultsD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showUnlockedVaults }" @click="store.updateSettings({ showUnlockedVaults: !store.settings.showUnlockedVaults })"></div>
            </div>
            <div class="row" data-key="showFavorites" data-keywords="侧边栏 收藏 favorites">
              <div class="info"><div class="t">{{ $t('settings.general.favorites') }}</div><div class="d">{{ $t('settings.general.favoritesD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showFavorites }" @click="store.updateSettings({ showFavorites: !store.settings.showFavorites })"></div>
            </div>
            <div class="row" data-key="showRecent" data-keywords="侧边栏 最近打开 recent">
              <div class="info"><div class="t">{{ $t('settings.general.recent') }}</div><div class="d">{{ $t('settings.general.recentD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showRecent }" @click="store.updateSettings({ showRecent: !store.settings.showRecent })"></div>
            </div>
            <div class="row" data-key="showWorkdir" data-keywords="侧边栏 工作目录 文件夹 workdir folder">
              <div class="info"><div class="t">{{ $t('settings.general.workdir') }}</div><div class="d">{{ $t('settings.general.workdirD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showWorkdir }" @click="store.updateSettings({ showWorkdir: !store.settings.showWorkdir })"></div>
            </div>
            <div class="row" data-key="showOutline" data-keywords="侧边栏 大纲 标题 目录 outline heading">
              <div class="info"><div class="t">{{ $t('settings.general.outline') }}</div><div class="d">{{ $t('settings.general.outlineD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showOutline }" @click="store.updateSettings({ showOutline: !store.settings.showOutline })"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gBrowse') }}</div>
            <div class="row" data-key="showHiddenFiles" data-keywords="隐藏文件 文件浏览 目录 hidden files">
              <div class="info"><div class="t">{{ $t('settings.general.hiddenFiles') }}</div><div class="d">{{ $t('settings.general.hiddenFilesD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showHiddenFiles }" @click="store.updateSettings({ showHiddenFiles: !store.settings.showHiddenFiles })"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gConvert') }}</div>
            <div class="row" data-key="convertMdAction" data-keywords="转为加密 mdl 明文 删除 保留 convert encrypt delete keep">
              <div class="info"><div class="t">{{ $t('settings.general.convertMd') }}</div><div class="d">{{ $t('settings.general.convertMdD') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.convertMdAction === 'ask' }" @click="store.updateSettings({ convertMdAction: 'ask' })">{{ $t('settings.general.convertAsk') }}</span>
                <span :class="{ on: store.settings.convertMdAction === 'delete' }" @click="store.updateSettings({ convertMdAction: 'delete' })">{{ $t('settings.general.convertDelete') }}</span>
                <span :class="{ on: store.settings.convertMdAction === 'keep' }" @click="store.updateSettings({ convertMdAction: 'keep' })">{{ $t('settings.general.convertKeep') }}</span>
              </div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">{{ $t('settings.general.gAssoc') }}</div>
            <div class="row" data-key="setDefaultHandler" data-keywords="默认 打开方式 文件关联 md mdl mdlb default handler">
              <div class="info"><div class="t">{{ $t('settings.general.setDefault') }}</div><div class="d">{{ $t('settings.general.setDefaultD') }}</div></div>
              <button class="btn" @click="applySetDefault">{{ $t('settings.general.setDefaultBtn') }}</button>
            </div>
          </div>
        </div>

        <!-- 外观 -->
        <div v-show="active === 'appear'" class="s-pane" data-pane="appear">
          <h2>{{ $t('settings.appear.h2') }}</h2>
          <div class="desc">{{ $t('settings.appear.desc') }}</div>
          <div class="group">
            <div class="group-head">{{ $t('settings.appear.gTheme') }}</div>
            <div class="row" data-key="theme" data-keywords="主题 浅色 深色 暗黑 dark light 跟随系统 theme">
              <div class="info"><div class="t">{{ $t('settings.appear.appearanceMode') }}</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.theme === 'light' }" @click="store.updateSettings({ theme: 'light' })">{{ $t('settings.appear.light') }}</span>
                <span :class="{ on: store.settings.theme === 'dark' }" @click="store.updateSettings({ theme: 'dark' })">{{ $t('settings.appear.dark') }}</span>
                <span :class="{ on: store.settings.theme === 'system' }" @click="store.updateSettings({ theme: 'system' })">{{ $t('settings.appear.followSystem') }}</span>
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
            <div class="row" data-key="codeTheme" data-keywords="代码高亮 配色 github monokai dracula code highlight">
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
            <div class="row" data-key="codeIndent" data-keywords="tab 缩进 空格 代码块 tab indent">
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
          <div class="group">
            <div class="group-head">{{ $t('settings.appear.gEditor') }}</div>
            <div class="row" data-key="showToolbar" data-keywords="工具栏 格式化 按钮 toolbar format">
              <div class="info"><div class="t">{{ $t('settings.appear.toolbar') }}</div><div class="d">{{ $t('settings.appear.toolbarD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showToolbar }" @click="store.updateSettings({ showToolbar: !store.settings.showToolbar })"></div>
            </div>
            <div class="row" data-key="showStatusbar" data-keywords="状态栏 行数 字数 底部 statusbar lines words">
              <div class="info"><div class="t">{{ $t('settings.appear.statusbar') }}</div><div class="d">{{ $t('settings.appear.statusbarD') }}</div></div>
              <div class="switch" :class="{ on: store.settings.showStatusbar }" @click="store.updateSettings({ showStatusbar: !store.settings.showStatusbar })"></div>
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
          </div>
        </div>

        <!-- 安全与加密 -->
        <div v-show="active === 'security'" class="s-pane" data-pane="security">
          <h2>{{ $t('settings.security.h2') }}</h2>
          <div class="desc">{{ $t('settings.security.desc') }}</div>

          <div class="group">
            <div class="group-head">{{ $t('settings.security.gScheme') }}</div>
            <div class="row" data-key="e2e" data-keywords="端到端 加密 密文 上传 end-to-end encrypt upload">
              <div class="info"><div class="t">{{ $t('settings.security.e2e') }}</div><div class="d">{{ $t('settings.security.e2eD') }}</div></div>
              <span class="tag tag-green">{{ $t('settings.security.enabledTag') }}</span>
            </div>
            <div class="row" data-key="tamper" data-keywords="防篡改 校验 损坏 篡改 tamper check corrupt">
              <div class="info"><div class="t">{{ $t('settings.security.tamper') }}</div><div class="d">{{ $t('settings.security.tamperD') }}</div></div>
              <span class="tag tag-green">{{ $t('settings.security.enabledTag') }}</span>
            </div>
            <div class="row" data-key="perFileKey" data-keywords="独立 密钥 文件密钥 泄露 per-file key leak">
              <div class="info"><div class="t">{{ $t('settings.security.perFileKey') }}</div><div class="d">{{ $t('settings.security.perFileKeyD') }}</div></div>
              <span class="tag tag-green">{{ $t('settings.security.enabledTag') }}</span>
            </div>
            <div class="row" data-key="bruteForce" data-keywords="抗暴力 破解 派生 密码猜测 brute force derive guess">
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

        <!-- 关于 -->
        <div v-show="active === 'about'" class="s-pane" data-pane="about">
          <div class="about-card">
            <div class="about-logo">
              <img :src="aboutIcon" alt="MarkLock" />
            </div>
            <div class="about-name">MarkLock</div>
            <div class="about-meta">
              <span class="about-ver">{{ $t('settings.about.version', { version: appVersion }) }}</span>
              <span class="about-sep"></span>
              <span class="about-tech">Tauri 2 · Vue 3</span>
            </div>
            <p class="about-desc">
              {{ $t('settings.about.desc') }}
            </p>
            <div class="about-feats">
              <div class="about-feat">
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2" /><path d="M8 11V7a4 4 0 0 1 8 0v4" /></svg>
                <span class="ft">{{ $t('settings.about.featE2e') }}</span>
                <span class="fd">{{ $t('settings.about.featE2eD') }}</span>
              </div>
              <div class="about-feat">
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="13" width="18" height="8" rx="2" /><path d="M7 17h.01" /><path d="M5.5 13 7 5h10l1.5 8" /></svg>
                <span class="ft">{{ $t('settings.about.featOffline') }}</span>
                <span class="fd">{{ $t('settings.about.featOfflineD') }}</span>
              </div>
              <div class="about-feat">
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z" /><path d="M9 12l2 2 4-4" /></svg>
                <span class="ft">{{ $t('settings.about.featTamper') }}</span>
                <span class="fd">{{ $t('settings.about.featTamperD') }}</span>
              </div>
            </div>
            <div class="about-actions">
              <div class="about-link-group">
                <span class="about-link-label">{{ $t('settings.about.checkUpdate') }}</span>
                <button class="about-link" @click="openLink(links.release)">GitHub</button>
                <span class="about-dot">·</span>
                <button class="about-link" @click="openLink(links.giteeRelease)">{{ $t('settings.about.giteeMirror') }}</button>
              </div>
              <span class="about-sep"></span>
              <div class="about-link-group">
                <span class="about-link-label">{{ $t('settings.about.projectHome') }}</span>
                <button class="about-link" @click="openLink(links.github)">GitHub</button>
                <span class="about-dot">·</span>
                <button class="about-link" @click="openLink(links.gitee)">{{ $t('settings.about.giteeMirror') }}</button>
              </div>
              <span class="about-sep"></span>
              <div class="about-link-group">
                <span class="about-link-label">{{ $t('settings.about.feedback') }}</span>
                <button class="about-link" @click="openLink(links.feedback)">GitHub</button>
                <span class="about-dot">·</span>
                <button class="about-link" @click="openLink(links.giteeFeedback)">{{ $t('settings.about.giteeMirror') }}</button>
              </div>
            </div>
            <div class="about-links-note">{{ $t('settings.about.linksNote') }}</div>
          </div>
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
.s-pane { max-width: 640px; }
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

.fake-select {
  display: inline-flex; align-items: center; justify-content: space-between; gap: 10px;
  min-width: 180px; height: 30px; padding: 0 10px;
  border: 1px solid var(--border); border-radius: var(--radius-sm);
  font-size: 13px; cursor: pointer; background: var(--panel);
}
.fake-select:hover { border-color: var(--primary-hover); }

.danger-zone { border-color: #ffccc7; }
.danger-zone .group-head { background: var(--red-bg); color: #cf1322; }

/* 关于页 */
.about-card { text-align: center; padding: 40px 24px 24px; max-width: 620px; margin: 0 auto; }
.about-logo {
  width: 78px; height: 78px; margin: 0 auto 18px;
}
.about-logo img {
  width: 100%; height: 100%; display: block;
  /* 中性灰黑软阴影：贴合图标轮廓又不染蓝 */
  filter: drop-shadow(0 4px 10px rgba(0, 0, 0, 0.14)) drop-shadow(0 1px 2px rgba(0, 0, 0, 0.10));
}
.about-name { font-size: 24px; font-weight: 700; letter-spacing: 0.3px; color: var(--text); }
.about-meta { display: flex; align-items: center; justify-content: center; gap: 10px; margin-top: 10px; }
.about-ver {
  font-size: 12px; font-weight: 600; color: var(--primary);
  background: var(--primary-bg); padding: 3px 11px; border-radius: 999px;
}
.about-sep { width: 1px; height: 12px; background: var(--border); }
.about-tech { font-size: 12px; color: var(--text-3); }
.about-desc {
  font-size: 13px; line-height: 1.85; color: var(--text-2);
  max-width: 460px; margin: 18px auto 22px;
}
.about-feats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; margin-bottom: 24px; }
.about-feat {
  display: flex; flex-direction: column; align-items: center; gap: 5px;
  padding: 14px 8px; border-radius: var(--radius-lg);
  border: 1px solid var(--border); background: var(--panel);
}
.about-feat svg { color: var(--primary); }
.about-feat .ft { font-size: 12.5px; font-weight: 600; color: var(--text); }
.about-feat .fd { font-size: 11px; color: var(--text-3); }
.about-actions { display: flex; flex-wrap: wrap; align-items: center; justify-content: center; gap: 10px 14px; }
.about-link-group { display: inline-flex; align-items: center; gap: 5px; }
.about-link-label { font-size: 12px; color: var(--text-3); }
.about-link {
  font-size: 12px; padding: 0; border: none; background: transparent;
  color: var(--primary); cursor: pointer; text-decoration: none;
  transition: color 0.15s;
}
.about-link:not(:disabled):hover { color: var(--primary-hover); text-decoration: underline; }
.about-link:disabled { opacity: 0.55; cursor: not-allowed; }
.about-dot { font-size: 12px; color: var(--text-4); user-select: none; }
.about-sep { width: 1px; height: 12px; background: var(--border); }
.about-links-note { margin-top: 12px; text-align: center; font-size: 11.5px; color: var(--text-4); }

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
</style>
