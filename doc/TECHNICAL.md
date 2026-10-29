# MarkLock 技术说明

面向开发与想了解内部实现的读者。产品层面的介绍见 [../README.md](../README.md)，架构演进时间线见 [ARCHITECTURE.md](ARCHITECTURE.md)。

- [整体架构](#整体架构)
- [加密设计](#加密设计)
- [Rust 加密核心](#rust-加密核心)
- [Tauri 命令清单](#tauri-命令清单)
- [前端结构与状态](#前端结构与状态)
- [目录结构](#目录结构)
- [便携便签的实现](#便携便签的实现)
- [开发与调试](#开发与调试)
- [常见构建问题](#常见构建问题)
- [编码约定](#编码约定)

## 整体架构

三层进程边界，密钥只存在于最内层：

```
┌─ WebView（Vue 3 + AntD，JS）────────────────────┐
│  只见明文视图与密文字节，拿不到任何密钥          │
│  src/stores/vault.ts  ← 元信息（路径/名称/锁定态）│
│  src/lib/tauri.ts     ← invoke 桥接层            │
└───────────────────┬──────────────────────────────┘
                    │ Tauri IPC（命令 + 事件）
┌───────────────────▼──────────────────────────────┐
│ Rust 壳（src-tauri/src/lib.rs）                  │
│  窗口 / 托盘 / 菜单栏 / 原生菜单 / 热角轮询       │
│  SessionStore：密钥驻留内存 + 自动锁定计时        │
├──────────────────────────────────────────────────┤
│ 加密核心（src-tauri/src/crypto/）                │
│  Argon2id 派生 → KEK → 包裹/解包裹 DEK → AES-GCM │
└──────────────────────────────────────────────────┘
```

- **壳**：Tauri 2。选它而非 Electron 的理由是体积小（Windows 安装包默认 3.5MB）、用系统 WebView、且能直接写 Rust 加密核心。
- **前端**：Vue 3（Options API）+ Vite + TypeScript + Ant Design Vue + CodeMirror 6。
- **加密**：全部在 Rust 侧，`aes-gcm` / `argon2` / `zeroize`。JS 侧不参与任何密钥运算。

## 加密设计

### 两种密文形态

| 形态 | 内容 | 密钥 |
|---|---|---|
| `.mdlb` 单文件库（MarkLock Bundle） | 单文件容器，内部是一棵加密文件树（文件名 + 目录结构 + 正文） | 库主密码；全库共享一个 DEK |
| `.mdl` 加密文件（MarkDown Lock） | 单个 Markdown 文件的密文 | 独立密码、独立 DEK |

库是**单文件**这一选择带来两个直接好处：磁盘上只有一个不透明 blob，连目录名和文件名都不可见；整份 blob 可以原样丢进 iCloud / Dropbox / 移动硬盘同步，不需要目录级的一致性协调。

### 磁盘布局

两种形态共用同一套帧格式，靠头里的 `format` 字段区分（`mlk-vault/1` vs `mlk/1`）：

```
[4 字节头长度][头 JSON（明文）][nonce][ciphertext]
```

头是明文，只存公开参数：

```
format        : "mlk-vault/1" | "mlk/1"
kdf           : argon2id, m=64MB, t=3, p=4, salt
wrapped_dek   : KEK 包裹后的 DEK
```

数据区是 AES-256-GCM 加密后的内容。`.mdlb` 的明文是整棵文件树的 JSON 序列化结果。

```
┌─ 单文件库（.mdlb）───────────────────────┐
│ 头（明文 JSON）                          │
│   format / kdf / wrapped_dek            │
│ 数据区（密文，AES-256-GCM）              │
│   nonce + ct = JSON(文件树)             │
│     ├─ 笔记/第一篇.md   "正文..."        │
│     ├─ 项目/方案.md     "正文..."        │
│     └─ ...                              │
└──────────────────────────────────────────┘
```

图片与附件不进容器，落**同级 `<文档名>.dat` 目录**，每个附件是独立的一份 `mlk/1` 容器（独立 DEK、由会话 KEK 包裹），因此磁盘上不落明文：

```
note.mdlb              库容器
note.dat/              附件目录（与容器同级）
  截图.png.mdat        头 + 密文（图片字节）
  合同.zip.mdat        非图片附件同样加密
```

`.dat` 目录名规则与前端 `assetAnchorInfo` 一致：单文件取自身名，库内文件取所属库名。解锁后这些密文解密到应用缓存 `<app_cache_dir>/mlk-assets/<hash(库id)>/<.dat 目录名>/<逻辑名>`，预览拿缓存目录当相对链接基准，缓存内布局与磁盘 `<名>.dat/<逻辑名>` 镜像，因此正文里的 `note.dat/截图.png` 直接命中。

### 密钥链路

```
主密码 + salt ──Argon2id──▶ KEK（32B，Zeroize 保证 drop 时清零）
                                │  AES-256-GCM
                                ▼
                        wrapped_dek ──▶ DEK（32B，随机）
                                │  AES-256-GCM
                                ▼
                         文件树 / Markdown 正文（密文）
```

- **KDF**：Argon2id，`m=64MiB / t=3 / p=4`。内存硬参数让 GPU/ASIC 暴力破解的成本极高，代价是每次解锁约 300ms 内可接受。
- **AEAD**：AES-256-GCM。认证加密，密文被外部改写或截断会直接拒绝打开，而不是解出乱码。
- **KEK 与 DEK 分离**：正文由随机 DEK 加密，DEK 由 KEK 包裹。**改主密码只需重新包裹 DEK（`rewrap`），不重加密正文**，所以大库改密是瞬时的。
- **附件与正文同锁**：每个 `.mdat` 的小 DEK 同样由会话 KEK 包裹，改主密码时必须一起重包裹——只换正文那把钥匙的话，历史图片会永久解不开，而且失败很安静（只是图不显示）。目录库靠遍历库内文件重包裹，单文件 / 单文件库靠 `asset::rewrap_assets` 定位同级 `.dat`。
- **内存清零**：KEK/DEK 都实现 `Zeroize`，锁定或会话过期时主动抹掉，不依赖 GC。

### 读写流程

- **解锁**：主密码 + salt → Argon2id → KEK → 解开 DEK → 解密整棵文件树到内存。个人笔记体量下全程 < 300ms。
- **写入**：解锁后改任一文件，整棵树重新序列化加密、整体重写 `.mdlb`。这个"整体重写"是单文件容器换来的简化——不需要增量索引和碎片管理，代价在个人体量下无感。
- **附件缓存**：打开页签、解锁完成、导出前都会调 `materialize_vault_assets` 把该库 `.dat` 里的 `.mdat` 解密到应用缓存（幂等：已缓存且不旧于源密文的条目跳过，所以粘贴一张图不会重解整库）；粘贴 / 拖入写入新附件后再刷一次缓存、才插链接，避免预览首渲裂图。单条读不了或解不开只跳过它自己并提示「N 个附件无法解密」，不让整库预览跟着崩；会话不存在（未解锁 / 已过期）整体返回 `NotUnlocked`，与「附件损坏」区分开。
- **iCloud「仅在线」文件**：macOS 上被云端化、本地没有字节的文件，读它会触发下载并瞬时返回 error 11。读写统一走 `crypto::io::read` / `write`，内部按 `UF_DATALESS` 预判、`NSFileManager` 主动召回、400ms 退避重试到 20 秒上限，并在召回前后发事件让前端显示带转圈的 loading；配套把六个读写命令挪进阻塞线程池（`spawn_crypto`），否则等待期会冻结 WebView、提示来不及渲染。
- **自动锁定**：`SessionStore` 每次读写刷新活动时间戳，超时（默认 5 分钟）由 `reap_expired` 清零对应密钥；窗口关闭时 `lock_all`。前端每 30 秒调一次 `reap_expired`，到期的库页签变占位。两处细节：**过期只在读取时报错、不就地删条目**（删除统一由 `reap_expired` 上报，否则前端拿不到 id、界面卡在「已解锁」假象）；**打字也算活动**——`markDirty` 会按库节流 15 秒调一次 `touch_session`，否则关掉自动保存后一直敲字会被误判闲置。此外回收轮询会先查 `session_ttl`，对剩余不足 90 秒的库先把脏页签落盘（此刻密钥仍可用），再交给后端回收，未保存内容不会随密钥一起消失。

### 安全防护

- **路径穿越**：库内路径拒绝绝对路径与 `..`，`rel` 只能在容器内寻址。
- **防篡改**：GCM tag 校验失败即拒绝解密。
- **失败次数限制**：连续错误密码后限制尝试。
- **剪贴板**：复制的解密内容 60 秒后自动清除；锁定时可一次性清除剪贴板。
- **休眠 / 锁屏锁定**（设置里的可开关项）：macOS 由 Rust 每秒轮询会话字典的 `CGSSessionScreenIsLocked`，在「未锁 → 已锁」时发 `marklock://screen-locked` 事件，是精确信号；mac 前端因此**不**采用「页面隐藏数秒即判定休眠」的启发式（切窗口、被遮挡、换 Space 都会让页面 `hidden`，会误锁），只保留阈值放宽到 30 秒的心跳兜底。Windows / Linux 无后端探测，维持 5 秒启发式。
- **附件明文只进应用缓存**：磁盘上的 `.dat` 一律是密文，解密出的明文只驻留 `<app_cache_dir>/mlk-assets`，随锁定 / 退出 / 自动锁定即时删除对应库的子目录；进程被强杀或崩溃来不及清，故启动时（此时必然还没有任何会话）整目录清空兜底。要说清边界：缓存目录不是加密区，能读该目录的环境（同用户下的其它进程）在解锁期间看得到明文图片——这是「预览需要文件路径」换来的代价，换来的是正文与磁盘永远只有密文。

## Rust 加密核心

`src-tauri/src/crypto/`：

| 模块 | 职责 |
|---|---|
| `mod.rs` | 子模块聚合导出 |
| `error.rs` | `CryptoError` 统一错误类型，`Serialize` 成前端可读消息 |
| `format.rs` | `.mdl` / `.mdlb` 格式常量、头/体结构与序列化边界 |
| `keys.rs` | KEK 派生、DEK 包裹 / 解包裹 / 重包裹 |
| `file.rs` | 单文件 `.mdl` 的序列化与反序列化 |
| `io.rs` | 原子写盘 + 滚动备份 + 旁路文件识别；iCloud「仅在线」文件的召回退避与前后通知 |
| `session.rs` | 解锁会话表 `SessionStore`、自动锁定计时、密钥清零 |
| `asset.rs` | 加密图片/附件：`.dat/<逻辑名>.mdat` 落盘、解锁解密到应用缓存、改密码重包裹、锁定清缓存 |
| `vault.rs` | 库级操作：创建 / 解锁 / 读写 / 增删改移 / 改密码 / 锁定 / 搜索 |

单元测试（`src-tauri/` 下 `cargo test --lib`）覆盖：密钥往返、错误密码、篡改检测、改密码重包裹、目录库全流程、库内文件夹、单文件库全流程（增删改移 + 改密码 + 磁盘单文件验证）、路径穿越防护、明文目录搜索、附件加解密往返与篡改、附件写入-解密-清缓存生命周期、改密码带动附件重包裹、坏附件只跳过自己不误伤其余、目录库含明文附件时仍能改密码。CI 的 release 流水线在打包前先跑这个测试。

## Tauri 命令清单

前端封装在 `src/lib/tauri.ts`。

**单文件库 `.mdlb`**

| 命令 | 说明 |
|---|---|
| `create_file_vault(path, password, hint?)` | 新建库 |
| `unlock_file_vault(vaultId, path, password, autoLockSecs?)` | 解锁，解密出整棵文件树 |
| `list_file_vault(vaultId, path)` | 列递归树（不含正文） |
| `read_file_vault(vaultId, path, rel)` / `write_file_vault(...)` | 读 / 写库内文件正文 |
| `create_in_file_vault(vaultId, path, rel, content)` | 新建文件 |
| `create_folder_in_file_vault(vaultId, path, rel)` | 新建文件夹 |
| `delete_in_file_vault(vaultId, path, rel)` | 删除文件 / 文件夹 |
| `rename_in_file_vault(vaultId, path, old, new)` | 重命名 / 移动 |
| `change_file_vault_password(vaultId, path, old, new)` | 改主密码（重包裹全库 DEK） |

**单文件 `.mdl`**

| 命令 | 说明 |
|---|---|
| `create_file(path, password, content, hint?)` | 新建加密文件 |
| `unlock(vaultId, path, password, autoLockSecs?)` | 解锁 |
| `read_file(vaultId, path)` / `write_file(vaultId, path, content)` | 读 / 写 |
| `change_password(vaultId, path, old, new)` | 改密码 |
| `lock(vaultId)` / `lock_all` | 锁定单个 / 全部 |
| `unlocked_count` / `reap_expired` | 状态查询 / 回收过期会话 |
| `touch_session(vaultId)` / `session_ttl(vaultId)` | 刷新会话活动时间 / 查距自动锁定剩余秒数（无会话返回 `null`，已过期返回 0） |

**加密附件（`.mdl` / `.mdlb`）**

| 命令 | 说明 |
|---|---|
| `write_encrypted_asset(vaultId, destDir, name, base64)` | 粘贴的图片/二进制 → 加密写 `<name>.mdat`，返回逻辑名 |
| `copy_encrypted_asset(vaultId, destDir, srcPath)` | 拖入的磁盘文件 → 加密复制进 `.dat`，返回逻辑名 |
| `materialize_vault_assets(vaultId, datDir, datDirName)` | 解密到应用缓存，返回 `{ cache_dir, failed }`（失败条数供前端提示） |

三个命令都在 `spawn_crypto` 阻塞线程池里跑；`lock` / `lock_all` / `quit_app` / `reap_expired` 顺带清除对应库的附件缓存。

**其它**

| 命令 | 说明 |
|---|---|
| `list_dir(dir)` | 列目录一层（工作目录视图） |
| `unlock_vault` / `create_vault` / `list_vault` / `create_in_vault` / `create_folder_in_vault` / `delete_in_vault` / `rename_in_vault` | 旧目录库模型，保留兼容 |
| `copy_file_to(src, dest)` | 覆盖式复制到指定路径（预览点二进制附件的「另存为」下载流） |
| `open_local_file(path)` | 交系统默认应用打开磁盘上已存在的普通文件（导出成功消息的「打开文件」）；只接受 `is_file` 的路径，与 `open_url` 的协议白名单是两套 |
| `openFileDialog` / `openDirDialog` / `saveFileDialog` / `saveFileVaultDialog` / `saveAttachmentDialog` | 走 `tauri-plugin-dialog` 的文件对话框 |

## 前端结构与状态

- **视图**：`src/views/` 四屏 + 便签窗口，路由 hash 模式（`#/unlock`、`#/editor`、`#/settings`、`#/dialogs`、`#/sticky`）。
- **状态**：`src/stores/vault.ts`（Pinia）统一管理已登记库列表、打开的页签、当前激活文件、收藏、最近、设置项，持久化到 localStorage（`marklock.recent` / `marklock.settings`）。**密钥与明文一律不进 store**，只存路径 / 名称 / 锁定态等元信息。
- **编辑器**：`src/components/CodeMirrorEditor.vue` 封装 CodeMirror 6，每个文件常驻实例以保留光标与滚动位置。
- **外部修改监控**：`src/lib/filewatch.ts` 是模块级单例，默认 2 秒轮询磁盘 mtime / 大小，并在窗口重新获得焦点时立刻检查。库内文件按整个容器监控，只重读已打开的那几个文件。
- **预览**：`src/lib/preview.ts` 是主编辑器与便携便签共用的 Markdown 渲染层，`src/components/MarkdownPreview.vue` 为对应组件。相对链接的基准目录由 `setAssetBaseDir` 写入，加密文件（`.mdl` / `.mdlb`）指向附件缓存根、其余指向磁盘上的锚点目录；该基准存 `globalThis` 而非模块作用域——marked 渲染器的闭包在模块初始化时就固化，dev 热重载会让新旧模块实例各持一份变量，旧闭包读到陈旧/空基准就表现为裂图 404。预览里点非文档类型的链接（zip、图片等）不按文本开页签，而是弹保存对话框走复制下载流。
- **i18n**：`src/locales/{zh,en}.ts`，zh 为原文，en 结构一一对应；仅收录用户可见文案。

## 目录结构

```
MarkLock/
├── package.json            # Vue/Vite/AntD/CodeMirror 依赖 + tauri 脚本
├── vite.config.ts          # 产物输出 dist，对齐 tauri frontendDist
├── src/                    # Vue 3 前端（Options API，未用 <script setup>）
│   ├── main.ts             # 挂载 Vue + AntD + 路由 + v-tip + 主题/平台检测
│   ├── router/index.ts
│   ├── styles/global.css   # CSS 变量主题，迁移自原型 base.css
│   ├── components/         # TitleBar / CodeMirrorEditor / MarkdownPreview / EncIcon / VaultTreeNode / WorkdirNode / AboutCard
│   ├── stores/vault.ts     # Pinia store
│   ├── lib/                # tauri.ts 桥接 / filewatch.ts / preview.ts / fonts.ts / sticky.ts / appinfo.ts / update.ts / aboutDialog.ts / links.ts / platform.ts / splash.ts / tooltip.ts
│   ├── locales/            # zh.ts（原文）/ en.ts
│   └── views/              # UnlockView / EditorView / SettingsView / DialogsView / StickyView
├── src-tauri/
│   ├── tauri.conf.json     # 正式发布配置
│   ├── tauri.dev.conf.json # 开发身份 com.marklock.dev，可与已装版双开
│   ├── tauri.win-offline.conf.json # WebView2 离线安装器覆盖配置
│   ├── capabilities/default.json
│   └── src/
│       ├── lib.rs          # 窗口/托盘/菜单栏/原生菜单/热角/便签 + crypto 命令注册
│       ├── crypto/         # 见上「Rust 加密核心」
│       ├── clipboard.rs    # 剪贴板读写与 60s 自动清除
│       ├── appinfo.rs      # 快速启动项：解析本地应用与图标
│       ├── appinfo_win.rs  # Windows 侧应用信息取数
│       └── assets.rs       # 资源与文件关联
├── scripts/                # gen-filetype-icons / gen-win-appicon / dev-vite.mjs
├── doc/                    # 本文档、ARCHITECTURE.md、ui-prototype/、screenshots/
└── .github/workflows/      # release.yml（三平台）/ release-win-offline.yml（离线包）
```

## 便携便签的实现

便签是一个**独立轻窗口**，与主窗口共享同一套加密会话与预览渲染：

- **窗口单例**：首次唤出时创建、之后复用（create-once），隐藏而非销毁，避免每次弹出都重建 WebView 带来的延迟与内存抖动。
- **圆角外观**：无边框 + 窗口透明（Rust `transparent(true)`，macOS 需 Cargo feature `macos-private-api` 与配置里的 `app.macOSPrivateApi` 同时开启）+ `.sticky` 自绘 12px 圆角和 1px 边框，`overflow: hidden` 把标题栏与正文一并裁成圆角；前端按 `main.ts` 写入的 `html[data-window="sticky"]` 标记把 `html/body/#app` 与启动屏底色透出，否则四角仍是方角。
- **拖边缩放**：右下角一块 16px 不可见热区，用 pointer capture + 每帧一次的 `setSize` 自行跟手（CSS 像素 × `scaleFactor` 换算成物理像素，下限与 Rust `min_inner_size` 的 300×220 对齐）。不走系统原生缩放：Windows 的原生 sizing 循环会在开始瞬间切一次焦点、触发 WebView `blur`，和「失焦即收起」冲突。相应地，收起前先问 `sticky_is_active`（Windows 直接比对前台窗口句柄），窗口还在前台就忽略这次 blur；macOS 不显示热区，无边框窗的边缘本就能拖。
- **唤出路径**：托盘 / 菜单栏图标的左、右键动作各自可在设置里指定（弹出菜单 / 显示主界面 / 弹出便签；默认 macOS 左键弹便签、其余平台左键显示主界面、右键弹菜单）——非「弹出菜单」的键由 Rust 关掉系统自动弹菜单并改收 Click 事件（Down），故 macOS 左键同样可用；两键都不绑菜单时设置页会给提醒，以及**屏幕热角**——Rust 侧轮询鼠标坐标，在指定角落连续停留超过设定时长后触发弹出；可选"必须按住修饰键"作为二次确认，避免路过角落误触。便签从触发的那个角落弹出；热角唤出不夺焦，靠「未交互即隐藏」兜底——弹出后鼠标不再移动、就在原地一点（点桌面 / 菜单栏 / 别的窗口），便签立即收起让路，因为这一击说明用户要的是光标下的东西而不是便签；光标落在便签内不算（那一击点在便签上，交给前端的 pointerdown 认领）。坐标或便签边界取不到时按「不算让路点击」处理——宁可这次不收，也不把点在便签上的一击误判成外面。
- **托盘菜单文案按平台分叉**：macOS 沿用系统惯例（「打开 MarkLock」「偏好设置」「退出 MarkLock」，两段分隔线）；Windows / Linux 的托盘菜单由系统绘制，字号、行高、左侧留白、通栏分隔线都改不动，能控的只有项数与字长，因此取短标签（「显示主界面」「设置」「退出」）且只在退出前留一条分隔线，避免菜单被拉得松散。文案随界面语言在 `build_tray_menu` 里按 `MENU_LANG` 生成，切换语言时重建。
- **内容源**：任意 `.md` 明文或 `.mdl` 加密文件；所在库锁定时便签内直接给密码框解锁。`.mdlb` 是多文件容器，暂不作为单个便签源。
- **读取失败要说清是哪种失败**：读不到正文不能一律当作「库未解锁」——明文笔记根本没有库可解，误显示密码框等于把人卡在一个按了没反应的表单上。故按原因分流：库未解锁走解锁表单，其余（IO 失败、库文件被移动）显示后端给出的原因加重试按钮；判别依据是后端错误消息里的固定中文文案，后端措辞变了这里要跟着改。
- **编辑预览一体**：编辑 / 分屏 / 预览三态复用主编辑器的 CodeMirror 与 `preview.ts`，保存走同一套 `write_file` / `write_file_vault` 命令，因此落盘一样是密文。相对链接的基准目录取便签文件**自己在磁盘上的目录**，不接主编辑器那套附件缓存，所以指向 `.mdl` 的便签里、以 `.mdat` 密文存储的图片当前不显示（明文库/普通文件的插图不受影响）。
- **快速启动**：便签底部可放本地应用快捷方式（仅图标 / 图标加文字）。macOS 从本地应用信息解析名称与图标，Windows 侧由 `appinfo_win.rs` 处理 `.lnk` / UWP 别名，故有独立的 `modName` 命名（Win 上 Command 对应 Win 键）。
- **开机自启动**：写 macOS LaunchAgent / Windows 当前用户启动项 / Linux XDG autostart，可选手否静默后台启动；若托盘图标被关闭则强制显示窗口，避免应用不可达。

## 开发与调试

### 环境要求

| | macOS | Windows |
|---|---|---|
| Rust | rustup(stable) | rustup-init.exe(stable, MSVC) |
| 编译器配套 | Xcode Command Line Tools | Visual Studio Build Tools，装「使用 C++ 的桌面开发」工作负载 |
| Node | 18+（实测 v22 可用） | 18+ |
| WebView | 系统自带 WKWebView | WebView2 Runtime（Win11 / 较新 Win10 已内置，老 Win10 需装 Evergreen 离线包） |

Windows 上 Rust 和 Build Tools 缺一不可：rustup 只给编译器，链接器 `link.exe` 来自 Build Tools。

**Windows 全新机器安装顺序**（装完每一项都重开终端）：

1. VS Build Tools — https://visualstudio.microsoft.com/zh-hans/downloads/ （「所有下载」→「Visual Studio 生成工具」），安装器里只勾**「使用 C++ 的桌面开发」**一个工作负载
2. Rust — https://rustup.rs/ 下载 `rustup-init.exe`，一路默认
3. Node — https://nodejs.org/ LTS 版
4. WebView2 Runtime — 老 Win10 从 https://developer.microsoft.com/microsoft-edge/webview2/ 下 Evergreen 独立安装包

验证：`where.exe cargo`、`where.exe link`、`node -v` 都有输出才算齐（PowerShell 里别用 `where`，那是 `Where-Object` 别名，会静默无输出）。

### 运行

```bash
npm ci             # 按 package-lock.json 安装（推荐，保证依赖版本一致）
npm run dev        # 仅 Vite dev server（http://localhost:1420）
npm run build      # 构建前端产物到 dist/
npm run dev:tauri  # 前端 + Tauri 壳一起跑（需 Rust 环境）
npm run dev:app    # 打包 debug 版「MarkLock Dev.app」并启动，自动拉起 Vite 热更新
                   # 改前端免重编，改 Rust 重跑约 5s
```

首次编译约 400 个 crate，mac 几分钟、Windows 十几分钟，属正常；之后增量编译是秒级。

macOS 27+ 不受理未打包裸二进制的菜单栏状态项，`dev:tauri` 下托盘图标不会出现（托盘逻辑本身正常执行），属系统限制非缺陷；验证托盘 / 菜单栏 / 便签相关功能请用 `npm run dev:app`。它走独立身份 `com.marklock.dev`（见 `src-tauri/tauri.dev.conf.json`），可与已安装版双开互不干扰，该身份仅供开发、不影响正式发布。`dev:app` 拉起的 Vite 会常驻后台，`dev:tauri` 自动复用它（见 `scripts/dev-vite.mjs`），两者切换不会撞 1420 端口。

测试与检查（`src-tauri/` 下）：

```bash
cargo test --lib   # 加密核心单元测试
cargo check        # 编译检查
```

### 打包与发布

- `.github/workflows/release.yml`：`v*` tag 或手动 dispatch 触发，先跑 `cargo test --lib`，再并行构建 macOS aarch64 / macOS x86_64 / Windows x64 安装包（`tauri-action`），产出**草稿** release。
- `.github/workflows/release-win-offline.yml`：仅手动触发，用 `tauri.win-offline.conf.json` 覆盖 `webviewInstallMode: offlineInstaller`，产物改名后 `gh release upload --clobber` 挂到对应 tag。默认包用 `downloadBootstrapper`，安装时联网下载 1.8MB 引导器，**下载失败会 `Abort` 整个 NSIS 安装**（`installer.nsi:562-571`），所以完全断网的机器必须用离线包。离线包实际约 209MB（微软 x64 独立运行本体积 212MB，Tauri 文档里"+around 127MB"的说法偏旧），Gitee 附件单文件限 100MB，因此只在 GitHub 提供。
- 版本号有**四处**：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock` 里 `[[package]] name = "marklock"` 那条。`tauri-action` 读 `tauri.conf.json`，那里漏改会静默打错 tag。

## 常见构建问题

**`failed to run 'cargo metadata' ... program not found`** — cargo 不在 PATH。`where.exe cargo` 或 `Get-Command cargo` 验证；无输出即未装 Rust，有输出就重开终端再试。

**`linker link.exe not found` 或链接报 `mainCRTStartup 无法解析`** — 前者是 cargo 没探测到 MSVC，后者是链接器架构与编译目标错配。统一解法：编译终端里先注入 MSVC 环境再编译。

```cmd
call "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
```

路径以本机实际安装为准（`dir /s /b "C:\Program Files (x86)\Microsoft Visual Studio\*vcvars64.bat"` 定位）。Windows 上每次新开终端都要注入一次；最省事是固定用开始菜单的「Developer Command Prompt for VS」当编译终端。macOS 无此要求。

**ARM64 Windows 虚拟机（Parallels / VMware on Apple Silicon）** — rustup 默认装 aarch64 宿主工具链，Build Tools 默认只勾 x64/x86 MSVC 工具，错配会报 `link.exe not found` 或 `mainCRTStartup 无法解析`。`--target` 救不了这个错配，必须把 Rust 宿主工具链整体切到 x86_64：

```bash
rustup toolchain install --force-non-host stable-x86_64-pc-windows-msvc
rustup default --force-non-host stable-x86_64-pc-windows-msvc
rustc -vV        # 确认 host: x86_64-pc-windows-msvc
```

两条命令都必须带 `--force-non-host`（ARM64 Windows 有 x64 模拟层，实际能跑）。

**`failed to run ...\WixTools314\light.exe`（打 MSI）** — WiX 3 是老 ANSI 工具，项目路径含中文或 `productName` 为中文会挂。绕过：`npm run build -- --bundles nsis` 只打 NSIS 安装器；或项目挪到纯 ASCII 路径 + `productName` 改 ASCII。此时裸 exe 其实已编译成功，在 `target/release/` 下可直接用。

**`error messaging the mach port for IMKCFRunLoopWakeUpReliable`（macOS 日志）** — 输入法框架的无害噪音，Safari / Electron 应用也有，忽略。

**项目目录 3G 多** — `src-tauri/target/` 是编译产物（debug 不优化且带调试符号），不是下载的文件，`cargo clean` 可回收。真正下载的 crate 源码在 `~/.cargo/registry`。

**从 mac 拷贝项目到 Windows 后报错** — 删掉拷来的 `src-tauri/target/` 和 `node_modules/`（`@tauri-apps/cli` 是平台相关原生二进制），重新 `npm install`。

**打包产物首次打开被拦** — 无 Apple Developer ID，安装包只做 ad-hoc 签名、未经公证，`spctl` 必然拒绝，属预期。旧构建提示「已损坏」是未做资源密封，`xattr -dr com.apple.quarantine /Applications/MarkLock.app` 即可。

## 界面原型

`doc/ui-prototype/` 是纯静态高保真原型（无外部依赖，双击即可看），也是 Vue 组件拆分的视觉基准：

```
doc/ui-prototype/
├── index.html            # 原型导航 + 内嵌预览（入口）
├── assets/base.css       # 共享基础样式（AntD 视觉基调）
└── screens/
    ├── unlock.html       # 01 启动与解锁
    ├── editor.html       # 02 主编辑界面（VSCode 式工作区）
    ├── settings.html     # 03 设置（安全与加密为核心页）
    └── dialogs.html      # 04 弹窗集合（新建库/解锁错误态/改主密码）
```

## 编码约定

- 前端组件一律 **Options API**，不使用 `<script setup>` 或 Composition API。
- 用户可见文案进词典：改 `src/locales/zh.ts`（原文）后同步 `en.ts`，结构一一对应；代码注释不入词典。
- 样式走 CSS 变量（`src/styles/global.css`），组件内避免硬编码颜色；主题与平台差异在 `main.ts` 检测后挂到根节点。
- 提交前在 `src-tauri/` 跑 `cargo test --lib`，确认加密核心无回归。
- 一批文件里「逐个换密钥再写盘」的变换一律**先算完所有新字节、再统一落盘**（`rewrap_assets` / `rewrite_dir_deks`）：边解析边写会在中途报错时留下半新半旧的库，那比直接失败严重得多。
- 可能长时间阻塞的磁盘读写命令走 `spawn_crypto`（阻塞线程池），不放在主线程同步执行——等待期间 WebView 会冻住，前端的 loading 提示来不及渲染。
- 较大改动先开 Issue 对齐方向。

## 技术栈一览

| 层 | 选型 | 理由 |
|---|---|---|
| 壳 | Tauri 2 | 体积小（对比 Electron）、系统 WebView、Rust 原生能力做加密 |
| 前端 | Vue 3 + Vite + TypeScript | Options API 组件，生态成熟 |
| 组件库 | Ant Design Vue | 桌面工具型 UI 匹配度高，视觉对齐其默认主题 |
| 编辑器 | CodeMirror 6 | Markdown 语法高亮 + 源码模式 + 搜索 / 折叠 / 自动补全 |
| 状态 | Pinia + localStorage | 只存元信息，密钥与明文不进 store |
| 加密 | Rust：`aes-gcm` / `argon2` / `zeroize` | 密钥操作全部在 Rust 侧 |
| macOS 系统集成 | `objc2` / `objc2-app-kit` / `objc2-foundation` | 便签与热角定位避开 Dock（`NSScreen.visibleFrame`）、iCloud「仅在线」文件主动召回（`NSFileManager`）；锁屏检测与光标/按键态走 CoreGraphics 外部声明，不引额外 crate。全部只在 macOS target 声明 |
