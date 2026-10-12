//! 预览字体清单：内置一份常见跨平台字体，每项都带跨 macOS/Windows 的回退栈，
//! 系统未安装首选字体时浏览器会自动回退到栈内下一个可用字体，故不会「选了却没有」。
//! 选中的字体 id 持久化到 settings.previewFontFamily（历史值 'default'/'sans'/'serif'/'mono' 仍在表内，向后兼容）。

import { lang } from '../locales'

export interface PreviewFont {
  /** 持久化用的稳定 id */
  id: string
  /** CSS font-family 值（含跨平台回退栈） */
  stack: string
  /** 简体中文显示名 */
  zh: string
  /** 英文显示名 */
  en: string
}

/** 顺序即下拉展示顺序：先通用族，再中文常用，最后西文常用。 */
export const PREVIEW_FONTS: PreviewFont[] = [
  { id: 'default', zh: '跟随界面', en: 'Follow UI', stack: '-apple-system, BlinkMacSystemFont, "PingFang SC", "Segoe UI", "Helvetica Neue", "Microsoft YaHei", Arial, sans-serif' },
  { id: 'sans', zh: '无衬线（系统）', en: 'Sans-serif (system)', stack: '-apple-system, BlinkMacSystemFont, "PingFang SC", "Segoe UI", "Helvetica Neue", "Microsoft YaHei", Arial, sans-serif' },
  { id: 'serif', zh: '衬线（系统）', en: 'Serif (system)', stack: 'Georgia, "Times New Roman", "Songti SC", "SimSun", serif' },
  { id: 'mono', zh: '等宽（系统）', en: 'Monospace (system)', stack: '"SF Mono", "JetBrains Mono", Menlo, Consolas, "Courier New", monospace' },
  // 中文常用
  { id: 'pingfang', zh: '苹方 PingFang SC', en: 'PingFang SC', stack: '"PingFang SC", "Microsoft YaHei", sans-serif' },
  { id: 'hiragino', zh: '冬青黑体 Hiragino Sans GB', en: 'Hiragino Sans GB', stack: '"Hiragino Sans GB", "PingFang SC", "Microsoft YaHei", sans-serif' },
  { id: 'yahei', zh: '微软雅黑 Microsoft YaHei', en: 'Microsoft YaHei', stack: '"Microsoft YaHei", "PingFang SC", sans-serif' },
  { id: 'songti', zh: '宋体 Songti SC', en: 'Songti SC', stack: '"Songti SC", "SimSun", serif' },
  { id: 'kaiti', zh: '楷体 KaiTi', en: 'KaiTi', stack: '"Kaiti SC", "STKaiti", "KaiTi", serif' },
  { id: 'fangsong', zh: '仿宋 FangSong', en: 'FangSong', stack: '"STFangsong", "FangSong", serif' },
  // 西文常用
  { id: 'georgia', zh: 'Georgia', en: 'Georgia', stack: 'Georgia, "Times New Roman", serif' },
  { id: 'times', zh: 'Times New Roman', en: 'Times New Roman', stack: '"Times New Roman", Georgia, serif' },
  { id: 'helvetica', zh: 'Helvetica Neue', en: 'Helvetica Neue', stack: '"Helvetica Neue", Arial, sans-serif' },
  { id: 'arial', zh: 'Arial', en: 'Arial', stack: 'Arial, "Helvetica Neue", "PingFang SC", sans-serif' },
  { id: 'code', zh: '等宽 Menlo/Consolas', en: 'Menlo / Consolas', stack: 'Menlo, Consolas, Monaco, "Courier New", monospace' },
]

const BY_ID = new Map(PREVIEW_FONTS.map((f) => [f.id, f]))

/** id → CSS font-family 栈；未知/历史 id 回退到表首（跟随界面，等价无衬线系统栈）。 */
export function resolveFontStack(id: string): string {
  return BY_ID.get(id)?.stack ?? PREVIEW_FONTS[0].stack
}

/** 当前语言下的显示名（读 lang.value，故在 computed 中调用可随语言切换重算）。 */
export function fontLabel(f: PreviewFont): string {
  return lang.value === 'en' ? f.en : f.zh
}
