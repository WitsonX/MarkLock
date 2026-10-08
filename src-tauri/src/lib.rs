//! MarkLock — 可加密的 Markdown 查看编辑器（Tauri 壳 + Rust 加密核心）。
//!
//! 相对旧 OA 壳的变化：
//! - 窗口加载本地 `dist/` 前端（不再加载远程 OA）
//! - 新增 `crypto` 模块并注册为 Tauri 命令，前端通过 `invoke` 调用

mod crypto;
mod assets;
#[cfg(target_os = "macos")]
mod clipboard;

use crypto::session::SessionStore;
use crypto::vault;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::MacosLauncher;

/// macOS 锁屏检测。
///
/// 前端的「心跳间隔 + 页面可见性」启发式只能覆盖**系统休眠**（定时器被挂起）；
/// 单纯锁屏（显示器锁定但系统仍在运行）时 webview 不会 hidden、定时器照常跑，
/// 前端无从感知。这里轮询 CoreGraphics 会话字典中的 `CGSSessionScreenIsLocked`：
/// 锁屏时该键出现且为 true，解锁后消失（on-console 键在纯锁屏时不变，故不能用它判断）。
#[cfg(target_os = "macos")]
mod screenlock {
    use std::ffi::{c_char, c_void};

    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// 复制当前会话字典（调用方拥有，需 `CFRelease`）。
        fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFStringRef) -> *const c_void;
        fn CFBooleanGetValue(boolean: *const c_void) -> u8;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    /// `kCFStringEncodingUTF8` 的值（CFString 编码常量）。
    const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// 会话字典中「屏幕是否已锁定」的键名：锁屏时该键出现且为 true，解锁后消失。
    /// 实测纯锁屏不会改动 `kCGSSessionOnConsoleKey`（on-console 只反映快速用户切换），
    /// 锁屏由本键标记；键名为 `CGSSessionScreenIsLocked`（无 `k` 前缀）。
    const SCREEN_LOCKED_KEY: &[u8] = b"CGSSessionScreenIsLocked\0";

    /// 屏幕当前是否已锁定。无法探测（字典缺失）时返回 `false`（视为未锁，避免误触发）。
    pub fn is_screen_locked() -> bool {
        unsafe {
            let dict = CGSessionCopyCurrentDictionary();
            if dict.is_null() {
                return false;
            }
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                SCREEN_LOCKED_KEY.as_ptr() as *const c_char,
                K_CFSTRING_ENCODING_UTF8,
            );
            let locked = if key.is_null() {
                false
            } else {
                let val = CFDictionaryGetValue(dict, key);
                // 未锁屏时系统不写该键 → val 为 null → 未锁；存在则取其布尔值。
                let r = if val.is_null() {
                    false
                } else {
                    CFBooleanGetValue(val) != 0
                };
                CFRelease(key);
                r
            };
            CFRelease(dict);
            locked
        }
    }
}

/// 系统文件类型关联：探测/设为「以 MarkLock 作为 .md / .mdl / .mdlb 默认打开程序」。
///
/// macOS：LaunchServices 以 UTI 查询扩展名默认处理器并与本 app 包路径比对；
/// 「设为默认」先尝试 LSSetDefaultRoleHandlerForContentType 一键设置并回读校验，
/// 若被系统的用户选择保护拦截则回退到「访达 → 显示简介 → 打开方式 → 全部更改」手动指引。
/// Windows：直接写 HKCU\Software\Classes 注册表完成关联（普通用户权限即可，不弹系统设置、
/// 不触碰受哈希保护的 UserChoice）；`reg query` 读 UserChoice/默认 ProgId 回读校验。
/// Linux：不探测（前端收到 false 且不支持引导时静默跳过，不打扰用户）。
#[cfg(target_os = "macos")]
mod fileassoc {
    use std::ffi::{c_char, c_void};

    type CFStringRef = *const c_void;
    type CFURLRef = *const c_void;

    // CoreServices 伞框架同时包含 CoreFoundation / LaunchServices / UTType 符号；
    // 直接挂 LaunchServices 在部分 SDK 下会找不到独立 framework（链接失败）。
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        /// 标签类常量：以文件扩展名（filename extension）作为标签来源。
        static kUTTagClassFilenameExtension: CFStringRef;
        /// 由扩展名派生 UTI（未知扩展名会得到一个稳定的 `dyn.*` 动态 UTI）。
        fn UTTypeCreatePreferredIdentifierForTag(
            tag_class: CFStringRef,
            tag: CFStringRef,
            conforming: CFStringRef,
        ) -> CFStringRef;
        /// 某 UTI 在指定角色下的默认应用 URL（Call-Get 规则，需 CFRelease）。
        fn LSCopyDefaultApplicationURLForContentType(
            uti: CFStringRef,
            role_mask: u32,
            out_err: *mut *const c_void,
        ) -> CFURLRef;
        /// 将某 bundle id 设为该 UTI 的默认处理程序（返回 OSStatus，0 = noErr）。
        /// 自 10.15 起废弃，但对 App 自己声明的文档类型通常仍生效；失败时回退手动指引。
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role_mask: u32,
            handler_id: CFStringRef,
        ) -> i32;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFStringGetCStringPtr(cf: CFStringRef, encoding: u32) -> *const c_char;
        fn CFStringGetLength(cf: CFStringRef) -> isize;
        fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
        fn CFStringGetCString(cf: CFStringRef, buf: *mut c_char, buf_len: isize, encoding: u32) -> u8;
        fn CFURLCopyFileSystemPath(url: CFURLRef, style: u32) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    const ENC: u32 = 0x0800_0100; // kCFStringEncodingUTF8
    const POSIX_STYLE: u32 = 0; // kCFURLPOSIXPathStyle
    const LS_ROLES_ALL: u32 = 0xFFFF_FFFF; // kLSRolesAll

    /// CFStringRef → Rust String（先取快速指针，为空再拷入缓冲）。
    unsafe fn cfstring_to_string(s: CFStringRef) -> Option<String> {
        if s.is_null() {
            return None;
        }
        let ptr = CFStringGetCStringPtr(s, ENC);
        if !ptr.is_null() {
            return Some(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned());
        }
        let len = CFStringGetLength(s);
        let cap = CFStringGetMaximumSizeForEncoding(len, ENC).max(0) as usize + 1;
        let mut buf: Vec<u8> = vec![0; cap];
        if CFStringGetCString(s, buf.as_mut_ptr() as *mut c_char, cap as isize, ENC) != 0 {
            let cstr = std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char);
            Some(cstr.to_string_lossy().into_owned())
        } else {
            None
        }
    }

    /// 本 app 包路径（…/MarkLock.app）：dev 模式下从 `target/debug` 裸二进制启动时无 .app 祖先，
    /// 返回 None → 探测判为「非默认」，属预期行为（安装包正式注册后才准确）。
    fn own_bundle_canon() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let bundle = exe.ancestors().find(|a| a.extension().map(|e| e == "app").unwrap_or(false))?;
        Some(bundle.canonicalize().unwrap_or_else(|_| bundle.to_path_buf()))
    }

    /// 查一个扩展名当前默认打开程序的绝对路径。
    unsafe fn default_app_for_ext(ext: &str) -> Option<std::path::PathBuf> {
        let ce = format!("{ext}\0");
        let tag = CFStringCreateWithCString(std::ptr::null(), ce.as_ptr() as *const c_char, ENC);
        if tag.is_null() {
            return None;
        }
        let uti = UTTypeCreatePreferredIdentifierForTag(kUTTagClassFilenameExtension, tag, std::ptr::null());
        CFRelease(tag);
        if uti.is_null() {
            return None;
        }
        let app = LSCopyDefaultApplicationURLForContentType(uti, LS_ROLES_ALL, std::ptr::null_mut());
        CFRelease(uti);
        if app.is_null() {
            return None;
        }
        let path_str = CFURLCopyFileSystemPath(app, POSIX_STYLE);
        let s = cfstring_to_string(path_str);
        if !path_str.is_null() {
            CFRelease(path_str);
        }
        CFRelease(app);
        let p = std::path::Path::new(&s?).canonicalize().ok()?;
        Some(p)
    }

    /// `.md` / `.markdown` / `.mdl` / `.mdlb` 的默认打开程序是否均为本应用（任一未归属即返回 false）。
    /// 扩展名集合须与 try_set_default 的注册列表保持一致。
    pub fn owns_extensions() -> bool {
        let Some(own) = own_bundle_canon() else { return false };
        unsafe {
            ["md", "markdown", "mdl", "mdlb"].into_iter().all(|ext| {
                default_app_for_ext(ext).map(|p| p == own).unwrap_or(false)
            })
        }
    }

    /// 尝试将本应用（按 bundle id）设为某扩展名的默认打开程序；返回 OSStatus == noErr。
    unsafe fn set_default_for_ext(ext: &str, bundle_id: &str) -> bool {
        let ce = format!("{ext}\0");
        let tag = CFStringCreateWithCString(std::ptr::null(), ce.as_ptr() as *const c_char, ENC);
        if tag.is_null() {
            return false;
        }
        let uti = UTTypeCreatePreferredIdentifierForTag(kUTTagClassFilenameExtension, tag, std::ptr::null());
        CFRelease(tag);
        if uti.is_null() {
            return false;
        }
        let be = format!("{bundle_id}\0");
        let bid = CFStringCreateWithCString(std::ptr::null(), be.as_ptr() as *const c_char, ENC);
        let status = LSSetDefaultRoleHandlerForContentType(uti, LS_ROLES_ALL, bid);
        if !bid.is_null() {
            CFRelease(bid);
        }
        CFRelease(uti);
        status == 0
    }

    /// 尝试把 .md / .markdown / .mdl / .mdlb 的默认程序设为本应用，随后回读校验。
    /// （LaunchServices 可能因「用户选择保护」而忽略设置，故以实际探测结果为准。）
    pub fn try_set_default(bundle_id: &str) -> bool {
        unsafe {
            for ext in ["md", "markdown", "mdl", "mdlb"] {
                let _ = set_default_for_ext(ext, bundle_id);
            }
        }
        owns_extensions()
    }
}

const WINDOW_TITLE: &str = "MarkLock";
const MAIN_WINDOW: &str = "main";

/// 全局会话存储（进程生命周期内唯一）。
fn store() -> &'static SessionStore {
    static STORE: OnceLock<SessionStore> = OnceLock::new();
    STORE.get_or_init(SessionStore::new)
}

/// 退出是否已经前端确认。为 true 时放行 ExitRequested，避免 quit_app 再次被拦截。
/// 初始为 false：首次退出请求（如 macOS Cmd+Q）会被 prevent_exit，转为前端未保存确认弹窗。
static EXIT_CONFIRMED: AtomicBool = AtomicBool::new(false);

/// 冷启动「打开文件」缓冲：macOS `RunEvent::Opened` 可能在前端 `listen()` 注册之前就触发，
/// Tauri 事件不缓冲会丢失。这里把路径暂存，前端启动后通过 `take_pending_open_paths` 主动拉取；
/// 应用已运行时的二次打开则靠信号事件触发同一拉取逻辑（drain 保证不重复）。
static PENDING_OPEN_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());

// ==================== Tauri 命令（前端 invoke 的入口） ====================

/// 把 CPU 密集的加密操作（Argon2id 派生约 1s）放到阻塞线程池执行。
/// 同步命令在主线程运行会冻结 WebView 渲染，导致前端「解锁中」的禁用态无法绘制。
async fn spawn_crypto<T>(
    f: impl FnOnce() -> Result<T, crypto::CryptoError> + Send + 'static,
) -> Result<T, crypto::CryptoError>
where
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| crypto::CryptoError::Io(format!("后台任务失败: {e}")))?
}

/// 把前端传入的自动锁定秒数转成会话超时：`None` 用默认 5 分钟；
/// `Some(0)`（设置选「不自动锁定」）须映射为「永不过期」，
/// 否则 `expired()` 的 `>= Duration::ZERO` 恒真，会话解锁后立即被回收。
fn auto_lock_from_secs(secs: Option<u64>) -> std::time::Duration {
    match secs {
        Some(0) => std::time::Duration::MAX,
        Some(n) => std::time::Duration::from_secs(n),
        None => crypto::session::DEFAULT_AUTO_LOCK,
    }
}

/// 解锁一个加密文件（`.mdl`）。成功后登记会话，后续读写、自动锁定生效。
#[tauri::command]
async fn unlock(
    vault_id: String,
    path: String,
    password: String,
    auto_lock_secs: Option<u64>,
) -> Result<(), crypto::CryptoError> {
    spawn_crypto(move || {
        let auto_lock = Some(auto_lock_from_secs(auto_lock_secs));
        vault::unlock_file(store(), &vault_id, Path::new(&path), &password, auto_lock)
    })
    .await
}

/// 解锁一个单文件库（`.mdlb`）。解密出全库文件树，登记会话。
#[tauri::command]
async fn unlock_file_vault(
    vault_id: String,
    path: String,
    password: String,
    auto_lock_secs: Option<u64>,
) -> Result<(), crypto::CryptoError> {
    spawn_crypto(move || {
        let auto_lock = Some(auto_lock_from_secs(auto_lock_secs));
        vault::unlock_file_vault(store(), &vault_id, Path::new(&path), &password, auto_lock)
    })
    .await
}

/// 创建一个新的单文件加密库（`.mdlb`），落盘到 `path`。
#[tauri::command]
async fn create_file_vault(
    path: String,
    password: String,
    hint: Option<String>,
) -> Result<usize, crypto::CryptoError> {
    spawn_crypto(move || {
        let bytes = vault::create_file_vault(&password, hint)?;
        std::fs::write(Path::new(&path), &bytes)
            .map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
        Ok(bytes.len())
    })
    .await
}

/// 列出已解锁的单文件库目录树（递归，不含内容）。
#[tauri::command]
fn list_file_vault(
    vault_id: String,
    path: String,
) -> Result<Vec<crypto::vault::FsNode>, crypto::CryptoError> {
    vault::list_file_vault_tree(store(), &vault_id, Path::new(&path))
}

/// 读取单文件库内一个文件的正文。
#[tauri::command]
fn read_file_vault(
    vault_id: String,
    path: String,
    rel: String,
) -> Result<String, crypto::CryptoError> {
    vault::read_file_vault(store(), &vault_id, Path::new(&path), &rel)
}

/// 写回单文件库内一个文件的正文。
#[tauri::command]
fn write_file_vault(
    vault_id: String,
    path: String,
    rel: String,
    content: String,
) -> Result<(), crypto::CryptoError> {
    vault::write_file_vault(store(), &vault_id, Path::new(&path), &rel, &content)
}

/// 在单文件库内创建文件。
#[tauri::command]
fn create_in_file_vault(
    vault_id: String,
    path: String,
    rel_path: String,
    content: String,
) -> Result<(), crypto::CryptoError> {
    vault::create_file_in_file_vault(store(), &vault_id, Path::new(&path), &rel_path, &content)
}

/// 在单文件库内创建文件夹。
#[tauri::command]
fn create_folder_in_file_vault(
    vault_id: String,
    path: String,
    rel_path: String,
) -> Result<(), crypto::CryptoError> {
    vault::create_folder_in_file_vault(store(), &vault_id, Path::new(&path), &rel_path)
}

/// 删除单文件库内的文件或目录。
#[tauri::command]
fn delete_in_file_vault(
    vault_id: String,
    path: String,
    rel_path: String,
) -> Result<(), crypto::CryptoError> {
    vault::delete_in_file_vault(store(), &vault_id, Path::new(&path), &rel_path)
}

/// 重命名 / 移动单文件库内的文件或目录。
#[tauri::command]
fn rename_in_file_vault(
    vault_id: String,
    path: String,
    old_rel: String,
    new_rel: String,
) -> Result<(), crypto::CryptoError> {
    vault::rename_in_file_vault(store(), &vault_id, Path::new(&path), &old_rel, &new_rel)
}

/// 修改单文件库主密码。
#[tauri::command]
async fn change_file_vault_password(
    vault_id: String,
    path: String,
    old_password: String,
    new_password: String,
) -> Result<(), crypto::CryptoError> {
    spawn_crypto(move || {
        vault::change_file_vault_password(
            store(),
            &vault_id,
            Path::new(&path),
            &old_password,
            &new_password,
        )
    })
    .await
}

/// 解锁一个加密库目录（`.mdlb` 目录 / 含 vault.json 的目录）。
#[tauri::command]
async fn unlock_vault(
    vault_id: String,
    dir: String,
    password: String,
    auto_lock_secs: Option<u64>,
) -> Result<(), crypto::CryptoError> {
    spawn_crypto(move || {
        let auto_lock = Some(auto_lock_from_secs(auto_lock_secs));
        vault::unlock_vault_dir(store(), &vault_id, Path::new(&dir), &password, auto_lock)
    })
    .await
}

/// 创建一个新的加密库目录（生成 vault.json）。
#[tauri::command]
async fn create_vault(
    dir: String,
    password: String,
    hint: Option<String>,
) -> Result<(), crypto::CryptoError> {
    spawn_crypto(move || vault::create_vault_dir(Path::new(&dir), &password, hint)).await
}

/// 列出目录内容（一层，用于工作目录视图）。
#[tauri::command]
fn list_dir(dir: String) -> Result<Vec<crypto::vault::FsNode>, crypto::CryptoError> {
    vault::list_dir(Path::new(&dir))
}

/// 列出已解锁库的目录树（递归，用于"打开的库"视图）。
#[tauri::command]
fn list_vault(
    vault_id: String,
    dir: String,
) -> Result<Vec<crypto::vault::FsNode>, crypto::CryptoError> {
    vault::list_vault_tree(store(), &vault_id, Path::new(&dir))
}

/// 在已解锁库内创建新文件（相对库根路径）。
#[tauri::command]
fn create_in_vault(
    vault_id: String,
    rel_path: String,
    content: String,
) -> Result<(), crypto::CryptoError> {
    vault::create_file_in_vault(store(), &vault_id, &rel_path, &content)
}

/// 在已解锁库内创建空文件夹（相对库根路径）。
#[tauri::command]
fn create_folder_in_vault(
    vault_id: String,
    rel_path: String,
) -> Result<(), crypto::CryptoError> {
    vault::create_folder_in_vault(store(), &vault_id, &rel_path)
}

/// 删除库内的文件或目录（相对库根路径）。
#[tauri::command]
fn delete_in_vault(
    vault_id: String,
    rel_path: String,
) -> Result<(), crypto::CryptoError> {
    vault::delete_in_vault(store(), &vault_id, &rel_path)
}

/// 重命名 / 移动库内的文件或目录（相对库根路径）。
#[tauri::command]
fn rename_in_vault(
    vault_id: String,
    old_rel: String,
    new_rel: String,
) -> Result<(), crypto::CryptoError> {
    vault::rename_in_vault(store(), &vault_id, &old_rel, &new_rel)
}

/// 锁定单个库（清零内存密钥）。
#[tauri::command]
fn lock(vault_id: String) {
    vault::lock_vault(store(), &vault_id);
}

/// 锁定全部库。
#[tauri::command]
fn lock_all() {
    vault::lock_all_vaults(store());
}

/// 退出应用：先锁定全部库清零内存密钥，再退出进程。
/// 前端在用户确认「退出」后调用（关窗事件已改为交由前端弹窗确认）。
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    vault::lock_all_vaults(store());
    // 标记已确认，放行随后的 ExitRequested（否则会被 run 回调再次 prevent_exit）。
    EXIT_CONFIRMED.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// macOS 原生菜单需动态回写的句柄：可勾选项（id → CheckMenuItem）+ 库/工作目录切换项（按状态改标题）。
#[cfg(target_os = "macos")]
struct MenuDyn {
    checks: std::collections::HashMap<String, tauri::menu::CheckMenuItem<tauri::Wry>>,
    vault_close: tauri::menu::MenuItem<tauri::Wry>,
    workdir_toggle: tauri::menu::MenuItem<tauri::Wry>,
}

/// 前端同步 macOS 原生菜单状态：视图勾选态 + 库/工作目录开关项的标题（是否有已打开库/工作目录）。
/// 非 macOS 无原生菜单，直接空实现（命令仍注册，保证跨平台编译一致）。
#[tauri::command]
fn sync_native_menu(
    app: tauri::AppHandle,
    checks: std::collections::HashMap<String, bool>,
    has_vaults: bool,
    has_workdir: bool,
) {
    #[cfg(target_os = "macos")]
    {
        if let Some(state) = app.try_state::<MenuDyn>() {
            for (id, checked) in &checks {
                if let Some(item) = state.checks.get(id) {
                    let _ = item.set_checked(*checked);
                }
            }
            let _ = state.vault_close.set_enabled(has_vaults);
            let _ = state
                .workdir_toggle
                .set_text(if has_workdir { "关闭工作目录" } else { "打开工作目录" });
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, checks, has_vaults, has_workdir);
    }
}

/// 读取并解密一个已解锁文件的正文。
#[tauri::command]
fn read_file(vault_id: String, path: String) -> Result<String, crypto::CryptoError> {
    vault::read_file(store(), &vault_id, Path::new(&path))
}

/// 加密并写回一个已解锁文件。
#[tauri::command]
fn write_file(vault_id: String, path: String, content: String) -> Result<(), crypto::CryptoError> {
    vault::write_file(store(), &vault_id, Path::new(&path), &content)
}

/// 创建一个新的加密文件，落盘到 `path`（返回写入字节数）。
#[tauri::command]
async fn create_file(
    path: String,
    password: String,
    content: String,
    hint: Option<String>,
) -> Result<usize, crypto::CryptoError> {
    spawn_crypto(move || {
        let bytes = vault::create_encrypted(&password, &content, hint)?;
        std::fs::write(Path::new(&path), &bytes)
            .map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
        Ok(bytes.len())
    })
    .await
}

/// 修改主密码（重包裹 DEK，无需全文重加密）。
#[tauri::command]
async fn change_password(
    vault_id: String,
    path: String,
    old_password: String,
    new_password: String,
) -> Result<(), crypto::CryptoError> {
    spawn_crypto(move || {
        vault::change_password(
            store(),
            &vault_id,
            Path::new(&path),
            &old_password,
            &new_password,
        )
    })
    .await
}

/// 读取一个普通（未加密）文件的明文正文。
#[tauri::command]
fn read_plain_file(path: String) -> Result<String, crypto::CryptoError> {
    let bytes = std::fs::read(Path::new(&path)).map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
    String::from_utf8(bytes).map_err(|_| crypto::CryptoError::BadHeader("文件不是合法 UTF-8".into()))
}

/// 明文写回一个普通（未加密）文件。
#[tauri::command]
fn write_plain_file(path: String, content: String) -> Result<(), crypto::CryptoError> {
    std::fs::write(Path::new(&path), content.as_bytes())
        .map_err(|e| crypto::CryptoError::Io(e.to_string()))
}

/// 在工作目录（普通未加密目录）中创建一个新文件；已存在则失败（不覆盖）。
/// `path` 为目标文件绝对路径，`content` 为初始正文。
#[tauri::command]
fn create_plain_file(path: String, content: String) -> Result<(), crypto::CryptoError> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let p = Path::new(&path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
    }
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .map_err(|e| crypto::CryptoError::Io(format!("创建文件失败: {e}")))?;
    f.write_all(content.as_bytes())
        .map_err(|e| crypto::CryptoError::Io(e.to_string()))
}

/// 在工作目录中创建一个新文件夹（递归建父目录，已存在则失败）。
#[tauri::command]
fn create_plain_dir(path: String) -> Result<(), crypto::CryptoError> {
    std::fs::create_dir(Path::new(&path)).map_err(|e| crypto::CryptoError::Io(e.to_string()))
}

/// 重命名 / 移动一个普通文件路径（明文，目标已存在则失败）。
#[tauri::command]
fn rename_path(from: String, to: String) -> Result<(), crypto::CryptoError> {
    if Path::new(&to).exists() {
        return Err(crypto::CryptoError::Io("目标名称已存在".into()));
    }
    std::fs::rename(&from, &to).map_err(|e| crypto::CryptoError::Io(e.to_string()))
}

/// 删除一个普通文件 / 文件夹（明文）：移入系统回收站，可从回收站恢复。
///
/// 仅服务于工作目录的明文节点（库内文件走 delete_in_vault）。macOS 用 Finder 的
/// AppleScript、Windows 用 PowerShell + VisualBasic 的 SendToRecycleBin、Linux 用
/// `gio trash`；命令失败（目标不存在、无桌面环境等）时上抛 CryptoError::Io，
/// 绝不做永久删除兜底，避免误删不可恢复。
#[tauri::command]
fn delete_path(path: String) -> Result<(), crypto::CryptoError> {
    let p = Path::new(&path);
    // 先确认目标存在（不存在时提前给出明确错误）；Windows 需据此区分文件/目录选不同 API。
    let md = std::fs::symlink_metadata(p).map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
    #[allow(unused_variables)]
    let is_dir = md.is_dir();

    #[cfg(target_os = "macos")]
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("tell application \"Finder\" to delete (POSIX file (item 1 of argv) as alias)")
        .arg("-e")
        .arg("end run")
        .arg("--")
        .arg(&path)
        .output();
    #[cfg(target_os = "windows")]
    let out = {
        // PowerShell 单引号字符串内以两个单引号转义，避免路径含引号时命令注入/语法错误。
        let escaped = path.replace('\'', "''");
        let verb = if is_dir { "DeleteDirectory" } else { "DeleteFile" };
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Add-Type -AssemblyName Microsoft.VisualBasic; [Microsoft.VisualBasic.FileIO.FileSystem]::{}('{}','OnlyErrorDialogs','SendToRecycleBin')",
                    verb, escaped
                ),
            ])
            .output()
    };
    #[cfg(target_os = "linux")]
    let out = std::process::Command::new("gio").arg("trash").arg(&path).output();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let out: std::io::Result<std::process::Output> = Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "当前平台不支持移入回收站",
    ));

    let out = out.map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(crypto::CryptoError::Io(if err.is_empty() {
            "移入回收站失败".to_string()
        } else {
            err
        }));
    }
    Ok(())
}

/// 在系统文件管理器中定位（显示）一个文件。
///
/// macOS：`open -R`；Windows：`explorer /select,`；Linux：对父目录 `xdg-open`。
#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), crypto::CryptoError> {
    // `p` 仅 macOS / Linux 分支使用（Windows 直接用 `path` 拼 `/select,`），避免未使用告警。
    #[cfg(not(target_os = "windows"))]
    let p = Path::new(&path);
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open").arg("-R").arg(p).spawn();
    #[cfg(target_os = "windows")]
    let res = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path))
        .spawn();
    #[cfg(target_os = "linux")]
    let res = std::process::Command::new("xdg-open")
        .arg(p.parent().unwrap_or(p))
        .spawn();
    res.map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
    Ok(())
}

/// 在系统默认浏览器中打开一个 URL（用于 Markdown 预览区链接点击）。
///
/// 只允许 `http/https/mailto` 三种常见协议，其它协议（如 `file://`、`javascript:`、
/// 自定义 scheme）一律拒绝，避免把任意协议交给系统 shell 派发。macOS 用 `open`、
/// Windows 用 `rundll32 url.dll,FileProtocolHandler`（不经 cmd 的 `start`，绕开引号/
/// 特殊字符歧义）、Linux 用 `xdg-open`；spawn 失败复用 `CryptoError::Io` 上抛。
#[tauri::command]
fn open_url(url: String) -> Result<(), crypto::CryptoError> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:"))
    {
        return Err(crypto::CryptoError::Unsupported(format!("不支持打开的链接协议: {url}")));
    }
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let res = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", &url])
        .spawn();
    #[cfg(target_os = "linux")]
    let res = std::process::Command::new("xdg-open").arg(&url).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let res: std::io::Result<std::process::Child> = Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "当前平台不支持打开外部链接",
    ));
    res.map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
    Ok(())
}

/// Windows：查一个扩展名当前生效的关联 ProgId。
///
/// 先读 HKCU FileExts 的 UserChoice（用户显式选择，受系统哈希保护）；不存在时回退
/// `HKCR\.ext` 的默认 ProgId（安装包/本应用自注册写入的关联）。reg.exe 查询键不存在时
/// 返回码非 0 但进程正常退出，须以 status 而非 spawn 结果判断是否回退。
#[cfg(target_os = "windows")]
fn windows_assoc_progId(ext: &str) -> Option<String> {
    let read = |args: &[&str]| -> Option<String> {
        let output = std::process::Command::new("reg").args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // reg.exe 中文系统输出 GBK，但 ProgId 本身是 ASCII，按行提取末尾字段即可。
        for line in text.lines() {
            let line = line.trim();
            // reg query 的数据类型列恒为大写 REG_SZ / REG_BINARY / REG_NONE，
            // 这里做大小写不敏感匹配，避免遗漏数据行。
            if line.to_ascii_uppercase().contains("REG_") {
                if let Some(v) = line.rsplit(|c: char| c.is_whitespace()).next() {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
        None
    };
    // UserChoice 是 FileExts\.ext 下的子键，真正生效的处理程序记录在其 ProgId 值里
    // （不是 .ext 键上名为 UserChoice 的值），故查询子键的 /v ProgId。
    let file_exts = format!("HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\{ext}");
    read(&["query", &format!("{file_exts}\\UserChoice"), "/v", "ProgId"])
        .or_else(|| read(&["query", &format!("HKCR\\{ext}"), "/ve"]))
}

/// Windows：`.md` / `.markdown` / `.mdl` / `.mdlb` 的默认打开程序是否均为本应用。
#[cfg(target_os = "windows")]
fn windows_owns_extensions() -> bool {
    // NSIS 安装包与本应用自注册均登记 `MarkLock.<ext>` 形式的 ProgId，按前缀匹配更稳。
    // 扩展名集合须与 windows_register_associations 的注册列表保持一致。
    [".md", ".markdown", ".mdl", ".mdlb"]
        .into_iter()
        .map(|e| {
            windows_assoc_progId(e)
                .map(|p| p.to_ascii_lowercase().starts_with("marklock."))
                .unwrap_or(false)
        })
        .all(|ok| ok)
}

/// Windows：解析某扩展名对应的文件类型图标（打包进 resources 的 .ico）绝对路径。
/// `.markdown` 复用 `md.ico`；dev 模式或资源缺失时返回 None，调用方回退到 exe 图标。
#[cfg(target_os = "windows")]
fn windows_icon_for_ext(app: &tauri::AppHandle, ext: &str) -> Option<String> {
    let stem = if ext == "markdown" { "md" } else { ext };
    let rel = format!("icons/filetypes/{stem}.ico");
    let path = app
        .path()
        .resolve(&rel, tauri::path::BaseDirectory::Resource)
        .ok()?;
    if path.exists() {
        Some(path.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// Windows：把 .md / .markdown / .mdl / .mdlb 直接关联到本 exe。
///
/// 写 HKCU\Software\Classes（当前用户视图，无需管理员权限），不触碰受哈希保护的
/// UserChoice、不弹系统设置。每扩展名登记 4 项：扩展名默认 ProgId、打开命令、图标、
/// 「打开方式」候选（OpenWithProgids）。若扩展名已有 UserChoice 锁定其它程序，注册表
/// 写入仍会成功但双击仍走 UserChoice——由调用方回读校验后决定是否提示手动步骤。
#[cfg(target_os = "windows")]
fn windows_register_associations(app: &tauri::AppHandle, exe_path: &str) -> bool {
    const EXTS: [&str; 4] = ["md", "markdown", "mdl", "mdlb"];
    let open_cmd = format!("\"{exe_path}\" \"%1\"");
    let exe_icon = format!("\"{exe_path}\",0");
    let mut all_ok = true;
    for ext in EXTS {
        // 优先用打包的专属 .ico（多尺寸，交给资源管理器按视图自动挑选最佳帧）；
        // 缺失（如 dev 模式）时回退到 exe 内嵌图标。
        let icon = windows_icon_for_ext(app, ext)
            .map(|p| format!("\"{p}\""))
            .unwrap_or_else(|| exe_icon.clone());
        let prog_id = format!("MarkLock.{ext}");
        let ext_key = format!("HKCU\\Software\\Classes\\.{ext}");
        let prog_key = format!("HKCU\\Software\\Classes\\{prog_id}");
        let writes: [Vec<String>; 4] = [
            vec!["add".into(), ext_key.clone(), "/ve".into(), "/t".into(), "REG_SZ".into(), "/d".into(), prog_id.clone(), "/f".into()],
            vec!["add".into(), format!("{prog_key}\\shell\\open\\command"), "/ve".into(), "/t".into(), "REG_SZ".into(), "/d".into(), open_cmd.clone(), "/f".into()],
            vec!["add".into(), format!("{prog_key}\\DefaultIcon"), "/ve".into(), "/t".into(), "REG_SZ".into(), "/d".into(), icon.clone(), "/f".into()],
            vec!["add".into(), format!("{ext_key}\\OpenWithProgids"), "/v".into(), prog_id.clone(), "/t".into(), "REG_NONE".into(), "/f".into()],
        ];
        for args in writes {
            let ok = std::process::Command::new("reg")
                .args(&args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                all_ok = false;
            }
        }
    }
    all_ok
}

/// Windows：通知资源管理器刷新文件关联缓存，让刚写入的注册表立即生效（免重启）。
#[cfg(target_os = "windows")]
fn windows_notify_shell_assoc_changed() {
    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(w_event_id: i32, u_flags: u32, dw_item1: *mut core::ffi::c_void, dw_item2: *mut core::ffi::c_void);
    }
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    const SHCNF_IDLIST: u32 = 0x0000;
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, core::ptr::null_mut(), core::ptr::null_mut());
    }
}

/// `.md` / `.mdl` / `.mdlb` 的默认打开程序是否为本应用。
///
/// 返回 false 也可能仅表示「无法探测」（如 dev 模式、Linux），前端应结合
/// `supports_default_app_guide` 判断是否弹窗询问。探测过程可能短暂阻塞（reg.exe），
/// 放到阻塞线程池执行，避免冻结主线程上的 WebView 渲染。
#[tauri::command]
async fn is_default_file_handler() -> Result<bool, crypto::CryptoError> {
    spawn_crypto(|| {
        #[cfg(target_os = "macos")]
        let owned = fileassoc::owns_extensions();
        #[cfg(target_os = "windows")]
        let owned = windows_owns_extensions();
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let owned = false;
        Ok(owned)
    })
    .await
}

/// 当前平台是否支持「引导设为默认打开程序」（macOS / Windows）。
#[tauri::command]
fn supports_default_app_guide() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// 尝试将 MarkLock 设为 .md / .mdl / .mdlb 的默认打开程序，返回处理结果标识：
/// - `"set"`：已成功设为默认并回读校验通过（macOS 一键设置 / Windows 直接写注册表）。
/// - `"manual"`：无法静默完成，需用户按指引手动操作（如系统已锁定其它程序为默认）。
#[tauri::command]
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
fn try_set_default_file_handler(app: tauri::AppHandle) -> Result<String, crypto::CryptoError> {
    #[cfg(target_os = "macos")]
    {
        // bundle id 与 tauri.conf.json 的 identifier 一致；LaunchServices 据此定位已注册的本应用。
        let owned = fileassoc::try_set_default("com.marklock.app");
        return Ok(if owned { "set".into() } else { "manual".into() });
    }
    #[cfg(target_os = "windows")]
    {
        // 直接写 HKCU\Software\Classes 关联注册表（普通用户权限即可，不弹系统设置）；
        // 已有 UserChoice 锁定其它程序时写入仍会成功但不会生效，以回读校验为准。
        let registered = std::env::current_exe()
            .map(|p| windows_register_associations(&app, &p.to_string_lossy()))
            .unwrap_or(false);
        if registered {
            windows_notify_shell_assoc_changed();
        }
        return Ok(if windows_owns_extensions() { "set".into() } else { "manual".into() });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok("manual".into())
    }
}

/// 磁盘文件戳记（外部修改监控用）：只取 `stat` 级别的两个字段，不读文件内容，
/// 因此可以高频轮询而不拖慢 UI（加密文件的内容变更判定交给上层在戳记变化时重读比对）。
#[derive(Debug, Clone, serde::Serialize)]
struct FileStamp {
    /// 最后修改时间（Unix 毫秒）；文件不存在时为 0
    mtime_ms: i64,
    /// 字节大小；文件不存在时为 0
    size: u64,
}

/// 批量查询文件戳记（外部修改监控）。一次 invoke 拿回全部打开页签的 stat，
/// 避免逐文件往返。入参可含重复路径（多个页签监控同一容器文件），内部去重。
/// 路径不存在（被删除 / 重命名）时返回全 0 戳记，由前端据此判定「文件已消失」。
#[tauri::command]
fn file_stamps(paths: Vec<String>) -> Vec<FileStamp> {
    fn stamp(path: &str) -> FileStamp {
        let meta = std::fs::metadata(Path::new(path)).ok();
        FileStamp {
            mtime_ms: meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        }
    }
    let mut cache: std::collections::HashMap<String, FileStamp> = std::collections::HashMap::new();
    paths
        .into_iter()
        .map(|p| {
            if let Some(s) = cache.get(&p) {
                return s.clone();
            }
            let s = stamp(&p);
            cache.insert(p, s.clone());
            s
        })
        .collect()
}

/// 取出并清空冷启动期间缓存的待打开文件路径（前端启动后调用一次，之后靠信号事件拉取）。
#[tauri::command]
fn take_pending_open_paths() -> Vec<String> {
    match PENDING_OPEN_PATHS.lock() {
        Ok(mut v) => v.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

/// 将一组路径追加到待打开缓存（仅保留真实存在的文件）。
#[cfg(target_os = "windows")]
fn buffer_open_paths(paths: impl IntoIterator<Item = String>) {
    if let Ok(mut v) = PENDING_OPEN_PATHS.lock() {
        for p in paths {
            if Path::new(&p).is_file() {
                v.push(p);
            }
        }
    }
}

/// Windows：从命令行参数提取文件路径（双击/右键「打开方式」会把文件作为参数传入，argv[0] 为 exe）。
#[cfg(target_os = "windows")]
fn argv_file_paths() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .filter_map(|a| a.into_string().ok())
        .collect()
}

/// 探测一个路径的类型（目录/文件、是否加密库、是否位于库内）。
///
/// 供拖入/系统打开时的路由：目录库→解锁；普通目录→挂工作目录；
/// `.mdl`→解锁单文件；库内文件→解锁所属库；其余→明文打开。
#[tauri::command]
fn inspect_path(path: String) -> Result<crypto::vault::PathInfo, crypto::CryptoError> {
    vault::inspect_path(Path::new(&path))
}

/// 路径是否仍存在（解锁页进入时校验登记库是否指向有效文件，供自动清理失效条目）。
#[tauri::command]
fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
}

/// 已解锁库数量。
#[tauri::command]
fn unlocked_count() -> usize {
    vault::unlocked_count(store())
}

/// 回收过期的会话（返回被自动锁定的库 id）。
#[tauri::command]
fn reap_expired() -> Vec<String> {
    vault::reap_expired(store())
}

/// 全局搜索：已解锁库（解密后匹配）+ 工作目录（明文匹配）。
#[tauri::command]
fn search(
    query: String,
    workdirs: Option<Vec<String>>,
) -> Result<Vec<crypto::vault::SearchHit>, crypto::CryptoError> {
    vault::search_all(store(), &query, &workdirs.unwrap_or_default())
}

/// 批量加密：对多个明文文件共用一次 Argon2id 派生，每个文件独立落盘为 `.mdl`。
/// `move_originals=true` 时把原文件移到同目录下的 `原文件/` 子目录。
#[tauri::command]
async fn batch_encrypt(
    paths: Vec<String>,
    password: String,
    move_originals: bool,
) -> Result<crypto::vault::BatchEncryptResult, crypto::CryptoError> {
    spawn_crypto(move || vault::batch_encrypt(&paths, &password, move_originals)).await
}

/// 把一个源目录（含子目录结构）内的 markdown/文本文件加密导入到已解锁的目标库。
/// 目标库可以是目录库或单文件库（`.mdlb`），由 vault_id 对应路径的类型自动区分。
#[tauri::command]
async fn import_dir_to_vault(
    vault_id: String,
    src_dir: String,
) -> Result<crypto::vault::ImportResult, crypto::CryptoError> {
    spawn_crypto(move || vault::import_dir_to_vault(store(), &vault_id, Path::new(&src_dir))).await
}

/// 读取系统剪贴板中的文件路径（图片/文件粘贴的原生兜底）。
///
/// WKWebView 从访达「拷贝文件」后 `clipboardData.files` 常拿不到磁盘路径，这里读
/// `public.file-url` 返回真实路径；非 macOS 平台返回空列表（前端据此走 Web 粘贴分支）。
#[tauri::command]
fn clipboard_file_paths() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        clipboard::clipboard_file_paths()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// 把一个源文件/目录复制进目标 `.dat` 目录（重名自动加序号），返回最终绝对路径。
///
/// 用于「拖入 → 复制到资源目录」。纯文件 IO，放阻塞线程池避免冻结 WebView。
#[tauri::command]
async fn copy_asset(dest_dir: String, src: String) -> Result<String, crypto::CryptoError> {
    spawn_crypto(move || assets::copy_into(&dest_dir, &src)).await
}

/// 把 base64 二进制内容写入目标 `.dat` 目录下的 `name`（重名自动加序号），返回最终绝对路径。
///
/// 用于「粘贴截图/剪贴板图片」：前端读出图片字节的 base64 交给这里落盘。
#[tauri::command]
async fn write_asset_bytes(
    dest_dir: String,
    name: String,
    base64: String,
) -> Result<String, crypto::CryptoError> {
    spawn_crypto(move || assets::write_bytes(&dest_dir, &name, &base64)).await
}

/// 读取本地文件为 data URL（`data:<mime>;base64,…`），供导出 HTML 内联 `.dat` 目录里的图片。
#[tauri::command]
async fn read_asset_data_url(path: String) -> Result<String, crypto::CryptoError> {
    spawn_crypto(move || assets::read_data_url(&path)).await
}

pub fn run() {
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();
    // Windows 单实例：双击文件二次启动时，把参数转发给已运行实例（缓存 + 信号事件让前端打开），
    // 避免开出第二个窗口。仅 Windows 启用（macOS 走 RunEvent::Opened，无需此插件）。
    #[cfg(target_os = "windows")]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            buffer_open_paths(args.into_iter().skip(1));
            if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
                let _ = win.emit("marklock://open-paths", ());
            }
        }));
    }
    builder
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            unlock,
            unlock_vault,
            unlock_file_vault,
            create_vault,
            create_file_vault,
            list_dir,
            list_vault,
            list_file_vault,
            create_in_vault,
            create_in_file_vault,
            create_folder_in_vault,
            create_folder_in_file_vault,
            delete_in_vault,
            delete_in_file_vault,
            rename_in_vault,
            rename_in_file_vault,
            lock,
            lock_all,
            quit_app,
            read_file,
            read_file_vault,
            write_file,
            write_file_vault,
            create_file,
            change_password,
            change_file_vault_password,
            read_plain_file,
            write_plain_file,
            create_plain_file,
            create_plain_dir,
            rename_path,
            delete_path,
            inspect_path,
            path_exists,
            reveal_in_finder,
            open_url,
            file_stamps,
            take_pending_open_paths,
            is_default_file_handler,
            supports_default_app_guide,
            try_set_default_file_handler,
            unlocked_count,
            reap_expired,
            search,
            batch_encrypt,
            import_dir_to_vault,
            clipboard_file_paths,
            copy_asset,
            write_asset_bytes,
            read_asset_data_url,
            sync_native_menu
        ])
        .setup(|app| {
            // Windows 冷启动：把命令行里的文件路径暂存，前端 mounted 后拉取打开。
            #[cfg(target_os = "windows")]
            buffer_open_paths(argv_file_paths());
            let handle = app.handle();

            // 加载本地前端（dist/index.html）
            let mut builder = tauri::WebviewWindowBuilder::new(
                handle,
                MAIN_WINDOW,
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title(WINDOW_TITLE)
            .inner_size(1280.0, 860.0)
            .min_inner_size(960.0, 640.0)
            .center();

            // macOS：隐藏原生标题栏，红绿灯按钮叠加在应用自绘工具条左上角
            #[cfg(target_os = "macos")]
            {
                builder = builder
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .hidden_title(true)
                    .traffic_light_position(tauri::LogicalPosition::new(14.0, 20.0));
            }

            builder.build()?;

            // macOS：安装自定义菜单接管 ⌘Q——避免系统默认 terminate 直接退出，
            // 改为通知前端弹「未保存确认」，确认后由 quit_app 退出。
            // 同时保留标准「编辑/窗口」子菜单，否则 ⌘C/⌘V 等剪贴板快捷键会失效。
            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItemBuilder, SubmenuBuilder};

                // App 菜单「MarkLock」：关于 + 偏好设置(⌘,) + 隐藏/显示全部 + 自定义退出(⌘Q)。
                // 退出仍用自定义 quit-app 项（不用预定义 quit，避免系统直接 terminate）。
                // 「关于」不用预定义 about（会弹系统 About 面板），改为自定义项路由到应用内「关于」页。
                let about = MenuItemBuilder::with_id("menu:about", "关于 MarkLock").build(handle)?;
                let prefs = MenuItemBuilder::with_id("menu:prefs", "偏好设置…")
                    .accelerator("Cmd+,")
                    .build(handle)?;
                let quit = MenuItemBuilder::with_id("quit-app", "退出 MarkLock")
                    .accelerator("Cmd+Q")
                    .build(handle)?;
                let app_submenu = SubmenuBuilder::new(handle, "MarkLock")
                    .item(&about)
                    .separator()
                    .item(&prefs)
                    .separator()
                    .hide_with_text("隐藏 MarkLock")
                    .separator()
                    .item(&quit)
                    .build()?;

                // 文件：整合应用内「主菜单」+ 库/工作目录开关（后两者按状态动态改标题，见 sync_native_menu）。
                // 带 ⌘ 的项用 MenuItemBuilder 显式声明 accelerator；macOS 上原生菜单在 AppKit 层拦截按键，
                // 不会与前端全局 keydown / CodeMirror keymap 双触发（前端处理器仍服务 Windows/Linux）。
                let new_file = MenuItemBuilder::with_id("menu:new-file", "新建文件").accelerator("Cmd+N").build(handle)?;
                let open_file = MenuItemBuilder::with_id("menu:open-file", "打开文件…").accelerator("Cmd+O").build(handle)?;
                let vault_open = MenuItemBuilder::with_id("menu:vault-open", "打开库").build(handle)?;
                let vault_close = MenuItemBuilder::with_id("menu:vault-close", "关闭所有库").enabled(false).build(handle)?;
                let workdir_toggle = MenuItemBuilder::with_id("menu:workdir-toggle", "打开工作目录").build(handle)?;
                let save = MenuItemBuilder::with_id("menu:save", "保存").accelerator("Cmd+S").build(handle)?;
                let close_tab = MenuItemBuilder::with_id("menu:close-tab", "关闭页签").accelerator("Cmd+W").build(handle)?;
                let file_submenu = SubmenuBuilder::new(handle, "文件")
                    .item(&new_file)
                    .text("menu:new-vault", "新建加密库")
                    .separator()
                    .item(&open_file)
                    .item(&vault_open)
                    .item(&vault_close)
                    .item(&workdir_toggle)
                    .separator()
                    .item(&save)
                    .item(&close_tab)
                    .separator()
                    .text("menu:batch-encrypt", "批量加密文件…")
                    .text("menu:import-dir", "导入目录到库…")
                    .text("menu:export-html", "导出 HTML…")
                    .build()?;

                // 编辑：保留系统剪贴板预定义项（⌘C/⌘V 等），否则覆盖默认菜单后这些快捷键失效。
                // 用 *_with_text 显式给出中文标题，避免预定义项按系统语言显示英文。
                let find = MenuItemBuilder::with_id("menu:find", "编辑器内搜索").accelerator("Cmd+F").build(handle)?;
                let find_global = MenuItemBuilder::with_id("menu:find-global", "全局搜索").accelerator("Cmd+Shift+F").build(handle)?;
                let edit_submenu = SubmenuBuilder::new(handle, "编辑")
                    .undo_with_text("撤销")
                    .redo_with_text("重做")
                    .separator()
                    .cut_with_text("剪切")
                    .copy_with_text("复制")
                    .paste_with_text("粘贴")
                    .select_all_with_text("全选")
                    .separator()
                    .item(&find)
                    .item(&find_global)
                    .build()?;

                let bold = MenuItemBuilder::with_id("menu:bold", "加粗").accelerator("Cmd+B").build(handle)?;
                let italic = MenuItemBuilder::with_id("menu:italic", "斜体").accelerator("Cmd+I").build(handle)?;
                let link = MenuItemBuilder::with_id("menu:link", "链接").accelerator("Cmd+K").build(handle)?;
                let format_submenu = SubmenuBuilder::new(handle, "格式")
                    .text("menu:h1", "标题 1")
                    .text("menu:h2", "标题 2")
                    .text("menu:h3", "标题 3")
                    .separator()
                    .item(&bold)
                    .item(&italic)
                    .text("menu:strike", "删除线")
                    .text("menu:code", "行内代码")
                    .text("menu:codeblock", "代码块")
                    .separator()
                    .text("menu:quote", "引用")
                    .text("menu:ul", "无序列表")
                    .text("menu:task", "任务列表")
                    .text("menu:table", "表格")
                    .separator()
                    .item(&link)
                    .text("menu:image", "图片")
                    .text("menu:hr", "分割线")
                    .build()?;

                // 视图：模式（单选勾选）+ 显示开关（勾选）用 CheckMenuItem，勾选态由前端 sync_native_menu 回写。
                let mk_check = |id: &str, text: &str| -> tauri::Result<CheckMenuItem<tauri::Wry>> {
                    CheckMenuItem::with_id(handle, id, text, true, false, None::<&str>)
                };
                let m_edit = mk_check("menu:mode-edit", "编辑模式")?;
                let m_split = mk_check("menu:mode-split", "分屏")?;
                let m_preview = mk_check("menu:mode-preview", "预览模式")?;
                let c_tabs = mk_check("menu:toggle-tabs", "顶部页签")?;
                let c_toolbar = mk_check("menu:toggle-toolbar", "格式工具栏")?;
                let c_statusbar = mk_check("menu:toggle-statusbar", "状态栏")?;
                let c_sidebar = CheckMenuItem::with_id(handle, "menu:toggle-sidebar", "显示侧边栏", true, false, Some("Cmd+J"))?;
                let c_openfiles = mk_check("menu:sec-open-files", "打开的文件")?;
                let c_vaults = mk_check("menu:sec-vaults", "加密库")?;
                let c_workdir = mk_check("menu:sec-workdir", "工作目录")?;
                let c_favorites = mk_check("menu:sec-favorites", "收藏")?;
                let c_recent = mk_check("menu:sec-recent", "最近打开")?;
                let c_outline = mk_check("menu:sec-outline", "大纲")?;
                // 侧边栏子菜单：显示开关 + 各分组开关（库/工作目录开关已移到「文件」菜单）。
                let sidebar_submenu = SubmenuBuilder::new(handle, "侧边栏")
                    .item(&c_sidebar)
                    .separator()
                    .item(&c_openfiles)
                    .item(&c_vaults)
                    .item(&c_workdir)
                    .item(&c_favorites)
                    .item(&c_recent)
                    .item(&c_outline)
                    .build()?;
                let lock_all = MenuItemBuilder::with_id("menu:lock-all", "全部锁定").accelerator("Cmd+L").build(handle)?;
                let view_submenu = SubmenuBuilder::new(handle, "视图")
                    .item(&m_edit)
                    .item(&m_split)
                    .item(&m_preview)
                    .separator()
                    .item(&c_tabs)
                    .item(&c_toolbar)
                    .item(&c_statusbar)
                    .item(&sidebar_submenu)
                    .separator()
                    .item(&lock_all)
                    .build()?;

                let window_submenu = SubmenuBuilder::new(handle, "窗口")
                    .minimize_with_text("最小化")
                    .fullscreen_with_text("全屏")
                    .build()?;

                let menu = MenuBuilder::new(handle)
                    .items(&[
                        &app_submenu,
                        &file_submenu,
                        &edit_submenu,
                        &format_submenu,
                        &view_submenu,
                        &window_submenu,
                    ])
                    .build()?;
                app.set_menu(menu)?;

                // 登记可勾选项句柄，供前端按当前设置/模式同步勾选态。
                let mut checks = std::collections::HashMap::new();
                checks.insert("menu:mode-edit".to_string(), m_edit);
                checks.insert("menu:mode-split".to_string(), m_split);
                checks.insert("menu:mode-preview".to_string(), m_preview);
                checks.insert("menu:toggle-tabs".to_string(), c_tabs);
                checks.insert("menu:toggle-toolbar".to_string(), c_toolbar);
                checks.insert("menu:toggle-statusbar".to_string(), c_statusbar);
                checks.insert("menu:toggle-sidebar".to_string(), c_sidebar);
                checks.insert("menu:sec-open-files".to_string(), c_openfiles);
                checks.insert("menu:sec-vaults".to_string(), c_vaults);
                checks.insert("menu:sec-workdir".to_string(), c_workdir);
                checks.insert("menu:sec-favorites".to_string(), c_favorites);
                checks.insert("menu:sec-recent".to_string(), c_recent);
                checks.insert("menu:sec-outline".to_string(), c_outline);
                handle.manage(MenuDyn { checks, vault_close, workdir_toggle });
            }

            // macOS：每秒轮询会话字典的 CGSSessionScreenIsLocked，锁屏（未锁 → 已锁）时
            // 发事件给前端；前端按「系统休眠 / 锁屏时锁定」开关决定是否全部锁定。
            // 休眠时该线程同样被挂起，唤醒后若已变为锁定态会立即补发。
            #[cfg(target_os = "macos")]
            {
                let ev = handle.clone();
                std::thread::spawn(move || {
                    let mut last_locked = screenlock::is_screen_locked();
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(1000));
                        let locked = screenlock::is_screen_locked();
                        if locked && !last_locked {
                            let _ = ev.emit("marklock://screen-locked", ());
                        }
                        last_locked = locked;
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != MAIN_WINDOW {
                return;
            }
            match event {
                // 关闭窗口（点红叉 / Cmd+Q）：不再直接退出，改为通知前端；
                // 由前端判断是否有未保存文件并弹窗确认，确认后调用 quit_app 命令退出。
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.emit("marklock://close-requested", ());
                }
                // 拖放事件由 Tauri 内置转发给前端（tauri://drag-enter/over/drop/leave），
                // 这里不再自定义同名事件，避免 payload 类型冲突。
                _ => {}
            }
        })
        .on_menu_event(|app, event| {
            let id = event.id().0.clone();
            // 接管自定义退出项（⌘Q / 菜单退出）：不直接退出，通知前端判断未保存并确认后 quit_app。
            if id == "quit-app" {
                if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = win.emit("marklock://close-requested", ());
                }
                return;
            }
            // 其余 menu:* 项统一转发前端，由 EditorView.runMenuAction 按 id 分派到对应方法。
            if id.starts_with("menu:") {
                if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = win.emit("marklock://menu", id);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("MarkLock 启动失败")
        .run(|app, event| match event {
            // macOS：Finder「打开方式」/ 把文件拖到 Dock 图标上打开
            // （配合 bundle.fileAssociations，让 MarkLock 可以成为默认 md 程序）
            // RunEvent::Opened 仅在 macOS 上存在，Windows/Linux 编译时不启用该分支。
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Opened { urls } => {
                let paths: Vec<String> = urls
                    .iter()
                    .filter_map(|u| u.to_file_path().ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                if !paths.is_empty() {
                    // 先暂存（解决冷启动时前端尚未注册监听器导致的丢事件），再发信号事件。
                    if let Ok(mut v) = PENDING_OPEN_PATHS.lock() {
                        v.extend(paths.iter().cloned());
                    }
                    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = win.show();
                        let _ = win.emit("marklock://open-paths", ());
                    }
                }
            }
            // macOS：程序运行中点击 Dock 图标 → 重新显示窗口（关闭是隐藏而非退出）
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { has_visible_windows, .. } => {
                if !has_visible_windows {
                    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            }
            // 退出请求（macOS Cmd+Q / 菜单退出）：未经确认时拦截并交给前端弹窗判断未保存；
            // 确认后 quit_app 会置 EXIT_CONFIRMED 并再次 exit，那时直接放行。
            tauri::RunEvent::ExitRequested { api, .. } => {
                if EXIT_CONFIRMED.load(Ordering::SeqCst) {
                    return;
                }
                api.prevent_exit();
                if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = win.emit("marklock://close-requested", ());
                }
            }
            _ => {}
        });
}
