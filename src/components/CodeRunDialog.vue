<script lang="ts">
import { defineComponent } from 'vue'
import type { RunShell } from '../lib/codeRun'
import { isWindows } from '../lib/platform'

/**
 * 「运行代码块」确认弹窗：主编辑器与便携便签共用。
 * 代码块正文来自当前文档，运行意味着把它交给本机终端、以用户权限执行；
 * 弹窗完整展示将执行的代码，勾选「不再提醒」后由宿主把 settings.confirmRunCode 落成 false，
 * 之后点运行直接执行（可在设置 → 安全里重新打开确认）。
 */
export default defineComponent({
  name: 'CodeRunDialog',
  props: {
    open: { type: Boolean, default: false },
    code: { type: String, default: '' },
    shell: { type: String as () => RunShell, default: 'bash' },
  },
  emits: ['update:open', 'run'],
  data() {
    return {
      dontAsk: false,
      // 便签窗只有一个很小的浮动窗口，弹窗宽度跟着窗口收，避免超出屏幕
      width: 460,
    }
  },
  computed: {
    shellLabel(): string {
      return this.shell === 'powershell' ? 'PowerShell' : 'Bash'
    },
    winFallbackNote(): boolean {
      return isWindows && this.shell !== 'powershell'
    },
  },
  watch: {
    open(v: boolean) {
      if (!v) return
      this.dontAsk = false
      this.width = Math.min(460, Math.max(280, window.innerWidth - 32))
    },
  },
  methods: {
    close() {
      this.$emit('update:open', false)
    },
    submit() {
      this.$emit('run', this.dontAsk)
      this.close()
    },
  },
})
</script>

<template>
  <a-modal
    :open="open"
    :title="$t('editor.runTitle')"
    :width="width"
    :footer="null"
    @cancel="close"
    @update:open="(v: boolean) => $emit('update:open', v)"
  >
    <div class="run-dlg">
      <div class="run-dlg-hint">{{ $t('editor.runHint', { shell: shellLabel }) }}</div>
      <div v-if="winFallbackNote" class="run-dlg-hint">{{ $t('editor.runHintWinFallback') }}</div>
      <pre class="run-dlg-code">{{ code.trim() }}</pre>
      <a-checkbox v-model:checked="dontAsk" class="run-dlg-check">{{ $t('editor.runDontAsk') }}</a-checkbox>
      <div class="run-dlg-foot">
        <button class="btn" @click="close">{{ $t('common.cancel') }}</button>
        <button class="btn btn-primary" @click="submit">{{ $t('common.run') }}</button>
      </div>
    </div>
  </a-modal>
</template>

<style scoped>
.run-dlg { display: flex; flex-direction: column; gap: 12px; }
.run-dlg-hint { font-size: 13px; line-height: 1.65; color: var(--text-2); }
.run-dlg-code {
  margin: 0;
  max-height: 180px;
  overflow: auto;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--hover);
  color: var(--text);
  font-family: var(--mono);
  font-size: 12.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
}
.run-dlg-check { font-size: 12.5px; color: var(--text-2); }
.run-dlg-foot { display: flex; justify-content: flex-end; gap: 8px; padding-top: 4px; }
.run-dlg-foot .btn {
  display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  height: 32px; padding: 0 15px; font-size: 14px; border-radius: var(--radius-sm);
  border: 1px solid var(--border); background: var(--panel); color: var(--text);
  cursor: pointer; transition: all 0.2s; white-space: nowrap;
}
.run-dlg-foot .btn:hover { color: var(--primary-hover); border-color: var(--primary-hover); }
.run-dlg-foot .btn-primary { background: var(--primary); border-color: var(--primary); color: #fff; }
.run-dlg-foot .btn-primary:hover { background: var(--primary-hover); border-color: var(--primary-hover); color: #fff; }
</style>