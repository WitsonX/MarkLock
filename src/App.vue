<script lang="ts">
import { defineComponent } from 'vue'
import { storeToRefs } from 'pinia'
import { listen } from '@tauri-apps/api/event'
import { message } from 'ant-design-vue'
import zhCN from 'ant-design-vue/es/locale/zh_CN'
import enUS from 'ant-design-vue/es/locale/en_US'
import router from './router'
import { useVaultStore } from './stores/vault'
import { setLang, resolveLang, lang, t } from './locales'
import * as tauri from './lib/tauri'
import { isMac } from './lib/platform'
import { LINKS } from './lib/links'
import { runUpdateCheck, shouldAutoCheck, markChecked } from './lib/update'
import AboutCard from './components/AboutCard.vue'
import { aboutDialogOpen, openAboutDialog } from './lib/aboutDialog'

/** 帮助菜单项→外链映射：在 App 层统一处理，保证解锁页/设置页等非编辑器页点击也有响应。 */
const HELP_LINKS: Record<string, string> = {
  'menu:help-github': LINKS.github,
  'menu:help-gitee': LINKS.gitee,
  'menu:feedback': LINKS.feedback,
  'menu:feedback-gitee': LINKS.giteeFeedback,
}

/** 系统深色偏好查询（惰性创建） */
let sysMedia: MediaQueryList | null = null
function getSysMedia(): MediaQueryList | null {
  if (typeof window === 'undefined' || !window.matchMedia) return null
  if (!sysMedia) sysMedia = window.matchMedia('(prefers-color-scheme: dark)')
  return sysMedia
}

export default defineComponent({
  name: 'App',
  components: { AboutCard },
  setup() {
    const store = useVaultStore()
    const { settings } = storeToRefs(store)

    // 把「跟随系统」解析成实际的 light/dark，写入 <html data-theme>
    const apply = () => {
      const t = settings.value.theme
      const dark = t === 'dark' || (t === 'system' && !!(getSysMedia()?.matches))
      document.documentElement.dataset.theme = dark ? 'dark' : 'light'
    }
    apply()
    getSysMedia()?.addEventListener('change', apply)

    // 把「跟随系统」解析成实际的 zh/en，写入 lang ref + <html lang>，并同步 macOS 原生菜单语言
    const applyLang = () => {
      setLang(resolveLang(settings.value.language))
      tauri.setNativeMenuLang(lang.value).catch(() => {})
    }
    applyLang()

    // 同步「关闭最小化到托盘」开关到后端（决定红叉是隐藏还是退出）；启动时先同步一次。
    tauri.setCloseToTray(settings.value.closeToTray).catch(() => {})
    // 同步「显示托盘图标」开关到后端（运行时创建/移除托盘）；启动时先同步一次。
    tauri.setTrayEnabled(settings.value.showTrayIcon).catch(() => {})

    // 托盘「全部锁定」：后端事件→前端统一走 store.lockAll（含锁定前自动保存 + 页签锁定态刷新）。
    // 上移到常驻的 App.vue，避免非编辑器路由时 EditorView 未挂载而无人响应。
    listen('marklock://tray-lock-all', () => {
      store.lockAll().catch(() => {})
    }).catch(() => {})

    // 窗口隐藏到托盘后的一次性提示（仅首次弹导，告知用户如何从托盘恢复/退出）。
    // macOS 无托盘概念且 Dock 可唤回，改用 Mac 专属词条（菜单栏 / Dock 语义）。
    listen('marklock://hidden-to-tray', () => {
      if (localStorage.getItem('marklock.trayHintShown')) return
      localStorage.setItem('marklock.trayHintShown', '1')
      message.info(t(isMac ? 'settings.general.trayHiddenHintMac' : 'settings.general.trayHiddenHint'))
    }).catch(() => {})

    // 退出（点红叉 / Cmd+Q）兜底：后端 prevent_close 后广播 close-requested。
    // 原先只有 EditorView 监听，切到设置/解锁页后组件卸载、无人响应导致无法退出。
    // 非编辑器页不能直接退出（可能仍有未保存文件），而是暂存退出请求并跳回编辑器，
    // 由 EditorView 挂载后消费 pendingQuit，走原有的未保存确认弹窗流程。
    listen('marklock://close-requested', () => {
      if (router.currentRoute.value.name === 'editor') return
      store.pendingQuit = true
      router.push('/editor')
    }).catch(() => {})

    // 原生菜单「关于 MarkLock」：直接弹应用内独立对话框（常驻于本组件模板），
    // 不再路由到设置页；卡片内容与设置页「关于」页签共用 AboutCard。
    listen<string>('marklock://menu', (e) => {
      if (e.payload === 'menu:about') {
        openAboutDialog()
        return
      }
      // 检查更新：走智能检测（比版本→弹窗跳转下载），与设置页/关于页共用同一逻辑。
      if (e.payload === 'menu:check-update') {
        runUpdateCheck(resolveLang(settings.value.language) === 'zh', { auto: false }).catch(() => {})
        return
      }
      // 帮助菜单：用系统浏览器打开对应链接（Gitee 为国内镜像），全局生效不依赖当前路由。
      const url = HELP_LINKS[e.payload]
      if (url) tauri.openUrl(url).catch(() => {})
    }).catch(() => {})

    // 启动后延迟 3s 静默检查更新：按所选频率门控（默认每周），发现新版才弹窗引导跳转下载；
    // GitHub / Gitee 两源按界面语言优先顺序回落，两源均不可达则静默跳过。
    window.setTimeout(() => {
      const freq = settings.value.updateCheckFreq
      if (!shouldAutoCheck(freq)) return
      markChecked()
      runUpdateCheck(resolveLang(settings.value.language) === 'zh', { auto: true }).catch(() => {})
    }, 3000)

    return { settings, apply, applyLang, aboutDialogOpen }
  },
  computed: {
    // Ant Design 内置组件文案（空状态/分页等）跟随当前语言
    antdLocale() {
      return lang.value === 'en' ? enUS : zhCN
    },
  },
  watch: {
    'settings.theme'() {
      this.apply()
    },
    'settings.language'() {
      this.applyLang()
    },
    'settings.closeToTray'(enabled: boolean) {
      tauri.setCloseToTray(enabled).catch(() => {})
    },
    'settings.showTrayIcon'(enabled: boolean) {
      tauri.setTrayEnabled(enabled).catch(() => {})
    },
  },
})
</script>

<template>
  <a-config-provider :locale="antdLocale">
    <router-view />

    <!-- 独立「关于」对话框：任意路由下均可唤起，默认支持 mask 点击 + Esc 键关闭 -->
    <a-modal
      v-model:open="aboutDialogOpen"
      :title="null"
      :footer="null"
      :width="680"
      :closable="true"
      centered
      destroy-on-close
      class="about-modal"
    >
      <AboutCard />
    </a-modal>
  </a-config-provider>
</template>

<style>
/* 去掉默认 header 的高度残留，让关闭 X 与卡片顶部保持呼吸 */
.about-modal .ant-modal-body { padding-top: 24px; }
.about-modal .ant-modal-close { top: 6px; inset-inline-end: 6px; }
</style>
