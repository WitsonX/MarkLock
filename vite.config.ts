import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri 前端产物输出到 dist（与 src-tauri/tauri.conf.json 的 frontendDist 对齐）
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  // 绿色版（canary）构建把提交号注入前端，用于在「关于」卡片标出构建来源；
  // 正式 release 不设置该环境变量，注入为空串，界面上不显示。
  define: {
    __MARKLOCK_BUILD__: JSON.stringify(process.env.MARKLOCK_BUILD ?? ''),
  },
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: 'dist',
    target: 'es2021',
    // 每次构建前自动清空 dist，避免带 hash 的旧产物逐次堆积
    // （否则 Tauri 打包会把整个 dist 内嵌进可执行文件，导致体积异常膨胀）。
    emptyOutDir: true,
  },
})
