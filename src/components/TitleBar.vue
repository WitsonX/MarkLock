<script lang="ts">
import { defineComponent } from 'vue'

export default defineComponent({
  name: 'TitleBar',
  props: {
    // 主标题（可选），如「设置」
    title: { type: String, default: '' },
    // 右侧副标题（可选），如「MarkLock」
    subtitle: { type: String, default: '' },
    // 是否透明（解锁页、弹窗页用透明标题栏）
    transparent: { type: Boolean, default: false },
  },
})
</script>

<template>
  <div class="titlebar" data-tauri-drag-region="deep" :style="transparent ? 'background:transparent;border-bottom:none;' : ''">
    <span v-if="title" style="font-size: 13px; font-weight: 600;">{{ title }}</span>
    <span v-else style="font-size: 13px; color: var(--text-3);">MarkLock</span>
    <span v-if="subtitle" style="font-size: 12px; color: var(--text-3);">{{ subtitle }}</span>
    <slot />
  </div>
</template>
