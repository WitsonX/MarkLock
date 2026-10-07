//! 与 Rust 加密核心的桥接层。
//!
//! 通过 Tauri 的 `invoke` 调用 Rust 命令（见 `src-tauri/src/lib.rs`）。
//! 前端不接触任何密钥材料：密码只用于传给 Rust 派生 KEK，
//! 解密后的明文由 Rust 返回，密钥本身永不过 JS。

import { invoke } from '@tauri-apps/api/core'
import { open, save } from '@tauri-apps/plugin-dialog'

// ==================== 类型 ====================

/** 目录树节点（Rust FsNode 的镜像） */
export interface FsNode {
  name: string
  path: string
  is_dir: boolean
  is_vault: boolean
  children: FsNode[]
}

/** 路径探测结果（inspect_path 的镜像） */
export interface PathInfo {
  is_dir: boolean
  is_vault: boolean
  /** 是否为单文件库（`.mdlb`），区别于单文件 `.mdl` */
  is_file_vault: boolean
  /** 文件位于某个库目录内时，为该库目录路径 */
  parent_vault: string | null
}

/** 批量加密命令的返回结构（Rust BatchEncryptResult 镜像）。 */
export interface BatchEncryptResult {
  /** 生成的 `.mdl` 绝对路径 */
  encrypted: string[]
  /** 移动到同目录「原文件/」子目录后的绝对路径 */
  moved: string[]
  /** 失败明细：`[原路径, 错误消息]` */
  failed: Array<[string, string]>
}

/** 目录导入到库的返回结构（Rust ImportResult 镜像）。 */
export interface ImportResult {
  /** 成功导入的相对库根路径 */
  imported: string[]
  /** 被跳过的相对路径或原因（隐藏 / 目标已存在） */
  skipped: string[]
  /** 失败明细：`[相对路径, 错误消息]` */
  failed: Array<[string, string]>
}

// ==================== 库操作 ====================

/** 解锁一个加密文件（`.mdl`），成功后登记会话。 */
export function unlock(vaultId: string, path: string, password: string, autoLockSecs?: number) {
  return invoke<void>('unlock', {
    vaultId,
    path,
    password,
    autoLockSecs: autoLockSecs ?? null,
  })
}

/** 解锁一个加密库目录（含 vault.json 的目录）。 */
export function unlockVault(vaultId: string, dir: string, password: string, autoLockSecs?: number) {
  return invoke<void>('unlock_vault', {
    vaultId,
    dir,
    password,
    autoLockSecs: autoLockSecs ?? null,
  })
}

/** 解锁一个单文件库（`.mdlb`）。 */
export function unlockFileVault(
  vaultId: string,
  path: string,
  password: string,
  autoLockSecs?: number,
) {
  return invoke<void>('unlock_file_vault', {
    vaultId,
    path,
    password,
    autoLockSecs: autoLockSecs ?? null,
  })
}

/** 创建一个新的单文件加密库（`.mdlb`），落盘到 `path`。 */
export function createFileVault(path: string, password: string, hint?: string) {
  return invoke<number>('create_file_vault', { path, password, hint: hint ?? null })
}

/** 列出已解锁的单文件库目录树（递归，不含内容）。 */
export function listFileVault(vaultId: string, path: string) {
  return invoke<FsNode[]>('list_file_vault', { vaultId, path })
}

/** 读取单文件库内一个文件的正文。 */
export function readFileVault(vaultId: string, path: string, rel: string) {
  return invoke<string>('read_file_vault', { vaultId, path, rel })
}

/** 写回单文件库内一个文件的正文。 */
export function writeFileVault(vaultId: string, path: string, rel: string, content: string) {
  return invoke<void>('write_file_vault', { vaultId, path, rel, content })
}

/** 在单文件库内创建文件。 */
export function createInFileVault(vaultId: string, path: string, relPath: string, content: string) {
  return invoke<void>('create_in_file_vault', { vaultId, path, relPath, content })
}

/** 在单文件库内创建文件夹。 */
export function createFolderInFileVault(vaultId: string, path: string, relPath: string) {
  return invoke<void>('create_folder_in_file_vault', { vaultId, path, relPath })
}

/** 删除单文件库内的文件或目录。 */
export function deleteInFileVault(vaultId: string, path: string, relPath: string) {
  return invoke<void>('delete_in_file_vault', { vaultId, path, relPath })
}

/** 重命名 / 移动单文件库内的文件或目录。 */
export function renameInFileVault(vaultId: string, path: string, oldRel: string, newRel: string) {
  return invoke<void>('rename_in_file_vault', { vaultId, path, oldRel, newRel })
}

/** 修改单文件库主密码。 */
export function changeFileVaultPassword(
  vaultId: string,
  path: string,
  oldPassword: string,
  newPassword: string,
) {
  return invoke<void>('change_file_vault_password', {
    vaultId,
    path,
    oldPassword,
    newPassword,
  })
}

/** 创建一个新的加密库目录（生成 vault.json）。 */
export function createVault(dir: string, password: string, hint?: string) {
  return invoke<void>('create_vault', { dir, password, hint: hint ?? null })
}

/** 锁定单个库（清零内存密钥）。 */
export function lock(vaultId: string) {
  return invoke<void>('lock', { vaultId })
}

/** 锁定全部库。 */
export function lockAll() {
  return invoke<void>('lock_all')
}

/** 退出应用（后端先锁定全部库清零密钥再退出进程）。 */
export function quitApp() {
  return invoke<void>('quit_app')
}

// ==================== 文件读写 ====================

/** 读取并解密一个已解锁文件的正文。 */
export function readFile(vaultId: string, path: string) {
  return invoke<string>('read_file', { vaultId, path })
}

/** 加密并写回一个已解锁文件。 */
export function writeFile(vaultId: string, path: string, content: string) {
  return invoke<void>('write_file', { vaultId, path, content })
}

/** 读取一个普通（未加密）文件的明文正文。 */
export function readPlainFile(path: string) {
  return invoke<string>('read_plain_file', { path })
}

/** 明文写回一个普通（未加密）文件。 */
export function writePlainFile(path: string, content: string) {
  return invoke<void>('write_plain_file', { path, content })
}

/** 在工作目录（普通未加密目录）中创建一个新文件（绝对路径，已存在则失败）。 */
export function createPlainFile(path: string, content: string) {
  return invoke<void>('create_plain_file', { path, content })
}

/** 在工作目录中创建一个新文件夹（绝对路径）。 */
export function createPlainDir(path: string) {
  return invoke<void>('create_plain_dir', { path })
}

/** 重命名 / 移动一个普通文件路径（明文，绝对路径）。 */
export function renamePath(from: string, to: string) {
  return invoke<void>('rename_path', { from, to })
}

/** 删除一个普通文件 / 文件夹（明文，绝对路径；目录递归删除）。 */
export function deletePath(path: string) {
  return invoke<void>('delete_path', { path })
}

/** 创建一个新的加密文件，落盘到 `path`。 */
export function createFile(path: string, password: string, content: string, hint?: string) {
  return invoke<number>('create_file', { path, password, content, hint: hint ?? null })
}

/** 在已解锁库内创建新文件（相对库根路径）。 */
export function createInVault(vaultId: string, relPath: string, content: string) {
  return invoke<void>('create_in_vault', { vaultId, relPath, content })
}

/** 在已解锁库内创建空文件夹（相对库根路径）。 */
export function createFolderInVault(vaultId: string, relPath: string) {
  return invoke<void>('create_folder_in_vault', { vaultId, relPath })
}

/** 删除库内的文件或目录（相对库根路径）。 */
export function deleteInVault(vaultId: string, relPath: string) {
  return invoke<void>('delete_in_vault', { vaultId, relPath })
}

/** 重命名 / 移动库内的文件或目录（相对库根路径）。 */
export function renameInVault(vaultId: string, oldRel: string, newRel: string) {
  return invoke<void>('rename_in_vault', { vaultId, oldRel, newRel })
}

/** 修改主密码（重包裹 DEK，无需全文重加密）。 */
export function changePassword(
  vaultId: string,
  path: string,
  oldPassword: string,
  newPassword: string,
) {
  return invoke<void>('change_password', {
    vaultId,
    path,
    oldPassword,
    newPassword,
  })
}

// ==================== 目录浏览 ====================

/** 列出目录内容（一层，用于工作目录视图）。 */
export function listDir(dir: string) {
  return invoke<FsNode[]>('list_dir', { dir })
}

/** 列出已解锁库的目录树（递归，用于"打开的库"视图）。 */
export function listVault(vaultId: string, dir: string) {
  return invoke<FsNode[]>('list_vault', { vaultId, dir })
}

/** 探测一个路径的类型（拖入文件/目录时的路由依据）。 */
export function inspectPath(path: string) {
  return invoke<PathInfo>('inspect_path', { path })
}

// ==================== 外部修改监控 ====================

/** 磁盘文件戳记（stat 级别，不读内容）。 */
export interface FileStamp {
  /** 最后修改时间（Unix 毫秒）；文件不存在时为 0 */
  mtime_ms: number
  /** 字节大小；文件不存在时为 0 */
  size: number
}

/**
 * 批量查询文件戳记（外部修改监控）。
 * 返回数组与入参 `paths` 一一对应；重复路径在 Rust 侧去重后仍按原顺序展开。
 */
export function fileStamps(paths: string[]) {
  return invoke<FileStamp[]>('file_stamps', { paths })
}

// ==================== 会话状态 ====================

/** 已解锁库数量。 */
export function unlockedCount() {
  return invoke<number>('unlocked_count')
}

/** 回收过期的会话（返回被自动锁定的库 id）。 */
export function reapExpired() {
  return invoke<string[]>('reap_expired')
}

// ==================== 搜索 ====================

/** 一条搜索结果（命中某个已解锁库 / 工作目录明文 / 打开的页签的文件名或内容）。 */
export interface SearchHit {
  vault_id: string
  vault_name: string
  rel_path: string
  snippet: string
  /** 是否为工作目录明文文件命中（true 时用 rel_path 相对工作目录根拼接打开） */
  plain: boolean
  /** 前端本地（打开的页签）命中：对应页签 path，点击直接激活页签；Rust 结果无此字段 */
  open_path?: string
  /** 是否为文件名/路径命中（而非正文内容命中） */
  name_hit?: boolean
}

/** 搜索已解锁库与工作目录（明文）的文件内容。 */
export function search(query: string, workdirs: string[] = []) {
  return invoke<SearchHit[]>('search', { query, workdirs })
}

// ==================== 文件对话框 ====================

/**
 * 打开文件对话框。
 * @param preferEncrypted 首个（默认展示的）过滤器是否为加密文件：
 *   解锁页传 `true`（优先 .mdl/.mdlb）；「打开文件」传 `false`（优先 md/txt 等文本）。
 *   两种顺序都同时提供加密 / 文本 / 所有三类过滤，可在对话框内切换。
 */
export async function openFileDialog(preferEncrypted = true) {
  const encrypted = { name: 'MarkLock 加密文件', extensions: ['mdl', 'mdlb'] }
  const plain = { name: 'Markdown / 文本', extensions: ['md', 'markdown', 'mdown', 'mkd', 'txt'] }
  const all = { name: '所有文件', extensions: ['*'] }
  const path = await open({
    multiple: false,
    filters: preferEncrypted ? [encrypted, plain, all] : [plain, encrypted, all],
  })
  return typeof path === 'string' ? path : null
}

/** 保存文件对话框（选择新建 .mdlb 单文件库的路径）。 */
export async function saveFileVaultDialog(defaultName = '未命名.mdlb') {
  const path = await save({
    defaultPath: defaultName,
    filters: [{ name: 'MarkLock 加密库', extensions: ['mdlb'] }],
  })
  return typeof path === 'string' ? path : null
}

/** 打开目录对话框（选择加密库目录）。 */
export async function openDirDialog() {
  const path = await open({
    directory: true,
    multiple: false,
  })
  return typeof path === 'string' ? path : null
}

/** 在系统文件管理器（访达）中定位一个文件。 */
export function revealInFinder(path: string) {
  return invoke<void>('reveal_in_finder', { path })
}

/** 在系统默认浏览器中打开一个 http/https/mailto 链接（预览区链接点击）。 */
export function openUrl(url: string) {
  return invoke<void>('open_url', { url })
}

/** 同步 macOS 原生菜单状态：视图/侧边栏分组勾选态 + 「文件」菜单里库/工作目录开关项的标题（打开↔关闭）；非 macOS 为无害空操作。 */
export function syncNativeMenu(checks: Record<string, boolean>, hasVaults: boolean, hasWorkdir: boolean) {
  return invoke<void>('sync_native_menu', { checks, hasVaults, hasWorkdir })
}

/** 取出并清空冷启动缓存的待打开文件路径（修复 macOS 双击打开丢事件）。 */
export function takePendingOpenPaths() {
  return invoke<string[]>('take_pending_open_paths')
}

// ==================== 系统文件类型关联 ====================

/** `.mdl` / `.mdlb` 的默认打开程序是否为本应用（无法探测时也为 false）。 */
export function isDefaultFileHandler() {
  return invoke<boolean>('is_default_file_handler')
}

/** 当前平台是否支持「引导设为默认打开程序」（macOS / Windows）。 */
export function supportsDefaultAppGuide() {
  return invoke<boolean>('supports_default_app_guide')
}

/** 尝试将 MarkLock 设为 .md/.mdl/.mdlb 默认打开程序；返回 'set'（成功）或 'manual'（需手动步骤）。 */
export function trySetDefaultFileHandler() {
  return invoke<'set' | 'manual'>('try_set_default_file_handler')
}

/** 保存文件对话框（选择新建 .mdl / 明文 .md 的路径）。 */
export async function saveFileDialog(defaultName = '未命名.mdl', kind: 'encrypted' | 'plain' = 'encrypted') {
  const path = await save({
    defaultPath: defaultName,
    filters: kind === 'plain'
      ? [{ name: 'Markdown', extensions: ['md', 'markdown', 'mdown', 'mkd', 'txt'] }]
      : [{ name: 'MarkLock 加密文件', extensions: ['mdl'] }],
  })
  return typeof path === 'string' ? path : null
}

/** 保存文件对话框（导出 HTML 的路径）。 */
export async function saveHtmlDialog(defaultName = '未命名.html') {
  const path = await save({
    defaultPath: defaultName,
    filters: [{ name: 'HTML', extensions: ['html', 'htm'] }],
  })
  return typeof path === 'string' ? path : null
}

/**
 * 多选明文文件对话框（用于「批量加密」）。
 * 默认先展示 Markdown / 文本过滤器，允许切到「所有文件」；取消返回空数组。
 */
export async function openMultipleTextFilesDialog(): Promise<string[]> {
  const r = await open({
    multiple: true,
    filters: [
      { name: 'Markdown / 文本', extensions: ['md', 'markdown', 'mdown', 'mkd', 'txt'] },
      { name: '所有文件', extensions: ['*'] },
    ],
  })
  if (Array.isArray(r)) return r.filter((x): x is string => typeof x === 'string')
  return []
}

/** 批量加密多个明文文件为独立 `.mdl`；`moveOriginals=true` 时把原文件移到同目录 `原文件/` 子目录。 */
export function batchEncrypt(paths: string[], password: string, moveOriginals: boolean) {
  return invoke<BatchEncryptResult>('batch_encrypt', { paths, password, moveOriginals })
}

/** 将源目录（含子目录结构）内的 markdown/文本文件加密导入到已解锁的目标库。 */
export function importDirToVault(vaultId: string, srcDir: string) {
  return invoke<ImportResult>('import_dir_to_vault', { vaultId, srcDir })
}

// ==================== 图片/文件资源（粘贴与拖入） ====================

/**
 * 读取系统剪贴板中的文件路径（粘贴图片/文件时的 macOS 原生兜底）。
 * WKWebView 从访达「拷贝文件」后 clipboardData.files 常为空，这里取回真实磁盘路径；
 * 非 macOS 平台恒返回空列表。
 */
export function clipboardFilePaths() {
  return invoke<string[]>('clipboard_file_paths')
}

/**
 * 把一个源文件/目录复制进 `.dat` 资源目录（重名自动加序号），返回落盘后的绝对路径。
 * @param destDir 目标 `.dat` 目录的绝对路径（不存在会自动创建）
 * @param src     被复制文件/目录的绝对路径
 */
export function copyAsset(destDir: string, src: string) {
  return invoke<string>('copy_asset', { destDir, src })
}

/**
 * 把剪贴板图片等二进制（base64 / data URL）写入 `.dat` 资源目录，返回落盘后的绝对路径。
 * @param destDir 目标 `.dat` 目录绝对路径
 * @param name    期望文件名（如 image.png）
 * @param base64  纯 base64 或完整 data URL
 */
export function writeAssetBytes(destDir: string, name: string, base64: string) {
  return invoke<string>('write_asset_bytes', { destDir, name, base64 })
}

/**
 * 读取本地文件为 data URL（`data:<mime>;base64,…`），用于导出 HTML 时内联 `.dat` 目录里的图片。
 */
export function readAssetDataUrl(path: string) {
  return invoke<string>('read_asset_data_url', { path })
}
