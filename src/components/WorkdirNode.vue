<script lang="ts">
import { defineComponent, type PropType } from 'vue'
import EncIcon from './EncIcon.vue'

/** 工作目录树节点（含懒加载标记） */
interface WorkNode {
  name: string
  path: string
  is_dir: boolean
  is_vault: boolean
  children: WorkNode[]
  open?: boolean
  loaded?: boolean
}

/**
 * 工作目录树的递归节点组件。
 *
 * 渲染任意层级的普通文件夹 / 库节点（目录库、.mdlb、.mdl）/ 普通文件；
 * 文件夹点击展开/折叠（懒加载由父级 EditorView 负责），子项容器带 data-drop-kind，
 * 使「把文件夹内的文件拖到上层」时 closest 能解析到其所属目录。
 * 组件通过 name 自引用递归，事件统一上抛给 EditorView。
 */
export default defineComponent({
  name: 'WorkdirNode',
  components: { EncIcon },
  props: {
    nodes: { type: Array as PropType<WorkNode[]>, required: true },
    /** 当前激活文件路径（蓝色高亮） */
    activePath: { type: String, default: '' },
    /** 右键菜单临时高亮路径（灰色） */
    highlight: { type: String, default: '' },
    /** 拖拽命中的放置目标 key（与 EditorView 约定：wddir:<path>） */
    dropKey: { type: String, default: '' },
    /** 是否处于拖拽中（内部拖拽或外部拖入，控制放置高亮） */
    dragging: { type: Boolean, default: false },
    /** 深度（用于缩进） */
    depth: { type: Number, default: 0 },
  },
  emits: ['toggle', 'open-file', 'open-vault', 'ctx-node'],
  computed: {
    /** 行左内边距（px）：基础 12，每深一层加 16。 */
    pad(): string {
      return 12 + this.depth * 16 + 'px'
    },
  },
  methods: {
    isCtx(nodePath: string): boolean {
      if (nodePath === this.activePath) return false
      return !!this.highlight && this.highlight === nodePath
    },
    /** 拖拽是否命中该文件夹（key 与 EditorView 侧一致：wddir:<path>）。 */
    isDropInto(nodePath: string): boolean {
      return this.dragging && this.dropKey === `wddir:${nodePath}`
    },
    onCtx(node: WorkNode, e: MouseEvent) {
      this.$emit('ctx-node', { node, e })
    },
  },
})
</script>

<template>
  <div class="wdn">
    <template v-for="node in nodes" :key="node.path">
      <!-- 普通文件夹（可展开，子项容器为放置目标） -->
      <template v-if="node.is_dir && !node.is_vault">
        <div
          class="t-item"
          :class="{ 'drop-into': isDropInto(node.path), 'ctx-on': isCtx(node.path) }"
          data-drag-kind="wd-dir"
          :data-drag-path="node.path"
          data-drop-kind="wd-dir"
          :data-drop-path="node.path"
          :style="{ paddingLeft: pad }"
          v-tip="node.path"
          @click="$emit('toggle', node)"
          @contextmenu.stop="onCtx(node, $event)"
        >
          <svg class="lead chev-i" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#8a919e" stroke-width="2" stroke-linecap="round" :style="!node.open ? 'transform:rotate(-90deg)' : ''"><path d="M6 9l6 6 6-6" /></svg>
          <svg class="lead" style="width:16px;height:16px" viewBox="0 0 24 24" fill="none" stroke="#faad14" stroke-width="1.8" stroke-linejoin="round"><path d="M4 7V6a2 2 0 0 1 2-2h2l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" /></svg>
          <span class="fname">{{ node.name }}</span>
          <span class="acts">
            <span class="icon-btn" title="更多操作" @click.stop="onCtx(node, $event)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
          </span>
        </div>
        <div class="wdn-sub" v-show="node.open" data-drop-kind="wd-dir" :data-drop-path="node.path">
          <WorkdirNode
            v-if="node.children && node.children.length"
            :nodes="node.children"
            :active-path="activePath"
            :highlight="highlight"
            :drop-key="dropKey"
            :dragging="dragging"
            :depth="depth + 1"
            @toggle="(n: any) => $emit('toggle', n)"
            @open-file="(n: any) => $emit('open-file', n)"
            @open-vault="(n: any) => $emit('open-vault', n)"
            @ctx-node="(p: any) => $emit('ctx-node', p)"
          />
          <div v-else-if="node.loaded" class="wdn-empty" :style="{ paddingLeft: 12 + (depth + 1) * 16 + 'px' }">空文件夹</div>
        </div>
      </template>

      <!-- 库节点（目录库 / .mdlb / .mdl）：原子项，不展开、不可拖拽 -->
      <div
        v-else-if="node.is_vault"
        class="t-item"
        :class="{ on: activePath === node.path, 'ctx-on': isCtx(node.path) }"
        :data-wpath="node.path"
        :style="{ paddingLeft: pad }"
        v-tip="node.path"
        @click="$emit('open-vault', node)"
        @contextmenu.stop="onCtx(node, $event)"
      >
        <span style="width:14px;flex:0 0 14px;"></span>
        <EncIcon v-if="node.is_dir" class="lead" type="vault" color="#1677ff" :size="14" />
        <EncIcon v-else-if="node.name.endsWith('.mdlb')" class="lead" type="vault" color="#1677ff" :size="14" />
        <EncIcon v-else class="lead" type="file" color="#1677ff" :size="14" />
        <span class="fname">{{ node.name }}</span>
        <span class="acts">
          <span class="icon-btn" title="更多操作" @click.stop="onCtx(node, $event)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
        </span>
      </div>

      <!-- 普通文件（可拖拽源，data-wpath 供切页签滚入可见） -->
      <div
        v-else
        class="t-item"
        :class="{ on: activePath === node.path, 'ctx-on': isCtx(node.path) }"
        data-drag-kind="wd-file"
        :data-drag-path="node.path"
        :data-wpath="node.path"
        :style="{ paddingLeft: pad }"
        v-tip="node.path"
        @click="$emit('open-file', node)"
        @contextmenu.stop="onCtx(node, $event)"
      >
        <span style="width:14px;flex:0 0 14px;"></span>
        <EncIcon class="lead" type="plain" color="#8a919e" :size="14" />
        <span class="fname">{{ node.name }}</span>
        <span class="acts">
          <span class="icon-btn" title="更多操作" @click.stop="onCtx(node, $event)"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.7" /><circle cx="12" cy="12" r="1.7" /><circle cx="19" cy="12" r="1.7" /></svg></span>
        </span>
      </div>
    </template>
  </div>
</template>

<style scoped>
.wdn { width: 100%; }
.wdn-sub { width: 100%; }
.t-item {
  display: flex; align-items: center; gap: 5px;
  height: 29px; padding-right: 10px;
  font-size: 13px; color: var(--text-2);
  cursor: pointer; -webkit-user-select: none; user-select: none;
  position: relative;
}
.t-item:hover { background: var(--hover); }
.t-item.on { background: var(--primary-bg); color: var(--primary-active); font-weight: 500; }
.t-item.ctx-on { background: var(--hover); }
.t-item.drop-into { background: var(--primary-bg); box-shadow: inset 0 0 0 1.5px var(--primary); }
.t-item .fname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.t-item svg.lead { flex: 0 0 auto; }
.chev-i { transition: transform 0.15s; }
.t-item .acts { flex: 0 0 auto; display: none; gap: 2px; align-items: center; }
.t-item:hover .acts { display: flex; }
.t-item .acts .icon-btn { width: 18px; height: 18px; display: flex; align-items: center; justify-content: center; color: var(--text-4); border-radius: 4px; }
.t-item .acts .icon-btn:hover { background: var(--border); color: var(--text); }
.wdn-empty { height: 24px; display: flex; align-items: center; font-size: 12px; color: var(--text-4); }
</style>
