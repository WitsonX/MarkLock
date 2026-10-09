<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import TitleBar from '../components/TitleBar.vue'
import { useVaultStore } from '../stores/vault'
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
      // 原生菜单「关于」事件解绑句柄
      _unlistenMenu: null as UnlistenFn | null,
      // 设置搜索关键词
      searchQuery: '',
      // 「重置所有配置」确认弹窗
      resetModalOpen: false,
      resetting: false,
      nav: [
        { key: 'general', label: '通用', icon: 'gear' },
        { key: 'appear', label: '外观', icon: 'appear' },
        { key: 'security', label: '安全与加密', icon: 'shield', core: true },
        { key: 'keys', label: '快捷键', icon: 'keys' },
        { key: 'about', label: '关于', icon: 'about' },
      ] as NavItem[],
      // 自动锁定时长选项（秒）
      autoLockOptions: [
        { label: '1 分钟', value: 60 },
        { label: '5 分钟', value: 300 },
        { label: '15 分钟', value: 900 },
        { label: '30 分钟', value: 1800 },
        { label: '不自动锁定', value: 0 },
      ],
      // 自动保存间隔选项（秒）
      autoSaveOptions: [
        { label: '10 秒', value: 10 },
        { label: '30 秒', value: 30 },
        { label: '1 分钟', value: 60 },
        { label: '2 分钟', value: 120 },
        { label: '5 分钟', value: 300 },
        { label: '10 分钟', value: 600 },
        { label: '关闭', value: 0 },
      ],
      // 编辑器字体大小选项（px，与 ⌘+/⌘- 的 2px 步进对齐，10–28）
      fontSizeOptions: Array.from({ length: 10 }, (_, i) => ({ label: `${10 + i * 2} px`, value: 10 + i * 2 })),
      // 预览内容栏最大宽选项（px）；0 表示不限制、铺满面板宽度
      maxWidthOptions: [
        { label: '铺满', value: 0 },
        { label: '640 px', value: 640 },
        { label: '780 px', value: 780 },
        { label: '900 px', value: 900 },
        { label: '1080 px', value: 1080 },
        { label: '1280 px', value: 1280 },
      ],
    }
  },
  computed: {
    store() {
      return useVaultStore()
    },
    /** 快捷键符号：macOS 用 ⌘，其它平台用 Ctrl（与快捷键页展示一致） */
    modKey() {
      return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent) ? '⌘' : 'Ctrl'
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
        if (r === 'set') message.success('已将 MarkLock 设为 .md / .mdl / .mdlb 的默认打开程序')
        else message.warning('自动设置未成功。请右键文件 →「打开方式」→ 选择 MarkLock 并勾选「始终使用此应用」（macOS：显示简介 → 打开方式 → 全部更改…）')
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
        message.error(`重置失败：${String(e)}`)
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
    <TitleBar title="设置" subtitle="MarkLock">
      <span class="back" data-tauri-drag-region="false" title="返回" @click="$router.push('/editor')">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 12H5M12 19l-7-7 7-7" /></svg>
        <span class="back-txt">返回</span>
      </span>
    </TitleBar>

    <div class="settings">
      <!-- 左侧菜单 -->
      <nav class="s-nav">
        <!-- 设置搜索：按标题/描述/关键词过滤，命中行高亮，可点击跳转定位 -->
        <div class="s-search">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><path d="M21 21l-4.35-4.35" /></svg>
          <input v-model="searchQuery" type="text" placeholder="搜索设置" spellcheck="false" />
          <span v-if="searchQuery" class="clear" title="清空" @click="clearSearch">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M18 6L6 18M6 6l12 12" /></svg>
          </span>
        </div>
        <div v-show="searchQuery.trim()" class="s-results">
          <div v-for="r in searchResults" :key="r.key" class="s-result" @click="gotoResult(r.key)">
            <div class="rt">{{ r.title }}</div>
            <div class="rp">{{ r.paneLabel }} · {{ r.groupLabel }}</div>
          </div>
          <div v-if="!searchResults.length" class="s-result-empty">未找到匹配的设置项</div>
        </div>
        <template v-if="!searchQuery.trim()">
          <div class="cap">偏好设置</div>
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
            <span v-if="item.core" class="tag tag-gold" style="margin-left:auto;height:18px;font-size:10px;">核心</span>
          </div>
        </template>
      </nav>

      <!-- 内容区 -->
      <div class="s-body">
        <!-- 通用 -->
        <div v-show="active === 'general'" class="s-pane" data-pane="general">
          <h2>通用</h2>
          <div class="desc">启动行为与文件保存策略</div>
          <div class="group">
            <div class="group-head">启动</div>
            <div class="row" data-key="restoreLastSession" data-keywords="会话 页签 恢复 启动">
              <div class="info"><div class="t">启动时恢复上次会话</div><div class="d">重新打开上次打开的页签（加密库仍需输入密码）</div></div>
              <div class="switch" :class="{ on: store.settings.restoreLastSession }" @click="store.updateSettings({ restoreLastSession: !store.settings.restoreLastSession })"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">页签与保存</div>
            <div class="row" data-key="showTabs" data-keywords="页签 tab 标签栏 顶部">
              <div class="info"><div class="t">顶部页签</div><div class="d">在窗口顶部显示已打开文件的页签栏</div></div>
              <div class="switch" :class="{ on: store.settings.showTabs }" @click="store.updateSettings({ showTabs: !store.settings.showTabs })"></div>
            </div>
            <div class="row" data-key="watchExternalChanges" data-keywords="监控 外部修改 刷新 重载">
              <div class="info"><div class="t">监控外部修改</div><div class="d">打开的文件被其它程序修改时及时重新加载；页签有未保存内容时只提醒，不顶掉你的修改</div></div>
              <div class="switch" :class="{ on: store.settings.watchExternalChanges }" @click="store.updateSettings({ watchExternalChanges: !store.settings.watchExternalChanges })"></div>
            </div>
            <div class="row" data-key="autoSaveSecs" data-keywords="自动保存 保存 静默">
              <div class="info"><div class="t">自动保存</div><div class="d">定时静默保存有未保存修改的页签（含普通明文文件）；草稿与锁定文件不参与，磁盘存在外部冲突的页签不覆盖</div></div>
              <a-select
                :value="store.settings.autoSaveSecs"
                style="width: 150px;"
                :options="autoSaveOptions"
                @change="(v: number) => store.updateSettings({ autoSaveSecs: v })"
              />
            </div>
          </div>
          <div class="group">
            <div class="group-head">侧边栏分组</div>
            <div class="row" data-key="showOpenFiles" data-keywords="侧边栏 打开的文件">
              <div class="info"><div class="t">打开的文件</div><div class="d">默认不显示；开启后在侧边栏列出已打开的文件，使用顶部页签切换时可保持关闭避免重复</div></div>
              <div class="switch" :class="{ on: store.settings.showOpenFiles }" @click="store.updateSettings({ showOpenFiles: !store.settings.showOpenFiles })"></div>
            </div>
            <div class="row" data-key="showUnlockedVaults" data-keywords="侧边栏 加密库 库">
              <div class="info"><div class="t">加密库</div><div class="d">在侧边栏列出已登记的加密库；锁定后仍保留，便于二次点击快速解锁重开</div></div>
              <div class="switch" :class="{ on: store.settings.showUnlockedVaults }" @click="store.updateSettings({ showUnlockedVaults: !store.settings.showUnlockedVaults })"></div>
            </div>
            <div class="row" data-key="showFavorites" data-keywords="侧边栏 收藏">
              <div class="info"><div class="t">收藏</div><div class="d">在侧边栏列出收藏的库与文件，便于快速访问</div></div>
              <div class="switch" :class="{ on: store.settings.showFavorites }" @click="store.updateSettings({ showFavorites: !store.settings.showFavorites })"></div>
            </div>
            <div class="row" data-key="showRecent" data-keywords="侧边栏 最近打开">
              <div class="info"><div class="t">最近打开</div><div class="d">默认不显示；开启后记录最近打开的库与文件</div></div>
              <div class="switch" :class="{ on: store.settings.showRecent }" @click="store.updateSettings({ showRecent: !store.settings.showRecent })"></div>
            </div>
            <div class="row" data-key="showWorkdir" data-keywords="侧边栏 工作目录 文件夹">
              <div class="info"><div class="t">工作目录</div><div class="d">在侧边栏显示已打开的工作目录文件树</div></div>
              <div class="switch" :class="{ on: store.settings.showWorkdir }" @click="store.updateSettings({ showWorkdir: !store.settings.showWorkdir })"></div>
            </div>
            <div class="row" data-key="showOutline" data-keywords="侧边栏 大纲 标题 目录">
              <div class="info"><div class="t">大纲</div><div class="d">显示当前文件的 Markdown 标题结构，点击可跳转到对应位置</div></div>
              <div class="switch" :class="{ on: store.settings.showOutline }" @click="store.updateSettings({ showOutline: !store.settings.showOutline })"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">文件浏览</div>
            <div class="row" data-key="showHiddenFiles" data-keywords="隐藏文件 文件浏览 目录">
              <div class="info"><div class="t">显示隐藏文件</div><div class="d">在工作目录中显示以 . 开头的隐藏文件与文件夹</div></div>
              <div class="switch" :class="{ on: store.settings.showHiddenFiles }" @click="store.updateSettings({ showHiddenFiles: !store.settings.showHiddenFiles })"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">加密转换</div>
            <div class="row" data-key="convertMdAction" data-keywords="转为加密 mdl 明文 删除 保留">
              <div class="info"><div class="t">转为加密文件后处置原明文文件</div><div class="d">从普通 md 页签右键「转为加密文件」生成 .mdl 后，如何处置原来的明文文件</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.convertMdAction === 'ask' }" @click="store.updateSettings({ convertMdAction: 'ask' })">每次询问</span>
                <span :class="{ on: store.settings.convertMdAction === 'delete' }" @click="store.updateSettings({ convertMdAction: 'delete' })">删除原文件</span>
                <span :class="{ on: store.settings.convertMdAction === 'keep' }" @click="store.updateSettings({ convertMdAction: 'keep' })">保留原文件</span>
              </div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">文件关联</div>
            <div class="row" data-key="setDefaultHandler" data-keywords="默认 打开方式 文件关联 md mdl mdlb">
              <div class="info"><div class="t">设为默认打开程序</div><div class="d">一键将 MarkLock 设为 .md / .mdl / .mdlb 的默认打开程序，无需进入系统设置</div></div>
              <button class="btn" @click="applySetDefault">设为默认…</button>
            </div>
          </div>
        </div>

        <!-- 外观 -->
        <div v-show="active === 'appear'" class="s-pane" data-pane="appear">
          <h2>外观</h2>
          <div class="desc">主题与编辑器排版</div>
          <div class="group">
            <div class="group-head">主题</div>
            <div class="row" data-key="theme" data-keywords="主题 浅色 深色 暗黑 dark light 跟随系统">
              <div class="info"><div class="t">外观模式</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.theme === 'light' }" @click="store.updateSettings({ theme: 'light' })">浅色</span>
                <span :class="{ on: store.settings.theme === 'dark' }" @click="store.updateSettings({ theme: 'dark' })">深色</span>
                <span :class="{ on: store.settings.theme === 'system' }" @click="store.updateSettings({ theme: 'system' })">跟随系统</span>
              </div>
            </div>
            <div class="row" data-key="codeTheme" data-keywords="代码高亮 配色 github monokai dracula">
              <div class="info"><div class="t">代码高亮主题</div><div class="d">预览区代码块的配色方案</div></div>
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
            <div class="row" data-key="codeIndent" data-keywords="tab 缩进 空格 代码块">
              <div class="info"><div class="t">代码 Tab 缩进</div><div class="d">预览区代码块的 Tab 宽度（空格数）</div></div>
              <div class="seg">
                <span :class="{ on: store.settings.codeIndent === 2 }" @click="store.updateSettings({ codeIndent: 2 })">2 格</span>
                <span :class="{ on: store.settings.codeIndent === 4 }" @click="store.updateSettings({ codeIndent: 4 })">4 格</span>
              </div>
            </div>
            <div class="code-preview-wrap">
              <div class="code-preview-label">预览效果</div>
              <pre class="code-preview" :class="`hljs-theme-${store.settings.codeTheme}`" :style="{ '--code-indent': store.settings.codeIndent }"><code class="hljs language-javascript" v-html="previewCodeHtml"></code></pre>
            </div>
          </div>
          <div class="group">
            <div class="group-head">编辑器界面</div>
            <div class="row" data-key="showToolbar" data-keywords="工具栏 格式化 按钮">
              <div class="info"><div class="t">格式工具栏</div><div class="d">编辑器上方的 Markdown 格式化工具栏</div></div>
              <div class="switch" :class="{ on: store.settings.showToolbar }" @click="store.updateSettings({ showToolbar: !store.settings.showToolbar })"></div>
            </div>
            <div class="row" data-key="showStatusbar" data-keywords="状态栏 行数 字数 底部">
              <div class="info"><div class="t">状态栏</div><div class="d">窗口底部的行数、字数和保存状态</div></div>
              <div class="switch" :class="{ on: store.settings.showStatusbar }" @click="store.updateSettings({ showStatusbar: !store.settings.showStatusbar })"></div>
            </div>
            <div class="row" data-key="previewDblClickToggle" data-keywords="双击 预览 分屏 切换 视图">
              <div class="info"><div class="t">双击预览区切换视图</div><div class="d">双击预览区域时在分屏与预览之间切换；关闭后双击不再切换</div></div>
              <div class="switch" :class="{ on: store.settings.previewDblClickToggle }" @click="store.updateSettings({ previewDblClickToggle: !store.settings.previewDblClickToggle })"></div>
            </div>
          </div>
          <div class="group">
            <div class="group-head">排版</div>
            <div class="row" data-key="editorFontSize" data-keywords="字体 字号 font size 放大 缩小 缩放 zoom">
              <div class="info"><div class="t">编辑器字体大小</div><div class="d">源码编辑区与预览区的字号（预览大 1px）；也可用 {{ modKey }}+ {{ modKey }}- 实时调节，{{ modKey }}0 重置为 14 px</div></div>
              <a-select
                :value="store.settings.editorFontSize"
                style="width: 120px;"
                :options="fontSizeOptions"
                @change="(v: number) => store.updateSettings({ editorFontSize: v })"
              />
            </div>
            <div class="row" data-key="editorMaxWidth" data-keywords="预览 内容栏 宽度 排版 限宽 max width 铺满">
              <div class="info"><div class="t">预览内容最大宽度</div><div class="d">预览与分屏模式下内容栏的最大宽度（限宽后水平居中）；不影响源码编辑区，选「铺满」则跟随面板宽度不再限宽</div></div>
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
          <h2>安全与加密</h2>
          <div class="desc">锁定后将立即清零内存中的密钥，需重新输入主密码解锁</div>

          <div class="group">
            <div class="group-head">加密方案</div>
            <div class="row" data-key="e2e" data-keywords="端到端 加密 密文 上传">
              <div class="info"><div class="t">端到端加密</div><div class="d">正文与目录结构均密文存储，绝不上传</div></div>
              <span class="tag tag-green">默认启用</span>
            </div>
            <div class="row" data-key="tamper" data-keywords="防篡改 校验 损坏 篡改">
              <div class="info"><div class="t">防篡改校验</div><div class="d">文件被外部修改或损坏时拒绝打开</div></div>
              <span class="tag tag-green">默认启用</span>
            </div>
            <div class="row" data-key="perFileKey" data-keywords="独立 密钥 文件密钥 泄露">
              <div class="info"><div class="t">独立文件密钥</div><div class="d">每个文件使用随机密钥，单文件泄露不影响其他</div></div>
              <span class="tag tag-green">默认启用</span>
            </div>
            <div class="row" data-key="bruteForce" data-keywords="抗暴力 破解 派生 密码猜测">
              <div class="info"><div class="t">抗暴力破解</div><div class="d">解锁需大量计算，极大拖慢密码猜测</div></div>
              <span class="tag tag-green">默认启用</span>
            </div>
          </div>

          <div class="group">
            <div class="group-head">锁定策略</div>
            <div class="row" data-key="autoLockSecs" data-keywords="自动锁定 无操作 闲置 超时">
              <div class="info"><div class="t">无操作自动锁定</div><div class="d">锁定后内存中的密钥立即清零</div></div>
              <a-select
                :value="store.settings.autoLockSecs"
                style="width: 150px;"
                :options="autoLockOptions"
                @change="(v: number) => store.updateSettings({ autoLockSecs: v })"
              />
            </div>
            <div class="row" data-key="lockOnMinimize" data-keywords="最小化 锁定 窗口">
              <div class="info"><div class="t">窗口最小化时锁定</div></div>
              <div class="switch" :class="{ on: store.settings.lockOnMinimize }" @click="store.updateSettings({ lockOnMinimize: !store.settings.lockOnMinimize })"></div>
            </div>
            <div class="row" data-key="lockOnSleep" data-keywords="休眠 锁屏 睡眠 锁定">
              <div class="info"><div class="t">系统休眠 / 锁屏时锁定</div></div>
              <div class="switch" :class="{ on: store.settings.lockOnSleep }" @click="store.updateSettings({ lockOnSleep: !store.settings.lockOnSleep })"></div>
            </div>
            <div class="row" data-key="clearClipboardOnLock" data-keywords="剪贴板 复制 清除 粘贴">
              <div class="info"><div class="t">锁定后清除剪贴板</div><div class="d">防止解密内容残留（复制后也会定时清除）</div></div>
              <div class="switch" :class="{ on: store.settings.clearClipboardOnLock }" @click="store.updateSettings({ clearClipboardOnLock: !store.settings.clearClipboardOnLock })"></div>
            </div>
          </div>

          <div class="group danger-zone">
            <div class="group-head">危险操作</div>
            <div class="row" data-key="resetAll" data-keywords="重置 清空 恢复默认 配置">
              <div class="info"><div class="t">重置所有配置</div><div class="d">偏好设置回默认，并清空已登记的库、收藏、最近打开与工作目录等本地记录；磁盘上的文件不会删除</div></div>
              <button class="btn btn-danger btn-sm" @click="resetModalOpen = true">重置…</button>
            </div>
          </div>
        </div>

        <!-- 快捷键 -->
        <div v-show="active === 'keys'" class="s-pane" data-pane="keys">
          <h2>快捷键</h2>
          <div class="desc">全局与编辑器快捷键（macOS 用 ⌘，Windows/Linux 用 Ctrl）</div>
          <div class="group">
            <div class="group-head">文件与页签</div>
            <div class="row" data-key="kb-save" data-keywords="保存 save 文件">
              <div class="info"><div class="t">保存文件</div><div class="d">{{ modKey }} + S</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">S</span></span>
            </div>
            <div class="row" data-key="kb-new" data-keywords="新建 new 文件 草稿">
              <div class="info"><div class="t">新建文件</div><div class="d">{{ modKey }} + N</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">N</span></span>
            </div>
            <div class="row" data-key="kb-open" data-keywords="打开 open 文件">
              <div class="info"><div class="t">打开文件</div><div class="d">{{ modKey }} + O</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">O</span></span>
            </div>
            <div class="row" data-key="kb-close" data-keywords="关闭页签 close tab">
              <div class="info"><div class="t">关闭当前页签</div><div class="d">{{ modKey }} + W</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">W</span></span>
            </div>
            <div class="row" data-key="kb-reopen" data-keywords="撤销关闭 重新打开 reopen undo 页签">
              <div class="info"><div class="t">重新打开已关闭页签</div><div class="d">{{ modKey }} + ⇧ + T（仅本次启动期间，最多撤销 20 个）</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">⇧</span> <span class="kbd">T</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">搜索</div>
            <div class="row" data-key="kb-find" data-keywords="搜索 替换 find replace">
              <div class="info"><div class="t">编辑器内搜索 / 替换</div><div class="d">{{ modKey }} + F</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">F</span></span>
            </div>
            <div class="row" data-key="kb-gsearch" data-keywords="全局搜索 库 工作目录 search">
              <div class="info"><div class="t">全局搜索（库 + 工作目录）</div><div class="d">{{ modKey }} + ⇧ + F</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">⇧</span> <span class="kbd">F</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">视图</div>
            <div class="row" data-key="kb-view" data-keywords="切换 编辑 分屏 预览 视图">
              <div class="info"><div class="t">切换 编辑 / 分屏 / 预览</div><div class="d">{{ modKey }} + E</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">E</span></span>
            </div>
            <div class="row" data-key="kb-sidebar" data-keywords="侧边栏 显示 隐藏 栏">
              <div class="info"><div class="t">显示 / 隐藏侧边栏</div><div class="d">{{ modKey }} + J</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">J</span></span>
            </div>
            <div class="row" data-key="kb-fontsize" data-keywords="字号 字体 放大 缩小 重置 zoom font">
              <div class="info"><div class="t">放大 / 缩小 / 重置字号</div><div class="d">{{ modKey }} + = 放大、{{ modKey }} + − 缩小（2px 步进）、{{ modKey }} + 0 重置，编辑与预览联动</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">=</span> / <span class="kbd">−</span> / <span class="kbd">0</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">编辑器格式化</div>
            <div class="row" data-key="kb-bold" data-keywords="加粗 bold 粗体">
              <div class="info"><div class="t">加粗</div><div class="d">{{ modKey }} + B</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">B</span></span>
            </div>
            <div class="row" data-key="kb-italic" data-keywords="斜体 italic">
              <div class="info"><div class="t">斜体</div><div class="d">{{ modKey }} + I</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">I</span></span>
            </div>
            <div class="row" data-key="kb-link" data-keywords="链接 link 插入">
              <div class="info"><div class="t">插入链接</div><div class="d">{{ modKey }} + K</div></div>
              <span><span class="kbd">{{ modKey }}</span> <span class="kbd">K</span></span>
            </div>
          </div>
          <div class="group">
            <div class="group-head">安全</div>
            <div class="row" data-key="kb-lock" data-keywords="锁定 全部锁 安全 lock">
              <div class="info"><div class="t">全部锁定</div><div class="d">{{ modKey }} + L</div></div>
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
              <span class="about-ver">版本 1.0.0</span>
              <span class="about-sep"></span>
              <span class="about-tech">Tauri 2 · Vue 3</span>
            </div>
            <p class="about-desc">
              为机密笔记而生的加密 Markdown 工作台。端到端加密与防篡改校验双重护航，
              每个文件独立密钥、抗暴力破解派生；数据始终以密文静卧本地，断网亦可用，隐私从不离开你的设备。
            </p>
            <div class="about-feats">
              <div class="about-feat">
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2" /><path d="M8 11V7a4 4 0 0 1 8 0v4" /></svg>
                <span class="ft">端到端加密</span>
                <span class="fd">密钥与明文永不落盘</span>
              </div>
              <div class="about-feat">
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="13" width="18" height="8" rx="2" /><path d="M7 17h.01" /><path d="M5.5 13 7 5h10l1.5 8" /></svg>
                <span class="ft">完全离线</span>
                <span class="fd">零上传 · 断网可用</span>
              </div>
              <div class="about-feat">
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z" /><path d="M9 12l2 2 4-4" /></svg>
                <span class="ft">防篡改校验</span>
                <span class="fd">独立密钥 · 改动即失效</span>
              </div>
            </div>
            <div class="about-actions">
              <div class="about-link-group">
                <span class="about-link-label">检查更新</span>
                <button class="about-link" @click="openLink(links.release)">GitHub</button>
                <span class="about-dot">·</span>
                <button class="about-link" @click="openLink(links.giteeRelease)">Gitee 镜像</button>
              </div>
              <span class="about-sep"></span>
              <div class="about-link-group">
                <span class="about-link-label">项目主页</span>
                <button class="about-link" @click="openLink(links.github)">GitHub</button>
                <span class="about-dot">·</span>
                <button class="about-link" @click="openLink(links.gitee)">Gitee 镜像</button>
              </div>
              <span class="about-sep"></span>
              <div class="about-link-group">
                <span class="about-link-label">反馈建议</span>
                <button class="about-link" @click="openLink(links.feedback)">GitHub</button>
                <span class="about-dot">·</span>
                <button class="about-link" @click="openLink(links.giteeFeedback)">Gitee 镜像</button>
              </div>
            </div>
            <div class="about-links-note">Gitee 为国内镜像站，国内网络访问更快</div>
          </div>
        </div>
      </div>
    </div>

    <!-- 重置所有配置：二次确认 -->
    <a-modal
      v-model:open="resetModalOpen"
      title="重置所有配置"
      :width="460"
      :footer="null"
      :mask-closable="!resetting"
      :closable="!resetting"
    >
      <div class="reset-box">
        <div class="reset-lead">将把应用恢复到刚安装的状态，包括：</div>
        <ul class="reset-list">
          <li>全部偏好设置（通用 / 外观 / 安全与加密）恢复默认值</li>
          <li>已登记的库、收藏的库与文件、最近打开记录</li>
          <li>已打开的页签清单、工作目录</li>
          <li>侧边栏宽度、分屏比例、分组与目录树的展开收起状态</li>
        </ul>
        <div class="reset-note">磁盘上的库与文件不会被删除或修改，重新使用只需再次打开并输入主密码。</div>
        <div v-if="store.recent.length" class="reset-warn">当前解锁的 {{ unlockedVaultCount }} 个库会立即锁定，内存密钥清零。</div>
        <div v-if="unsavedCount" class="reset-warn">有 {{ unsavedCount }} 个页签含未保存内容（含未落盘草稿），加密文件会先写回磁盘，草稿与明文文件的未保存修改将被丢弃。</div>
        <div class="modal-foot">
          <button class="btn" :disabled="resetting" @click="resetModalOpen = false">取消</button>
          <button class="btn btn-danger" :disabled="resetting" @click="submitReset">{{ resetting ? '重置中…' : '确认重置' }}</button>
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
  width: 208px; flex: 0 0 208px;
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
