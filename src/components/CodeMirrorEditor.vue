<script lang="ts">
import { defineComponent, watch, onMounted, onBeforeUnmount, ref, computed } from 'vue'
import { EditorView, keymap, lineNumbers, highlightActiveLineGutter, highlightSpecialChars, drawSelection, dropCursor, rectangularSelection, crosshairCursor, highlightActiveLine } from '@codemirror/view'
import { EditorState, Compartment, Transaction, ChangeSet } from '@codemirror/state'
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { syntaxHighlighting, defaultHighlightStyle, bracketMatching, foldGutter, indentOnInput } from '@codemirror/language'
import { searchKeymap, highlightSelectionMatches, search, openSearchPanel } from '@codemirror/search'
import { autocompletion, completionKeymap, closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete'
import { oneDark } from '@codemirror/theme-one-dark'

export default defineComponent({
  name: 'CodeMirrorEditor',
  props: {
    modelValue: { type: String, default: '' },
    readonly: { type: Boolean, default: false },
    onSave: { type: Function, default: null },
    wordWrap: { type: Boolean, default: true },
    onScroll: { type: Function, default: null },
  },
  emits: ['update:modelValue'],
  setup(props, { emit }) {
    const editorRef = ref<HTMLDivElement>()
    let view: EditorView | null = null

    // 可动态重配置的扩展必须放进 Compartment，否则 dispatch reconfigure 会抛异常
    const wrapCompartment = new Compartment()
    const themeCompartment = new Compartment()
    const activeLineCompartment = new Compartment()
    const readOnlyCompartment = new Compartment()

    const isDark = computed(() => document.documentElement.dataset.theme === 'dark')

    const customLightTheme = EditorView.theme({
      '&': {
        backgroundColor: 'var(--editor-bg)',
        color: 'var(--text)',
        height: '100%',
      },
      '.cm-content': {
        caretColor: 'var(--primary)',
        fontFamily: 'var(--font-mono, ui-monospace, monospace)',
        // 字号跟随全局设置变量（--editor-font-size 由 EditorView 写在 documentElement 上），
        // 避免改字号时重建主题/重新配置 Compartment
        fontSize: 'var(--editor-font-size, 14px)',
        lineHeight: '1.6',
      },
      '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--primary)' },
      // 选中背景：用半透明主色蓝，明显区别于编辑器底色与当前行底色；聚焦态更深、失焦态更浅
      '.cm-selectionBackground': {
        backgroundColor: 'rgba(22, 119, 255, 0.16)',
      },
      '&.cm-focused .cm-selectionBackground': {
        backgroundColor: 'rgba(22, 119, 255, 0.30)',
      },
      '.cm-content ::selection': {
        backgroundColor: 'rgba(22, 119, 255, 0.30)',
      },
      '.cm-gutters': {
        backgroundColor: 'var(--editor-bg, #f5f5f5)',
        color: 'var(--text-4, #999)',
        border: 'none',
        borderRight: '1px solid var(--border)',
      },
      '.cm-activeLineGutter': {
        backgroundColor: 'var(--hover)',
        color: 'var(--text)',
      },
      '.cm-foldGutter .cm-gutterElement': {
        color: 'var(--text-4, #999)',
      },
    }, { dark: false })

    const customDarkTheme = EditorView.theme({
      '&': {
        backgroundColor: 'var(--editor-bg)',
        color: 'var(--text)',
        height: '100%',
      },
      '.cm-content': {
        caretColor: 'var(--primary)',
        fontFamily: 'var(--font-mono, ui-monospace, monospace)',
        fontSize: 'var(--editor-font-size, 14px)',
        lineHeight: '1.6',
      },
      '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--primary)' },
      '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': {
        backgroundColor: 'var(--primary-bg, rgba(22, 119, 255, 0.25))',
      },
      '.cm-gutters': {
        backgroundColor: 'var(--editor-bg, #1f1f1f)',
        color: 'var(--text-4, #666)',
        border: 'none',
        borderRight: '1px solid var(--border)',
      },
      '.cm-activeLineGutter': {
        backgroundColor: 'var(--hover)',
        color: 'var(--text)',
      },
      '.cm-foldGutter .cm-gutterElement': {
        color: 'var(--text-4, #666)',
      },
    }, { dark: true })

    // 当前行背景必须半透明：CodeMirror 选中图层 zIndex 为 -1，画在行内背景之下，
    // 若当前行用不透明色会把落在当前行上的多行选区高亮整个盖住
    const activeLineLight = EditorView.theme({
      '.cm-activeLine': { backgroundColor: 'rgba(15, 23, 42, 0.045)' },
    }, { dark: false })
    const activeLineDark = EditorView.theme({
      '.cm-activeLine': { backgroundColor: 'rgba(255, 255, 255, 0.06)' },
    }, { dark: true })

    const updateListener = EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        const newValue = update.state.doc.toString()
        emit('update:modelValue', newValue)
      }
    })

    const scrollListener = EditorView.domEventHandlers({
      scroll: (_event, v) => {
        if (!props.onScroll) return
        // 上报当前视口顶部的源码行号（供分屏行级同步）：用内容区左缘 + 视口顶命中坐标反查 pos
        const vr = v.scrollDOM.getBoundingClientRect()
        const cr = v.contentDOM.getBoundingClientRect()
        const p = v.posAtCoords({ x: cr.left + 2, y: vr.top + 1 })
        const pos = typeof p === 'number' ? p : (p as any)?.pos
        const line = pos != null ? v.state.doc.lineAt(pos).number : 1
        props.onScroll(line)
      },
    })

    const saveKeymap = keymap.of([{
      key: 'Mod-s',
      run: () => {
        if (props.onSave) {
          props.onSave()
          return true
        }
        return false
      },
    }])

    // ---------- 格式化操作（供工具栏/快捷键调用） ----------
    /** 用符号包裹选区；无选区时插入占位文本并选中 */
    const wrapSelection = (before: string, after = before, placeholder = '文本') => {
      if (!view) return
      const st = view.state
      const { from, to } = st.selection.main
      const sel = st.sliceDoc(from, to) || placeholder
      view.dispatch({
        changes: { from, to, insert: before + sel + after },
        selection: { anchor: from + before.length, head: from + before.length + sel.length },
      })
      view.focus()
    }

    /** 选区覆盖的行设置/取消标题（再点同级取消） */
    const setHeading = (level: number) => {
      if (!view) return
      const st = view.state
      const { from, to } = st.selection.main
      const first = st.doc.lineAt(from).number
      const last = st.doc.lineAt(to).number
      const want = '#'.repeat(level) + ' '
      const sel = st.selection.main
      const changes: { from: number; to: number; insert: string }[] = []
      for (let n = first; n <= last; n++) {
        const line = st.doc.line(n)
        const cur = (line.text.match(/^#{1,6}\s+/) || [''])[0]
        changes.push({ from: line.from, to: line.from + cur.length, insert: cur === want ? '' : want })
      }
      // 显式映射光标：空行光标正好在插入点上，默认会留在符号前（行首），
      // assoc=1 让其跟到插入前缀之后，可继续输入；行中/行尾光标仍正常位移
      const cs = ChangeSet.of(changes, st.doc.length)
      view.dispatch({
        changes: cs,
        selection: { anchor: cs.mapPos(sel.anchor, 1), head: cs.mapPos(sel.head, 1) },
      })
      view.focus()
    }

    /** 选区覆盖的行切换行前缀（引用/列表）：全部已有则移除，否则补上 */
    const toggleLinePrefix = (prefix: string) => {
      if (!view) return
      const st = view.state
      const { from, to } = st.selection.main
      const first = st.doc.lineAt(from).number
      const last = st.doc.lineAt(to).number
      const lines = []
      for (let n = first; n <= last; n++) lines.push(st.doc.line(n))
      const allHas = lines.every((l) => l.text.startsWith(prefix))
      const sel = st.selection.main
      const changes: { from: number; to: number; insert: string }[] = []
      for (const l of lines) {
        if (allHas) changes.push({ from: l.from, to: l.from + prefix.length, insert: '' })
        else if (!l.text.startsWith(prefix)) changes.push({ from: l.from, to: l.from, insert: prefix })
      }
      // 同 setHeading：光标跟随到插入前缀之后，避免空行点完跳到行首
      if (changes.length) {
        const cs = ChangeSet.of(changes, st.doc.length)
        view.dispatch({
          changes: cs,
          selection: { anchor: cs.mapPos(sel.anchor, 1), head: cs.mapPos(sel.head, 1) },
        })
      }
      view.focus()
    }

    /** 插入独立块（表格/分割线），当前行非空时先空一行隔开 */
    const insertBlock = (text: string) => {
      if (!view) return
      const st = view.state
      const { from, to } = st.selection.main
      const lead = st.doc.lineAt(from).text.trim() ? '\n' : ''
      const insert = lead + text + '\n'
      view.dispatch({
        changes: { from, to, insert },
        selection: { anchor: from + insert.length },
      })
      view.focus()
    }

    /** 插入代码块：有选区时用 ``` 围栏包裹选中文本；无选区插入空围栏 */
    const insertCodeBlock = () => {
      if (!view) return
      const st = view.state
      const { from, to } = st.selection.main
      const sel = st.sliceDoc(from, to)
      const lead = st.doc.lineAt(from).text.trim() ? '\n' : ''
      const body = sel ? '```\n' + sel + '\n```' : '```\n\n```'
      // 光标落在开围栏后的内容行首，可直接输入代码（需要语言标注时按 ↑ 回上一行）
      view.dispatch({
        changes: { from, to, insert: lead + body },
        selection: { anchor: from + lead.length + 4 },
      })
      view.focus()
    }

    /** 插入链接/图片，选中 url 占位符便于直接替换 */
    const insertLink = (image = false) => {
      if (!view) return
      const st = view.state
      const { from, to } = st.selection.main
      const sel = st.sliceDoc(from, to)
      const text = sel || (image ? 'alt' : '链接文字')
      const bang = image ? '!' : ''
      const insert = `${bang}[${text}](url)`
      const urlFrom = from + bang.length + 1 + text.length + 2
      view.dispatch({
        changes: { from, to, insert },
        selection: { anchor: urlFrom, head: urlFrom + 3 },
      })
      view.focus()
    }

    const formatKeymap = keymap.of([
      { key: 'Mod-b', run: () => (wrapSelection('**'), true) },
      { key: 'Mod-i', run: () => (wrapSelection('*'), true) },
      { key: 'Mod-k', run: () => (insertLink(false), true) },
    ])

    const createExtensions = (dark: boolean) => [
      lineNumbers(),
      highlightActiveLineGutter(),
      highlightSpecialChars(),
      history(),
      foldGutter(),
      drawSelection(),
      dropCursor(),
      EditorState.allowMultipleSelections.of(true),
      indentOnInput(),
      syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
      bracketMatching(),
      closeBrackets(),
      autocompletion(),
      rectangularSelection(),
      crosshairCursor(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      search({ top: true }),
      keymap.of([
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...completionKeymap,
        indentWithTab,
      ]),
      saveKeymap,
      formatKeymap,
      markdown({ base: markdownLanguage }),
      themeCompartment.of(dark ? oneDark : customLightTheme),
      activeLineCompartment.of(dark ? activeLineDark : activeLineLight),
      updateListener,
      scrollListener,
      // 只读要同时上两道：EditorState.readOnly 只拦住 command / 粘贴 / 剪切，
      // 普通字符输入走 contenteditable 的 DOM 变更（仅被 observer 忽略，会留下 DOM 与 doc 不一致），
      // 故再关掉 editable，让 contentDOM 不再 contenteditable，键盘/IME 根本改不到正文
      readOnlyCompartment.of([EditorState.readOnly.of(props.readonly), EditorView.editable.of(!props.readonly)]),
      wrapCompartment.of(props.wordWrap ? EditorView.lineWrapping : []),
    ]

    /** 视口顶部所在的源码行号（与 scrollListener 同口径：用内容区左缘 + 视口顶反查 pos）。 */
    function topViewportLine(v: EditorView): number {
      const vr = v.scrollDOM.getBoundingClientRect()
      const cr = v.contentDOM.getBoundingClientRect()
      const p = v.posAtCoords({ x: cr.left + 2, y: vr.top + 1 })
      const pos = typeof p === 'number' ? p : (p as any)?.pos
      return pos != null ? v.state.doc.lineAt(pos).number : 1
    }

    onMounted(() => {
      if (!editorRef.value) return

      const state = EditorState.create({
        doc: props.modelValue,
        extensions: [
          EditorState.phrases.of({
            'Find': '查找',
            'Replace': '替换',
            'next': '下一个',
            'previous': '上一个',
            'all': '全部',
            'match case': '区分大小写',
            'regexp': '正则',
            'by word': '全字匹配',
            'replace': '替换',
            'replace all': '全部替换',
            'close': '关闭',
            'Go to line': '跳转到行',
            'go': '跳转',
            'replaced match on line $': '已替换第 $ 行的匹配',
            'replaced $ matches': '已替换 $ 处匹配',
            'current match': '当前匹配',
            'on line': '位于第',
            'Completions': '自动补全',
            'Folded lines': '已折叠行',
            'Unfolded lines': '已展开行',
            'to': '至',
            'folded code': '已折叠代码',
            'unfold': '展开',
            'Selection deleted': '已删除选区',
            'Control character': '控制字符',
          }),
          ...createExtensions(isDark.value),
        ],
      })

      view = new EditorView({
        state,
        parent: editorRef.value,
      })
    })

    // 外部内容变更（如监控到被其它程序修改后重载）：整篇替换但不进撤销历史、
    // 保留光标与视口所在行，避免把用户光标与滚动位置扔到文末。
    // （用户自己输入时 modelValue 与 doc 已一致，不会走到替换分支。）
    watch(() => props.modelValue, (newVal) => {
      if (!view) return
      const current = view.state.doc.toString()
      if (current === newVal) return
      const st = view.state
      const anchor = Math.min(st.selection.main.anchor, newVal.length)
      const head = Math.min(st.selection.main.head, newVal.length)
      const topLine = topViewportLine(view)
      view.dispatch({
        changes: { from: 0, to: current.length, insert: newVal },
        selection: { anchor, head },
        annotations: Transaction.addToHistory.of(false),
      })
      // 整篇替换后按行号重新对位视口（行数变少时 revealLine 会自动夹取）
      revealLine(topLine)
    })

    watch(() => props.readonly, (newVal) => {
      if (!view) return
      view.dispatch({
        effects: readOnlyCompartment.reconfigure([EditorState.readOnly.of(newVal), EditorView.editable.of(!newVal)]),
      })
    })

    watch(() => props.wordWrap, (newVal) => {
      if (!view) return
      view.dispatch({
        effects: wrapCompartment.reconfigure(newVal ? EditorView.lineWrapping : []),
      })
    })

    watch(isDark, (dark) => {
      if (!view) return
      view.dispatch({
        effects: [
          themeCompartment.reconfigure(dark ? oneDark : customLightTheme),
          activeLineCompartment.reconfigure(dark ? activeLineDark : activeLineLight),
        ],
      })
    })

    onBeforeUnmount(() => {
      view?.destroy()
    })

    const scrollToRatio = (ratio: number) => {
      if (!view) return
      const el = view.scrollDOM
      const maxScroll = el.scrollHeight - el.clientHeight
      el.scrollTop = ratio * maxScroll
    }

    /** 将指定源码行滚动到视口顶部（分屏反向同步用）；越界自动夹取，不改变选区。 */
    const revealLine = (line: number) => {
      if (!view) return
      const total = view.state.doc.lines
      const n = Math.max(1, Math.min(Math.round(line) || 1, total))
      const pos = view.state.doc.line(n).from
      view.dispatch({ effects: EditorView.scrollIntoView(pos, { y: 'start' }) })
    }

    /** 编辑器当前是否真的可见（非折叠、非 display:none）。预览模式下源码面板宽度被收到 0，
     *  此时绝不能把焦点抢给编辑器，否则用户随手敲的键会落进文档改掉内容。 */
    const isVisible = () => {
      if (!view) return false
      const el = view.dom
      return el.clientWidth > 2 && el.clientHeight > 2 && !!el.offsetParent
    }

    /** 光标定位到指定源码行行首并滚入视口（大纲跳转用）；越界自动夹取。
     *  仅在编辑器可见时才抢焦点，预览模式下只滚动定位，避免键盘输入落进文档。 */
    const setCursorToLine = (line: number) => {
      if (!view) return
      const total = view.state.doc.lines
      const n = Math.max(1, Math.min(Math.round(line) || 1, total))
      const pos = view.state.doc.line(n).from
      view.dispatch({ selection: { anchor: pos, head: pos }, effects: EditorView.scrollIntoView(pos, { y: 'start' }) })
      if (isVisible()) view.focus()
    }

    /** 选中 [from,to) 字符区间并滚入视口（全局搜索命中定位用），居中显示命中内容。
     *  可见时才抢焦点，预览模式（源码面板收起）下反而要把焦点还出去，见函数内说明。
     *  返回 false 表示实例还未挂载或正文还没同步进 doc（起点越界），调用方可下一帧重试。 */
    const revealRange = (from: number, to: number) => {
      if (!view) return false
      const len = view.state.doc.length
      if (from >= len) return false
      const a = Math.max(0, Math.min(Math.floor(from) || 0, len))
      const b = Math.max(a, Math.min(Math.ceil(to) || a, len))
      const apply = (withSelection: boolean) => {
        if (!view) return
        view.dispatch({
          selection: withSelection ? { anchor: a, head: b } : undefined,
          effects: EditorView.scrollIntoView(a, { y: 'center' }),
        })
        view.requestMeasure()
      }
      apply(true)
      // 页签刚从 display:none 切为可见时，首帧布局尺寸可能还没稳定，下一帧重新测量并对齐一次
      requestAnimationFrame(() => apply(false))
      // 焦点只在编辑器真可见（编辑 / 分屏）时才抢；预览模式下源码面板宽度为 0，
      // 而切页签时 store.activePath 的 watcher 会把焦点给编辑器，带着选中命中词的选区时，
      // 用户随手敲的第一个键就会在看不见的地方把命中内容替掉，故这里把焦点还出去。
      // （预览模式另靠 readOnly + editable=false 兜底，键盘根本改不到正文。）
      if (isVisible()) view.focus()
      else if (view.hasFocus) view.contentDOM.blur()
      return true
    }

    const openSearch = () => {
      if (view) openSearchPanel(view)
    }

    /** 聚焦编辑器（新建/切换文件后可直接输入） */
    const focus = () => {
      view?.focus()
    }

    /** 请求重新测量布局（字号等 CSS 变量变化后调用，让 CodeMirror 刷新行高缓存） */
    const requestMeasure = () => {
      view?.requestMeasure()
    }

    /** 在当前光标处插入文本（替换选区）：粘贴/拖入图片文件时插入 markdown 链接用。 */
    const insertText = (text: string) => {
      if (!view) return
      const { from, to } = view.state.selection.main
      view.dispatch({
        changes: { from, to, insert: text },
        selection: { anchor: from + text.length },
      })
      view.focus()
    }

    /** 当前视口顶部所在源码行号；实例未创建时回退为 1，供父组件切页签时同步预览区。 */
    const getTopViewportLine = (): number => (view ? topViewportLine(view) : 1)

    return { editorRef, scrollToRatio, revealLine, setCursorToLine, revealRange, openSearch, focus, requestMeasure, insertText, wrapSelection, setHeading, toggleLinePrefix, insertBlock, insertCodeBlock, insertLink, getTopViewportLine }
  },
})
</script>

<template>
  <div ref="editorRef" class="cm-editor-wrap"></div>
</template>

<style scoped>
.cm-editor-wrap {
  height: 100%;
  width: 100%;
  overflow: hidden;
  flex: 1;
}
.cm-editor-wrap :deep(.cm-editor) {
  height: 100%;
  width: 100%;
}
.cm-editor-wrap :deep(.cm-scroller) {
  overflow: auto;
  font-family: var(--font-mono, ui-monospace, monospace);
}
</style>
