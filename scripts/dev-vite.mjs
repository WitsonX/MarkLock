// dev:tauri 的 beforeDevCommand 替身：若 1420 端口已被本项目的 vite 服务
// （例如 npm run dev:app 后台拉起的那个），直接复用并退出，避免 strictPort 冲突；
// 否则前台启动 vite（等价于原 beforeDevCommand "npm run dev"）。
import { spawn } from 'node:child_process'

const PORT = 1420

try {
  const res = await fetch(`http://localhost:${PORT}/`, { signal: AbortSignal.timeout(1500) })
  const body = await res.text()
  // 校验响应确实是本项目的 vite 页面（index.html 标题含 MarkLock），防止误复用其他占用者
  if (res.ok && body.toLowerCase().includes('marklock')) {
    console.log(`[dev-vite] 端口 ${PORT} 已有本项目 vite 在跑，直接复用`)
    process.exit(0)
  }
} catch {
  // 端口空闲或探测失败，走正常启动
}

const child = spawn('npm', ['run', 'dev'], {
  stdio: 'inherit',
  shell: process.platform === 'win32',
})
child.on('exit', (code) => process.exit(code ?? 0))
