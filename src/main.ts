import { createApp } from 'vue'
import { createPinia } from 'pinia'
import Antd from 'ant-design-vue'
import 'ant-design-vue/dist/reset.css'
import App from './App.vue'
import router from './router'
import tip from './lib/tooltip'
import { dismissSplash } from './lib/splash'
import './styles/global.css'

// 平台标记：macOS 用隐藏原生标题栏方案（红绿灯叠加到自绘工具条），
// Windows/Linux 保留系统原生标题栏，自绘工具条左侧无需预留红绿灯空间。
const ua = navigator.userAgent.toLowerCase()
const isMac = ua.includes('mac os') || (navigator.platform || '').toLowerCase().includes('mac')
document.documentElement.dataset.platform = isMac ? 'macos' : 'other'

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.use(Antd)
app.directive('tip', tip)
try {
  app.mount('#app')
} finally {
  // 挂载完成（或异常）后收起启动屏；内部保证最短展示时长后淡出移除
  dismissSplash()
}
