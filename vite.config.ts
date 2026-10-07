import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri 前端产物输出到 dist（与 src-tauri/tauri.conf.json 的 frontendDist 对齐）
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
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
