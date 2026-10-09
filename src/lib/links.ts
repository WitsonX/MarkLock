// MarkLock 项目对外链接集中管理：原生菜单「帮助」、主菜单、设置「关于」页共用，避免各处硬编码。
export const LINKS = {
  /** GitHub 仓库主页 */
  github: 'https://github.com/WitsonX/MarkLock',
  /** Gitee 仓库主页（国内镜像） */
  gitee: 'https://gitee.com/Witson/MarkLock',
  /** 发布页（检查更新跳转此处，取最新 Release） */
  release: 'https://github.com/WitsonX/MarkLock/releases/latest',
  /** 发布页 Gitee 国内镜像 */
  giteeRelease: 'https://gitee.com/Witson/MarkLock/releases',
  /** 反馈 / 提 Issue 页 */
  feedback: 'https://github.com/WitsonX/MarkLock/issues',
  /** 反馈页 Gitee 国内镜像 */
  giteeFeedback: 'https://gitee.com/Witson/MarkLock/issues',
} as const
