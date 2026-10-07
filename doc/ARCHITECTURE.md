# MarkLock — 架构与演进文档

> 本文档记录 MarkLock 从项目启动到现在的完整架构设计与功能演进史，是团队/接手者的第一手参考。
> 快速上手、环境要求、运行命令见 [README](../README.md)。

---

## 一、项目定位

**MarkLock** 是一个「可加密的 Markdown 查看/编辑器」，桌面应用。

- 壳：Tauri 2（Rust），前端 Vue 3 + Ant Design Vue，加密核心全部在 Rust 侧。
- 所有数据只存本地，文件落盘即密文，密钥永不出设备、不进 JS、不上传。
- 两种加密对象：
  - **加密库 `.mdlb`**（MarkLock Bundle）——单文件容器，内部是一棵加密文件树（文件名/目录结构/正文全密文）。
  - **加密文件 `.mdl`**（MarkDown Lock）——单个加密 Markdown 文件。

---

## 二、总体架构

```
┌──────────────────────────────────────────────────────────┐
│                      前端（Vue 3）                         │
│  views/ EditorView·UnlockView·SettingsView·DialogsView    │
│  components/ TitleBar·VaultTreeNode                        │
│  stores/vault.ts (Pinia)   lib/tauri.ts (invoke 桥)        │
│  lib/tooltip.ts (v-tip 指令)                               │
│              │  invoke('xxx', {...})  ← 只传路径/密码/明文 │
│              ▼                                              │
│              │        （密钥材料永不过 JS）                 │
├──────────────────────────────────────────────────────────┤
│                      壳层（Rust / Tauri 2）                 │
│  lib.rs —— 窗口管理 + ~30 个 Tauri 命令注册                │
│  crypto/ —— 加密核心（见下）                               │
│              │                                              │
│              ▼                                              │
│          文件系统（.mdlb 单文件 / .mdl 单文件 / 明文）       │
└──────────────────────────────────────────────────────────┘
```

### 数据流原则

1. **密钥不进前端**：密码只用于传给 Rust 派生 KEK；解密后的明文由 Rust 返回给前端显示，密钥本身永不跨 JS 边界。
2. **会话只在内存**：解锁状态存于 Rust 的 `SessionStore`（进程级单例），重启即失效；前端 store 只持久化「路径/名称/锁定态」等元信息，不持久化 unlocked 标记（读回时强制清零）。
3. **锁定 = 清密钥**：锁定单个库或全部库时，Rust 清零对应内存密钥；前端只清空该库加密页签的明文内容，普通明文文件不受影响。

---

## 三、加密核心（Rust `src-tauri/src/crypto/`）

### 3.1 模块划分

| 模块 | 职责 |
|---|---|
| `error.rs` | `CryptoError` 统一错误类型（WrongPassword / BadHeader / IntegrityFailed / NotUnlocked / Io / WeakPassword 等），`Serialize` 输出可读中文消息 |
| `format.rs` | `.mdl` / `.mdlb` 文件格式常量（`mlk/1`、`mlk-vault/1`）与头/体结构（KdfParams / FileHeader / VaultMeta / EncryptedBody）、base64 工具 |
| `keys.rs` | `Kek` / `Dek` 类型（Zeroize 派生 drop 清零）；`derive_kek`（Argon2id）；`generate_dek`；DEK 包裹/解包裹/重包裹；`encrypt_body`/`decrypt_body`（AES-256-GCM） |
| `file.rs` | `.mdl` 序列化/反序列化（4 字节头长度 + 头 JSON + nonce + ct） |
| `session.rs` | `SessionStore`（`Mutex<HashMap>` 全局单例）；`UnlockedVault` 持 kek/kdf/dek + last_activity + auto_lock；自动锁定计时与过期回收 |
| `vault.rs` | 业务流程：创建/解锁/读写/改密码/锁定、库树遍历、文件增删改移、**搜索** |

### 3.2 密钥链路

```
主密码 + salt ──Argon2id(m=64MiB, t=3, p=4)──▶ KEK（32B，内存清零）
      │  AES-256-GCM
      ▼
wrapped_dek ──▶ DEK（32B）
      │  AES-256-GCM
      ▼
明文（markdown 源码 / 文件树 JSON）
```

- **`.mdl` 单文件**：每个文件独立随机 DEK，文件头自带 KDF 参数与 wrapped_dek，独立密码。
- **`.mdlb` 单文件库**：全库共享一个 DEK；文件头存「全库共享 DEK」的 wrapped_dek，数据区是整棵文件树（文件名+目录结构+正文）的 JSON 序列化密文。磁盘上只有一个不透明 blob。
- **改主密码**：只重包裹 DEK（`rewrap`），不重加密正文。

### 3.3 文件布局

```
单文件库（.mdlb）                       单文件（.mdl）
┌──────────────────────────┐           ┌──────────────────────────┐
│ 头（明文 JSON）           │           │ 头（明文 JSON）           │
│  format: "mlk-vault/1"   │           │  format: "mlk/1"         │
│  kdf: argon2id 参数+salt  │           │  kdf / wrapped_dek / hint│
│  wrapped_dek             │           │  wrapped_dek             │
│ 数据区（密文）            │           │ 数据区（密文）            │
│  AES-256-GCM 加密的       │           │  AES-256-GCM 加密的正文   │
│  整棵文件树 JSON          │           │                          │
└──────────────────────────┘           └──────────────────────────┘
```

---

## 四、前端结构

### 4.1 目录

```
src/
├── main.ts                  # 入口：挂 Vue + AntD + Pinia + v-tip 指令；平台检测写 data-platform
├── App.vue                  # 主题解析（light/dark/system + matchMedia 跟随系统）
├── router/index.ts          # hash 四屏路由
├── styles/global.css        # 全局样式（CSS 变量，含深色主题变量）
├── components/
│   ├── TitleBar.vue         # 标题栏（macOS 红绿灯叠加 + 标题）
│   └── VaultTreeNode.vue    # 库树递归节点（文件夹展开/折叠 + 悬停新建/重命名/删除）
├── stores/vault.ts          # Pinia store（会话/打开文件/最近/收藏/设置）
├── lib/
│   ├── tauri.ts             # Rust 命令 invoke 桥 + 文件对话框封装
│   ├── filewatch.ts         # 外部修改监控（文件戳记轮询器）
│   └── tooltip.ts           # 全局 v-tip 指令（单例气泡，不受滚动裁剪）
└── views/
    ├── UnlockView.vue       # 解锁（极简：目标库 + 密码 + 解锁/取消）
    ├── EditorView.vue       # 主编辑界面（核心）
    ├── SettingsView.vue     # 设置
    └── DialogsView.vue      # 弹窗集合（新建库/改密码/新建加密文件）
```

> 前端统一 **Options API**，未用 `<script setup>`（用户硬性约定）。

### 4.2 主界面（EditorView）信息架构

- 侧边栏五分组（可拖拽排序、可各自显隐）：**打开的文件 → 打开的库 → 收藏 → 最近打开 → 工作目录**
- 顶部页签栏（可拖拽排序，独立开关）
- 编辑模式：编辑 / 分屏 / 预览（手写轻量 markdown 渲染器）
- 「更多」菜单：新建文件 / 新建加密库 / 复制到加密库 / 偏好设置 / 侧边栏显隐开关

### 4.3 状态管理（Pinia + localStorage）

`stores/vault.ts` 统一管理，localStorage 键：

| 键 | 内容 |
|---|---|
| `marklock.recent` | 最近/已登记库列表（含锁定态，读回强制 unlocked=false） |
| `marklock.settings` | 设置 |
| `marklock.favorites` / `marklock.favoriteFiles` | 收藏的库 / 文件 |
| `marklock.recentFiles` | 最近打开的文件 |
| `marklock.openFiles` | 上次打开的页签清单（仅元信息） |
| `marklock.sectionOrder` | 侧边栏分组顺序 |
| `marklock.workdir` | 工作目录根 |

密钥与明文一律不进 store。

---

## 五、Tauri 命令清单

> 前端 `src/lib/tauri.ts` 已全部封装为 Promise API。

### 5.1 单文件库（.mdlb）

| 命令 | 说明 |
|---|---|
| `create_file_vault(path, password, hint?)` | 新建单文件库 |
| `unlock_file_vault(vaultId, path, password, autoLockSecs?)` | 解锁（解密整棵文件树） |
| `list_file_vault(vaultId, path)` | 列递归树（不含内容） |
| `read_file_vault` / `write_file_vault` | 读/写库内文件 |
| `create_in_file_vault` / `create_folder_in_file_vault` | 库内新建文件/文件夹 |
| `delete_in_file_vault` / `rename_in_file_vault` | 库内删除/重命名移动 |
| `change_file_vault_password` | 改主密码（重包裹全库 DEK） |

### 5.2 目录库（.mdlb 目录，旧模型保留兼容）

| 命令 | 说明 |
|---|---|
| `create_vault` / `unlock_vault` | 新建/解锁目录库 |
| `list_dir` / `list_vault` | 列目录一层 / 列库递归树 |
| `create_in_vault` / `create_folder_in_vault` | 库内新建文件/文件夹 |
| `delete_in_vault` / `rename_in_vault` | 库内删除/重命名移动 |

### 5.3 单文件（.mdl）与通用

| 命令 | 说明 |
|---|---|
| `unlock` / `lock` / `lock_all` | 解锁单文件 / 锁定单个 / 锁定全部 |
| `read_file` / `write_file` / `create_file` | 单文件读/写/新建 |
| `change_password` | 改单文件主密码 |
| `read_plain_file` / `write_plain_file` | 明文读写 |
| `inspect_path` | 探测路径类型（目录/库/库内文件/明文） |
| `unlocked_count` / `reap_expired` | 状态查询 / 回收过期会话 |
| `search(query, workdirs?)` | 全局搜索（库 + 工作目录明文） |
| `file_stamps(paths)` | 批量查文件 mtime + 大小（外部修改监控） |

### 5.4 外部修改监控

打开的页签可能被其它程序改写。`lib/filewatch.ts` 导出模块级单例 `externalChanges`（`ExternalChangePoller` 实例），
每 2s（以及窗口重新聚焦 / 回到前台时）把全部页签对应的磁盘路径批量 `file_stamps` 一次，与上一轮基线比对 mtime + size：

- 单例而非组件持有：路由切到设置/解锁页会销毁 EditorView，此时只 `detach()` 摘掉回调（定时器与基线保留），回到编辑器 `attach()` 后首轮比对即可补上「离开期间」的改动；设置里关掉开关才 `stop()`
- 开关：设置 → 通用 →「监控外部修改」（`settings.watchExternalChanges`，默认开）
- 监控目标：普通文件 / `.mdl` / 目录库内文件 = 自身路径；`.mdlb` 库内文件 = 容器路径（库内文件不是独立磁盘文件）
- 戳记变化后**逐页签重读正文比对**（容器变了不代表每个已打开文件都变了），内容确实不同才算外部修改
- 无未保存修改 → 直接重新加载并提示；有未保存修改 → 不顶掉用户内容，页签挂橙色角标 + 提示，可右键/点状态栏「重新加载」
- 文件消失（删除/重命名）→ 红色角标 + 提示，保留页签内容
- 排除草稿与任何取不到明文会话的加密页签（锁定后明文已清零，重读会把内容灌回内存）
- 自己的保存：写盘后 `sync()` 刷新基线，内容比对本身也能排除自身写入

### 5.5 权限（capabilities/default.json）

```json
["core:default", "core:window:allow-start-dragging", "notification:default", "autostart:default", "dialog:default"]
```

> 关键：`core:window:allow-start-dragging` 是窗口拖拽的前提（`core:default` 不含它，缺失会导致拖拽静默失效）。

---

## 六、关键机制

### 6.1 窗口与标题栏（macOS）

- `TitleBarStyle::Overlay` + `hidden_title(true)` + `traffic_light_position(14.0, 20.0)`：原生红绿灯叠加到应用自绘工具条左上角，去掉系统标题栏。
- 拖拽：titlebar 加 `data-tauri-drag-region="deep"`；按钮区 `="false"` 禁止拖拽。
- 关闭：`CloseRequested` → `lock_all_vaults` + `app_handle().exit(0)`（点红叉整个程序退出）。
- 跨平台：macOS 专属代码包在 `#[cfg(target_os="macos")]`，前端用 `navigator.userAgent` 检测平台（写 `data-platform`）。

### 6.2 拖放（两类，需区分）

- **外部文件/目录拖入**：`getCurrentWebviewWindow().onDragDropEvent()`，仅在右侧主面板触发（侧边栏区域留给排序）；路由：明文→打开、目录→挂工作目录、`.mdl`/`.mdlb`→登记+解锁。
- **列表排序**：纯鼠标事件（HTML5 draggable 在 WKWebView 失效），移动超阈值判定拖动，elementFromPoint 命中目标。

### 6.3 主题

- 三档 `light` / `dark` / `system`；`system` 用 `matchMedia('(prefers-color-scheme: dark)')` 跟随系统。
- 深色变量覆盖在 `global.css` 的 `html[data-theme='dark']`；各组件硬编码浅色已替换为 CSS 变量。

### 6.4 搜索

- 范围：已解锁库（加密内容解密后匹配）+ 工作目录明文文件 + 打开的页签（内存内容）。
- 匹配：char 级、大小写不敏感子串；每文件最多 3 条命中片段，整体上限 100。
- 交互：300ms 防抖、键盘上下选中（循环）、Enter 打开、Esc 关闭、关键字高亮、结果去重。

### 6.5 自动锁定

- Rust `SessionStore` 维护活动时间戳，超时（默认 5 分钟）`reap_expired` 清零密钥；前端每 30 秒轮询一次。

---

## 七、演进时间线（2026-09-24 起）

### 阶段一：原型与骨架（09-24）

| 里程碑 | 内容 |
|---|---|
| **M1 原型确认** | 产出 `doc/ui-prototype/` 四屏静态高保真原型（unlock/editor/settings/dialogs），对齐 AntD 视觉，确立「VSCode 式多库工作区」信息架构：侧边栏五分组 + 顶部页签 + 编辑/分屏/预览 |
| **M2 前端骨架** | Vite + Vue 3 + Ant Design Vue 初始化，四屏静态页面按原型还原（Options API） |
| **M3 加密核心** | Rust `crypto/` 六模块落地：`.mdl` 读写、Argon2id 解锁、DEK 包裹/重包裹、自动锁定计时；10+ 单元测试通过 |
| **M2.5 功能接线** | 前后端打通，**库=目录加密模型**（`.mlkv` 目录 + vault.json，后改名 `.mdlb` 目录），Pinia + localStorage，四屏真实功能 |

### 阶段二：单文件库重构与功能完善（09-25）

| 主题 | 内容 |
|---|---|
| 启动直达 + 拖入挂载 | 启动直接进主界面；拖入目录挂为工作目录 |
| 打开文件 | 普通文件明文读写 + 加密文件 `.mdl` 走加密链路 |
| 侧边栏开关 + 收藏 | 分组显隐开关、收藏库与文件、锁定语义修正（锁定只清加密内容） |
| 拖放链路重做 | 支持工作目录外文件/目录；`inspect_path` 路由；`pendingOpen` 解锁后自动打开 |
| 会话恢复 | 工作目录 + 打开页签跨启动恢复（加密文件需重解锁） |
| 扩展名重命名 | `.mdk`→`.mdl`、`.mlkv`→`.mdlb`（纯改名，格式不变） |
| 新建草稿流程 | 页签栏「+」开空白草稿，保存时才选目标+密码；密码放宽至 1 位以上 |
| 单文件剥离「库」概念 | `.mdl` 不再是库，页签内就地密码框解锁；修锁定后误跳解锁页 |
| 拖放排序 | 收藏/打开文件/库 顺序固定 + 拖放排序（HTML5 失效→纯鼠标事件→去手柄整条拖动） |
| 加密库支持文件夹 | 递归 `VaultTreeNode` 组件，库内建文件夹/文件 |
| 更多菜单扩充 | 新建文件/新建加密库/复制到加密库；保存选库时内联新建库 |
| 库内文件管理 | 删除/重命名/移动（含路径穿越防护） |
| **单文件容器重构** | 加密库从「目录 + vault.json」改为**单文件容器 `.mdlb`**（全库共享 DEK，整树整体加密） |
| 解锁交互简化 | 就地密码解锁 + 解锁页极简化 + 可取消 |

### 阶段三：UI 打磨与搜索（09-26）

| 主题 | 内容 |
|---|---|
| 就地解锁修库树 | 解锁成功后刷新库树（此前只恢复页签内容） |
| tooltip 重构 | 纯 CSS → 全局 `v-tip` 指令（单例、不受滚动裁剪、方位/延迟/锁定隐私/相对时间） |
| 快捷键 | Cmd/Ctrl+W 关闭当前页签（非窗口） |
| 去标题栏 | macOS Overlay 红绿灯叠加；**拖拽失效根因 = 缺 `allow-start-dragging` ACL 权限**；关闭即退出；跨平台适配；红绿灯像素级校准（y=20） |
| 设置页做通 | 真实绑定 + 未实现项标注「即将推出」 |
| 拖拽排序 | 页签 + 侧边栏分组（`sectionOrder`） |
| 双开关 + 菜单 | 「打开的文件」拆两独立开关；更多菜单点外部关闭 |
| 主题 | 浅色/深色/跟随系统 |
| **全局搜索** | 库内容搜索 → 覆盖工作目录明文 → 修中文 snippet 偏移 → 键盘上下选中 → 打开页签参与 → 关键字高亮、面板加高、去路径行 |

---

## 八、待办（当前状态）

- **M4 编辑器**：CodeMirror 接入（替换当前 textarea）、语法高亮、自动保存防抖链路
- **M5 打磨**：历史快照、快捷键体系、打包分发
- 已知技术债：
  - `vue-tsc --noEmit` 有历史遗留类型错误（`unlockedVaults` getter、DragDropEvent 等，不影响 vite build）
  - AntD 全量引入导致 chunk 偏大（>500KB），可做按需引入/manualChunks 优化
  - 单文件 `.mdl` 会话 `dek=None`，Rust 侧 `search_vaults` 用 `dek.is_some()` 判库类型会误判（已解锁 `.mdl` 目前靠前端页签命中兜底，Rust 侧未修）
  - macOS Overlay 模式窗口未聚焦时无法拖动（Tauri issue #4316）

---

## 九、测试

Rust 侧 `cargo test --lib`，覆盖：

- 密钥往返、错误密码、篡改检测
- 改密码重包裹
- 会话锁定与过期回收
- 目录库全流程、库内文件夹
- 单文件库全流程（增删改移 + 改密码 + 磁盘单文件）
- 路径穿越防护
- 明文目录搜索（中文命中、大小写不敏感、隐藏文件/密文跳过）

---

## 十、扩展点

- **新增 Tauri 命令**：在 `src-tauri/src/lib.rs` 注册新函数并加入 `invoke_handler!`；在 `src/lib/tauri.ts` 添加对应前端封装。
- **更换/扩展加密算法**：在 `crypto/format.rs` 定义新的 KDF 或 cipher 标识；在 `crypto/keys.rs` 实现新的派生/加解密逻辑，保持 `Zeroize` 语义。
- **新增库形态**：在 `crypto/vault.rs` 增加新的库类型分支（例如云同步库、多主密码库）。
- **前端功能扩展**：在 `src/router/index.ts` 添加新路由与视图；在 `src/stores/vault.ts` 扩展状态与动作，坚持「不触碰密钥」原则。

---

## 十一、故障排查

| 现象 | 定位 | 处理 |
|------|------|------|
| 解锁失败（`WrongPassword`） | KEK 解 DEK 或探针校验失败 | 检查密码；参考 `CryptoError::WrongPassword` |
| 文件头解析失败（`BadHeader`） | `.mdl` / `.mdlb` 文件损坏或格式不符 | 校验 `header.format`、`cipher`、`kdf.algo` |
| 会话过期（`NotUnlocked`） | 自动锁定超时，`SessionStore` 已回收会话 | 通过 `reap_expired` 拿到被锁 id 并刷新 UI，重新解锁 |
| 「路径超出库范围」 | 相对路径含 `..` 或绝对路径 | 前端只传库内相对路径；后端已做拒绝 |
