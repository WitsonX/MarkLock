<script lang="ts">
import { defineComponent } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { UnlistenFn } from '@tauri-apps/api/event'

// Windows 无边框窗的窗口三键：原生标题栏已在 Rust 侧去掉（lib.rs 的 decorations(false) 分支），
// 与 macOS 红绿灯叠加同思路，把系统标题条整合进自绘工具条，三键贴齐窗右端。
// 关闭走 win.close() → CloseRequested，复用「关闭到托盘 / 未保存确认」既有语义；
// 最大化态监听 onResized 实时切换 最大化/还原 图标（双击标题栏、拖顶边吸附同样生效）。
export default defineComponent({
  name: 'WindowControls',
  data() {
    return {
      maximized: false,
      _unlisten: null as UnlistenFn | null,
    }
  },
  async mounted() {
    const win = getCurrentWindow()
    this.maximized = await win.isMaximized().catch(() => false)
    this._unlisten = await win.onResized(() => {
      win.isMaximized().then((v) => (this.maximized = v)).catch(() => {})
    })
  },
  unmounted() {
    this._unlisten?.()
  },
  methods: {
    minimize() {
      getCurrentWindow().minimize().catch(() => {})
    },
    toggleMaximize() {
      getCurrentWindow().toggleMaximize().catch(() => {})
    },
    close() {
      getCurrentWindow().close().catch(() => {})
    },
  },
})
</script>

<template>
  <!-- 整块退出拖拽区：三键是可点控件，标题栏其余空白仍拖拽 / 双击最大化；
       右键不冒泡到标题栏的视图菜单（原生标题键右键是系统菜单语义） -->
  <div class="win-controls" data-tauri-drag-region="false" @contextmenu.stop>
    <button type="button" class="wc-btn" :aria-label="$t('win.minimize')" v-tip="{ text: $t('win.minimize'), place: 'bottom' }" @click="minimize">
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5h10" /></svg>
    </button>
    <button type="button" class="wc-btn" :aria-label="maximized ? $t('win.restore') : $t('win.maximize')" v-tip="{ text: maximized ? $t('win.restore') : $t('win.maximize'), place: 'bottom' }" @click="toggleMaximize">
      <svg v-if="!maximized" width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="0.5" y="0.5" width="9" height="9" /></svg>
      <svg v-else width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M2.5 2.5v-2h7v7h-2" /><rect x="0.5" y="2.5" width="7" height="7" /></svg>
    </button>
    <button type="button" class="wc-btn wc-close" :aria-label="$t('win.close')" v-tip="{ text: $t('win.close'), place: 'bottom' }" @click="close">
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" /></svg>
    </button>
  </div>
</template>

<style scoped>
/* 贴齐窗右端：抵消 .titlebar 的 0 14px 内边距（负 margin 只作用于右缘，左侧间距由父级 gap 控制） */
.win-controls {
  display: flex;
  align-self: stretch;
  margin-right: -14px;
  flex: 0 0 auto;
}
/* Win11 标题键形态：46px 宽、整条高、直角、箭头光标；悬停浅灰、关闭悬停系统红 */
.wc-btn {
  width: 46px;
  border: none;
  background: transparent;
  color: var(--text-2);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: default;
  transition: background 0.12s ease, color 0.12s ease;
}
.wc-btn:hover { background: var(--hover); color: var(--text); }
.wc-btn:active { background: var(--border); color: var(--text); }
.wc-btn:focus:not(:focus-visible) { outline: none; }
.wc-close:hover { background: #c42b1c; color: #fff; }
.wc-close:active { background: #b2261a; color: #fff; }
.wc-btn svg {
  fill: none;
  stroke: currentColor;
  stroke-width: 1;
  shape-rendering: crispEdges;
}
</style>
