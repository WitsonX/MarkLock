<script lang="ts">
import { defineComponent } from 'vue'
import { storeToRefs } from 'pinia'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { message } from 'ant-design-vue'
import zhCN from 'ant-design-vue/es/locale/zh_CN'
import enUS from 'ant-design-vue/es/locale/en_US'
import router from './router'
import { useVaultStore } from './stores/vault'
import { setLang, resolveLang, lang, t } from './locales'
import * as tauri from './lib/tauri'
import { isMac } from './lib/platform'
import { isStickyWindow } from './lib/sticky'
import { LINKS } from './lib/links'
import { runUpdateCheck, shouldAutoCheck, markChecked } from './lib/update'
import { applyCustomSkins } from './lib/skins'
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

    // 把热角四项配置（开关 / 停留 / 修饰键门控 / 修饰键）整体同步到后端轮询线程。
    const syncHotCorner = () => {
      const hc = settings.value.hotCorner
      tauri
        .setHotCorner(hc.enabled, hc.dwellMs, hc.requireModifier, hc.modifier)
        .catch(() => {})
    }

    // 把「跟随系统」解析成实际的 light/dark，写入 <html data-theme>；
    // 皮肤与明暗正交，另写 <html data-skin>，自定义皮肤的 CSS 需先注入才有对应规则。
    const apply = () => {
      const t = settings.value.theme
      const dark = t === 'dark' || (t === 'system' && !!(getSysMedia()?.matches))
      document.documentElement.dataset.theme = dark ? 'dark' : 'light'
      applyCustomSkins(settings.value.customSkins)
      document.documentElement.dataset.skin = settings.value.skin || 'default'
    }
    apply()
    getSysMedia()?.addEventListener('change', apply)

    // 把「跟随系统」解析成实际的 zh/en，写入 lang ref + <html lang>，并同步 macOS 原生菜单语言
    const applyLang = () => {
      setLang(resolveLang(settings.value.language))
      tauri.setNativeMenuLang(lang.value).catch(() => {})
    }
    applyLang()

    // 便签窗只承载便签视图：跳过主窗口专属的托盘同步 / 全局事件监听 / 更新检查，
    // 避免小窗里误响应 close-requested 等主窗口语义（主题与语言两窗共用，见上）。
    if (!isStickyWindow()) {
      // 同步「关闭最小化到托盘」开关到后端（决定红叉是隐藏还是退出）；启动时先同步一次。
      tauri.setCloseToTray(settings.value.closeToTray).catch(() => {})
      // 同步「保存时保留上一版备份」开关到后端（关闭后原子写盘不再生成 .mlk-bak）；启动时先同步一次。
      tauri.setBackupEnabled(settings.value.backupEnabled).catch(() => {})
      // 同步「显示托盘图标」开关到后端（运行时创建/移除托盘）；启动时先同步一次。
      tauri.setTrayEnabled(settings.value.showTrayIcon).catch(() => {})
      // 同步「托盘点击弹出便签」的鼠标键到后端（决定左/右键弹便签、另一键弹菜单）；启动时先同步一次。
      tauri.setTrayStickyButton(settings.value.trayStickyButton).catch(() => {})
      // 开机自启：仅在设置为开启时重写系统启动项，避免默认（关闭）用户每次启动都去碰注册表 / plist。
      // 主要兼容“用户先前开启后又从系统设置里手动删除启动项”的少见场景，确保 UI 与系统状态最终一致。
      if (settings.value.autoStart) tauri.setAutoStart(true).catch(() => {})
      // 自启启动时 Rust 已以隐藏态创建主窗（看到 --autostarted 参数），这里按设置决定是否弹出：
      //   ① autoStartShowWindow=true → 用户显式要求弹窗；
      //   ② 或 showTrayIcon=false → 托盘不可达，强制弹窗避免应用隐形。
      // 手动启动时窗口本就可见，show()/setFocus() 为无害的 no-op，不需专门区分。
      if (settings.value.autoStartShowWindow || !settings.value.showTrayIcon) {
        const win = getCurrentWindow()
        win.show().then(() => win.setFocus()).catch(() => {})
      }
      // 同步便签弹出角落到后端（托盘左键定位 / 热角共用）；启动时先同步一次。
      tauri.setStickyCorner(settings.value.hotCorner.corner).catch(() => {})
      // 同步热角开关、停留时长与修饰键门控到后端轮询线程；启动时先同步一次。
      syncHotCorner()

      // 便签窗「设置」按钮：Rust 已唤回主窗，这里跳转设置页并定位到「便携便签」分区
      // （tab=sticky 由 SettingsView 读取；便签是独立 webview，改不了主窗路由）。
      listen('marklock://open-settings', () => {
        router.push('/settings?tab=sticky').catch(() => {})
      }).catch(() => {})

      // 托盘菜单「偏好设置」：Rust 已唤回主窗，这里跳设置页并定位到「通用」分区。
      listen('marklock://open-preferences', () => {
        router.push('/settings?tab=general').catch(() => {})
      }).catch(() => {})

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

      // 启动清理：把磁盘上已不存在的登记库/上次打开页签从列表中移除，
      // 避免侧边栏出现点不开的库、或 restoreSession 为已删除的加密文件恢复出「锁定占位」。
      // runStartupPrune 幂等，EditorView.restoreSession 内部也会 await 同一 Promise 保证时序。
      store
        .runStartupPrune()
        .then(({ vaults, files }) => {
          if (vaults.length) {
            message.warning(t('unlock.prunedMsg', { names: vaults.map((v) => v.name).join('、') }))
          }
          if (files.length) {
            message.warning(t('unlock.prunedFilesMsg', { names: files.join('、') }))
          }
        })
        .catch(() => {})
    }

    return { settings, apply, applyLang, aboutDialogOpen, syncHotCorner }
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
    'settings.skin'() {
      this.apply()
    },
    'settings.customSkins'() {
      this.apply()
    },
    'settings.language'() {
      this.applyLang()
    },
    'settings.closeToTray'(enabled: boolean) {
      tauri.setCloseToTray(enabled).catch(() => {})
    },
    'settings.backupEnabled'(enabled: boolean) {
      tauri.setBackupEnabled(enabled).catch(() => {})
    },
    'settings.showTrayIcon'(enabled: boolean) {
      tauri.setTrayEnabled(enabled).catch(() => {})
    },
    'settings.trayStickyButton'(button: 'off' | 'left' | 'right') {
      tauri.setTrayStickyButton(button).catch(() => {})
    },
    'settings.hotCorner.corner'(corner: 'topLeft' | 'bottomLeft' | 'topRight' | 'bottomRight') {
      tauri.setStickyCorner(corner).catch(() => {})
    },
    'settings.hotCorner.enabled'(enabled: boolean) {
      this.syncHotCorner()
    },
    'settings.hotCorner.dwellMs'() {
      this.syncHotCorner()
    },
    'settings.hotCorner.requireModifier'() {
      this.syncHotCorner()
    },
    'settings.hotCorner.modifier'() {
      this.syncHotCorner()
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
