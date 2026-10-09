<script lang="ts">
import { defineComponent, type PropType } from 'vue'
import type { FsNode } from '../lib/tauri'

/**
 * 库内目录树的递归节点组件。
 *
 * 渲染任意层级的文件夹与文件；文件夹可展开/折叠；
 * 文件点击、文件夹上「新建文件/文件夹」通过事件上抛给 EditorView 处理。
 * 组件自身通过 name 递归调用，无需额外注册。
 */
export default defineComponent({
  name: 'VaultTreeNode',
  props: {
    nodes: { type: Array as PropType<FsNode[]>, required: true },
    /** 库 id（拖拽源/目标定位用） */
    vaultId: { type: String, default: '' },
    /** 当前拖拽命中的放置目标 key（与 EditorView 约定一致） */
    dropKey: { type: String, default: '' },
    /** 是否处于文件拖拽中（控制放置高亮） */
    dragging: { type: Boolean, default: false },
    /** 库颜色（文件图标描边） */
    color: { type: String, default: '#1677ff' },
    /** 当前激活文件路径（高亮） */
    activePath: { type: String, default: '' },
    /** 右键菜单临时高亮路径（与 activePath 同样以蓝底标出，菜单关闭即清空） */
    highlight: { type: String, default: '' },
    /** 折叠状态表（key = 节点 path） */
    closed: { type: Object as PropType<Record<string, boolean>>, required: true },
    /** 深度（用于缩进） */
    depth: { type: Number, default: 0 },
    /** 祖先嵌套容器已累积的缩进（px），用于抵消每层根 padding 的重复叠加 */
    offset: { type: Number, default: 0 },
    /** 外层包裹容器（如 .t-sub）自身的 padding-left，同样需要抵消 */
    base: { type: Number, default: 0 },
  },
  emits: ['open-file', 'new-file', 'new-folder', 'rename-node', 'delete-node', 'ctx-node'],
  computed: {
    isClosed(): (id: string) => boolean {
      return (id: string) => !!this.closed[id]
    },
  },
  methods: {
    /** 选中判断（仅当前激活页签，蓝色）：单文件库内文件的页签路径是 `库路径#相对路径`，需剥前缀后再比；
     *  库内相对路径统一用 '/'，而页签/盘符路径可能带 '\'，归一化分隔符再比。 */
    isOn(nodePath: string): boolean {
      const ap = this.activePath
      if (!ap) return false
      const rel = ap.includes('#') ? ap.slice(ap.lastIndexOf('#') + 1) : ap
      const norm = (s: string) => s.replace(/\\/g, '/')
      return norm(ap) === norm(nodePath) || norm(rel) === norm(nodePath)
    },
    /** 右键菜单临时高亮（非激活页签，灰色）：highlight 直接与节点 path 比对；若已是激活页签则让位给蓝色。 */
    isCtx(nodePath: string): boolean {
      if (this.isOn(nodePath)) return false
      const hp = this.highlight
      if (!hp) return false
      const norm = (s: string) => s.replace(/\\/g, '/')
      return norm(hp) === norm(nodePath)
    },
    /** 行缩进（px）：按 depth 计算并抵消祖先容器/外层包裹已累积的缩进，避免嵌套叠加。 */
    pad(extra: number): string {
      return Math.max(0, 8 + this.depth * 12 + extra - this.offset - this.base) + 'px'
    },
    toggle(id: string) {
      this.closed[id] = !this.closed[id]
    },
    onFile(node: FsNode) {
      if (node.is_dir) return
      this.$emit('open-file', node)
    },
    onNewFile(node: FsNode) {
      this.$emit('new-file', node)
    },
    onNewFolder(node: FsNode) {
      this.$emit('new-folder', node)
    },
    onRename(node: FsNode) {
      this.$emit('rename-node', node)
    },
    onDelete(node: FsNode) {
      this.$emit('delete-node', node)
    },
    /** 右键节点：上传鼠标事件与节点，交由 EditorView 自绘上下文菜单。 */
    onCtx(node: FsNode, e: MouseEvent) {
      this.$emit('ctx-node', { node, e })
    },
    /** 当前拖拽是否命中该放置目标（key 与 EditorView 侧一致：vltdir:<vaultId>:<path>）。 */
    isDropInto(nodePath: string): boolean {
      return this.dragging && this.dropKey === `vltdir:${this.vaultId}:${nodePath}`
    },
  },
})
</script>

<template>
  <div class="vtn">
    <template v-for="node in nodes" :key="node.path">
      <!-- 文件夹 -->
      <template v-if="node.is_dir">
        <div class="vtn-row vtn-folder" :class="{ 'ctx-on': isCtx(node.path), 'drop-into': isDropInto(node.path) }" :data-drag-kind="'vlt-dir'" :data-drag-path="node.path" :data-drag-vault="vaultId" data-drop-kind="vlt-dir" :data-drop-path="node.path" :data-drop-vault="vaultId" :style="{ paddingLeft: pad(0) }" v-tip="node.path" @click="toggle(node.path)" @contextmenu.prevent.stop="onCtx(node, $event)">
          <svg class="lead chev-i" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#8a919e" stroke-width="2" stroke-linecap="round" :style="isClosed(node.path) ? 'transform:rotate(-90deg)' : ''"><path d="M6 9l6 6 6-6" /></svg>
          <svg class="lead" style="width:16px;height:16px" viewBox="0 0 24 24" fill="none" stroke="#faad14" stroke-width="1.8" stroke-linejoin="round"><path d="M4 7V6a2 2 0 0 1 2-2h2l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" /></svg>
          <span class="fname">{{ node.name }}</span>
          <span class="acts">
            <span class="icon-btn" title="更多操作" @click.stop="onCtx(node, $event)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
          </span>
        </div>
        <div v-show="!isClosed(node.path)" data-drop-kind="vlt-dir" :data-drop-path="node.path" :data-drop-vault="vaultId">
          <VaultTreeNode
            v-if="node.children && node.children.length"
            :nodes="node.children"
            :vault-id="vaultId"
            :drop-key="dropKey"
            :dragging="dragging"
            :color="color"
            :active-path="activePath"
            :highlight="highlight"
            :closed="closed"
            :depth="depth + 1"
            :offset="offset + 8 + depth * 12"
            @open-file="onFile"
            @new-file="onNewFile"
            @new-folder="onNewFolder"
            @rename-node="onRename"
            @delete-node="onDelete"
            @ctx-node="(p: any) => $emit('ctx-node', p)"
          />
        </div>
      </template>

      <!-- 文件（data-vpath 供切页签时滚入可见） -->
      <div
        v-else
        class="vtn-row vtn-file"
        :class="{ on: isOn(node.path), 'ctx-on': isCtx(node.path) }"
        :data-drag-kind="'vlt-file'"
        :data-drag-path="node.path"
        :data-drag-vault="vaultId"
        :data-vpath="node.path"
        :style="{ paddingLeft: pad(26) }"
        v-tip="node.path"
        @click="onFile(node)"
        @contextmenu.prevent.stop="onCtx(node, $event)"
      >
        <svg class="lead" style="width:15px;height:15px" viewBox="0 0 24 24" fill="none" :stroke="color" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" /><path d="M14 3v6h6" /></svg>
        <span class="fname">{{ node.name }}</span>
        <span class="acts">
          <span class="icon-btn" title="更多操作" @click.stop="onCtx(node, $event)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
        </span>
      </div>
    </template>
  </div>
</template>

<style scoped>
.vtn { width: 100%; }
.vtn-row {
  display: flex; align-items: center; gap: 5px;
  height: 28px; padding-right: 10px;
  font-size: 13px; color: var(--text-2);
  cursor: pointer; -webkit-user-select: none; user-select: none;
}
.vtn-row:hover { background: var(--hover); }
.vtn-row .fname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.vtn-row svg.lead { flex: 0 0 auto; }
.vtn-row.on { background: var(--primary-bg); color: var(--primary-active); font-weight: 500; }
/* 右键菜单临时选中（非当前激活页签）：中性灰底 */
.vtn-row.ctx-on { background: var(--hover); }
/* 文件拖拽命中放置目标（文件夹行）：主色底 + 内描边 */
.vtn-row.drop-into { background: var(--primary-bg); box-shadow: inset 0 0 0 1.5px var(--primary); }
.vtn-row .acts { flex: 0 0 auto; display: none; gap: 2px; align-items: center; }
.vtn-row:hover .acts { display: flex; }
.icon-btn {
  width: 18px; height: 18px; display: flex; align-items: center; justify-content: center;
  color: var(--text-4); border-radius: 4px;
}
.icon-btn:hover { background: var(--hover); color: var(--primary-active); }
.icon-btn.danger:hover { background: #fff1f0; color: #cf1322; }
</style>
