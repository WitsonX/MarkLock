<script lang="ts">
import { defineComponent } from 'vue'
import { storeToRefs } from 'pinia'
import { listen } from '@tauri-apps/api/event'
import router from './router'
import { useVaultStore } from './stores/vault'
import * as tauri from './lib/tauri'
import { LINKS } from './lib/links'

/** 帮助菜单项→外链映射：在 App 层统一处理，保证解锁页/设置页等非编辑器页点击也有响应。 */
const HELP_LINKS: Record<string, string> = {
  'menu:help-github': LINKS.github,
  'menu:help-gitee': LINKS.gitee,
  'menu:check-update': LINKS.release,
  'menu:check-update-gitee': LINKS.giteeRelease,
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

    // 退出（点红叉 / Cmd+Q）兜底：后端 prevent_close 后广播 close-requested。
    // 原先只有 EditorView 监听，切到设置/解锁页后组件卸载、无人响应导致无法退出。
    // 非编辑器页不能直接退出（可能仍有未保存文件），而是暂存退出请求并跳回编辑器，
    // 由 EditorView 挂载后消费 pendingQuit，走原有的未保存确认弹窗流程。
    listen('marklock://close-requested', () => {
      if (router.currentRoute.value.name === 'editor') return
      store.pendingQuit = true
      router.push('/editor')
    }).catch(() => {})

    // 原生菜单「关于 MarkLock」：路由到设置页的「关于」页签。
    // 已在设置页时不重复 push（SettingsView 自身监听同一事件直接切页签）。
    listen<string>('marklock://menu', (e) => {
      if (e.payload === 'menu:about') {
        if (router.currentRoute.value.name !== 'settings') {
          router.push({ path: '/settings', query: { tab: 'about' } })
        }
        return
      }
      // 帮助菜单：用系统浏览器打开对应链接（Gitee 为国内镜像），全局生效不依赖当前路由。
      const url = HELP_LINKS[e.payload]
      if (url) tauri.openUrl(url).catch(() => {})
    }).catch(() => {})

    return { settings, apply }
  },
  watch: {
    'settings.theme'() {
      this.apply()
    },
  },
})
</script>

<template>
  <router-view />
</template>
