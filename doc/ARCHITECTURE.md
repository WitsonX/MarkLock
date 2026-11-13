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
| `io.rs` | 原子写盘（临时文件 → fsync → 滚动备份 → rename → fsync 父目录）与旁路文件识别；iCloud「仅在线」文件的召回退避与前后通知钩子 |
| `session.rs` | `SessionStore`（`Mutex<HashMap>` 全局单例）；`UnlockedVault` 持 kek/kdf/dek + last_activity + auto_lock；自动锁定计时与过期回收 |
| `asset.rs` | 加密图片/附件：`.dat/<逻辑名>.mdat` 落盘、解锁解密到应用缓存、改密码重包裹、锁定/退出清缓存 |
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
- **图片/附件**：每个附件一份独立 DEK、同样由会话 KEK 包裹，与正文共用同一把主密码锁；因此改密码时必须连 `.dat` 里的附件一起重包裹。

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

附件（与文档同级的 .dat 目录，每个附件一份独立容器）
note.mdlb / note.mdl
note.dat/截图.png.mdat    头（wrapped_dek 由会话 KEK 包裹）+ 密文图片字节
      ↓ 解锁后解密到应用缓存，预览按它解析相对链接
<app_cache_dir>/mlk-assets/<hash(库id)>/note.dat/截图.png
```

---

## 四、前端结构

### 4.1 目录

```
src/
├── main.ts                  # 入口：挂 Vue + AntD + Pinia + v-tip 指令；平台检测写 data-platform
├── App.vue                  # 主题与皮肤解析（light/dark/system + matchMedia 跟随系统；data-skin + 自定义皮肤 CSS 注入）
├── router/index.ts          # hash 五屏路由（便签窗加载同一前端，路由到 /sticky）
├── styles/global.css        # 全局样式（CSS 变量，含深色主题变量与预览/编辑器派生 token）
├── styles/skins.css         # 内置配色皮肤（蓝灰 Nord / 夜蓝 Nocturne / 卡布奇诺 Catppuccin / 纸色 Sepia / 日光 Solarized / 暖棕 Gruvbox，每套各含亮暗两版）
├── components/
│   ├── TitleBar.vue         # 标题栏（macOS 红绿灯叠加 + 标题）
│   ├── VaultTreeNode.vue    # 库树递归节点（文件夹展开/折叠 + 悬停新建/重命名/删除）
│   ├── WorkdirNode.vue      # 工作目录树递归节点
│   ├── CodeMirrorEditor.vue # CodeMirror 6 编辑器封装（markdown 语言 + 语法高亮）
│   ├── MarkdownPreview.vue  # marked 渲染的预览面板
│   ├── EncIcon.vue          # 加密标识图标
│   └── AboutCard.vue        # 关于卡片（版本/检查更新）
├── stores/vault.ts          # Pinia store（会话/打开文件/最近/收藏/设置）
├── lib/
│   ├── tauri.ts             # Rust 命令 invoke 桥 + 文件对话框封装
│   ├── filewatch.ts         # 外部修改监控（文件戳记轮询器）
│   ├── tooltip.ts           # 全局 v-tip 指令（单例气泡，不受滚动裁剪）+ bindHoverTip 委托版（v-html 内容用）
│   ├── preview.ts           # marked 共享渲染层（代码高亮/复制/任务清单/图片 asset）
│   ├── wysiwyg.ts           # 预览就地编辑：DOM→Markdown 序列化 + execCommand 命令层
│   ├── previewSearch.ts     # 预览内查找：渲染 DOM 上建 haystack、包裹 <mark> 命中并定位
│   ├── fonts.ts             # 预览字体内置清单（跨平台回退栈）
│   ├── skins.ts             # 皮肤注册表：内置清单、token 分组、自定义皮肤 CSS 生成/注入/实时预览
│   ├── locales/             # 中英文语言包 + 语言切换
│   ├── sticky.ts            # 便签窗判定 + 按文件类型读写便签正文
│   ├── launchArgs.ts        # 快速启动项「启动参数」原始串的引号感知拆分
│   ├── splash.ts            # 启动屏淡出控制
│   └── update.ts            # 轻量检查更新（GitHub/Gitee 双源 + 频率门控）
└── views/
    ├── UnlockView.vue       # 解锁（极简：目标库 + 密码 + 解锁/取消）
    ├── EditorView.vue       # 主编辑界面（核心）
    ├── StickyView.vue       # 便携便签独立置顶小窗
    ├── SettingsView.vue     # 设置
    └── DialogsView.vue      # 弹窗集合（新建库/改密码/新建加密文件）
```

> 前端统一 **Options API**，未用 `<script setup>`（用户硬性约定）。

### 4.2 主界面（EditorView）信息架构

- 侧边栏六分组（可拖拽排序、可各自显隐，显隐开关在设置 → 外观 → 界面元素）：**打开的文件 → 加密库 → 工作目录 → 收藏 → 最近打开 → 大纲**
- 顶部页签栏（可拖拽排序，独立开关）
- 编辑模式：编辑 / 分屏 / 预览（marked 渲染，任务清单复选框可交互）
- 「更多」菜单：新建文件 / 新建加密库 / 复制到加密库 / 偏好设置 / 侧边栏显隐开关
- **菜单小图标**：主菜单与自绘右键菜单的少数重点项配线性小图标（`MENU_ICONS` 映射 → `menuIcon()` 生成内联 svg）。两个坑写死在实现里：`v-html` 注入的内容拿不到 scoped CSS（`data-v` 只加在模板节点上），尺寸与描边必须写在 svg 自身属性上，否则 WebKit 渲染成 0×0；图标用**绝对定位压在左侧勾选槽上**而不是流内元素——图标与 `✓` 不会同时出现，占位只算一份，有无图标的项文字才从同一 x 起排。
- **右键子菜单**：自定义菜单项新增 `kind:'sub'`（带 `items` 数组），展开靠 CSS `:hover` 而不是 JS 状态；子菜单固定 224px 宽，`showCtxMenu` 按「鼠标 x + 主菜单宽 + 子菜单宽 > 视口宽」置 `subLeft`，把它翻到左侧，避免贴右边界时被裁。导出三项就收在这一层，与 macOS 主菜单里「文件 → 导出」的层级对齐。
- **预览里点附件**：文档类型开页签；zip / 图片 / pdf 等二进制无法按文本解析，改弹保存对话框再复制过去（下载流），避免一点击就报 UTF-8 / 文件头错误。
- **同名页签的目录后缀**：一次打开多个显示名相同的文件时，顶部页签与侧边栏「打开的文件」里的那几个自动带上所属目录小字（`EditorView.vue` 的 `listSubtitles` / `tabRelDir`）。按显示名分组，**只给冲突组生成**；后缀从最近的目录段开始逐级向上加长，直到在组内唯一，相对目录用尽仍不唯一（如两者互为前缀）就退到完整相对目录。目录基准按库类型分别取——`.mdlb` 取 `#` 后的库内相对路径、目录库相对库根、工作目录内明文相对工作目录根、其余取绝对路径的 dirname。未保存草稿与锁定中的库文件一律返回空串：前者还没有磁盘位置，后者不能暴露真实路径（与页签 tooltip 的锁定态口径一致）。

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

密钥与明文一律不进 store。页签上的 `assetDir` 是个例外口径：它只是「该库附件缓存根目录」这一个路径字符串，既不持久化（不在 `marklock.openFiles` 的字段白名单里），也不带内容——锁定时会连同后端缓存一起清掉，重新解锁再 materialize 一次。

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
| `touch_session(vault_id)` / `session_ttl(vault_id)` | 刷新会话活动时间 / 查距自动锁定剩余秒数 |
| `search(query, workdirs?)` | 全局搜索（库 + 工作目录明文） |
| `file_stamps(paths)` | 批量查文件 mtime + 大小（外部修改监控）|
| `set_backup_enabled(enabled)` | 同步「保存时保留上一版备份」开关到 Rust 侧 |
| `write_encrypted_asset(vaultId, destDir, name, base64)` / `copy_encrypted_asset(vaultId, destDir, srcPath)` | 加密写入 / 加密复制一个附件，返回逻辑名 |
| `materialize_vault_assets(vaultId, datDir, datDirName)` | 把 `.dat` 内密文附件解密到应用缓存，返回 `{ cache_dir, failed }` |
| `copy_file_to(src, dest)` | 覆盖式复制到指定路径（预览点二进制附件的下载流） |
| `open_local_file(path)` | 交系统默认应用打开磁盘上已存在的普通文件（导出成功消息的「打开文件」） |
| `launch_local(path, args)` | 快速启动本地应用并透传启动参数（macOS `open --args`，带参时加 `-n` 另起；Windows 追加；Linux 忽略） |
| `take_pending_open_paths()` | 取干后端缓存的「系统打开文件」路径（冷启动 / 非编辑器页跳转后各取一次） |
| `export_pdf(html, target)` | 把自包含 HTML 交隐藏 WebView 静默渲染成 PDF 写入 target（macOS `createPDFWithConfiguration` 逐页截取 + PDFKit 合并，Windows WebView2 `PrintToPdfStream`，Linux 返回不支持） |
| `set_show_hide_shortcut(shortcut)` | 注册 / 更换 / 清除「显示 / 隐藏主界面」的全局热键（Tauri accelerator 串，空串 = 清除）：先反注册上一个再注册新的，失败返回可读错误且不改后端记录，前端据此回滚（见 §6.12） |

> 可能长时间阻塞在磁盘上的读写命令（解锁/读/写/附件 materialize/批量加密）都包在 `spawn_crypto` 里跑阻塞线程池，同步命令在主线程执行会冻结 WebView。

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
["core:default", "core:window:allow-start-dragging", "core:window:allow-set-size", "notification:default", "autostart:default", "dialog:default"]
```

> 关键：`core:window:allow-start-dragging` 是窗口拖拽的前提（`core:default` 不含它，缺失会导致拖拽静默失效）；`core:window:allow-set-size` 同理是便签拖边缩放（`setSize`）的前提，缺失时拖动无反应且静默拒绝。
>
> 便签圆角需要透明窗口。macOS 上 `transparent(true)` 只有开启了 Cargo feature `macos-private-api` 才会真正下发（`tauri-runtime-wry` 按该 feature 条件编译，缺失时静默忽略、debug 构建打一行提示）；而 `app.macOSPrivateApi: true` 会让 `tauri-build` 校验该 feature 已在 `Cargo.toml` 声明，两者必须同时存在。此组合与 Mac App Store 上架不兼容。

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

### 6.3 主题与皮肤

- **主题（明暗）**：三档 `light` / `dark` / `system`；`system` 用 `matchMedia('(prefers-color-scheme: dark)')` 跟随系统。深色变量覆盖在 `global.css` 的 `html[data-theme='dark']`；各组件硬编码浅色已替换为 CSS 变量。
- **皮肤（配色）**：与明暗**正交**，落在 `html[data-skin]`。`skins.css` 里每套皮肤都写成 `[data-skin='x'][data-theme='light']` 与 `…[data-theme='dark']` 两份，避免亮色值渗进深色。
- **派生 token**：`:root` 把预览用的 `--md-*`、编辑器用的 `--cm-*` 默认值声明成 `var(--bg)` 之类的基础变量，皮肤只改基础色就能自动级联到预览与 CodeMirror；需要微调时可直接覆盖派生 token。
- **自定义皮肤**：存于 `settings.customSkins`（localStorage），由 `lib/skins.ts` 的 `buildCustomSkinCss` 生成与内置同形状的 CSS，用 `textContent` 写入 `<style id="marklock-custom-skins">`（不走 innerHTML，变量名与值均经白名单/`;{}<>` 过滤）。编辑时用独立的 `<style id="marklock-skin-preview">` + `data-skin="__preview__"` 做所见即所得，取消 / 保存 / 组件卸载时清理。
- **以内置皮肤为底稿**：卡片上的「复制并编辑」走 `forkSkin`——`snapshotSkinTokens` 从 `document.styleSheets` 里读出该皮肤浅、深两套**显式声明**的令牌（跨域样式表读取失败即跳过），连同新 id 交给编辑器；只取显式声明的令牌，未声明的项继续继承基线，改起来不会把整套底色硬抄一遍。
- **CodeMirror**：不再引 `oneDark`，改用 `EditorView.theme(spec, { dark })` 直接读 CSS 变量，语法色由 `HighlightStyle.define` 走 `--cm-*` / `--hl-*`，`defaultHighlightStyle` 仅作 fallback。
- **导出 HTML / PDF**：`buildStandaloneHtml` 通过 `cssVar()` 读运行时变量，导出件跟随当前皮肤；PDF 一并带上背景色（Windows 侧显式 `SetShouldPrintBackgrounds(true)`），代码块底色与表格斑马纹不会在成品里丢掉。

### 6.4 搜索

- 范围：已解锁库（加密内容解密后匹配）+ 工作目录明文文件 + 打开的页签（内存内容）。
- 匹配：char 级、大小写不敏感子串；每文件最多 3 条命中片段，整体上限 100。
- 交互：300ms 防抖、键盘上下选中（循环）、Enter 打开、Esc 关闭、关键字高亮、结果去重。
- 预览模式下的 ⌘F 不属于这套：它不切视图，直接在渲染出的 DOM 上找，见 §6.13。

### 6.5 自动锁定

- Rust `SessionStore` 按会话记录 `last_activity`，超时（默认 5 分钟，档位 60/300/900/1800/0）后密钥清零；前端每 30 秒轮询一次 `reap_expired`，到期的库页签变回占位。
- **过期 ≠ 就地移除**：`with_vault` 遇到过期会话只返回 `NotUnlocked`，条目保留，删除统一由 `reap_expired` 完成。若在读取路径静默 `remove`，前端轮询就拿不到该 id，界面会停在「已解锁」的假象上，保存永远报「尚未解锁」且无法自愈。代价是过期密钥在内存中最多滞留一个轮询周期。
- **打字计入活动**：后端只统计真正落盘的读写，关闭自动保存时「一直敲字不保存」会被误判为闲置。前端 `markDirty` → `scheduleSessionTouch` 按库节流 15 秒调用一次 `touch_session`（内部就是 `with_vault(id, |_| Ok(()))`，顺带完成过期校验）。
- **到期前先落盘**：`reapExpired()` 遍历已解锁库，对 `session_ttl < 90`（不足两个轮询周期）的库先执行 `autoSaveEncryptedBeforeLock`——此刻会话仍有效，写回成功也顺带刷新活动时间——再交给后端回收，避免未保存内容随密钥一起蒸发。
- **休眠与锁屏**（`settings.lockOnSleep`）：macOS 由 Rust 每秒轮询会话字典的 `CGSSessionScreenIsLocked`，从「未锁 → 已锁」时发出 `marklock://screen-locked` 事件，这是精确信号。前端的「页面隐藏超过 N 秒即判定休眠」启发式在 mac 上不可靠——切到别的窗口、被遮挡、换 Space 都会让页面变 `hidden`，会把正常切窗误判为休眠，所以 mac 跳过该启发式、只保留心跳兜底且阈值放宽到 30 秒（真实休眠挂起远大于此）；Windows / Linux 没有后端探测，维持 5 秒阈值。

### 6.6 便签窗（透明圆角与自定义缩放）

- **圆角**：便签是无边框窗，圆角靠「窗口透明 + `.sticky` 自绘 12px 圆角 + 1px 边框 + `overflow: hidden`」。透明底色需要三层同时透出：Rust `transparent(true)`、前端 `html[data-window="sticky"]` 下 `html/body/#app` 背景置透明（`main.ts` 按 `isStickyWindow()` 打标记），以及 `index.html` 内联启动屏 `#splash` 同步圆角，否则首帧闪过方角底色。
- **缩放**：右下角不可见热区（16px，斜向双箭头光标）自行跟手，不用系统原生缩放（`startResizeDragging`）。原因：Windows 的原生 sizing 循环开始瞬间会切一次焦点，WebView2 随之触发 `blur`，与「失焦即收起」冲突，表现为按住手柄一动便签就消失。实现用 pointer capture（拖出窗口仍继续收 `pointermove`）+ `requestAnimationFrame` 节流 `setSize`，`screenX/Y` 是 CSS 像素、窗口尺寸是物理像素，需乘 `scaleFactor`；下限 300×220 与 Rust `min_inner_size` 对齐。macOS 不显示手柄：无边框窗的 `styleMask` 仍含 `Resizable`，边缘本就能直接拖拽缩放（且 tao 在 mac 的 `drag_resize_window` 返回 `NotSupported`，代码也触发不了），再放一块热区只是噪声。
- **失焦收起**：`blur` 不等于窗口失去激活，非 mac 平台收起前先问 `sticky_is_active`（Windows 直接比对 `GetForegroundWindow()` 与本窗 hwnd，绕开 WebView 内部焦点）；仍在前台就忽略这次 blur。打开系统文件选择框同理由 `picking` 标志挡住。
- **「在主界面中打开」是纯前端跨窗事件**：便签与主窗同一进程、两套 webview，路由属于主窗，所以便签只能把目标递过去而不能替它跳转。链是 `save(true)` 静默落盘 → `showMainWindow()` → `emitTo('main', 'marklock://sticky-open-note', target)`，主窗按 §6.10 的「两处监听」形状接（编辑器在位直接开，否则常驻 `App.vue` 存意图再 `router.push('/editor')`）。开页分派照抄会话恢复的口径，`kind='mdl'` 还要再按 `vaultId === path` 分单文件 .mdl 与目录库内文件——两者读的命令不同，一律当 .mdl 开只会得到一个永远解不开的占位页签。不新增 Rust 命令：`core:default` 里的 `core:event:default` 已含 `allow-emit-to`。
- **便签的预览就地编辑复用序列化器，不复用视图代码**：改渲染结果 → 反解析这一条链的唯一真源是 `src/lib/wysiwyg.ts`，主编辑器与便签各自持有冻结/防抖状态；两边预览 DOM 的形状不同（主编辑器按块包 `.mb` 并带 `data-l` 行锚点供分屏同步，便签走 `preview.ts` 整篇渲染、裸块级子元素），所以序列化器必须两种形状都吃得下——对齐办法不是猜，而是同一批用例在两种形状上各跑一遍，比对幂等结果与格式化命令产出的源码是否逐字一致（实测一致）。只读 ⇄ 编辑那颗开关刻意做成便签本地状态：两个 webview 各有一份 Pinia 实例却共用同一个 localStorage 键，写全局项等于隔窗互相覆盖，与便签 `wordWrap` 只存本地是同一条理由。
- **便签顶部不摆格式化工具栏，只留一颗只读 ⇄ 编辑**：`.sticky-float` 一行靠右，装「编辑 / 分屏 / 预览」段和它左边那颗眼睛 / 铅笔，容器 `pointer-events:none`、两颗控件各自恢复吃点击；整层平时 `.35`，`:hover` 沿 DOM 祖先链匹配到子控件时转 1（容器不吃事件不影响父层拿 hover）。撤掉工具栏的理由是宽度与价值：15 个按钮 + 3 根分隔线约 437px，便签缺省只有 400 逻辑点，而窗内改格式最快的路径本就是切到编辑态写标记。摆法试过三种——同一行左右挤（英文切换段实测 150px > 预留的 136px，直接压住按钮）、上下两行（工具栏自己折成两行）、只留一颗（实测行宽 164px 中文 / 182px 英文，连 300px 最小窗宽都放得下，`overlap:-6`）。
- **托盘键是开关**：绑「弹出便签」的左 / 右键再点一次不是重复弹出，而是收起——先 `emit` 一条 `marklock://sticky-hide`（前端监听里静默落盘未保存内容），随即 `hide()`，与托盘菜单里隐藏便签走的是同一条链；判定只看窗口 `is_visible()`，窗口从未创建过自然算「未显示」。同一次点击既能开也能关，不需要第二个入口。

### 6.7 图片与附件（密文落盘 + 解密缓存）

- **为什么绕一道缓存**：预览显示图片要有文件路径，而附件在磁盘上是 `.mdat` 密文，渲染层读不了。所以这条链的形状是固定的：解锁 → 解密到应用缓存 → 预览拿缓存目录当 `assetBaseDir`。缓存内按 `<.dat 目录名>/<逻辑名>` 镜像，正文里的相对链接不改写就能命中。
- **写入即密文**：粘贴走 `write_encrypted_asset`（base64 → 容器），拖入走 `copy_encrypted_asset`（磁盘字节 → 容器），磁盘名 `逻辑名.mdat`，重名在扩展名前补 `-1`/`-2`。目录库（文件夹模型）与普通明文文件的附件仍是明文，未接入这套。
- **一张坏图不该拖垮整库**：单条读不了 / 解不开只跳过它自己并计入 `failed`，前端提示「N 个附件无法解密」；会话不存在（未解锁 / 已过期）整体返回 `NotUnlocked`，与「附件损坏」区分——否则整目录解析失败被前端静默 catch，所有图片一起消失。
- **缓存生命周期**：`lock` / `lock_all` / `quit_app` / `reap_expired` 删除对应库的缓存子目录；进程被强杀或崩溃来不及清，`setup` 时（此刻必然还没有任何会话）整目录清空兜底。store 侧同步清掉页签的 `assetDir`，重新解锁会再 materialize 一次。
- **改密码同锁**：附件的小 DEK 由会话 KEK 包裹，改主密码必须一起重包裹（单文件 / 单文件库走 `asset::rewrap_assets`，目录库在遍历库内文件时顺带处理 `.dat`）。两处都遵循「先算完所有新字节、再统一写盘」，中途失败不会留下半新半旧的库。
- **导出**：三种成品（HTML / 单文件 HTML / PDF，见 §6.11）共用一条链：先 `ensureAssetsMaterialized` 让附件进缓存，再由 `inlineExportAssets` 处理本地资源——常规 HTML 把图片与非图片附件一并复制进导出文件旁的 `<html名>.dat/` 并改写为相对链接（单个复制失败时图片退回内联 data URL），单文件 HTML 与 PDF 一律内联成 data URL。成功消息带「打开文件 / 打开文件夹」两个行内动作。

### 6.8 iCloud「仅在线」文件（召回兼容）

- **症状**：macOS 上被 iCloud 云端化、本地没有字节的文件，读它会在触发下载的瞬间返回 error 11（`Resource deadlock avoided`）。直接抛给用户就成了「读文件失败」，而真实状态只是「还在下载」。
- **处理**：读写入口统一收进 `crypto::io::read` / `write`，先用 `stat` 的 `UF_DATALESS` 标志位判断要不要召回（免开文件、无副作用），首次撞墙时再用 `NSFileManager.startDownloadingUbiquitousItemAtURL` 主动触发，然后 400ms 退避重试、上限 20 秒。只认 error 11，其它 IO 错误原样返回。
- **可见性**：召回开始 / 结束各发一次 `marklock://cloud-recall(-done)` 事件，广播到所有窗口，前端按路径为 key 显示 / 关闭带转圈的 loading——数秒等待不再表现为无声卡顿。
- **前提**：这些读写可能真的在等，所以六个读写命令必须走 `spawn_crypto`（见 §5.3 注）；主线程同步执行会把 WebView 冻住，loading 来不及绘制，用户只看到卡死。
- **边界**：全局搜索仍在主线程用裸 `fs::read`，未接召回——iCloud 未下载的笔记在搜索里只按文件名命中，不会卡住界面，这是有意的取舍。

### 6.9 快速启动项的启动参数

- **存的是原始串**：`QuickLaunchItem.args?: string` 存用户在设置里敲的原文（空即无参，旧数据没这个字段），点击启动时才由 `lib/launchArgs.ts` 的 `splitLaunchArgs` 拆成数组——引号段（双/单）视为整体，其余按空白切分。存原始串而不是数组，用户回看和二次编辑时看到的正是自己写的那句。
- **无注入面**：参数作为 `Vec<String>` 逐个交给 `Command::args`，不拼 shell 字符串，与 `open_url` 那套协议白名单是两个不相干的口子。
- **平台差异**：macOS `open <路径> --args …`，且因为 `open` 对已在运行的应用只激活、argv 不送达，**带参时固定附 `-n` 另起进程**（不带参保持原样，不额外起实例）；Windows 把参数直接追加到 `explorer <路径>` 之后；Linux 的 `xdg-open` 没有参数透传语义，参数被静默忽略。
- **允许同路径多项**：重复判定从「同路径即重复」放宽成「同路径且同样无参才算重复」，好让同一个应用带不同参数各占一项；因此列表 key 从 `item.path` 换成 `path#i`，便签那条也一样。图标缓存仍按路径取，两项共用同一图标。
- **可拖拽排序**：设置页每项前的把手按下、移动超 4px 才进入拖动（仍是 §6.2 那套纯鼠标事件，`document.elementFromPoint` + `closest('[data-ql-index]')` 命中行）。指针在悬停行的上半/下半决定插到该行**之前/之后**，高亮线跟着画该行顶边或底边——线画在哪，最终就落在哪。`store.reorderQuickLaunch(from, to)` 的 `to` 是「插到原第 `to` 行之前」，摘出 `from` 会让它之后的行整体前移一格，故插入点随之减一；`at === from` 时直接返回，不做无意义的写盘。便签底部按这同一份数组顺序渲染。

### 6.10 从系统打开文件的到达路径

- **入口按平台分叉**：macOS 全靠 `RunEvent::Opened`（访达双击、拖到 Dock、「打开方式」、`open -a MarkLock 文件`），Windows 冷启动读 argv、二次启动由单实例插件把参数转给已运行实例，Linux 没做接入口。
- **为什么必须缓冲**：Tauri 事件不排队，冷启动时 `Opened` 往往早于前端 `listen()` 注册，直接发就永久丢失。所以路径先进 Rust 侧 `PENDING_OPEN_PATHS`，事件只当「去取」的信号，`take_pending_open_paths` 一次取干——顺带保证不会重复建页签。
- **两处监听按路由分工**：`EditorView` 在位时自己的监听直接 drain；不在编辑器页（设置 / 解锁 / 对话框）时由常驻的 `App.vue` 接手，置 `pendingOpenPathsNavigate` 再跳回 `/editor`，`EditorView` 在 `onMounted` **末尾**消费——此时 `_sessionRestore` 已赋值，拉取能与冷启动恢复串行；若上面的启动 drain 已抢先取走路径，再 drain 只拿到空数组，无害。
- **便签送来的目标共用这套形状**：`marklock://sticky-open-note`（见 §6.6）走同样的「两处监听 + store 暂存 + onMounted 末尾消费」，区别只在载荷——系统入口送的是路径且必须经 Rust 缓冲，便签送的是现成的 `StickyNoteTarget`，主窗一直活着（隐藏而非销毁），事件不存在冷启动丢失，所以不必加缓冲。

### 6.11 导出 PDF（隐藏 WebView 静默渲染）

- **为什么不引第三方引擎**：PDF 的排版必须和屏上预览一致，另起一套渲染（wkhtmltopdf / Typst / 手搓 print CSS）等于维护第二份样式。所以链路是「复用屏上渲染出的 HTML，交系统 WebView 自己打印」。
- **自包含是硬前提**：渲染窗口的加载源是 `data:text/html;base64,…`（macOS）或临时文件 `file://`（Windows），两者都没有可用的 base URL，正文里的相对链接一律解不出来。因此 PDF 走 `inlineAll`，图片与非图片附件在拼 HTML 时全部内联成 data URL。
- **一次导出一个窗口，标签自增**：`pdf-export-<seq>`（`AtomicU64`）。固定标签会撞 `webview already exists`——上一个窗口的 `destroy` 经事件循环异步生效，销毁还没完成就复用同名标签必然失败。窗口 `visible(false)` + `skip_taskbar(true)`，尺寸取接近 A4 的 794×1123（96dpi 逻辑像素），让字体与图片按接近打印的分辨率布局。
- **编排形状**：命令线程把「建窗」派发到主线程，然后在通道上 `recv_timeout(120s)`；`on_page_load` 只认首次 `Finished`（`AtomicBool` 挡住后续加载），等 500ms 让图片与字体绘制稳定，再 `run_on_main_thread(capture_pdf)`。`Sender` 不是 `Sync`，而 `on_page_load` 要求闭包 `Fn + Send + Sync`，故套进 `Arc<Mutex<Option<Sender>>>`、截取时取走。收尾（按标签销毁窗口 + 删临时文件）统一放在 `export_pdf`，成功 / 平台报错 / 超时三条路径都不留窗口；平台回调里不销毁自己的宿主窗口——Windows 的完成回调是 WebView2 调进来的，在里面销毁宿主有 use-after-free 风险。
- **macOS 必须自己分页**：`WKWebView.createPDFWithConfiguration` 的 `rect` 只是单页裁剪区域、不产生多页，直接截得到的是被截断的一页。做法是先注入一段 JS，把 `.md` 的直接子块逐个装进固定页高（841.89px）的 `overflow:hidden` 容器、装不下就开新页并返回页数，再按 `rect.y = i × 页高` 逐页截取，最后用 PDFKit `insertPage_atIndex` 顺序合并。分页点落在**块边界**，不会出现半行字跨页；容器继承 `.md` 的原内边距，每页留白一致。
- **Windows 交给引擎**：`ICoreWebView2_16::PrintToPdfStream` + `ICoreWebView2Environment6::CreatePrintSettings`（A4 英寸、四边距 0、无页眉页脚、打背景色——代码块底色与表格斑马纹全靠背景，不打就成白底），分页由 Chromium 的打印排版完成，不跑分页脚本。老 Runtime 缺接口时 cast 失败，明确报「WebView2 运行时版本过低」，而不是静默出一个空白 PDF。回调没用 `wait_for_async_operation`：本函数本身就跑在主线程回调里，再套一层 `GetMessage` 消息泵有重入风险。
- **版式按平台给不同 px**：mac 的 `createPDF` 按 pt 出图（A4 = 595×842pt），WebView2 按 CSS px 排版（96dpi 下 A4 = 794×1123px）。前端按平台写 `body { width }` 与 `font-size`（595 / 11.25 对 793 / 15），两端换算出的 pt 才相等，同一份文档在两端的字号与行位置观感一致；`@page { margin: 0 }` 把留白交给 `.md` 的内边距，与 mac 分页容器继承的内边距是同一份。
- **平台能力缺口**：Linux 的 WebKitGTK 既没有 `createPDF` 也没有 WebView2 的打印接口，`spawn_pdf_window` 在非 mac/Win 上编译成一个直接经通道报「当前平台不支持导出 PDF」的桩（不建窗口）；命令仍照常注册，前端四处菜单按 `isLinux` 不给这一项（macOS 主菜单本就只在 mac 安装）。
- **写盘复用 `crypto::io::write`**：PDF 是明文产物，但落点可能落在被 iCloud 云端化的目录上，所以走同一套召回与原子写盘，不为导出一条单独的写路径。

### 6.12 「显示 / 隐藏主界面」的全局热键

- **为什么只能靠系统级钩子**：主窗被收进托盘（`hide()`）或被别的 app 完全遮住时，webview 根本收不到按键，应用内快捷键在那一刻是失效的——而这恰恰是最需要「一键唤回」的时刻。所以这把键走 `tauri-plugin-global-shortcut`，由系统投递，与本窗是否在前台无关。
- **默认不占任何键**：`settings.showHideWindowShortcut` 默认空串 = 不注册，必须用户在「设置 → 快捷键 → 全局」里录一次才挂上钩子。装好应用就抢走某个系统组合是不可接受的。
- **注册形状**：`set_show_hide_shortcut(shortcut)` 先把 `SHOW_HIDE_ACCEL` 里记着的那个 accelerator 反注册，再注册新值；空串只清除。存的是**字符串**而不是 Shortcut 对象，因为换键时必须能反注册旧值——旧键若还生效，用户会遇到「两个键都在切主窗」。注册失败（写法非法 / 已被系统占用）返回可读错误，且**不改动后端已存值**，前端据此把设置回滚并把旧值重新下发一次，保证界面显示的键和真正生效的键是同一个。
- **两处下发，只有一处会报错**：启动时 `App.vue` 同步一次，且落在 `!isStickyWindow()` 分支里——便签窗是另一个 webview，若它也注册一遍，同一把 accelerator 第二次必然报 `HotKey already registered`。此后用户的改动只由设置页就地调用（要即时反馈）。代价是启动那一次的失败被 `.catch(() => {})` 吞掉：设置里照常显示着热键，实际按了没反应。
- **切换判定**：`toggle_main_window` 把「可见 + 未最小化 + 已聚焦」当作唯一会隐藏的分支，其余（隐藏 / 最小化 / 可见但被别的 app 挡在后面）一律取消最小化 + 显示 + 聚焦。宁可「按一次只是把它提到最前」，也不让窗口被遮住时热键反而把它收走、再也够不着。
- **录制端**：录制时挂 `window` **捕获阶段**的 keydown（先于编辑器处理，免得把组合键录进正文），键名只从 `KeyboardEvent.code` 映射到白名单（字母 / 数字 / F1–F24 / 一组常见控制键），修饰键归一为 `CmdOrCtrl` + `Alt` + `Shift`。除 F 键外必须至少带一个修饰键，否则只提示不录入；Esc 取消，Backspace / Delete 清除，组件卸载时摘监听。
- **插件回调不分键**：`.with_handler` 对任何已注册快捷键的 `Pressed` 都调 `toggle_main_window`。当前只注册了这一把所以成立，加第二把全局快捷键时必须改成按 accelerator 分派。
- **平台缺口**：Linux 后端是 x11rb，只吃 X11；没有 X 连接时（Wayland 会话）它的事件线程直接退出，`register` 走「发给已死线程」的分支**返回成功**，表现为设了键却永远不触发、也不报错。macOS 的 F1–F4 默认被系统当成亮度 / 背光键，裸 F 键要先把「按标准功能键使用」打开才有意义。录制端也不校验冲突：全局钩子是系统级的，录成 `CmdOrCtrl+S` 会把**所有应用**的保存抢成切换主窗。

### 6.13 预览内查找

- **搜的是渲染结果，不是源码**：全局搜索搜「所有文件的源码」，CodeMirror 面板搜「当前文档的源码」，这一条搜「当前文档渲染出的 DOM」——只有后者能在预览模式下把命中定位到用户看得见的那句话上。三者管道各不相同，所以另建 `src/lib/previewSearch.ts`，不复用前两者。
- **拼 haystack，块边界垫 `\n`**：把 `.md` 里的正文文本节点按文档序拼成一条字符串跑正则，快是快，代价是「上一段段尾 + 下一段段首」会拼出假命中。做法是每个块级元素前后各垫一个 `\n`，且这个 `\n` **不映射到任何文本节点**：命中若不能完整映射回去就整条丢弃。行内标签（`strong` / `em` / `a` / `code`）不垫，所以被行内语法拆开的命中照样找到，只是包成同组的多个 `<mark class="pv-search">`，导航时整组一起点亮。垫的是换行，顺带让 `m` 标志下的 `^` / `$` 落在块边界上，与编辑器逐行搜索的语义对齐。
- **不搜的子树**：应用自己注入的控件（代码块复制按钮）、表单控件、脚本样式、嵌入式媒体，以及 `SVG`——`<mark>` 不是合法的 SVG 子元素，包进去会把图形渲染弄坏。
- **高亮是临时 chrome，两条出口都得清干净**：`v-html` 重新注入（切页签、外部重载、附件缓存刷新）会让高亮随旧 DOM 一起消失，所以 `renderedPreview` 的 watcher 与 `commitPreview` 在新 DOM 上重跑一次搜索，`pvSearchNav` 也要先确认当前 mark 仍 `isConnected`（就地编辑可能把命中的字删了）再决定滚动还是重搜。就地编辑那条更危险：改一个字就触发序列化，所以 `wysiwyg.ts` 的 `rawHtml()` 在取 `outerHTML` 前按类名剥掉 `mark.pv-search` / `mark.pv-hit`，否则一次「搜完随手改字」就把 `<mark>` 写进源码，HTML / PDF 导出同理。行内路径本就透明穿过 `MARK`，用户手写的 `<mark>`（不带这两个类名）不受影响。
- **搜索条不挤压排版**：外层 `.pv-search-row` 是 `position:sticky; top:10px; height:0; pointer-events:none` 的零高行，真正吃点击的 `.pv-search-bar` 挂在其内——既能浮在正文之上，又不占正文一行的高度、不挡住下方点击。起始项取视口顶以下的第一个命中（浏览器查找同款），全滚过了回绕到 0；定位时对齐到视口上三分之一，落在横向可滚代码块里的命中再补一次横向滚动。
- **入口按模式分叉**：`findAction` 在预览模式开这条搜索条，编辑 / 分屏仍聚焦编辑器打开 CodeMirror 面板（带替换）；原生菜单 `menu:find` 与 ⌘G / ⇧⌘G（仅搜索条打开时劫持）走同一判定。搜索条只认 `pvCanEdit`（纯预览 + 有明文），与「是否允许就地编辑」的 `settings.previewInlineEdit` 无关，只读预览一样能找。macOS 主菜单该项的标题随之从「编辑器内搜索」改为「查找」。
- **便签没有这条**：便签窗复用了 `wysiwyg.ts`，没复用 `previewSearch.ts`。

### 6.14 「关闭库」与「锁定库」按同一条不可读处理

- **两种状态在数据上本来就不同**：锁定是 `recent` 里那条登记的 `unlocked=false`（登记还在，Rust 侧会话已清）；关闭是整条登记被移出 `recent`（关闭库 / 清空最近打开），此时连库名、库色都查不到——而页签还开着，`vaultId` 照旧指向那个 id。
- **「查不到登记」必须算不可读**：`isActiveLocked` / `activeVaultUnlocked` / `vaultSessionAlive` / `isVaultFileLocked` 四处原先对 `recent.find` 落空的写法不一致，有两条是「当作放行，后端 `NotUnlocked` 兜底」。放行的实际后果不是兜住了，而是 materialize 对着一句没有会话的库逐张试解密、条条失败，最后报成「N 个附件无法解密」——把「库关了」说成「附件坏了」。现在四处统一为**查不到 = 不可读**，并且先按 `isVaultFile` 把明文页签排除在外，否则普通 `.md` 会因为「在 `recent` 里找不到自己的 vaultId」被误判成锁定。
- **掩码补齐到标题栏与搜索**：页签与侧边栏早就有「锁定库文件显示成『xx 库文件』」的约定，但标题栏的 `activeFileName` 直接读 `f.name`，关闭库后真实文件名就挂在窗口顶上；现在它同样走 `fileDisplayName`。全局搜索的「打开的页签」这一路也跳过这类页签，文件名与内容都不给。库名与库色则先回退收藏快照、再回退按库根路径推导出的名字，掩码标签因此不会退化成「 库文件」这种半截话。
- **重建登记只有一个出口**：`store.ensureVaultRegistered(id)`——命中 `recent` 直接返回；否则取收藏快照登记回来；再否则按 `vaultId` 本身推导（`.mdlb` 结尾算单文件库，其余算目录库）。三个调用点：就地解锁提交、`recentOpenVault`、`recentOpenFile`。就地解锁那处是必需的，`unlockVault` 对没有登记的 id 直接报「未找到该库」，于是「关闭库 → 在页签里输密码」这条最自然的路以前必然失败。
- **副作用要说清**：`ensureVaultRegistered` 是真写回 `recent` 并持久化，不是只读兜底。所以关闭库后就地解锁等于把这个库重新登记回来，且拿到的是按 `VAULT_COLORS` 轮转分配的新色（收藏快照里带色的除外），与关闭前那次不一定一致。

### 6.15 视图切换与就地编辑的滚动对位

- **「一切换就往上滚一下」不是同步逻辑坏了，是宽度在动**：预览 ⇄ 分屏之间面板宽度按 0.24s 过渡，正文一边变窄一边重排、行数变多，同一个 `scrollTop` 因此指向越来越靠前的内容。用「切换前记下百分比或行号、切完还原」这类做法都得先假设一个排版模型，排版一改就失准。
- **锚的是元素，不是位置数字**：`_previewAnchor()` 在顶层块 `.mb` 里取「顶部不超过视口顶的最后一个块」，连该块顶到视口顶的像素偏移一起存下；过渡期间用 `requestAnimationFrame` 逐帧 `_pinPreviewAnchor()`，每帧只量这一个元素再还原 `scrollTop`，280ms（比过渡多留 40ms 余量）后收尾。锚块若被 `v-html` 重新注入就 `isConnected` 为假，该帧直接放弃、交给收尾对位，不去猜新块的位置。
- **切到分屏还要把源码面板带过去**：`_previewTopLine()` 按顶层块之间线性插值算出预览当前顶部对应哪一行，交给 `revealLine`。基准是「用户刚才在读的那份预览」，而不是编辑器上一次的位置。
- **另一半是别把程序化滚动当成用户滚动**：双向同步的口径是「编辑器滚 → 预览按块顶定位」，而 `revealLine` 之后 CodeMirror 还会补一次 measure 再发一个 scroll，行号与刚设的那个至多差 1。把它当成用户滚动反灌一次，预览就重新按块顶定位，表现为往上错半行到一行。`_syncEchoLine` / `_syncEchoAt` 在 500ms 内吃掉这种回波，一旦真滚动离开该行立即恢复。
- **0 宽的源码面板不参与同步**：预览模式下编辑器被收成 0 宽，隐藏编辑器每次整篇替换都会重设视口并发出 scroll，而 0 宽下的「视口顶行」换算本就失真。所以 `onEditorScroll` 在 `currentMode === 'preview'` 直接丢弃，`CodeMirrorEditor` 的 modelValue watcher 也只在 `isVisible()` 时才做行号对位——这是 macOS 上「在预览里打字、正文每字往上跑」的根因。
- **一轮切换只认最后一次**：`_modeSyncToken` 每次 `setMode` 自增，逐帧循环与收尾都先比令牌，早于最新那轮的循环自己退出；`setMode` 遇到同模式直接早退，切换全程由 `syncingScroll` 压住双向同步，收尾后再等 120ms 才放开。

### 6.16 就地编辑态的链接与「v-html 内容的悬浮提示」

- **普通点击不跳转**：编辑态里点链接会把焦点从 contenteditable 抢走，还没防抖回写的改动随之丢失、光标也没了，所以照 VSCode 的口径改成 ⌘ / Ctrl+点击才打开，普通点击只落光标。拦的只有非 `#` 链接——`#` 锚点保留默认行为，两种点击都照旧在页内滚。打开走的是只读态那一条分派（外链进系统浏览器、库内相对路径在应用内打开、`#` 锚点页内滚动），不为编辑态另写一份链接逻辑。
- **只有按住修饰键才给「可点」样式**：`.md.wysiwyg-on.pv-link-hot a` 在 ⌘ / Ctrl 按住期间才加下划线与手型，平时保持正文观感。修饰键由 `keydown` / `keyup` 只认 `Meta` / `Control` 维护，并在窗口 `blur` 时清零——⌘Tab 切走时应用收不到 keyup，不清就会一直卡在「可点」态。
- **`v-tip` 指令在这里用不上**：指令要编译到模板节点上，而预览正文是 `v-html` 注入的。`lib/tooltip.ts` 因此多了一个委托版 `bindHoverTip(root, { pick, text, enabled })`：监听挂在容器上（绑组件根而不是 `.md`，那个节点随 `assetVersion` 换 key 重建，挂它身上的监听会丢），命中后仍按同一个 `SHOW_DELAY` 延迟，到点再验「仍悬停同一元素 + 元素还在文档里 + `enabled()`」才显示。就地编辑每次回写都可能换掉 DOM，拿旧节点定位必然贴错，这层复验不是保险而是必需。文案随修饰键状态换：没按住时教怎么点，已按住改说「点击打开链接」。
- **主编辑器与便签各写一份**：两处宿主不同（`EditorView` 的 `.pane-preview` 与 `MarkdownPreview` 的面板），修饰键监听 + `pv-link-hot` + `bindHoverTip` 三件套写法一致，但状态字段挂在各自组件上，没有抽公共实现。

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

## 八、演进现状与待办

### 已完成（原 M4/M5 规划项）

- **M4 编辑器**：CodeMirror 6 接入（替换 textarea，markdown 语言 + 语法高亮 + 主题）；自动保存定时器链路（`autoSaveSecs` 可配）
- **M5 打磨**：快捷键体系（页签切换/撤销关闭等，设置页专设快捷键分区）；打包分发（GitHub Actions：`release.yml` + `release-win-offline.yml` 双工作流）；原子写盘 + 滚动备份（`.bak` 系列，「保存时保留上一版备份」开关），提供「恢复上一版」的数据基础

### 剩余待办

- 备份文件的**前端恢复入口**（当前仅 Rust 侧滚动备份落盘，暂无「一键恢复上一版」UI）
- **Linux 的 PDF 导出**（WebKitGTK 没有静默打印 API，要么驱动它走 CDP/`Page.printToPDF`，要么引外部引擎；现在直接不给入口）

### 已知技术债

- AntD 全量引入导致 chunk 偏大（>500KB），可做按需引入/manualChunks 优化
- 单文件 `.mdl` 会话 `dek=None`，Rust 侧 `search_vaults` 用 `dek.is_some()` 判库类型会误判（已解锁 `.mdl` 目前靠前端页签命中兜底，Rust 侧未修）
- macOS Overlay 模式窗口未聚焦时无法拖动（Tauri issue #4316）
- **附件加密只覆盖 `.mdl` / `.mdlb`**：目录库（文件夹模型）与普通明文文件的插图仍以明文落在 `.dat`（判据在 `isEncryptedAssetFile`）。接入目录库要一起决定预览基准、改密重包裹范围与「库内出现明文图」是否可接受。
- **便签不接附件缓存**：便签的预览基准取笔记自己在磁盘上的目录，指向 `.mdl` 的便签里 `.mdat` 附件不显示（会话在同一进程里，补 materialize 可行，未做）。
- **附件明文在解锁期间驻留应用缓存**：`<app_cache_dir>/mlk-assets` 本身不加密，同用户的其它进程可读；锁定 / 退出 / 自动锁定清除，强杀残留由启动清理兜底。
- **目录库改密的判定口径**：`.dat` 目录内条目与非文本扩展名的散落文件按附件跳过；名字仍像笔记（文本扩展名或无扩展名）却不是容器的文件会拒绝整次改密——现在拒绝是安全的（先算完再写盘，库未被改动），但库里放了个明文 `.json` 之类仍会挡住改密。
- **搜索不触发 iCloud 召回**：`search` 仍在主线程用裸 `fs::read`，未下载的文件读不了就只按文件名命中；接召回需要先把它挪到线程池并设逐文件短超时，否则一次搜索能等 20 秒。
- **快速启动参数只按代码语义写，未逐平台实测**：macOS 的 `open -n … --args` 依赖目标应用自己做单实例转发；Windows 走 `explorer <路径> <参数…>`，`explorer` 有自己的 `/e`、`/root` 语义，后续参数是否真能送达待验证（无效就改 `cmd /c start "" <路径> <参数>`）；Linux 的 `xdg-open` 无透传入口，参数静默丢弃且 UI 不提示。
- **macOS 带参启动恒另起新进程**：不带参时保持原样，带参就固定 `-n`，没有开关——`open` 对已运行应用不送 argv，这是唯一能让参数送达的路。不支持多实例的应用会越点越多。
- **便签快捷方式条看不出哪项带参**：只在 hover 的 title 里拼了 `路径 参数`，而设置页有主色描边的「参数」按钮，两处不对等。
- **双击文件会把用户从设置页拽回编辑器**：这是「让文件尽快打开」的取舍，正在编辑的皮肤草稿等表单状态会随之丢失。
- **导出 PDF 的两端分页不可能逐页对齐**：macOS 靠注入的 JS 按块边界装箱切页，Windows 直接吃 WebView2 的打印排版，同一份文档在两端的换页位置会有差异；追求逐页一致需要统一成一种分页策略（mac 之外也跑脚本，或 mac 改用 CSS 分页），目前接受。
- **macOS 分页只在块边界切，不切内容**：单个块（超长代码块、超长表格、超高图片）高过一页时，装它的那页容器 `overflow:hidden` 会把超出部分裁掉。分页高度在 `Finished` 后固定 500ms 一次性测量，图片尚未解码完时量到的高度偏低，结果可能是页数偏少、末页内容被截。
- **macOS 单页截取失败是静默的**：某页的 `createPDF` 回调拿到 nil 只 `eprintln!` 一行，合并时跳过，成品表现为「少一页」而不是报错。宁可失败得吵一些，但目前实现如此。
- **图文档的自包含 HTML 很大**：PDF / 单文件 HTML 把所有附件内联成 base64，体积按 4/3 膨胀；macOS 走超长 `data:` URL、Windows 走临时文件（超长 data URL 会被 WebView2 拒绝）。极大图片密度下仍可能触及 WebView 的 URL 长度或内存上限，失败表现为 120 秒超时或空白 PDF，没有前置检查。
- **导出期间没有进度/等待提示**：后端最长等 120 秒，前端只在结束时提示成功或错误；期间菜单已收起，用户看不出应用在做这件事。
- **Windows 的 PDF 代码在本机无法编译验证**：`cargo check --target x86_64-pc-windows-msvc` 在 `tauri-winres` 构建脚本处失败（需要 `llvm-rc`），首次真编译发生在 CI；接口链（`ICoreWebView2_16` / `Environment6`）与 `IStream` 读取的运行时行为同样待 CI/真机确认。
- **全局热键启动注册失败是静默的**：`App.vue` 那一次同步用 `.catch(() => {})` 吞掉，热键被占用 / 平台不支持时设置里仍显示着那个键，用户只看到「按了没反应」。要么补一次启动后的失败提示，要么在设置页显示「已注册 / 未注册」状态。
- **全局热键不做任何冲突校验**：不与 app 内快捷键比对，也不比对系统其它应用；录成 `CmdOrCtrl+S` 这类常用组合会全局抢占，连别的应用的保存都被切成切换主窗。Wayland 会话下注册还会「假成功」（见 §6.12）。
- **插件回调按「只有一把快捷键」的前提写**：任何已注册全局快捷键 `Pressed` 都会切主窗，加第二把（例如「立即全部锁定」）时必须先改成按 accelerator 分派，否则新功能一注册就抢走主窗切换。
- **`toggle_main_window` 用 `is_focused()` 当「在最前」**：主窗可见但被别的 app 挡住时按热键是提到最前而不是收起，符合「先够得着」的取舍，但与「切换」二字的直觉不完全一致；无焦点 API 能区分「可见且在最前」。
- **录制允许 F1–F24，提示文案写的是 F1–F12**：`codeToAccelKey` 的白名单与 `shortcutNeedMod` 的措辞口径不一致（macOS 上 F13 以后本就没有物理键，但代码是 F24）。
- **预览内查找每次按键全量重扫，没有防抖**：`input` 一次就重建 haystack、重跑正则、重包全部 `<mark>`。常规文档无感，几十万字或代码块密布的长文会边打字边卡；补防抖要先定「防抖期间旧高亮和命中数算不算落后」。
- **正则模式直接吃用户输入，可能把 webview 卡死**：灾难性回溯（`(a+)+$` 这类）在主线程 JS 里没有超时出口，只能强杀窗口。要么给 haystack 长度或匹配步数设上限，要么收回正则模式。
- **「高亮不进源码」靠类名约定维系**：`wysiwyg.ts` 的 `rawHtml()` 只剥 `mark.pv-search` / `mark.pv-hit` 两个类名。以后再有别的全局注入标记（跳转高亮、批注、修订）必须同时加进这条选择器，漏一个就是把 `<mark>` 写进用户的文档。
- **锁定的页签在预览模式按 ⌘F 没有反应**：`openPvSearch` 以 `pvCanEdit`（要求有明文）为前提，不满足就直接返回，不解释为什么。
- **便签窗没有查找**：小窗反而更需要「在这篇里找」，目前只能切编辑态用系统的滚轮慢慢翻。
- **关闭库后就地解锁会把库重新登记回来，色值还可能变**：`ensureVaultRegistered` 走路径推导分支时按 `VAULT_COLORS` 轮转取色，与关闭前那条登记的颜色不一定相同（收藏过的库带快照色，不受影响）。要保住原色得在移出登记时留一份快照，目前没留。
- **滚动对位的几个数字是量出来的经验值**：过渡 0.24s、逐帧钉 280ms、无锚点那轮 300ms 后收尾、放开同步再延 120ms、回波窗口 500ms 且容差 ±1 行。改过渡时长、给预览做虚拟化或换排版，这些数要一起重估；回波窗口设太长会连用户真正的快速滚动也吃掉。
- **锚块只从顶层 `.mb` 里取**：渲染层哪天不再包 `.mb`，`_previewAnchor()` 就返回 null、切换退化成「不锚定」；嵌套块不参与，所以单个超长块内部滚动时锚点仍是块顶，块内位置只靠一次 `dy` 保住。
- **`bindHoverTip` 与 `v-tip` 两套并存**：共用同一个气泡单例，却是两个调用面，`pick` / `text` / `enabled` 的语义目前只在预览链接这一处被真机验过；等第二个 v-html 场景出现再抽公共，现在抽等于凭猜测定接口。
- **`place:'left'` 的算式暂时没有调用者**：预览浮标改成标题栏按钮后 left 方位无人使用，当年为它补的「先量气泡宽度」仍在原地。接上下一个使用者时要真机复核，别再照抄 right 的锚点。

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
- 加密附件：加解密往返与篡改检测、写入 → 解密到缓存 → 清缓存生命周期、历史明文附件的兼容复制
- 改密码带动附件重包裹（用旧钥匙再也解不开、非容器条目被跳过）、坏附件只跳过自己且其余照常可用、缓存已新于源时不重复解密
- 目录库内含明文附件（`.dat` 里的图片 + 散落 png）时改密码照样成功、附件字节不变；塞一个「像笔记却不是容器」的文件时改密码被拒，且旧密码仍能解锁读正文（证明没有半新半旧）

> 导出 PDF 没有自动化测试：它依赖真实 WebView 的渲染与系统打印接口，`cargo test --lib` 覆盖不到。改动后需手工在三端验证，至少覆盖纯文本文档、含图文档与超长代码块三种情况（后者最容易暴露分页裁剪）。

> 「显示 / 隐藏主界面」全局热键同样测不到：它要求进程之外的系统级按键事件。真机至少验三态——主窗收进托盘后、被别的 app 完全遮住后、正在前台时——各按一次，确认前两态能唤回、第三态会收起；再改一次键，确认**旧键立刻失效**（还生效说明反注册没走到）。

> 预览内查找也只能真机验：它读的是浏览器渲染出的 DOM，`cargo test --lib` 覆盖不到。手工至少覆盖五条——① 上一段结尾与下一段开头拼起来正好等于查询词时**不得**命中；② 命中被 `**加粗**`、行内代码拆开时整组一起点亮，且 ⌘G 把它算作一条；③ 开着就地编辑时搜完随手改一个字再保存，源码里不得出现 `<mark>`，导出 HTML 同样不得出现；④ 外部修改触发重载后高亮仍在（重注入会重跑搜索）；⑤ Esc 两处都要按到——焦点在搜索框里（`stopPropagation`，不连带关掉别的层）与焦点已点回正文（窗口级兜底）——都要收起搜索条并还原高亮。

> 视图切换的滚动对位与就地编辑的链接交互也只能真机验：前者依赖真实的宽度过渡与 CodeMirror 的 measure 时序，后者依赖 webview 的原生选区行为。手工至少覆盖四条——① 预览滚到文档中段再切分屏，正文不得整体上跳，源码面板要落在用户刚读的那一句附近，切回预览同样不跳；② macOS 在预览里连续打字半分钟，正文应停在原位、不随每次回写往上跑；③ 就地编辑里普通点击只落光标、⌘ / Ctrl+点击才打开，`#` 锚点两种点击都只做页内滚动；④ 按住修饰键时下划线与「点击打开链接」气泡出现，松手回到「按住 ⌘ 点击打开链接」，中途 ⌘Tab 切出再回来提示态不得卡住（靠 `blur` 兜底）。

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
| 图片全不显示但正文正常 | 附件未解密到缓存（会话丢失 / materialize 失败被静默 catch） | 重新解锁该库；确认页签的缓存根目录非空 |
| 提示「N 个附件无法解密」 | `.dat` 内某些 `.mdat` 损坏或不是当前主密码加密的 | 其余附件照常显示；坏的那几张需重新导入 |
| 改主密码报格式/头错误 | 库根或子目录里有「名字像笔记却不是 `mlk/1` 容器」的文件（明文 `.json`、无扩展名文件等） | 把它移出库或改名成非文本扩展名；`.dat` 内的明文附件已被自动跳过 |
| mac 上读文件报 error 11（`Resource deadlock avoided`） | iCloud「仅在线」文件正在召回下载 | 等 loading 结束（上限 20 秒）；反复失败就在访达里右键「立即下载」 |
| 便签点了带参的快捷方式没反应 | 目标应用不接 argv（macOS 另起的新实例没做单实例转发；Windows `explorer` 可能吞掉参数） | 先去掉参数验证启动本身是否正常，再按应用自己的参数语法调整 |
| 双击 `.md` 后应用跳到编辑器却没开文件（Linux） | Linux 未接「打开文件」入口（mac 走 `RunEvent::Opened`、Win 走 argv + 单实例插件） | 暂用应用内「打开文件」；Linux 侧接入口是待办 |
| 导出 PDF 报「WebView2 运行时版本过低」 | `PrintToPdfStream` 在 `ICoreWebView2_16`（约 Edge 115+）、`CreatePrintSettings` 在 `Environment6`（约 105+），cast 失败即老 Runtime | 更新 WebView2 Runtime（随 Edge 浏览器 / 系统更新）后重试；老机器可先导出单文件 HTML |
| 导出 PDF 卡住后报「超时或失败」 | 命令线程最多等 120 秒；图文档内联成自包含 HTML 后过大、隐藏窗口没能渲染完 | 减少正文图片或改用「导出 HTML（单文件）」；仍失败看 stderr 的 `[PDF-CAPTURE]` |
| 导出的 PDF 少页或内容被裁 | macOS 按块边界装箱，单个块高过一页时超出部分被 `overflow:hidden` 裁掉；某页截取失败只写 stderr 并在合并时跳过 | 拆短超长代码块 / 表格；图片高的话改用分页更碎的排版 |
| Linux 菜单里没有「导出 PDF」 | WebKitGTK 无静默打印接口，`export_pdf` 在该平台只返回「当前平台不支持导出 PDF」 | 有意为之（见 §6.11 与 §8 待办）；可用「导出 HTML（单文件）」交给浏览器打印 |
| 设了全局热键但按了没反应 | 组合已被系统/其它应用占用，注册失败发生在启动那一次（被 `.catch` 吞掉）；或 Linux 的 Wayland 会话（x11rb 无 X 连接时注册会「假成功」） | 到「设置 → 快捷键 → 全局」重录一个未被占用的组合（会立刻报错并回滚），换个带修饰键的字母键；Wayland 下改到 X11/XWayland 会话再用 |
| 按一次热键窗口没收起、反而被提到最前 | 判定把「可见 + 未最小化 + 已聚焦」当作唯一隐藏分支，主窗被别的 app 遮住时 `is_focused()` 为假 | 设计如此（先保证够得着），再按一次即收起；见 §6.12 |
| 托盘同一个键点两下便签就没了 | 绑「弹出便签」的左右键现在是开关：便签可见时再点改为收起（收起前静默落盘） | 不是失效；要重新弹出再点一次即可（见 §6.6） |
| 预览里 ⌘F 找不到屏幕上明明有的词 | 查找跑在渲染 DOM 上：跨块的「命中」（上段段尾 + 下段段首）会被丢弃，`SVG` 与应用注入的控件不参与，锁定的页签没有明文可搜 | 改搜单段内部的片段，或切编辑 / 分屏用编辑器搜索（带替换）；见 §6.13 |
| 关掉库之后页签变成「xx 库文件」、内容区要求解锁 | 关闭 = 整条登记从 `recent` 移出，前端与锁定同口径按不可读处理，同时不暴露文件名 | 直接在页签里输密码（会顺带把库重新登记回来），或从侧边栏 / 最近重新打开该库；见 §6.14 |
| 在预览里打字正文一行行往上跑，或预览 ⇄ 分屏切换时往上跳一下 | 宽度过渡期间同一个 `scrollTop` 指向更靠前的内容；收成 0 宽的隐藏编辑器又会发出失真的滚动事件反灌预览 | 已按 §6.15 处理（锚块逐帧钉回 + 预览模式丢弃编辑器滚动）。若仍复现，多半是锚块没取到（`v-html` 刚重建），下一次滚动会自己纠正 |
| 就地编辑里点链接没反应 | 编辑态普通点击只落光标——跳走会把未回写的改动和光标一起弄丢，只有 ⌘ / Ctrl+点击才打开 | 按住 ⌘ / Ctrl 再点，悬停时有提示；见 §6.16 |
