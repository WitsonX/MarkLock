<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import { listen } from '@tauri-apps/api/event'
import { useVaultStore } from '../stores/vault'
import type { StickyNoteTarget } from '../stores/vault'
import * as tauri from '../lib/tauri'
import { readNote, writeNote } from '../lib/sticky'
import { appIcons, ensureAppIcons } from '../lib/appinfo'
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
      picking: false,
      syncingScroll: false,
      saving: false,
      // 预览模式勾选任务清单的确认弹窗（与主编辑器同语义）：pendingTaskSrc 为改好勾选的源码，
      // text/checked 仅供展示（哪条任务、要勾上还是取消）
      showTaskConfirm: false,
      pendingTaskSrc: '',
      pendingTaskText: '',
      pendingTaskChecked: false,
      pwd: '',
      unlockErr: false,
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
  },
  watch: {
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
    window.addEventListener('keydown', this.onKey)
    window.addEventListener('blur', this.onBlur)
    // 用户点进/聚焦便签即视为「要用它」：解除后端「移开即隐藏」，此后交回 blur/Esc 收起。
    window.addEventListener('pointerdown', this.onEngage)
    window.addEventListener('focus', this.onEngage)
  },
  beforeUnmount() {
    window.removeEventListener('keydown', this.onKey)
    window.removeEventListener('blur', this.onBlur)
    window.removeEventListener('pointerdown', this.onEngage)
    window.removeEventListener('focus', this.onEngage)
  },
  methods: {
    async load() {
      const t = this.target
      if (!t) {
        this.content = ''
        this.needUnlock = false
        return
      }
      this.loading = true
      try {
        this.content = await readNote(t)
        this.dirty = false
        this.needUnlock = false
      } catch {
        // 读失败最常见原因是库未解锁；交由解锁表单处理（明文读取失败也走这里提示）
        this.needUnlock = true
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
    onInput(v: string) {
      this.content = v
      this.dirty = true
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
     *  落在代码块复制按钮 / 任务清单复选框上时跳过，避免误切视图。 */
    onPreviewDblClick(e: MouseEvent) {
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
    onBlur() {
      // 打开系统文件选择框会让本窗短暂失焦，此时不能收起（否则选完文件回来窗已隐藏）。
      if (this.picking) return
      // 失焦即收起：先落盘再隐藏，保证便签内容不丢
      this.hide()
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
    onEngage() {
      tauri.stickyEngage().catch(() => {})
    },
    /** 打开主窗口设置页：由 Rust 唤回主窗（含托盘隐藏态）并通知跳转。 */
    openSettings() {
      tauri.openSettings().catch(() => {})
    },
    launch(path: string) {
      tauri.launchLocal(path).catch((e) => message.error(String(e)))
    },
  },
})
</script>

<template>
  <div class="sticky">
    <!-- 自绘标题栏：整条可拖拽移动窗口（无边框窗靠它移动）；模式切换 / 设置 / 关闭集中在右上角 -->
    <div class="sticky-bar" data-tauri-drag-region>
      <span class="grip" aria-hidden="true">⠿</span>
      <span class="title" data-tauri-drag-region>{{ fileName || $t('sticky.title') }}</span>
      <span v-if="dirty" class="dot" :title="$t('sticky.unsaved')"></span>
      <div class="seg modes">
        <span :class="{ on: mode === 'edit' }" @click="mode = 'edit'">{{ $t('sticky.edit') }}</span>
        <span :class="{ on: mode === 'split' }" @click="mode = 'split'">{{ $t('sticky.split') }}</span>
        <span :class="{ on: mode === 'preview' }" @click="mode = 'preview'">{{ $t('sticky.preview') }}</span>
      </div>
      <button class="x" :title="$t('sticky.settings')" @click="openSettings">⚙</button>
      <button class="x" :title="$t('sticky.close')" @click="hide">×</button>
    </div>

    <div class="sticky-body">
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

      <!-- 正常三态：面板常驻布局流 + flex-basis 滑动过渡（对齐主编辑器 .panes 切换动画；不用 v-show，
           因 WKWebView 下 display 切换会中断 flex-basis 过渡） -->
      <template v-else>
        <div class="panes" :class="`mode-${mode}`">
          <div class="pane pane-editor" :class="{ collapsed: mode === 'preview' }">
            <CodeMirrorEditor ref="editorCm" :model-value="content" @update:model-value="onInput" :on-save="save" :on-scroll="onEditorScroll" />
          </div>
          <div ref="previewPane" class="pane pane-preview" :class="{ collapsed: mode === 'edit' }" @dblclick="onPreviewDblClick" @scroll="onPreviewScroll">
            <MarkdownPreview :source="content" :asset-base-dir="baseDir" @task-toggle="onTaskToggle" />
          </div>
        </div>
      </template>
    </div>

    <div v-if="quickLaunch.length" class="sticky-launch">
      <button v-for="item in quickLaunch" :key="item.path" :title="item.path" @click="launch(item.path)">
        <img v-if="iconMap[item.path]" :src="iconMap[item.path]" class="ql-ico" alt="" />
        <span>{{ item.name }}</span>
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
/* 模式切换分段控件：样式对齐主编辑器工具栏的 .seg（见 EditorView.vue），靠右贴关闭按钮 */
.modes {
  margin-left: auto;
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
  border-radius: 999px;
  padding: 4px 12px;
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}
.sticky-launch button:hover { color: var(--primary); border-color: var(--primary); }
.ql-ico { width: 22px; height: 22px; object-fit: contain; flex: none; }
</style>
