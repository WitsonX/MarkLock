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
- **导出产物是明文**：HTML / 单文件 HTML / PDF 都按用户在保存对话框里选的落点明文写盘，不受库加密保护——这是「发给别人也能看」的题中之义，但成品离开应用后就再无保密性。Windows 渲染 PDF 还要先把自包含 HTML 写进系统临时文件（Chromium 内核不允许顶层 `data:` 导航，而内联图片后的 URL 动辄数 MB），页面加载完成立刻删、命令收尾再兜底删一次，明文在临时目录里只存活一次导航的时间；macOS 走 `data:text/html` URL，全程不落盘。
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
| `launch_local(path, args)` | 快速启动本地应用 / 路径，`args` 为已拆分的启动参数数组：macOS 经 `open --args` 透传且带参时固定加 `-n` 另起进程，Windows 追加到程序参数后，Linux 的 `xdg-open` 无透传语义故忽略 |
| `export_pdf(html, target)` | 把前端拼好的自包含 HTML 交隐藏 WebView 静默渲染成 PDF 写入 `target`：macOS 用 `WKWebView createPDFWithConfiguration`（不分页，故先跑 JS 按 A4 页高切页、逐页截取再用 PDFKit 合并），Windows 用 WebView2 `PrintToPdfStream`（引擎自己分页），Linux 无对应 API 直接返回「当前平台不支持导出 PDF」。字节经通道回命令线程，120 秒超时兜底并按标签销毁隐藏窗口，成功 / 报错 / 超时三条路径都不留窗口 |
| `set_show_hide_shortcut(shortcut)` | 注册 / 更换 / 清除「显示 / 隐藏主界面」的系统级全局热键：参数是 Tauri accelerator 串（如 `CmdOrCtrl+Alt+H`），空串表示清除。先把 `SHOW_HIDE_ACCEL` 里记的旧值反注册再注册新值；注册失败（写法非法 / 已被系统占用）返回可读错误且**不改动后端已存值**，前端据此回滚。非桌面平台是 no-op 桩（`#[cfg(not(desktop))]`），命令保持注册以兼容 invoke 签名 |
| `openFileDialog` / `openDirDialog` / `saveFileDialog` / `saveFileVaultDialog` / `saveAttachmentDialog` / `saveHtmlDialog` / `savePdfDialog` | 走 `tauri-plugin-dialog` 的文件对话框 |

## 前端结构与状态

- **视图**：`src/views/` 四屏 + 便签窗口，路由 hash 模式（`#/unlock`、`#/editor`、`#/settings`、`#/dialogs`、`#/sticky`）。设置页各分区共用同一个 `.s-body` 滚动容器（`v-show` 切换），切换分区时把内容区滚回顶部，免得上一分区的滚动位置残留；搜索跳转的 `scrollIntoView` 在 `$nextTick` 里执行、晚于这个同步 watcher，不受影响。
- **状态**：`src/stores/vault.ts`（Pinia）统一管理已登记库列表、打开的页签、当前激活文件、收藏、最近、设置项，持久化到 localStorage（`marklock.recent` / `marklock.settings`）。**密钥与明文一律不进 store**，只存路径 / 名称 / 锁定态等元信息。
- **编辑器**：`src/components/CodeMirrorEditor.vue` 封装 CodeMirror 6，每个文件常驻实例以保留光标与滚动位置。
- **外部修改监控**：`src/lib/filewatch.ts` 是模块级单例，默认 2 秒轮询磁盘 mtime / 大小，并在窗口重新获得焦点时立刻检查。库内文件按整个容器监控，只重读已打开的那几个文件。
- **预览**：`src/lib/preview.ts` 是主编辑器与便携便签共用的 Markdown 渲染层，`src/components/MarkdownPreview.vue` 为对应组件。相对链接的基准目录由 `setAssetBaseDir` 写入，加密文件（`.mdl` / `.mdlb`）指向附件缓存根、其余指向磁盘上的锚点目录；该基准存 `globalThis` 而非模块作用域——marked 渲染器的闭包在模块初始化时就固化，dev 热重载会让新旧模块实例各持一份变量，旧闭包读到陈旧/空基准就表现为裂图 404。预览里点非文档类型的链接（zip、图片等）不按文本开页签，而是弹保存对话框走复制下载流。`MarkdownPreview.vue` 只负责「把父级给的那份 HTML 放进 DOM + 收拢点击交互」，自己不渲染：props 是 `html` / `source` / `editable`，`html` 由便签自己渲染并可在编辑期冻结（见「便携便签的实现」），`source` 仅用于只读态任务清单按序号回改源码。
- **从系统打开文件**：macOS 走 `RunEvent::Opened`（访达双击、拖到 Dock、「打开方式」、`open -a MarkLock 文件` 都汇到这一条），Windows 冷启动读 argv、二次启动由单实例插件把参数转给已运行实例；Linux 没有接入口。路径一律先进 Rust 侧的 `PENDING_OPEN_PATHS` 缓冲，再发 `marklock://open-paths`，前端用 `take_pending_open_paths` 一次取干——事件不缓冲，冷启动时先于 `listen()` 触发就丢了，所以缓冲必须放后端，取干也保证不重复建页签。监听分两处：`EditorView` 在位时直接取；不在编辑器页（设置 / 解锁）时由常驻的 `App.vue` 置 `pendingOpenPathsNavigate` 并跳回 `/editor`，`EditorView` 在 `onMounted` 末尾消费——那时 `_sessionRestore` 已赋值，能与冷启动恢复串行。
- **预览就地编辑**：预览视图（`currentMode === 'preview'`）把 `.md` 变成 `contenteditable`，改渲染结果再反解析回源码，由 `src/lib/wysiwyg.ts` 负责「DOM → Markdown」这一个方向；源码 → DOM 仍走 `preview.ts` 的 marked，不换渲染引擎，皮肤、代码主题、预览字体、加密附件解析与 HTML/PDF 导出全都保持不变。渲染层与序列化器共用一套标签约定（`<pre><code class="language-x">`、`input.task-cb`、`.codespan-wrap`、顶层块外包 `.mb`），**marked 的 renderer 一改，`wysiwyg.ts` 必须同步**。开关是 `settings.previewInlineEdit`（默认开），设置页「显示」和预览态工具栏右端的只读 ⇄ 编辑按钮（眼睛 / 铅笔图标）写的是同一个值，工具栏隐藏时则由预览区右上角的浮动按钮接手。
  - 保住光标的办法是冻住注入：模板里 `v-html` 绑的不是 `renderedPreview` 而是 `previewHtml` 数据字段，`renderedPreview` 的 watcher 在 `previewFrozen` 期间直接返回——Vue 只在绑定字符串与上次 `!==` 时才重设 `innerHTML`，而每次按键后的防抖回写会让 `content` 变、进而让 `renderedPreview` 变，不冻住就等于边打字边重建 DOM。解冻时机由 `previewSelfContent` 判定：内容变化若等于自己刚写回的那份就是内部回写，继续冻；否则（磁盘重载、外部改动、附件缓存刷新）一律解冻重注入，让源码赢，避免用户拿着旧 DOM 把外部修改覆盖掉。
  - 回写：`input` 事件 220ms 防抖 → `domToMarkdown` → `preserveTrailing`（保住文件原有的结尾换行习惯）→ `setContentFor` 记脏；保存、导出、关页签、自动保存、离开编辑器路由都先 `flushPreviewDom()` 补一次，避免读到未回写的旧源码。输入法合成期间不回写（`compositionstart/end`），粘贴统一降级成 `insertText` 纯文本，富文本 HTML 不会污染源码。
  - 浏览器差异集中在回车：进入编辑态（`previewEditable` 翻 true）时下发 `defaultParagraphSeparator=p`（WebKit 默认插 `<div>`，会被序列化器按原始 HTML 整块保留，源码里就多出一层壳）；`<pre>` 内的回车由我们自己拦成 `insertText('\n')`，且代码块必须走 `codeText()` 逐节点读取而不是 `textContent`——浏览器留下的是 `<br>` 或 `<div>`，`textContent` 读成零宽，两行代码会被并成一行，那是真丢内容。任务复选框与复制按钮带 `contenteditable="false"`，图片把原始路径另存 `data-src`（否则回写会把 `a.png` 写成 `asset://` 长地址）。任务项里的回车另由 `wysiwyg.ts` 的 `enterInTaskItem()` 接管：勾选框是 `<input>` 而不是列表样式，浏览器断出的新 `<li>` 不会自带，所以 `insertParagraph` 之后紧跟一次 `insertHTML` 补上勾选框（嵌套列表里也留在同一层），两步都是 execCommand、⌘Z 两步撤回原文；条目本身没有文字则不接管，保留「空条目回车退出列表」的惯例。主编辑器与便签共用这一个入口，两处 `onPreviewEditKeydown` 只在 `<pre>` 分支之后接一句 `if (enterInTaskItem(e, root)) this.onPreviewEditInput()`。
  - 「编辑态不给外框」这条设计要靠一行 `outline: none` 才成立：`.md` 一旦可编辑又拿到焦点，WKWebView 会按 UA 样式画一圈**系统蓝**焦点环（与皮肤主色无关，紫皮下仍是蓝的），而预览满宽时这圈的左右两条恰好被滚动容器裁掉、顶条在视口之上，只剩正文底部那一条露出来——文档末尾凭空多出一条约 3px 的蓝线，看着像渲染坏了。`global.css` 的 `.md.wysiwyg-on:focus { outline: none }` 收掉它，只作用于编辑态，插入符颜色与选中色都不动。
  - 工具栏在预览态同样出现（`showToolbar` 一处判断，不再按模式排除），格式化入口 `fmtHeading/fmtWrap/fmtPrefix/fmtCodeBlock/fmtTable/fmtHr/fmtLink` 先分叉：`previewEditable` 为真走 `applyPvw → applyPreviewFormat`（`wysiwyg.ts` 的命令层），否则走原来的 CodeMirror API，两套共用同一批参数（`**`、`> `、`- [ ] `）。命令层的原则是**只调 `document.execCommand`，不手工改 DOM**——只有命令会被浏览器记进原生撤销栈，⌘Z 才能逐步撤回；同时先 `styleWithCSS=false`，让 Blink 产出 `<b>/<i>/<s>` 而不是 `<span style=…>` 壳子（`<span>` 只在浏览器兜底时才出现，序列化器额外认 `text-decoration:line-through`、`font-style:italic`、`font-weight≥600`）。只读态整排按钮 `.toolbar.fmt-off` 置灰且 `pointer-events:none`，只留右端切换按钮可用；工具栏容器 `@mousedown.prevent`，否则点按钮会把焦点从 contenteditable 抢走、选区随之丢失。链接/图片走弹窗（先 `cloneRange()` 存住选区，确认后在 `$nextTick` 里还原再落命令）。
  - 「能不能就地编辑」拆成计算属性 `pvCanEdit`（纯预览 + 有明文），`previewEditable` 只是它再与 `settings.previewInlineEdit` 相与；由此 `pvFloatShow = pvCanEdit && !showToolbar`——工具栏隐藏时那颗切换按钮没有落点，改挂到预览区右上角的半透明浮标：`.pv-float-row` 是 `.pane-preview` 的第一个子元素，`position:sticky; top:10px; height:0` 让它贴着滚动容器顶部且完全不占正文位置，行 `pointer-events:none`、按钮再恢复吃点击，默认 `opacity:0`、`.pane-preview:hover` 时 .55、自身 hover 时 1。这颗浮标用的 `v-tip` 是 `place:'left'`，顺带把 `lib/tooltip.ts` 的 left 方位修对了：原先照抄 right 的算式，只算了目标左缘、没先量气泡宽度，等于把气泡的**左缘**贴在图标左侧、整块盖住被提示的控件；现在先读 `offsetWidth/offsetHeight` 再定位（`x = r.left - tw - 10`），12px 的箭头盒尖在盒中心，锚点也相应从 `r.left - 6` 退到 `r.left - 12`。left 方位此前没有任何调用点，所以这个错位一直没暴露。
  - 命令层的四个 Blink 坑都在 `wysiwyg.ts` 里就地解决：`Selection` 上没有 `commonAncestorContainer`（那是 `Range` 的属性，选区判断读错就表现为所有命令落到文档末尾）；`root.focus()` 会清掉刚设好的程序选区，须先 `cloneRange` 存档、focus 后比对首尾容器与偏移再决定是否还原；`insertHTML` 插入「文字与 href 相同」的 `<a>` 会被当成手打的网址链接，把相邻文字一起吞进去并改写 href，所以空选区先用占位文字建锚点再 `selectNodeContents`，`text === url` 时改用 `insertText` + `createLink`；`insertUnorderedList` 在段落开头可能留下 `<p><ul>…</ul></p>`、`formatBlock(blockquote)` 可能留下没有内层 `<p>` 的引用，序列化器对这两种形状都做了拆包（`blockMd` 的 `P` 分支发现块级子节点就转交 `blocksMd`，`blocksMd` 遍历 `childNodes` 而非 `children`，否则会丢掉块之间的裸文本节点——那是真丢内容）。
  - 有损是这套做法的固有代价：Setext 标题、`__下划线粗体__`、表格定界行的空格数、列表用 `-` 还是 `*` 会被规整成一种写法，渲染结果不变。验证在浏览器里跑三套：源码往返 37 条 + contenteditable 真实 DOM 16 条（`rt.html`）、格式化命令 26 条（`fmt.html`）、真实按键与工具栏入口 23 条（`live.html`，含 ⌘B 原生加粗、⌘Z 逐步撤回、`<pre>` 内回车、输入法合成、只读态不吃命令）。断言的是幂等——「回写的源码再渲染一次、再序列化仍得到同一份」——而不是字节或 DOM 相同。
- **导出**：页签右键、侧边栏与工作目录的节点右键、「更多」菜单、macOS 主菜单里，「导出 HTML」/「导出 HTML（单文件）」/「导出 PDF」三项收进一个「导出」子菜单（自定义右键菜单的 `kind:'sub'` 项靠 CSS `:hover` 展开；子菜单宽 224px，靠近右边界时由 `subLeft` 翻到左侧）。三个入口共用 `exportRenderedDoc(f, { inlineAll, forPdf })`（一进来先 `flushPreviewDom()`，把预览里尚未防抖回写的就地编辑落到源码）：正文走预览同一套 `renderMarkdown`（渲前临时把相对链接基准指向被导出的那个文件，非激活页签也能导对图），再去掉离线页里没用的代码块复制按钮、给任务复选框补 `disabled` 保持只读观感。常规 HTML 把本地资源复制进 `<导出名>.dat/` 并改写相对链接（单个复制失败时图片退回内联 data URL，其余原样保留）；单文件 HTML 与 PDF 一律全部内联成 data URL，非图片附件的链接补 `download` 属性保住原文件名——PDF 的隐藏 WebView 没有 base URL，不自包含就什么都渲染不出来。PDF 版式按平台给不同 px 使 pt 相等（mac 595/11.25、其余 793/15），`@page { margin: 0 }` 把留白交给 `.md` 内边距，图片 / 代码块 / 表格 `break-inside: avoid`。本页签或所属库锁定时没有明文可渲染，子菜单整体置灰；Linux 无静默渲染 API，四处菜单都不给「导出 PDF」这一项（`lib/platform.ts` 的 `isLinux`）。
- **全局显示/隐藏快捷键**：设置页快捷键分区新增录制框（`SettingsView.vue` 的 `startRecordShortcut` / `onRecordKey` / `applyShortcut`），按下组合后按 `KeyboardEvent.code`（不是 `key`，避免大小写与输入法干扰）拼成 Tauri accelerator 串（`CmdOrCtrl`/`Alt`/`Shift` + 键名，F1–F24 允许无修饰键，其余必须有修饰键），存进 `settings.showHideWindowShortcut` 并调用 `set_show_hide_shortcut` 注册；注册失败（已被系统占用等）**回滚到旧值再重新注册旧值**，避免界面显示一个没生效的快捷键。启动时 `App.vue` 在非便签窗分支同步一次；用户改值只由设置页下发，两处不重复注册。录制用 window 捕获阶段的 `keydown`（早于编辑器与页面自身处理），组件卸载时反注册监听。
- **同名页签的目录副标题**：`EditorView.vue` 的计算属性 `tabSubtitles` 由 `listSubtitles` 按 `fileDisplayName` 分组，只给**撞名**的页签生成副标题，逐级向上取最近的目录段直到在该组内唯一，仍不唯一则退到完整相对目录。目录基准 `tabRelDir` 按库类型分流：单文件库取 `#` 后的库内相对路径、目录库相对 `vaultId`、工作目录内明文相对 `workdirRoot`、其余取绝对路径的 dirname；新建草稿与已锁定库文件返回空串（不泄露真实路径，与页签 tooltip 的锁定态口径一致）。渲染为顶栏 `.tab-sub` 与侧栏 `.dir-sub` 两处小字。
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
│   ├── lib/                # tauri.ts 桥接 / filewatch.ts / preview.ts / wysiwyg.ts / fonts.ts / sticky.ts / appinfo.ts / update.ts / aboutDialog.ts / links.ts / platform.ts / splash.ts / tooltip.ts / launchArgs.ts
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
- **唤出路径**：托盘 / 菜单栏图标的左、右键动作各自可在设置里指定（弹出菜单 / 显示主界面 / 弹出便签；默认 macOS 左键弹便签、其余平台左键显示主界面、右键弹菜单）——非「弹出菜单」的键由 Rust 关掉系统自动弹菜单并改收 Click 事件（Down），故 macOS 左键同样可用；绑「弹出便签」的键是**开关**——便签窗当前可见（只判 `is_visible()`）时再点改为收起：先 `emit` 一条 `marklock://sticky-hide`（前端监听里做静默落盘），随即 `hide()`，与托盘菜单里隐藏便签那条同语义；不可见时才走原来的就近摆到图标旁。两键都不绑菜单时设置页会给提醒，以及**屏幕热角**——Rust 侧轮询鼠标坐标，在指定角落连续停留超过设定时长后触发弹出；可选"必须按住修饰键"作为二次确认，避免路过角落误触。便签从触发的那个角落弹出；热角唤出不夺焦，靠「未交互即隐藏」兜底——弹出后鼠标不再移动、就在原地一点（点桌面 / 菜单栏 / 别的窗口），便签立即收起让路，因为这一击说明用户要的是光标下的东西而不是便签；光标落在便签内不算（那一击点在便签上，交给前端的 pointerdown 认领）。坐标或便签边界取不到时按「不算让路点击」处理——宁可这次不收，也不把点在便签上的一击误判成外面。
- **托盘菜单文案按平台分叉**：macOS 沿用系统惯例（「打开 MarkLock」「偏好设置」「退出 MarkLock」，两段分隔线）；Windows / Linux 的托盘菜单由系统绘制，字号、行高、左侧留白、通栏分隔线都改不动，能控的只有项数与字长，因此取短标签（「显示主界面」「设置」「退出」）且只在退出前留一条分隔线，避免菜单被拉得松散。文案随界面语言在 `build_tray_menu` 里按 `MENU_LANG` 生成，切换语言时重建。
- **内容源**：任意 `.md` 明文或 `.mdl` 加密文件；所在库锁定时便签内直接给密码框解锁。`.mdlb` 是多文件容器，暂不作为单个便签源。
- **读取失败要说清是哪种失败**：读不到正文不能一律当作「库未解锁」——明文笔记根本没有库可解，误显示密码框等于把人卡在一个按了没反应的表单上。故按原因分流：库未解锁走解锁表单，其余（IO 失败、库文件被移动）显示后端给出的原因加重试按钮；判别依据是后端错误消息里的固定中文文案，后端措辞变了这里要跟着改。
- **编辑预览一体**：编辑 / 分屏 / 预览三态复用主编辑器的 CodeMirror 与 `preview.ts`，保存走同一套 `write_file` / `write_file_vault` 命令，因此落盘一样是密文。相对链接的基准目录取便签文件**自己在磁盘上的目录**，不接主编辑器那套附件缓存，所以指向 `.mdl` 的便签里、以 `.mdat` 密文存储的图片当前不显示（明文库/普通文件的插图不受影响）。
- **顶部浮层只留两颗：只读 ⇄ 编辑 + 模式切换段**：`.sticky-float` 一行靠右（`justify-content:flex-end`、`gap:6px`、容器 `pointer-events:none`，两颗控件各自恢复吃点击），浮现规则仍是 `focused && overBody`，平时 `.35` 透明、指针压到任一控件转 1——`:hover` 沿 DOM 祖先链匹配，容器不吃事件并不妨碍父层拿到子级的 hover，所以整层只需一条 `:hover`。只留「编辑 / 分屏 / 预览」段和它左边那颗眼睛 / 铅笔（`v-if="pvCanEdit"`，即只在纯预览态出现）。**整条格式化工具栏撤掉了**：400 逻辑点装不下 15 个按钮 + 3 根分隔线（约 437px），而便签里要改格式直接进编辑态写标记就行，重复一排水印按钮只换来一条折行的工具栏。摆法试过三种——同一行左右挤（英文切换段实测 150px > 预留的 136px，直接压住按钮）、上下两行（工具栏自己折成两行）、最后只留一颗（实测行宽 164px 中文 / 182px 英文，300px 最小窗宽也放得下，`overlap:-6`）。随工具栏一并删掉的是 `fmtHeading` / `fmtWrap` / `fmtPrefix` / `fmtCodeBlock` / `fmtTable` / `fmtHr` / `fmtLink`、`cmApi`、编辑态的链接 / 图片弹窗与 `applyPvw`（主编辑器那套原样保留），以及「自动换行」那颗开关——便签的 `wordWrap` 目前只剩默认值（开），窗内暂无入口。
- **便签预览就地编辑**：与主编辑器同一套语义（改渲染结果 → `wysiwyg.ts` 反解析回源码 → 记脏 → 同一个 `write_file` / `write_file_vault` 落盘），差别在接线位置与开关归属：
  - **`MarkdownPreview.vue` 的 HTML 改由父级注入**：props 从 `source` 自己渲染变成 `html`（父级的 `previewHtml` 数据字段）+ `source`（仅供只读态的任务清单按序号回改源码定位）+ `editable`；`input` / `keydown` / `paste` / `compositionstart` / `compositionend` 一律上抛给父级，冻结与防抖都在父级做。之所以要组件配合，是因为 `v-html` 绑的必须是**冻结过的那一份**——编辑期间 Vue 不能重设 `innerHTML`，否则每次防抖回写引起源码变化、进而重渲染，光标就没了。主编辑器是内联的 `.md` div，便签这一份在共享组件里，故契约改动只影响便签（该组件目前仅 `StickyView` 使用）。
  - **开关是便签自己的会话状态 `pvEdit`，不是全局 `settings.previewInlineEdit`**：两个 webview 各有各的 Pinia 实例却共用同一个 localStorage 键，谁写谁就把对面刚写的覆盖掉；先例是便签的 `wordWrap` 同样只存本地。设置页那颗总开关只管主编辑器，便签要的是一直能当场切。
  - **便签的预览 DOM 是裸块级子元素**（`preview.ts` 整篇 `marked.parse`，没有主编辑器按块包的 `.mb`、也没有 `data-l` 行锚点），序列化器按「顶层是若干块元素」写，两种形态实测往返逐字一致；命令层 `applyPreviewFormat` 便签已不再调用（工具栏撤了），但同一批用例仍在两种 DOM 形态上各跑一遍比对，保证主编辑器那条路径不被便签改动带偏。`preserveTrailing` 保住结尾换行时用的基准是便签自己的 `content`。
  - 冻结/解冻、220ms 防抖回写、粘贴降级纯文本、输入法合成期间不回写、`<pre>` 内回车插裸换行、进入编辑态下发 `defaultParagraphSeparator=p` 全部照搬主编辑器；`save()`（收起 / Esc / ⌘S / 锁定 / 在主界面中打开都走它）开头先 `flushPreviewDom()`，双击切换在编辑态让位给双击选词。
- **在主界面中打开这一篇**：标题栏齿轮前那颗图标。便签是独立 webview，改不了主窗路由，所以路径是：先 `save(true)` 静默落盘（主窗从磁盘读，不落盘就读到旧内容，两边还会互相覆盖）→ `showMainWindow()` 唤回主窗 → `emitTo('main', 'marklock://sticky-open-note', target)` 把 `StickyNoteTarget` 原样递过去（`core:default` 已含 `core:event:default`，里面有 `allow-emit-to`，无需新增权限或 Rust 命令）。主窗侧照抄「从系统打开文件」那套两处监听：`EditorView` 在位时直接接，不在编辑器页（设置 / 解锁 / 对话框）时由常驻 `App.vue` 存 `store.pendingStickyNote` 并 `router.push('/editor')`，`EditorView.onMounted` 末尾消费。开页分派**照抄 `restoreSession` 的口径**：`plain` → `openPlainFile`；`mdl` 且 `vaultId === path`（单文件 .mdl）→ `openEncryptedFile`；`mdl` 且 `vaultId` 是目录（目录库内加密文件）→ 已解锁走 `openFile(vaultId, path)`；`mdlb` → 已解锁走 `openFileVaultFile(vaultId, rel)`。库未解锁或已不在最近列表时，把**库路径**（`vaultId || path`，不是文件路径）交给 `openDroppedPaths`，由它登记库并跳解锁页——比自建一套「解锁后再续开」的暂存链路更省，代价是解锁回来后要点第二下。`unmounted()` 里与其余监听一起反注册。
- **快速启动**：便签底部可放本地应用快捷方式（仅图标 / 图标加文字），项前的把手可拖拽排序（与侧边栏分组/页签同一纯鼠标事件写法；指针落在目标行的上半/下半决定插到该行之前/之后，高亮线跟着画顶边/底边，落点与线一致。`store.reorderQuickLaunch(from, to)` 的 `to` 语义是「插到原第 `to` 行之前」，先摘出 `from` 会让它后面的行前移一格，故插入点随之减一；数组顺序即便签底部的展示顺序）。macOS 从本地应用信息解析名称与图标，Windows 侧由 `appinfo_win.rs` 处理 `.lnk` / UWP 别名，故有独立的 `modName` 命名（Win 上 Command 对应 Win 键）。每项可另存「启动参数」原始串（空格分隔、引号段视为整体，`lib/launchArgs.ts` 拆分），`launch_local` 收到参数数组后 macOS 走 `open <路径> --args …`、Windows 追加到程序后；参数逐个传给 `Command` 不经 shell，无注入面。因 `open` 对已运行应用只激活、argv 不送达，macOS 带参时固定附 `open -n` 另起进程（无开关，对新实例行为用户无感）。
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
- 快捷键芯片（`.kbd`）与设置页的录制框用系统 UI 字体（`-apple-system` / `Segoe UI` 栈），不用 `--mono`：等宽字体会把 ⇧、⌥ 这类修饰键符号画得又小又靠下，看着像缺字。
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
| 导出 PDF | macOS：`objc2-web-kit` / `objc2-pdf-kit` / `block2`；Windows：`webview2-com` / `windows` | 用系统 WebView 自带的打印接口在隐藏窗口里静默生成（WKWebView `createPDFWithConfiguration` 逐页截取后由 PDFKit 合并；WebView2 `PrintToPdfStream` 直接出流），不引第三方 PDF 引擎、也不需要 wkhtmltopdf；依赖分别只在对应 target 声明 |
| 全局快捷键 | `tauri-plugin-global-shortcut` 2（底层 `global-hotkey`） | 「显示 / 隐藏主界面」要在主窗隐藏或失焦时也能唤起，应用内快捷键做不到，只能挂系统级钩子。macOS 走 Carbon `RegisterEventHotKey`，Windows 走 `RegisterHotKey`，Linux 走 x11rb（仅 X11 会话）；accelerator 由前端按设置动态注册 / 反注册，不预占任何组合 |
