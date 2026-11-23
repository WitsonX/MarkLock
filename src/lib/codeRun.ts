//! 预览代码块「运行」：可运行语言判定、右上角按钮标记、代码块正文读取。
//! 真正的执行在 Rust（`run_in_terminal`）——这里只把语言映射成运行环境并交给终端。
//!
//! 运行按钮与复制按钮同形（同一套 `.cc-*` 图标 / 文字结构），追加在语言标签与复制之间；
//! 渲染层在 `lib/preview.ts` 生成，就地编辑切换语言后由 `lib/codeLang.ts` 调 `syncRunButton` 同步。

import { t as translate } from '../locales'
import * as tauri from './tauri'

/** 运行环境：bash 系（macOS/Linux 用系统 bash，Windows 用 Git Bash）与 PowerShell。 */
export type RunShell = 'bash' | 'powershell'

/** 可运行的围栏语言 → 运行环境。别名与 highlight.js 一致：`sh`/`zsh`/`ps1` 这些常见写法都认。 */
const RUN_LANGS: Record<string, RunShell> = {
  bash: 'bash',
  sh: 'bash',
  shell: 'bash',
  zsh: 'bash',
  powershell: 'powershell',
  ps1: 'powershell',
  pwsh: 'powershell',
}

/** 该语言能否在终端里运行；不能返回 null。 */
export function runShellOf(lang: string): RunShell | null {
  return RUN_LANGS[(lang || '').toLowerCase()] ?? null
}

/** 运行按钮标记（空串 = 该语言不可运行，不渲染按钮）。 */
export function runButtonHtml(lang: string): string {
  if (!runShellOf(lang)) return ''
  const txt = translate('common.run')
  const aria = translate('editor.runCodeAria')
  return `<button type="button" contenteditable="false" class="code-run-btn" aria-label="${aria}" title="${txt}"><svg class="cc-ico" viewBox="0 0 16 16" width="12" height="12" fill="currentColor" stroke="none" aria-hidden="true"><path d="M5.2 3.3c0-.5.55-.8.98-.54l6.2 3.98c.42.27.42.88 0 1.15l-6.2 3.98a.64.64 0 0 1-.98-.54z"/></svg><span class="cc-txt">${txt}</span></button>`
}

/** 代码块语言变了（就地编辑里改围栏）后增删运行按钮：插在复制按钮之前，保持「语言 / 运行 / 复制」次序。 */
export function syncRunButton(pre: HTMLElement, lang: string): void {
  const ctl = pre.querySelector('.code-ctl')
  if (!ctl) return
  const old = ctl.querySelector('.code-run-btn')
  if (!runShellOf(lang)) {
    old?.remove()
    return
  }
  if (old) return
  const copy = ctl.querySelector('.code-copy-btn')
  const html = runButtonHtml(lang)
  if (!html) return
  if (copy) copy.insertAdjacentHTML('beforebegin', html)
  else ctl.insertAdjacentHTML('beforeend', html)
}

/** 从运行按钮所在代码块取正文与运行环境；不是可运行的代码块返回 null。 */
export function runInfoOf(btn: HTMLElement): { shell: RunShell; text: string } | null {
  const pre = btn.closest('pre')
  const code = pre?.querySelector('code')
  if (!pre || !code) return null
  const m = (code.getAttribute('class') || '').match(/language-([\w+.-]+)/)
  const shell = runShellOf(m ? m[1] : '')
  if (!shell) return null
  const text = (code.textContent ?? '').replace(/^\n/, '').replace(/\n$/, '')
  if (!text.trim()) return null
  return { shell, text }
}

/** 交给 Rust 在终端新页签里执行。 */
export function runCode(shell: RunShell, code: string): Promise<void> {
  return tauri.runInTerminal(shell, code)
}