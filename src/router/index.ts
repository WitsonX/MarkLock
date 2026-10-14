import { createRouter, createWebHashHistory } from 'vue-router'
import { isStickyWindow } from '../lib/sticky'

const routes = [
  { path: '/', redirect: '/editor' },
  {
    path: '/unlock',
    name: 'unlock',
    component: () => import('../views/UnlockView.vue'),
  },
  {
    path: '/editor',
    name: 'editor',
    component: () => import('../views/EditorView.vue'),
  },
  {
    path: '/settings',
    name: 'settings',
    component: () => import('../views/SettingsView.vue'),
  },
  {
    path: '/dialogs',
    name: 'dialogs',
    component: () => import('../views/DialogsView.vue'),
  },
  {
    path: '/sticky',
    name: 'sticky',
    component: () => import('../views/StickyView.vue'),
  },
]

const router = createRouter({
  history: createWebHashHistory(),
  routes,
})

// 窗口与路由一一绑定：便签窗（Rust 标签 sticky）只允许 /sticky，主窗口只允许其它路由。
// 守卫统一兜底所有入口（初始加载、编程式跳转、托盘/热角唤出），避免串窗。
router.beforeEach((to) => {
  const sticky = isStickyWindow()
  if (sticky && to.name !== 'sticky') return { name: 'sticky' }
  if (!sticky && to.name === 'sticky') return { name: 'editor' }
  return true
})

export default router
