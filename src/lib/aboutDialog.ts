// 「关于」独立弹窗的全局开关：EditorView 主菜单 / 原生菜单 均可通过 openAboutDialog() 唤起，
// App.vue 常驻渲染 a-modal，路由切换时不影响弹窗可见性。
import { ref } from 'vue'

export const aboutDialogOpen = ref(false)

export function openAboutDialog() {
  aboutDialogOpen.value = true
}

export function closeAboutDialog() {
  aboutDialogOpen.value = false
}
