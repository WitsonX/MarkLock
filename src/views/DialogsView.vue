<script lang="ts">
import { defineComponent } from 'vue'
import { message } from 'ant-design-vue'
import TitleBar from '../components/TitleBar.vue'
import { useVaultStore } from '../stores/vault'
import * as tauri from '../lib/tauri'

export default defineComponent({
  name: 'DialogsView',
  components: { TitleBar },
  data() {
    return {
      // 三个弹窗的可见性（默认全展开，便于评审）
      visible: { create: true, unlock: true, changePwd: true },
      // 新建库表单
      createForm: { name: '', path: '', pwd: '', confirm: '' },
      creating: false,
      // 修改主密码
      changeForm: { current: '', next: '', confirm: '' },
      changing: false,
      unlockPwd: '',
    }
  },
  computed: {
    store() {
      return useVaultStore()
    },
    // 新建库密码强度（4 格）
    createStrength(): number {
      const p = this.createForm.pwd
      if (p.length >= 12 && /[A-Z]/.test(p) && /[0-9]/.test(p) && /[^A-Za-z0-9]/.test(p)) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
    changeStrength(): number {
      const p = this.changeForm.next
      if (p.length >= 12) return 4
      if (p.length >= 8) return 3
      if (p.length >= 4) return 2
      return p.length ? 1 : 0
    },
  },
  methods: {
    async browsePath() {
      // 单文件库：选择保存为 .mdlb 文件的路径
      const path = await tauri.saveFileVaultDialog()
      if (path) this.createForm.path = path
    },
    async createVault() {
      const { name, path, pwd, confirm } = this.createForm
      if (!path) return message.warning('请选择保存位置')
      if (pwd.length < 1) return message.warning('请设置密码（至少 1 位）')
      if (pwd !== confirm) return message.warning('两次密码不一致')
      this.creating = true
      try {
        const filePath = path.endsWith('.mdlb') ? path : `${path}.mdlb`
        await tauri.createFileVault(filePath, pwd, undefined)
        const vname = name.trim() || (filePath.split(/[\\/]/).pop() || '').replace(/\.mdlb$/, '') || '未命名'
        this.store.registerVault({ id: filePath, name: vname, path: filePath, isDir: false, isFileVault: true })
        await this.store.unlockVault(filePath, pwd)
        message.success('单文件库已创建并解锁')
        this.$router.push('/editor')
      } catch (e) {
        message.error(String(e))
      } finally {
        this.creating = false
      }
    },
    async changePassword() {
      const { current, next, confirm } = this.changeForm
      const vault = this.store.unlockedVaults[0]
      if (!vault) return message.warning('请先解锁一个库')
      if (next.length < 1) return message.warning('请设置密码（至少 1 位）')
      if (next !== confirm) return message.warning('两次密码不一致')
      this.changing = true
      try {
        await tauri.changePassword(vault.id, vault.path, current, next)
        message.success('主密码已修改')
        this.changeForm = { current: '', next: '', confirm: '' }
      } catch (e) {
        message.error(String(e))
      } finally {
        this.changing = false
      }
    },
  },
})
</script>

<template>
  <div class="screen">
    <TitleBar transparent subtitle="弹窗集合 · 新建 / 解锁 / 修改主密码" />

    <div class="stage">
      <!-- ============ 弹窗 1：新建加密库 ============ -->
      <div class="m-col">
        <a-modal
          v-model:open="visible.create"
          :footer="null"
          width="460"
          :closable="true"
          title="新建加密库"
        >
          <div class="form-item">
            <label class="form-label"><b>保存位置</b></label>
            <div class="path-row">
              <input class="input" v-model="createForm.path" placeholder="选择 .mdlb 文件的保存路径" readonly />
              <button class="btn" @click="browsePath">浏览…</button>
            </div>
            <div class="form-hint">.mdlb 是单个加密文件：文件名、目录结构、正文全部密文，可直接放进 iCloud / Dropbox 同步</div>
          </div>
          <div class="form-item">
            <label class="form-label"><b>主密码</b></label>
            <input class="input" type="password" v-model="createForm.pwd" />
            <div class="strength">
              <i v-for="n in 4" :key="n" :class="'lv' + (createStrength >= n ? createStrength : '')"></i>
              <em v-if="createStrength >= 4">强 · 建议使用密码管理器生成</em>
              <em v-else-if="createStrength >= 3">中 · 建议混合符号</em>
              <em v-else>弱 · 建议 12 位以上并混合符号</em>
            </div>
          </div>
          <div class="form-item">
            <label class="form-label"><b>确认主密码</b></label>
            <input class="input" type="password" v-model="createForm.confirm" />
          </div>
          <div class="modal-foot">
            <button class="btn" @click="visible.create = false">取消</button>
            <button class="btn btn-primary" :disabled="creating" @click="createVault">{{ creating ? '创建中…' : '创建并解锁' }}</button>
          </div>
        </a-modal>
        <div class="modal-cap">① 新建加密库（含密码强度提示）</div>
      </div>

      <!-- ============ 弹窗 2：解锁（错误态） ============ -->
      <div class="m-col">
        <a-modal
          v-model:open="visible.unlock"
          :footer="null"
          width="400"
          title="解锁加密库"
        >
          <div class="unlock-hero">
            <div class="ico">
              <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="#1677ff" stroke-width="1.8" stroke-linecap="round"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
            </div>
            <div class="vn">工作笔记</div>
            <div class="vp">~/Documents/Vaults/work.mlkv</div>
          </div>
          <div class="form-item">
            <input class="input input-lg input-error" type="password" v-model="unlockPwd" />
            <div class="form-hint err">密码错误，请重试。连续失败 5 次后将锁定 10 分钟（还剩 4 次）</div>
          </div>
          <div class="form-item" style="display:flex;justify-content:space-between;font-size:13px;">
            <span class="link gray">使用密码提示</span>
            <span class="link gray">忘记密码？</span>
          </div>
          <div class="modal-foot stretch">
            <button class="btn btn-primary btn-lg btn-block">解锁</button>
          </div>
        </a-modal>
        <div class="modal-cap">② 解锁（密码错误态 + 失败次数限制）</div>
      </div>

      <!-- ============ 弹窗 3：修改主密码 ============ -->
      <div class="m-col">
        <a-modal
          v-model:open="visible.changePwd"
          :footer="null"
          width="440"
          title="修改主密码"
        >
          <div class="warn-bar">
            <svg width="15" height="15" style="flex:0 0 auto;margin-top:2px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 9v4M12 17h.01M10.3 3.9L1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" /></svg>
            <span>修改后，128 个文件只需以新密码重新加密密钥（约 3 秒），<b>无需全文重加密</b>。旧密码即刻失效且不可找回。</span>
          </div>
          <div class="form-item">
            <label class="form-label"><b>当前主密码</b></label>
            <input class="input" type="password" v-model="changeForm.current" />
          </div>
          <div class="form-item">
            <label class="form-label"><b>新主密码</b></label>
            <input class="input" type="password" v-model="changeForm.next" />
            <div class="strength">
              <i v-for="n in 4" :key="n" :class="'lv' + (changeStrength >= n ? changeStrength : '')"></i>
              <em v-if="changeStrength >= 3">中等 · 建议 12 位以上并混合符号</em>
              <em v-else>偏弱 · 建议 12 位以上并混合符号</em>
            </div>
          </div>
          <div class="form-item">
            <label class="form-label"><b>确认新主密码</b></label>
            <input class="input" type="password" v-model="changeForm.confirm" />
          </div>
          <div class="modal-foot">
            <button class="btn" @click="visible.changePwd = false">取消</button>
            <button class="btn btn-primary" :disabled="changing" @click="changePassword">{{ changing ? '修改中…' : '确认修改' }}</button>
          </div>
        </a-modal>
        <div class="modal-cap">③ 修改主密码（仅重新加密密钥）</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
body { background: var(--bg); }
.stage {
  flex: 1;
  display: flex; flex-wrap: wrap;
  align-items: flex-start; justify-content: center;
  gap: 28px;
  padding: 34px 26px;
  background:
    radial-gradient(circle at 1px 1px, var(--border) 1px, transparent 0) 0 0 / 22px 22px,
    var(--bg);
  overflow-y: auto;
}
.m-col { display: flex; flex-direction: column; align-items: center; }
.modal-cap { text-align: center; font-size: 12px; color: var(--text-3); margin-top: 10px; }

.path-row { display: flex; gap: 8px; }
.path-row .input { flex: 1; font-family: var(--mono); font-size: 13px; }

.unlock-hero { text-align: center; margin-bottom: 18px; }
.unlock-hero .ico {
  width: 52px; height: 52px; margin: 0 auto 10px;
  border-radius: 13px;
  background: var(--primary-bg);
  border: 1px solid var(--border);
  display: flex; align-items: center; justify-content: center;
}
.unlock-hero .vn { font-weight: 600; font-size: 15px; }
.unlock-hero .vp { font-size: 12px; color: var(--text-3); font-family: var(--mono); }

.warn-bar {
  display: flex; gap: 8px; align-items: flex-start;
  background: var(--gold-bg);
  border: 1px solid #ffe58f;
  border-radius: var(--radius-sm);
  padding: 9px 12px;
  font-size: 12.5px; color: #ad6800;
  margin-bottom: 16px; line-height: 1.6;
}

/* 表单 */
.form-item { margin-bottom: 16px; }
.form-label { display: block; font-size: 13px; color: var(--text-2); margin-bottom: 6px; }
.form-label b { color: var(--text); font-weight: 500; }
.form-hint { font-size: 12px; color: var(--text-3); margin-top: 6px; }
.form-hint.err { color: var(--red); }

.link { color: var(--primary); cursor: pointer; }
.link.gray { color: var(--text-3); }
.link.gray:hover { color: var(--text-2); }

/* 密码强度条 */
.strength { display: flex; gap: 4px; margin-top: 8px; align-items: center; }
.strength i { height: 4px; flex: 1; border-radius: 2px; background: var(--border-2); }
.strength i.lv1 { background: var(--red); }
.strength i.lv2 { background: var(--gold); }
.strength i.lv3 { background: #a0d911; }
.strength i.lv4 { background: var(--green); }
.strength em { font-style: normal; font-size: 12px; color: var(--text-3); margin-left: 4px; }

/* 按钮 */
.btn {
  display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  height: 32px; padding: 0 15px; font-size: 14px; border-radius: var(--radius-sm);
  border: 1px solid var(--border); background: var(--panel); color: var(--text);
  cursor: pointer; transition: all 0.2s; white-space: nowrap;
}
.btn:hover { color: var(--primary-hover); border-color: var(--primary-hover); }
.btn-primary { background: var(--primary); border-color: var(--primary); color: #fff; box-shadow: 0 2px 0 rgba(5, 145, 255, 0.1); }
.btn-primary:hover { background: var(--primary-hover); border-color: var(--primary-hover); color: #fff; }
.btn-lg { height: 38px; padding: 0 22px; font-size: 15px; border-radius: var(--radius); }
.btn-block { width: 100%; }

.modal-foot { display: flex; justify-content: flex-end; gap: 8px; padding-top: 4px; }
.modal-foot.stretch { justify-content: stretch; }
.modal-foot.stretch .btn { flex: 1; }

/* 输入框 */
.input {
  width: 100%; height: 32px; padding: 4px 11px; font-size: 14px;
  border: 1px solid var(--border); border-radius: var(--radius-sm);
  background: var(--panel); color: var(--text); outline: none; transition: all 0.2s;
}
.input::placeholder { color: var(--text-4); }
.input:hover { border-color: var(--primary-hover); }
.input:focus { border-color: var(--primary); box-shadow: 0 0 0 2px rgba(5, 145, 255, 0.1); }
.input-lg { height: 38px; font-size: 15px; border-radius: var(--radius); }
.input-error { border-color: var(--red); }
.input-error:focus { box-shadow: 0 0 0 2px rgba(255, 77, 79, 0.1); }
</style>
