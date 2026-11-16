<script lang="ts">
import { defineComponent } from 'vue'
import { isWindows } from '../lib/platform'
import WindowControls from './WindowControls.vue'

export default defineComponent({
  name: 'TitleBar',
  components: { WindowControls },
  props: {
    // 主标题（可选），如「设置」
    title: { type: String, default: '' },
    // 右侧副标题（可选），如「MarkLock」
    subtitle: { type: String, default: '' },
    // 是否透明（解锁页、弹窗页用透明标题栏）
    transparent: { type: Boolean, default: false },
  },
  setup() {
    return { isWindows }
  },
})
</script>

<template>
  <div class="titlebar" data-tauri-drag-region="deep" :style="transparent ? 'background:transparent;border-bottom:none;' : ''">
    <span v-if="title" style="font-size: 13px; font-weight: 600;">{{ title }}</span>
    <span v-else style="font-size: 13px; color: var(--text-3);">MarkLock</span>
    <span v-if="subtitle" style="font-size: 12px; color: var(--text-3);">{{ subtitle }}</span>
    <slot />
    <!-- Windows 无边框窗：原生标题栏已去掉，三键整合进本条右端（macOS 红绿灯在左、Linux 保留原生标题栏） -->
    <WindowControls v-if="isWindows" style="margin-left: auto;" />
  </div>
</template>
