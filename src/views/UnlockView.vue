<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import TitleBar from '../components/TitleBar.vue'
import { useVaultStore } from '../stores/vault'
import * as tauri from '../lib/tauri'

export default defineComponent({
  name: 'UnlockView',
  components: { TitleBar },
  data() {
    return {
      targetId: '',
      password: '',
      loading: false,
      error: '',
    }
  },
  computed: {
    store() {
      return useVaultStore()
    },
    vaults() {
      // 目录库与单文件库（.mdlb）；单文件 .mdl 不是库，不走解锁页
      return this.store.recent.filter((v) => v.isDir || v.isFileVault)
    },
    targetVault() {
      return this.vaults.find((v) => v.id === this.targetId) || null
    },
  },
  mounted() {
    // 目标库优先级：路由 query > pendingOpen > 唯一库自动选中
    const qid = (this.$route.query.id as string) || ''
    const pid = this.store.pendingOpen?.vaultId || ''
    const auto = qid || pid || (this.vaults.length === 1 ? this.vaults[0].id : '')
    if (auto && this.vaults.some((v) => v.id === auto)) this.targetId = auto
    // 进入解锁页自动聚焦密码框，可直接输入主密码
    this.$nextTick(() => {
      const el = this.$refs.pwdInput as HTMLInputElement | undefined
      el?.focus()
    })
    // Esc 取消（与「取消」按钮一致）
    window.addEventListener('keydown', this.onKeydown)
  },
  unmounted() {
    window.removeEventListener('keydown', this.onKeydown)
  },
  methods: {
    async doUnlock() {
      const vault = this.targetVault
      if (!vault) {
        message.warning('请先选择一个加密库')
        return
      }
      if (!this.password) {
        message.warning('请输入主密码')
        return
      }
      this.loading = true
      this.error = ''
      try {
        await this.store.unlockVault(vault.id, this.password)
        message.success('解锁成功')
        this.$router.push('/editor')
      } catch (e) {
        this.error = String(e)
        // 解锁失败后密码框重新可用，把焦点还给输入框方便重试
        this.$nextTick(() => {
          const el = this.$refs.pwdInput as HTMLInputElement | undefined
          el?.focus()
        })
      } finally {
        this.loading = false
      }
    },
    /** 取消：回到编辑器（丢弃未完成的解锁意图）。 */
    cancel() {
      this.store.pendingOpen = null
      this.$router.push('/editor')
    },
    /** 键盘 Esc：等同点击「取消」；解锁进行中忽略，避免中途切页。 */
    onKeydown(e: KeyboardEvent) {
      if (this.loading) return
      if (e.key === 'Escape') this.cancel()
    },
    async openVaultOrFile() {
      try {
        const path = await tauri.openFileDialog()
        if (!path) return
        const name = path.split(/[\\/]/).pop() || path
        if (path.endsWith('.mdlb')) {
          // 单文件库：登记后作为解锁目标
          this.store.registerVault({
            id: path,
            name: name.replace(/\.mdlb$/, ''),
            path,
            isDir: false,
            isFileVault: true,
          })
          this.targetId = path
          this.error = ''
        } else {
          // 单文件 .mdl 不是库：直接开加密文件页签，内容区显示密码框
          this.store.openEncryptedFile(path, name)
          this.$router.push('/editor')
        }
      } catch (e) {
        message.error(String(e))
      }
    },
  },
})
</script>

<template>
  <div class="screen">
    <TitleBar transparent />

    <div class="unlock-wrap">
      <div class="unlock-card">
        <!-- 品牌区 -->
        <div class="brand">
          <div class="brand-logo">
            <svg width="48" height="48" viewBox="0 0 96 96" fill="none" stroke="#fff" stroke-width="5" stroke-linecap="round" stroke-linejoin="round">
              <path d="M48 18 C41 18 27 23 22 28 L22 46 C22 63 35 72 48 79 C61 72 74 63 74 46 L74 28 C69 23 55 18 48 18 Z" />
              <path d="M37 57 V38 L48 50 L59 38 V57" />
            </svg>
          </div>
          <h1>MarkLock</h1>
          <p>可加密的 Markdown 查看编辑器<br />你的笔记，只有你能打开。</p>
          <div class="feat">
            <span><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="2"><path d="M20 6L9 17l-5-5" /></svg>端到端加密 · 防篡改校验</span>
            <span><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="2"><path d="M20 6L9 17l-5-5" /></svg>独立文件密钥 · 抗暴力破解</span>
            <span><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="2"><path d="M20 6L9 17l-5-5" /></svg>本地加密存储，离线可用，零上传</span>
            <span><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="2"><path d="M20 6L9 17l-5-5" /></svg>自动锁定 &amp; 剪贴板定时清除</span>
          </div>
          <div class="ver">v1.0.0 · macOS / Windows</div>
        </div>

        <!-- 解锁区：目标库 + 一个密码框 -->
        <div class="panel">
          <h2>解锁加密库</h2>

          <template v-if="vaults.length">
            <!-- 目标库 -->
            <div class="target" v-if="targetVault">
              <div class="target-ico" :style="{ borderColor: targetVault.color }">
                <svg v-if="!targetVault.isDir" width="20" height="20" viewBox="0 0 24 24" fill="none" :stroke="targetVault.color" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" /><path d="M14 3v6h6" />
                </svg>
                <svg v-else width="20" height="20" viewBox="0 0 24 24" fill="none" :stroke="targetVault.color" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M4 7V6a2 2 0 0 1 2-2h2l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" />
                </svg>
              </div>
              <div class="target-meta">
                <div class="target-name">{{ targetVault.name }}</div>
                <div class="target-path">{{ targetVault.path }}</div>
              </div>
            </div>
            <!-- 未指定目标：紧凑下拉选择 -->
            <div class="pwd-row" v-else>
              <select class="input input-lg" v-model="targetId" :disabled="loading">
                <option value="" disabled>选择加密库</option>
                <option v-for="v in vaults" :key="v.id" :value="v.id">{{ v.name }}</option>
              </select>
            </div>

            <div class="pwd-row">
              <input ref="pwdInput" class="input input-lg" type="password" placeholder="输入主密码" v-model="password" :disabled="loading" @keyup.enter="doUnlock" />
              <button class="btn btn-primary btn-lg" style="flex:0 0 92px;" :disabled="loading" @click="doUnlock">
                {{ loading ? '解锁中…' : '解锁' }}
              </button>
              <button class="btn btn-lg" style="flex:0 0 92px;" :disabled="loading" @click="cancel">取消</button>
            </div>
            <div class="error" v-if="error">{{ error }}</div>

            <div class="alt" v-if="!targetVault">
              <span class="link gray" @click="openVaultOrFile">打开加密库 / 文件…</span>
            </div>
          </template>

          <div class="empty" v-else>
            <p>还没有加密库</p>
            <p class="empty-sub">
              <span class="link" @click="openVaultOrFile">打开加密库 / 文件…</span>
              ，或在编辑器右上菜单「＋ 新建加密库」
            </p>
          </div>

          <div class="tip">
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#8a919e" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="9" /><path d="M12 8v4M12 16h.01" /></svg>
            密码仅用于本地解密，不会上传到任何服务器
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
body { background: var(--bg); }
.unlock-wrap {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  background:
    radial-gradient(1000px 500px at 15% 0%, rgba(22, 119, 255, 0.10), transparent 60%),
    radial-gradient(800px 420px at 90% 100%, rgba(114, 46, 209, 0.08), transparent 60%),
    var(--bg);
}
.unlock-card {
  width: 860px;
  height: 540px;
  display: flex;
  background: var(--panel);
  border: 1px solid var(--border-2);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-modal);
  overflow: hidden;
}

/* 左侧品牌区 */
.brand {
  width: 320px;
  flex: 0 0 320px;
  padding: 36px 30px;
  display: flex;
  flex-direction: column;
  color: #fff;
  background: linear-gradient(160deg, #1d39c4 0%, #1677ff 58%, #36cfc9 130%);
  position: relative;
  overflow: hidden;
}
.brand::before, .brand::after {
  content: "";
  position: absolute;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.08);
}
.brand::before { width: 260px; height: 260px; right: -90px; top: -70px; }
.brand::after { width: 180px; height: 180px; left: -60px; bottom: -50px; }
.brand-logo {
  width: 56px; height: 56px;
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.16);
  border: 1px solid rgba(255, 255, 255, 0.35);
  display: flex; align-items: center; justify-content: center;
  backdrop-filter: blur(4px);
  position: relative; z-index: 1;
}
.brand h1 { font-size: 26px; margin-top: 18px; letter-spacing: 0.5px; position: relative; z-index: 1; }
.brand p { font-size: 13px; opacity: 0.85; margin-top: 8px; line-height: 1.8; position: relative; z-index: 1; }
.brand .feat { margin-top: 26px; display: flex; flex-direction: column; gap: 12px; position: relative; z-index: 1; }
.brand .feat span { display: flex; align-items: center; gap: 8px; font-size: 12.5px; opacity: 0.92; }
.brand .ver { margin-top: auto; font-size: 12px; opacity: 0.6; position: relative; z-index: 1; }

/* 右侧解锁区：垂直居中的极简表单 */
.panel {
  flex: 1;
  display: flex;
  flex-direction: column;
  justify-content: center;
  padding: 28px 40px;
}
.panel h2 { font-size: 18px; font-weight: 600; }

.target {
  margin-top: 22px;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--bg);
}
.target-ico {
  width: 38px; height: 38px; flex: 0 0 38px;
  border-radius: 9px;
  display: flex; align-items: center; justify-content: center;
  background: var(--panel);
  border: 1px solid var(--border);
}
.target-meta { flex: 1; min-width: 0; }
.target-name { font-weight: 500; }
.target-path {
  font-size: 12px; color: var(--text-3);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  font-family: var(--mono);
}

.pwd-row { display: flex; gap: 8px; margin-top: 16px; }
.pwd-row .input { flex: 1; }
.error { margin-top: 8px; font-size: 12.5px; color: #cf1322; }

.alt { margin-top: 14px; font-size: 13px; }
.link { color: var(--primary); cursor: pointer; }
.link:hover { color: var(--primary-hover); }
.link.gray { color: var(--text-3); }
.link.gray:hover { color: var(--text-2); }

.empty {
  margin-top: 22px; padding: 24px; text-align: center;
  border: 1px dashed var(--border); border-radius: var(--radius);
  color: var(--text-3); font-size: 14px;
}
.empty-sub { font-size: 12px; color: var(--text-4); margin-top: 6px; }

.tip { margin-top: 26px; font-size: 12px; color: var(--text-3); display: flex; align-items: center; gap: 6px; }

/* 按钮与输入框（沿用原型基类） */
.btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 32px;
  padding: 0 15px;
  font-size: 14px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text);
  cursor: pointer;
  transition: all 0.2s;
  white-space: nowrap;
}
.btn:hover { color: var(--primary-hover); border-color: var(--primary-hover); }
.btn-primary {
  background: var(--primary);
  border-color: var(--primary);
  color: #fff;
  box-shadow: 0 2px 0 rgba(5, 145, 255, 0.1);
}
.btn-primary:hover { background: var(--primary-hover); border-color: var(--primary-hover); color: #fff; }
.btn-lg { height: 38px; padding: 0 22px; font-size: 15px; border-radius: var(--radius); }

.input {
  width: 100%;
  height: 32px;
  padding: 4px 11px;
  font-size: 14px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel);
  color: var(--text);
  outline: none;
  transition: all 0.2s;
}
.input::placeholder { color: var(--text-4); }
.input:hover { border-color: var(--primary-hover); }
.input:focus { border-color: var(--primary); box-shadow: 0 0 0 2px rgba(5, 145, 255, 0.1); }
.input-lg { height: 38px; font-size: 15px; border-radius: var(--radius); }
</style>
