//! 平台探测：macOS 判定收敛到此处，供 main.ts 注入 <html data-platform> 与组件文案/行为分支共用。
//! 注意：仅用于 UI 语义差异（如 macOS 无「系统托盘/通知区域」概念，图标在菜单栏），
//! 加解密、文件路径等能力差异仍由 Rust 侧 #[cfg(target_os = ...)] 决定。

const ua = navigator.userAgent.toLowerCase()

/** 是否运行在 macOS（Tauri 的 WKWebView UA 含 "Mac OS"；navigator.platform 作兜底） */
export const isMac = ua.includes('mac os') || (navigator.platform || '').toLowerCase().includes('mac')
