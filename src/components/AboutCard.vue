<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import { getVersion } from '@tauri-apps/api/app'
import { lang } from '../locales'
import * as tauri from '../lib/tauri'
import { LINKS } from '../lib/links'
import { runUpdateCheck } from '../lib/update'
// 关于卡片品牌 logo：与 Dock 图标同一光栅化流程生成的白底版（主图标源文件保持 #EAF2FE 不动）
import aboutIcon from '../assets/about-icon.png'

/**
 * 「关于」卡片：设置页「关于」页签与 App.vue 弹出的独立「关于」对话框共用同一组件，
 * 保证版本号读取、检查更新、外链跳转的三处入口完全同源，未来改版只需改一处。
 */
export default defineComponent({
  name: 'AboutCard',
  data() {
    return {
      aboutIcon,
      // 版本号：运行时从 Tauri 读取（源自 tauri.conf.json），避免在文案里写死
      appVersion: '',
      // 手动「立即检查更新」进行中标志（禁用按钮、防重复点击）
      checking: false,
    }
  },
  computed: {
    /** 关于页展示与跳转的项目链接 */
    links() {
      return LINKS
    },
  },
  mounted() {
    // 读取真实应用版本用于「关于」展示；非 Tauri 环境（纯 web dev）下静默降级
    getVersion().then((v) => (this.appVersion = v)).catch(() => {})
  },
  methods: {
    /** 外部链接：用系统浏览器打开（链接集中定义于 src/lib/links.ts）。 */
    openLink(url: string) {
      tauri.openUrl(url).catch((err) => message.error(String(err)))
    },
    /** 立即手动检查一次更新（不受频率门控）；结果展示（弹窗/轻提示）由 runUpdateCheck 统一处理。 */
    async runManualCheck() {
      if (this.checking) return
      this.checking = true
      try {
        await runUpdateCheck(lang.value === 'zh')
      } catch (e) {
        message.error(String(e))
      } finally {
        this.checking = false
      }
    },
  },
})
</script>

<template>
  <div class="about-card">
    <div class="about-logo">
      <img :src="aboutIcon" alt="MarkLock" />
    </div>
    <div class="about-name">MarkLock</div>
    <div class="about-meta">
      <span class="about-ver">{{ $t('settings.about.version', { version: appVersion }) }}</span>
      <span class="about-sep"></span>
      <span class="about-tech">Tauri 2 · Vue 3</span>
    </div>
    <p class="about-desc">
      {{ $t('settings.about.desc') }}
    </p>
    <div class="about-feats">
      <div class="about-feat">
        <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2" /><path d="M8 11V7a4 4 0 0 1 8 0v4" /></svg>
        <span class="ft">{{ $t('settings.about.featE2e') }}</span>
        <span class="fd">{{ $t('settings.about.featE2eD') }}</span>
      </div>
      <div class="about-feat">
        <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="13" width="18" height="8" rx="2" /><path d="M7 17h.01" /><path d="M5.5 13 7 5h10l1.5 8" /></svg>
        <span class="ft">{{ $t('settings.about.featOffline') }}</span>
        <span class="fd">{{ $t('settings.about.featOfflineD') }}</span>
      </div>
      <div class="about-feat">
        <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z" /><path d="M9 12l2 2 4-4" /></svg>
        <span class="ft">{{ $t('settings.about.featTamper') }}</span>
        <span class="fd">{{ $t('settings.about.featTamperD') }}</span>
      </div>
    </div>
    <div class="about-actions">
      <div class="about-link-group">
        <span class="about-link-label">{{ $t('settings.about.checkUpdate') }}</span>
        <button class="about-link" :disabled="checking" @click="runManualCheck">{{ checking ? $t('settings.update.checking') : $t('settings.about.checkNow') }}</button>
      </div>
      <span class="about-sep"></span>
      <div class="about-link-group">
        <span class="about-link-label">{{ $t('settings.about.projectHome') }}</span>
        <button class="about-link" @click="openLink(links.github)">GitHub</button>
        <span class="about-dot">·</span>
        <button class="about-link" @click="openLink(links.gitee)">{{ $t('settings.about.giteeMirror') }}</button>
      </div>
      <span class="about-sep"></span>
      <div class="about-link-group">
        <span class="about-link-label">{{ $t('settings.about.feedback') }}</span>
        <button class="about-link" @click="openLink(links.feedback)">GitHub</button>
        <span class="about-dot">·</span>
        <button class="about-link" @click="openLink(links.giteeFeedback)">{{ $t('settings.about.giteeMirror') }}</button>
      </div>
    </div>
    <div class="about-links-note">{{ $t('settings.about.linksNote') }}</div>
  </div>
</template>

<style scoped>
.about-card { text-align: center; padding: 40px 24px 24px; max-width: 620px; margin: 0 auto; }
.about-logo {
  width: 78px; height: 78px; margin: 0 auto 18px;
}
.about-logo img {
  width: 100%; height: 100%; display: block;
  /* 中性灰黑软阴影：贴合图标轮廓又不染蓝 */
  filter: drop-shadow(0 4px 10px rgba(0, 0, 0, 0.14)) drop-shadow(0 1px 2px rgba(0, 0, 0, 0.10));
}
.about-name { font-size: 24px; font-weight: 700; letter-spacing: 0.3px; color: var(--text); }
.about-meta { display: flex; align-items: center; justify-content: center; gap: 10px; margin-top: 10px; }
.about-ver {
  font-size: 12px; font-weight: 600; color: var(--primary);
  background: var(--primary-bg); padding: 3px 11px; border-radius: 999px;
}
.about-sep { width: 1px; height: 12px; background: var(--border); }
.about-tech { font-size: 12px; color: var(--text-3); }
.about-desc {
  font-size: 13px; line-height: 1.85; color: var(--text-2);
  max-width: 460px; margin: 18px auto 22px;
}
.about-feats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; margin-bottom: 24px; }
.about-feat {
  display: flex; flex-direction: column; align-items: center; gap: 5px;
  padding: 14px 8px; border-radius: var(--radius-lg);
  border: 1px solid var(--border); background: var(--panel);
}
.about-feat svg { color: var(--primary); }
.about-feat .ft { font-size: 12.5px; font-weight: 600; color: var(--text); }
.about-feat .fd { font-size: 11px; color: var(--text-3); }
.about-actions { display: flex; flex-wrap: wrap; align-items: center; justify-content: center; gap: 10px 14px; }
.about-link-group { display: inline-flex; align-items: center; gap: 5px; }
.about-link-label { font-size: 12px; color: var(--text-3); }
.about-link {
  font-size: 12px; padding: 0; border: none; background: transparent;
  color: var(--primary); cursor: pointer; text-decoration: none;
  transition: color 0.15s;
}
.about-link:not(:disabled):hover { color: var(--primary-hover); text-decoration: underline; }
.about-link:disabled { opacity: 0.55; cursor: not-allowed; }
.about-dot { font-size: 12px; color: var(--text-4); user-select: none; }
.about-links-note { margin-top: 12px; text-align: center; font-size: 11.5px; color: var(--text-4); }
</style>
