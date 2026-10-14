//! 便携便签的前端共享逻辑：窗口判定 + 按文件类型读写便签正文。
//!
//! 便签是一个独立的置顶小窗（Rust 标签 `sticky`），加载同一份前端但路由到 /sticky；
//! 本模块提供「当前是否便签窗」的判定，以及把设置里的 StickyNoteTarget 映射到
//! 对应读/写命令（明文 / 单文件 .mdl / 单文件库 .mdlb 内文件）。

import { getCurrentWindow } from '@tauri-apps/api/window'
import * as tauri from './tauri'
import type { StickyNoteTarget } from '../stores/vault'

/** 当前 WebView 是否为便携便签窗口（Rust 侧以 label `sticky` 创建）。 */
export function isStickyWindow(): boolean {
  try {
    return getCurrentWindow().label === 'sticky'
  } catch {
    return false
  }
}

/** 读取便签正文；库未解锁时后端会抛错，由调用方捕获后引导解锁。 */
export function readNote(target: StickyNoteTarget): Promise<string> {
  if (target.kind === 'plain') return tauri.readPlainFile(target.path)
  if (target.kind === 'mdlb') {
    return tauri.readFileVault(target.vaultId!, target.path, target.rel!)
  }
  return tauri.readFile(target.vaultId!, target.path)
}

/** 写回便签正文。 */
export function writeNote(target: StickyNoteTarget, content: string): Promise<void> {
  if (target.kind === 'plain') return tauri.writePlainFile(target.path, content)
  if (target.kind === 'mdlb') {
    return tauri.writeFileVault(target.vaultId!, target.path, target.rel!, content)
  }
  return tauri.writeFile(target.vaultId!, target.path, content)
}
