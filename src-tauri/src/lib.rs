//! MarkLock — 可加密的 Markdown 查看编辑器（Tauri 壳 + Rust 加密核心）。
//!
//! 相对旧 OA 壳的变化：
//! - 窗口加载本地 `dist/` 前端（不再加载远程 OA）
//! - 新增 `crypto` 模块并注册为 Tauri 命令，前端通过 `invoke` 调用

mod crypto;
mod assets;
#[cfg(target_os = "macos")]
mod clipboard;
#[cfg(target_os = "macos")]
mod appinfo;
#[cfg(target_os = "windows")]
mod appinfo_win;

use crypto::session::SessionStore;
use crypto::vault;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

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

/// 热角唤出：鼠标停留屏幕角落一段时间即弹出便携便签。
///
/// 系统没有「停留型热角」API，这里用后台线程轮询全局光标位置（骨架照抄上面的锁屏轮询线程
/// 与 macOS 会话字典探测）：光标进入角落小区域开始计时，停留满 dwell 触发一次 show_sticky，
/// 离开角落前不重复触发。用轮询而非全局事件钩子：无需辅助功能 / 输入监听权限。
mod hotcorner {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};
    use tauri::Manager;

    /// 是否启用热角（前端经 set_hot_corner 同步）。
    pub static ENABLED: AtomicBool = AtomicBool::new(false);
    /// 停留触发时长（毫秒）。
    pub static DWELL_MS: AtomicU32 = AtomicU32::new(500);
    /// 「鼠标移开即隐藏」是否已武装：仅热角唤出时置真（托盘/命令唤出是显式操作，不自动收起）。
    /// 用户点进便签（前端 sticky_engage）或隐藏窗口时置假，交回 blur/Esc 收起逻辑。
    pub static AUTOHIDE: AtomicBool = AtomicBool::new(false);
    /// 角落判定边长：光标距左边缘与上/下边缘均小于该值（像素/点）视为进入角落。
    const EDGE: f64 = 10.0;
    /// 轮询间隔（毫秒）。0.5s 级停留手势无需更高精度，也几乎不占 CPU。
    const POLL_MS: u64 = 100;
    /// 便签命中判定的外扩边距（像素/点）：把窗口矩形略微放大，
    /// 覆盖窗口 margin 与角落触发区之间的缝隙，避免鼠标停在缝里被误判为「已移开」。
    const HIT_PAD: f64 = 20.0;
    /// 光标移出「保持区域」（便签窗口 ∪ 触发角落）后到收起的宽限（毫秒）：
    /// 避免贴边抖动误收，也给足从角落移入窗口的时间。
    const LEAVE_GRACE_MS: u128 = 600;

    #[cfg(target_os = "macos")]
    mod cursor {
        use std::ffi::c_void;
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct CGPoint {
            pub x: f64,
            pub y: f64,
        }
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGEventCreate(source: *const c_void) -> *const c_void;
            fn CGEventGetLocation(event: *const c_void) -> CGPoint;
            fn CFRelease(cf: *const c_void);
        }
        /// 全局光标位置（逻辑点）。
        pub fn pos() -> Option<(f64, f64)> {
            unsafe {
                let ev = CGEventCreate(std::ptr::null());
                if ev.is_null() {
                    return None;
                }
                let p = CGEventGetLocation(ev);
                CFRelease(ev);
                Some((p.x, p.y))
            }
        }
    }

    #[cfg(target_os = "windows")]
    mod cursor {
        #[repr(C)]
        pub struct POINT {
            pub x: i32,
            pub y: i32,
        }
        extern "system" {
            fn GetCursorPos(lp: *mut POINT) -> i32;
        }
        /// 全局光标位置（物理像素）。
        pub fn pos() -> Option<(f64, f64)> {
            let mut p = POINT { x: 0, y: 0 };
            if unsafe { GetCursorPos(&mut p) } != 0 {
                Some((p.x as f64, p.y as f64))
            } else {
                None
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    mod cursor {
        /// 未支持平台：恒返回 None，热角自然不触发。
        pub fn pos() -> Option<(f64, f64)> {
            None
        }
    }

    /// 修饰键是否正被按下（全局硬件态，无需输入监听权限）。
    /// 编码：0=Shift 1=Control 2=Alt/Option 3=Command(macOS) / Win(Windows)。
    /// 供「按住修饰键 + 移到角落」触发模式使用。
    #[cfg(target_os = "macos")]
    pub fn modifier_held(id: u8) -> bool {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
        }
        // 各修饰键的左 / 右虚拟键码（HID keycode）
        let (left, right): (u16, u16) = match id {
            0 => (56, 60), // Shift
            1 => (59, 62), // Control
            2 => (58, 61), // Option / Alt
            _ => (55, 54), // Command
        };
        // kCGEventSourceStateHIDSystemState = 1：反映真实硬件按下态
        unsafe { CGEventSourceKeyState(1, left) || CGEventSourceKeyState(1, right) }
    }

    #[cfg(target_os = "windows")]
    pub fn modifier_held(id: u8) -> bool {
        extern "system" {
            fn GetAsyncKeyState(virt_key: i32) -> i16;
        }
        let vk = match id {
            0 => 0x10i32, // VK_SHIFT
            1 => 0x11,   // VK_CONTROL
            2 => 0x12,   // VK_MENU (Alt)
            _ => 0x5B,   // VK_LWIN（类比 macOS 的 Command）
        };
        // 返回值最高位为 1 表示当前正被按下
        unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn modifier_held(_id: u8) -> bool {
        false
    }

    /// 启动轮询线程（setup 调用一次，进程生命周期常驻）。
    pub fn start(app: tauri::AppHandle) {
        std::thread::spawn(move || {
            let mut since: Option<Instant> = None;
            let mut fired = false;
            // 「移开即隐藏」状态机：仅记录光标移出「保持区域」的时刻，达到宽限即收起。
            let mut left_at: Option<Instant> = None;
            loop {
                std::thread::sleep(Duration::from_millis(POLL_MS));

                // 光标坐标空间与显示器尺寸对齐：macOS 光标为逻辑点、Windows 为物理像素，
                // 显示器/窗口物理尺寸按平台换算到同一空间再比较。
                let cursor = cursor::pos();
                let scale = app
                    .primary_monitor()
                    .ok()
                    .flatten()
                    .map(|m| m.scale_factor())
                    .unwrap_or(1.0);
                // 屏幕尺寸（光标坐标空间），供角落命中判定使用：macOS 逻辑点 / 其他平台物理像素。
                let (screen_w, screen_h) = app
                    .primary_monitor()
                    .ok()
                    .flatten()
                    .map(|m| {
                        let s = m.size();
                        #[cfg(target_os = "macos")]
                        let (ww, hh) = (
                            s.width as f64 / m.scale_factor(),
                            s.height as f64 / m.scale_factor(),
                        );
                        #[cfg(not(target_os = "macos"))]
                        let (ww, hh) = (s.width as f64, s.height as f64);
                        (Some(ww), Some(hh))
                    })
                    .unwrap_or((None, None));
                let bottom = super::sticky_corner_is_bottom();
                let right = super::sticky_corner_is_right();
                // 光标是否落在触发角落（与唤出判定同一区域）：停在角落时不收起。
                let in_corner = match (cursor, screen_w, screen_h) {
                    (Some((cx, cy)), Some(w), Some(h)) => {
                        let horiz = if right { cx > w - EDGE } else { cx < EDGE };
                        let vert = if bottom { cy > h - EDGE } else { cy < EDGE };
                        horiz && vert
                    }
                    _ => false,
                };

                // ---- 移开即隐藏（仅热角武装时生效）----
                if AUTOHIDE.load(Ordering::SeqCst) {
                    // 「保持区域」= 便签窗口（外扩 HIT_PAD）∪ 触发角落；光标在其中即不计时。
                    let in_window = cursor
                        .and_then(|(cx, cy)| {
                            sticky_bounds_logical(&app, scale).map(|(x, y, w, h)| {
                                cx >= x - HIT_PAD
                                    && cx <= x + w + HIT_PAD
                                    && cy >= y - HIT_PAD
                                    && cy <= y + h + HIT_PAD
                            })
                        })
                        .unwrap_or(false);
                    if in_window || in_corner {
                        left_at = None;
                    } else {
                        let now = Instant::now();
                        match left_at {
                            Some(t) if now.duration_since(t).as_millis() >= LEAVE_GRACE_MS => {
                                if let Some(w) = app.get_webview_window(super::STICKY_WINDOW) {
                                    let _ = w.hide();
                                }
                                left_at = None;
                            }
                            Some(_) => {}
                            None => left_at = Some(now),
                        }
                    }
                } else {
                    left_at = None;
                }

                // ---- 热角唤出 ----
                if !ENABLED.load(Ordering::SeqCst) {
                    since = None;
                    fired = false;
                    continue;
                }
                // in_corner 已在上方按 cursor/屏幕尺寸是否为空一并判定（缺失时为 false）；
                // 若开启「按住修饰键触发」，还需对应修饰键正被按下才计入停留计时。
                if !in_corner || !super::sticky_modifier_held() {
                    since = None;
                    fired = false;
                    continue;
                }
                if fired {
                    continue;
                }
                let now = Instant::now();
                match since {
                    Some(t)
                        if now.duration_since(t).as_millis()
                            >= DWELL_MS.load(Ordering::SeqCst) as u128 =>
                    {
                        fired = true;
                        let _ = super::show_sticky(&app);
                        // 热角唤出后武装「移开即隐藏」。
                        AUTOHIDE.store(true, Ordering::SeqCst);
                    }
                    Some(_) => {}
                    None => since = Some(now),
                }
            }
        });
    }

    /// 便签窗口在外框坐标系下的矩形 (x, y, w, h)，已换算到与光标一致的空间
    /// （macOS 逻辑点 / 其他平台物理像素）。窗口不存在或不可见时返回 None。
    fn sticky_bounds_logical(
        app: &tauri::AppHandle,
        scale: f64,
    ) -> Option<(f64, f64, f64, f64)> {
        let win = app.get_webview_window(super::STICKY_WINDOW)?;
        if !win.is_visible().ok()? {
            return None;
        }
        let pos = win.outer_position().ok()?;
        let size = win.outer_size().ok()?;
        #[cfg(target_os = "macos")]
        let (x, y, w, h) = (
            pos.x as f64 / scale,
            pos.y as f64 / scale,
            size.width as f64 / scale,
            size.height as f64 / scale,
        );
        #[cfg(not(target_os = "macos"))]
        let (x, y, w, h) = (
            pos.x as f64,
            pos.y as f64,
            size.width as f64,
            size.height as f64,
        );
        let _ = scale;
        Some((x, y, w, h))
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
/// 便携便签独立窗口标签（与主窗口隔离，前端据此分流到 /sticky 路由）。
const STICKY_WINDOW: &str = "sticky";

/// 全局会话存储（进程生命周期内唯一）。
fn store() -> &'static SessionStore {
    static STORE: OnceLock<SessionStore> = OnceLock::new();
    STORE.get_or_init(SessionStore::new)
}

/// 退出是否已经前端确认。为 true 时放行 ExitRequested，避免 quit_app 再次被拦截。
/// 初始为 false：首次退出请求（如 macOS Cmd+Q）会被 prevent_exit，转为前端未保存确认弹窗。
static EXIT_CONFIRMED: AtomicBool = AtomicBool::new(false);

/// 系统托盘图标 id：语言切换时按此 id 取回托盘并重建菜单文案。
const TRAY_ID: &str = "main-tray";

/// 「关闭窗口时最小化到托盘」开关：为 true 时点关闭按钮改为隐藏窗口到托盘、程序后台常驻，
/// 而非走「未保存确认 → 退出」；为 false 时保持原退出语义。由前端经 set_close_to_tray 同步。
static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(false);

/// 托盘点击弹出便签的鼠标键：0=不弹出 1=左键（右键弹菜单，默认）2=右键（左键弹菜单）。
/// 前端经 set_tray_sticky_button 同步；install_tray 初值与此一致。
/// macOS 左键经 apply_tray_click_mode 关闭默认弹菜单后同样派发 Click（Down），三键跨平台可选。
static TRAY_STICKY_BUTTON: AtomicU8 = AtomicU8::new(1);

/// 便签弹出角落：0=屏幕左上 1=屏幕左下 2=屏幕右上 3=屏幕右下。前端经 set_sticky_corner 同步设置项；
/// 托盘左键唤出与（阶段二）热角定位共用，避免 Rust 侧回读前端 localStorage。
static STICKY_CORNER: AtomicU8 = AtomicU8::new(0);

/// 「按住修饰键才触发热角」开关与所选修饰键（0=Shift 1=Control 2=Alt 3=Command/Win），
/// 前端经 set_hot_corner 同步；默认关闭，即纯停留触发。
static STICKY_REQUIRE_MOD: AtomicBool = AtomicBool::new(false);
static STICKY_MODIFIER: AtomicU8 = AtomicU8::new(3);

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
        crypto::io::atomic_write(Path::new(&path), &bytes)?;
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

/// 同步「关闭窗口时最小化到托盘」开关到后端：为 true 时 `CloseRequested` 改为隐藏窗口到托盘。
/// 前端在设置项变更及启动时各调用一次，保证后端行为与 UI 开关一致。
#[tauri::command]
fn set_close_to_tray(enabled: bool) {
    CLOSE_TO_TRAY.store(enabled, Ordering::SeqCst);
}

/// 同步「保存时保留上一版备份」开关到后端：为 false 时原子写盘不再生成 `.mlk-bak` 备份。
/// 原子写盘（tmp → fsync → rename）本身始终生效，与开关无关。前端在启动及设置项变更时各调用一次。
#[tauri::command]
fn set_backup_enabled(enabled: bool) {
    crypto::io::set_backups_enabled(enabled);
}

/// 用最近一份 `.mlk-bak` 备份恢复指定文件（当前版本先滚动为新备份，故可再次撤销）。
/// 返回恢复后文件的字节数。无备份时返回可读错误。
#[tauri::command]
async fn restore_backup(path: String) -> Result<u64, crypto::CryptoError> {
    spawn_crypto(move || crypto::io::restore_latest_backup(Path::new(&path)).map(|n| n as u64)).await
}

/// 查询目标是否存在可恢复的最近一份 `.mlk-bak` 备份（廉价同步检查，供前端弹框前预检）。
#[tauri::command]
fn has_backup(path: String) -> bool {
    crypto::io::latest_backup_exists(Path::new(&path))
}

/// 同步「托盘点击弹出便签」的鼠标键（'off' | 'left' | 'right'）到后端，并即时应用到现有托盘。
#[tauri::command]
fn set_tray_sticky_button(app: tauri::AppHandle, button: String) {
    TRAY_STICKY_BUTTON.store(
        match button.as_str() {
            "left" => 1,
            "right" => 2,
            _ => 0,
        },
        Ordering::SeqCst,
    );
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        apply_tray_click_mode(&tray);
    }
}

/// 运行时开关系统托盘：开启时（若不存在）创建托盘图标，关闭时移除。
/// 供设置页「显示托盘图标」开关使用；图标被系统隐藏/移除后，前端「关→开」拨动可借此重建。
#[tauri::command]
fn set_tray_enabled(app: tauri::AppHandle, enabled: bool) {
    #[cfg(debug_assertions)]
    eprintln!("[tray] set_tray_enabled({enabled}), exists={}", app.tray_by_id(TRAY_ID).is_some());
    if enabled {
        if app.tray_by_id(TRAY_ID).is_none() {
            let _ = install_tray(&app);
        }
    } else {
        app.remove_tray_by_id(TRAY_ID);
    }
}

/// 开关「登录时自动启动」：调用 tauri-plugin-autostart 写入 / 移除系统级启动项
/// （macOS: LaunchAgent plist；Windows: HKCU Run 注册表；Linux: XDG autostart desktop 文件）。
/// 出错时把底层错误串化返回给前端提示；成功即已生效，无需重启应用。
#[tauri::command]
fn set_auto_start(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    if enabled { mgr.enable() } else { mgr.disable() }.map_err(|e| e.to_string())
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
                .set_text(if has_workdir {
                    ml("关闭工作目录", "Close Working Directory")
                } else {
                    ml("打开工作目录", "Open Working Directory")
                });
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, checks, has_vaults, has_workdir);
    }
}

/// 原生菜单语言标志：0=中文，1=英文。前端切换语言时经 set_native_menu_lang 更新并重建 macOS 菜单。
#[allow(dead_code)]
static MENU_LANG: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// 当前界面菜单语言是否为英文（macOS 原生菜单与各平台托盘菜单共用）。
#[allow(dead_code)]
#[inline]
fn menu_is_en() -> bool {
    MENU_LANG.load(std::sync::atomic::Ordering::Relaxed) == 1
}

/// 按当前菜单语言在中/英文案间选择（macOS 原生菜单 + 全平台托盘菜单）。
fn ml(zh: &str, en: &str) -> String {
    if menu_is_en() {
        en.to_string()
    } else {
        zh.to_string()
    }
}

/// 构建系统托盘菜单（文案按 MENU_LANG 取中/英）：显示主窗口 / 偏好设置 / 全部锁定 / 退出。
/// 菜单项 id 以 `tray:` 前缀，由托盘自身的 on_menu_event 分派（与 macOS 原生菜单 `menu:` 命名空间隔离）。
/// 语言切换时 set_native_menu_lang 会按 TRAY_ID 取回托盘并调用本函数重建菜单。
fn build_tray_menu(handle: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    let show = MenuItemBuilder::with_id("tray:show", ml("打开 MarkLock", "Show MarkLock")).build(handle)?;
    let prefs = MenuItemBuilder::with_id("tray:settings", ml("偏好设置", "Preferences")).build(handle)?;
    let lock = MenuItemBuilder::with_id("tray:lock-all", ml("全部锁定", "Lock All")).build(handle)?;
    let quit = MenuItemBuilder::with_id("tray:quit", ml("退出 MarkLock", "Quit MarkLock")).build(handle)?;
    MenuBuilder::new(handle)
        .item(&show)
        .item(&prefs)
        .separator()
        .item(&lock)
        .separator()
        .item(&quit)
        .build()
}

/// 创建系统托盘（全平台）：显示主窗口 / 全部锁定 / 退出。菜单文案随 MENU_LANG 取中/英，
/// 语言切换时由 set_native_menu_lang 按 TRAY_ID 取回托盘重建。图标用默认窗口图标。
/// 托盘菜单事件由本托盘的 on_menu_event 就地处理（区别于 macOS 原生菜单的全局 handler）。
/// setup 与 set_tray_enabled 共用本函数，保证「销毁后重建」与首次创建行为一致。
///
/// 按 TRAY_STICKY_BUTTON 应用托盘左右键的「弹菜单 / 派发 Click」行为：
/// 便签键不弹菜单（改由 Click 事件弹便签），其余情况（含 off）两键均弹菜单。
/// 右键菜单开关 Tauri 未封装，取内层 `tray_icon::TrayIcon` 设置（`with_inner_tray_icon`）。
fn apply_tray_click_mode(tray: &tauri::tray::TrayIcon<tauri::Wry>) {
    let mode = TRAY_STICKY_BUTTON.load(Ordering::SeqCst); // 0=off 1=left 2=right
    let sticky_left = mode == 1;
    let sticky_right = mode == 2;
    // 左键：sticky 在左 → 不弹菜单（发 Click）；否则（含 off）弹菜单。
    let _ = tray.set_show_menu_on_left_click(!sticky_left);
    // 右键：sticky 在右 → 不弹菜单（发 Click）；否则（含 off）弹菜单。
    let _ = tray.with_inner_tray_icon(move |inner| inner.set_show_menu_on_right_click(!sticky_right));
}

fn install_tray(handle: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::tray::TrayIconBuilder;
    let tray_menu = build_tray_menu(handle)?;
    let Some(icon) = handle.default_window_icon().cloned() else {
        #[cfg(debug_assertions)]
        eprintln!("[tray] 默认窗口图标为 None，跳过托盘创建");
        return Ok(());
    };
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&tray_menu)
        .tooltip(WINDOW_TITLE)
        .icon(icon)
        // 左右键「弹菜单 / 派发 Click」由 apply_tray_click_mode 统一应用（构建后调用）；
        // 这里先给左键一个与当前设置一致的初值（sticky 在左=mode1 时左键不弹菜单），避免短暂窗口期行为不符。
        .show_menu_on_left_click(TRAY_STICKY_BUTTON.load(Ordering::SeqCst) != 1)
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            if let TrayIconEvent::Click { button, button_state, position, rect, .. } = event {
                let mode = TRAY_STICKY_BUTTON.load(Ordering::SeqCst); // 0=off 1=left 2=right
                // 在 Down 触发而非 Up：macOS 上 NSStatusItem 对左键的 mouseUp 常被系统菜单/跟踪环吞掉，
                // 导致「左键弹便签」收不到 Up 而不生效；Down 是两键在菜单逻辑之前都会派发的首个事件，更可靠。
                if button_state != MouseButtonState::Down {
                    return;
                }
                // 命中「便签键」（由 TRAY_STICKY_BUTTON 决定）才弹便签；off 或另一键交给系统弹菜单。
                let hit = match button {
                    MouseButton::Left => mode == 1,
                    MouseButton::Right => mode == 2,
                    _ => false,
                };
                if hit {
                    let app = tray.app_handle();
                    // 就近摆在图标旁：rect 有效则用图标矩形（转物理像素；OS 上报的托盘 rect 实际为物理，
                    // scale 仅对 Logical 回退有意义），退化（尺寸 0）时回退到光标位置。
                    let scale = app
                        .primary_monitor()
                        .ok()
                        .flatten()
                        .map(|m| m.scale_factor())
                        .unwrap_or(1.0);
                    let rp: tauri::PhysicalPosition<i32> = rect.position.to_physical(scale);
                    let rs: tauri::PhysicalSize<u32> = rect.size.to_physical(scale);
                    let anchor = if rs.width > 0 && rs.height > 0 {
                        TrayAnchor { x: rp.x as f64, y: rp.y as f64, w: rs.width as f64, h: rs.height as f64 }
                    } else {
                        TrayAnchor { x: position.x, y: position.y, w: 0.0, h: 0.0 }
                    };
                    let _ = show_sticky_at(app, Some(anchor), true);
                } else {
                    // 菜单键（另一键或 off 双键）：系统即将弹菜单；若便签正显示，先立即隐藏
                    // 避免叠在菜单上（菜单在托盘图标处弹出，与便签锚点重合），
                    // 未保存内容由前端 marklock://sticky-hide 监听做静默落盘。
                    let app = tray.app_handle();
                    if let Some(win) = app.get_webview_window(STICKY_WINDOW) {
                        if win.is_visible().unwrap_or(false) {
                            let _ = win.emit("marklock://sticky-hide", ());
                            let _ = win.hide();
                        }
                    }
                }
            }
        })
        .on_menu_event(|tray, event| {
            let app = tray.app_handle();
            match event.id().0.as_str() {
                // 显示主窗口：取消最小化 + 显示 + 聚焦（隐藏到托盘后从托盘唤回）。
                "tray:show" => {
                    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = win.unminimize();
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
                // 偏好设置：唤回主窗口并跳转设置页（通用 pane；区别于便签 ⚙ 的 open-settings→sticky tab）。
                "tray:settings" => {
                    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = win.unminimize();
                        let _ = win.show();
                        let _ = win.set_focus();
                        let _ = win.emit("marklock://open-preferences", ());
                    }
                }
                // 全部锁定：交给前端统一走 store.lockAll（含锁定前自动保存 + 页签锁定态刷新）。
                "tray:lock-all" => {
                    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = win.emit("marklock://tray-lock-all", ());
                    }
                }
                // 退出：复用现有「未保存确认 → quit_app」流程（发 close-requested 而非直接 exit）。
                "tray:quit" => {
                    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                        let _ = win.emit("marklock://close-requested", ());
                    }
                }
                _ => {}
            }
        })
        .build(handle)?;
    // 按当前「便签键」设置应用左右键行为（右键菜单开关走内层 tray_icon）。
    apply_tray_click_mode(&tray);
    #[cfg(debug_assertions)]
    eprintln!("[tray] 托盘已创建 (install_tray ok)");
    Ok(())
}

/// 角落编码（STICKY_CORNER）：0=左上 1=左下 2=右上 3=右下。取纵向「下」半（左下 / 右下）。
fn sticky_corner_is_bottom() -> bool {
    let c = STICKY_CORNER.load(Ordering::SeqCst);
    c == 1 || c == 3
}

/// 取横向「右」半（右上 / 右下），供定位与热角判定使用。
fn sticky_corner_is_right() -> bool {
    let c = STICKY_CORNER.load(Ordering::SeqCst);
    c == 2 || c == 3
}

/// 触发门控：未开启「按住修饰键」时恒为真；开启后要求对应修饰键正被按下。
fn sticky_modifier_held() -> bool {
    if !STICKY_REQUIRE_MOD.load(Ordering::SeqCst) {
        return true;
    }
    hotcorner::modifier_held(STICKY_MODIFIER.load(Ordering::SeqCst))
}

/// 取回或创建便签窗口。窗口常驻：首次创建后只显隐、不销毁，保证弹出瞬时（见性能考量）。
/// 无边框 + 置顶 + 初始隐藏 + 不进任务栏，呈现为一张浮在桌面之上的轻窗口。
fn ensure_sticky_window(app: &tauri::AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    if let Some(win) = app.get_webview_window(STICKY_WINDOW) {
        return Ok(win);
    }
    let win = tauri::WebviewWindowBuilder::new(
        app,
        STICKY_WINDOW,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("MarkLock 便签")
    .inner_size(400.0, 560.0)
    .min_inner_size(300.0, 220.0)
    .always_on_top(true)
    .decorations(false)
    .resizable(true)
    .visible(false)
    .skip_taskbar(true)
    // macOS「first mouse」：便签显示时不夺焦（见 show_sticky），从别的 App 点回来时，
    // 默认第一击只用于激活窗口、不下发到内容。开启后 WKWebView 的 acceptsFirstMouse: 返回 YES，
    // 首次点击即直达内容（两击合一）。非 macOS 平台此选项为空操作。
    .accept_first_mouse(true)
    .build()?;
    Ok(win)
}

/// 按当前角落设置把便签窗口摆到主显示器对应角落（逻辑坐标，避开菜单栏 / 任务栏）。
fn position_sticky(app: &tauri::AppHandle, win: &tauri::WebviewWindow) {
    let Ok(Some(mon)) = app.primary_monitor() else { return };
    let scale = mon.scale_factor();
    let screen_w = mon.size().width as f64 / scale;
    let screen_h = mon.size().height as f64 / scale;
    // 取窗口当前真实尺寸（逻辑像素）：便签可被用户拖动 resize，不能用创建时的固定值，
    // 否则靠边 / 靠下角定位会按旧尺寸算 top / left，实际边越过屏幕沿导致出界。
    let (w, h) = win
        .inner_size()
        .map(|s| (s.width as f64 / scale, s.height as f64 / scale))
        .unwrap_or((400.0, 560.0));
    const MARGIN: f64 = 16.0;
    let y = if sticky_corner_is_bottom() {
        (screen_h - h - MARGIN).max(MARGIN)
    } else {
        // 上沿：下移避开 macOS 菜单栏 / Windows 顶部任务栏
        MARGIN + 24.0
    };
    let x = if sticky_corner_is_right() {
        (screen_w - w - MARGIN).max(MARGIN)
    } else {
        MARGIN
    };
    let _ = win.set_position(tauri::Position::Logical(tauri::LogicalPosition::new(x, y)));
}

/// 托盘/菜单栏图标的锚点（物理像素）：图标矩形的位置与尺寸。
/// rect 退化（尺寸为 0）时回退用光标位置，此时 w=h=0。
#[derive(Clone, Copy)]
struct TrayAnchor {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// 从托盘/菜单栏图标「就近」弹出时的定位：图标在屏幕上半（如 macOS 菜单栏）→ 便签摆其正下方；
/// 图标在下半（如 Windows 底部任务栏）→ 摆其正上方。水平以图标中心对齐，并夹在图标所在显示器可视范围内。
/// 全程用物理像素计算（rect / monitor / inner_size 均为物理），规避多屏不同缩放的换算误差。
fn position_sticky_near(win: &tauri::WebviewWindow, anchor: TrayAnchor) {
    // 便签可被用户 resize，取当前真实物理尺寸，不能用创建时固定值。
    let Ok(size) = win.inner_size() else { return };
    let (aw, ah) = (size.width as f64, size.height as f64);
    let icon_cx = anchor.x + anchor.w / 2.0;
    let icon_cy = anchor.y + anchor.h / 2.0;
    // 找包含图标中心的显示器（物理坐标包含测试；不依赖 monitor_from_point 的坐标语义）。
    let monitors = win.available_monitors().unwrap_or_default();
    let mon = monitors
        .iter()
        .find(|m| {
            let p = m.position();
            let s = m.size();
            icon_cx >= p.x as f64
                && icon_cx <= p.x as f64 + s.width as f64
                && icon_cy >= p.y as f64
                && icon_cy <= p.y as f64 + s.height as f64
        })
        .or_else(|| monitors.first());
    let Some(mon) = mon else { return };
    let mp = mon.position();
    let ms = mon.size();
    let scale = mon.scale_factor();
    let mon_l = mp.x as f64;
    let mon_t = mp.y as f64;
    let mon_r = mon_l + ms.width as f64;
    let mon_b = mon_t + ms.height as f64;
    let gap = 6.0 * scale; // 图标与便签间距
    let edge = 8.0 * scale; // 屏幕边缘留白
    // 竖直：图标在上半 → 摆下方；下半 → 摆上方。
    let mid_y = mon_t + ms.height as f64 / 2.0;
    let mut y = if icon_cy < mid_y {
        anchor.y + anchor.h + gap
    } else {
        anchor.y - gap - ah
    };
    // 水平：以图标中心对齐。
    let mut x = icon_cx - aw / 2.0;
    // 夹在显示器可视范围内（窗口比屏宽时以左上留白为准，避免 min>max）。
    x = x.max(mon_l + edge).min((mon_r - edge - aw).max(mon_l + edge));
    y = y.max(mon_t + edge).min((mon_b - edge - ah).max(mon_t + edge));
    let _ = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
        x.round() as i32,
        y.round() as i32,
    )));
}

/// 弹出便签（按「热角角落」设置定位）：取回/创建 → 定位 → 显示，并通知便签视图刷新内容
/// （隐藏期间文件可能被外部改动、库可能被锁定，显示时重新读取最稳妥）。
/// 热角 / 命令唤出均不夺焦：热角靠 AUTOHIDE「移开即隐藏」收起，命令由调用方自行处理焦点。
fn show_sticky(app: &tauri::AppHandle) -> tauri::Result<()> {
    show_sticky_at(app, None, false)
}

/// 弹出便签的统一实现：anchor 为 Some 时就近摆在托盘/菜单栏图标旁（托盘路径），
/// 为 None 时按角落设置定位（热角 / 前端命令路径）。
/// focus=true 时显示后立即夺焦（托盘弹出路径）：让便签成为当前焦点窗口，之后点别处即触发
/// 前端 `blur` → 自动保存并收起；否则「焦点原本在别的 App」时便签从未获焦、blur 不触发而收不掉。
fn show_sticky_at(app: &tauri::AppHandle, anchor: Option<TrayAnchor>, focus: bool) -> tauri::Result<()> {
    let win = ensure_sticky_window(app)?;
    match anchor {
        Some(a) => position_sticky_near(&win, a),
        None => position_sticky(app, &win),
    }
    win.show()?;
    // 托盘弹出：夺焦以便「点别处 → blur → 收起」生效；热角/命令不夺焦，避免打断当前工作。
    if focus {
        let _ = win.set_focus();
    }
    // 解除「移开即隐藏」（托盘/命令唤出是显式操作，应停留到用户主动收起）；
    // 热角路径会在调用本函数后重新武装。
    hotcorner::AUTOHIDE.store(false, Ordering::SeqCst);
    let _ = win.emit("marklock://sticky-show", ());
    Ok(())
}

/// 前端 / 托盘 / 热角共用的「弹出便签」命令。
#[tauri::command]
fn show_sticky_note(app: tauri::AppHandle) -> tauri::Result<()> {
    show_sticky(&app)
}

/// 隐藏便签窗口（不销毁，下次弹出瞬时）。
#[tauri::command]
fn hide_sticky_note(app: tauri::AppHandle) -> tauri::Result<()> {
    hotcorner::AUTOHIDE.store(false, Ordering::SeqCst);
    if let Some(win) = app.get_webview_window(STICKY_WINDOW) {
        win.hide()?;
    }
    Ok(())
}

/// 便签「打开 MarkLock」主图标按钮：唤回主窗口（取消最小化 / 托盘隐藏态后显示并聚焦，不跳转页面）。
#[tauri::command]
fn show_main_window(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// 便签「设置」按钮：唤回主窗口（隐藏到托盘 / 最小化后）并通知其跳转设置页。
/// 唤回走 Rust 三步（与托盘「打开 MarkLock」同语义），比前端 show() 更可靠；
/// 跳转由主窗监听 marklock://open-settings 完成（便签是独立 webview，改不了主窗路由）。
#[tauri::command]
fn open_settings(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        let _ = win.emit("marklock://open-settings", ());
    }
}

/// 用户已点进便签（获得焦点 / 交互）：解除「移开即隐藏」，此后由 blur/Esc/关闭按钮收起。
#[tauri::command]
fn sticky_engage() {
    hotcorner::AUTOHIDE.store(false, Ordering::SeqCst);
}

/// 同步便签弹出角落（'topLeft' | 'bottomLeft' | 'topRight' | 'bottomRight'）到后端，供定位与热角共用。
#[tauri::command]
fn set_sticky_corner(corner: String) {
    let v = match corner.as_str() {
        "bottomLeft" => 1,
        "topRight" => 2,
        "bottomRight" => 3,
        _ => 0, // topLeft
    };
    STICKY_CORNER.store(v, Ordering::SeqCst);
}

/// 同步热角开关、停留时长、「按住修饰键触发」及其修饰键到后端轮询线程。
#[tauri::command]
fn set_hot_corner(enabled: bool, dwell_ms: u32, require_modifier: bool, modifier: String) {
    hotcorner::ENABLED.store(enabled, Ordering::SeqCst);
    hotcorner::DWELL_MS.store(dwell_ms, Ordering::SeqCst);
    STICKY_REQUIRE_MOD.store(require_modifier, Ordering::SeqCst);
    let m = match modifier.as_str() {
        "shift" => 0,
        "control" => 1,
        "alt" => 2,
        _ => 3, // command（Windows 上映射为 Win 键）
    };
    STICKY_MODIFIER.store(m, Ordering::SeqCst);
}

/// 前端切换界面语言时同步菜单语言：更新语言标志，macOS 重建原生菜单，全平台重建托盘菜单。
#[tauri::command]
fn set_native_menu_lang(app: tauri::AppHandle, lang: String) {
    MENU_LANG.store(if lang == "en" { 1 } else { 0 }, std::sync::atomic::Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    {
        let _ = install_native_menu(&app);
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(menu) = build_tray_menu(&app) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

/// 构建并安装 macOS 原生菜单（文案按 MENU_LANG 取中/英）。
/// 语言切换时前端调用 set_native_menu_lang 会再次执行本函数重建菜单，并重新登记 MenuDyn 句柄。
#[cfg(target_os = "macos")]
fn install_native_menu(handle: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItemBuilder, SubmenuBuilder};

    // App 菜单「MarkLock」：关于 + 偏好设置(⌘,) + 隐藏(⌘H) + 自定义退出(⌘Q)；「显示全部」类项置于「窗口」菜单。
    // 退出仍用自定义 quit-app 项（不用预定义 quit，避免系统直接 terminate）。
    // 「关于」不用预定义 about（会弹系统 About 面板），改为自定义项路由到应用内「关于」页。
    let about = MenuItemBuilder::with_id("menu:about", ml("关于 MarkLock", "About MarkLock")).build(handle)?;
    let prefs = MenuItemBuilder::with_id("menu:prefs", ml("偏好设置…", "Preferences…"))
        .accelerator("Cmd+,")
        .build(handle)?;
    let quit = MenuItemBuilder::with_id("quit-app", ml("退出 MarkLock", "Quit MarkLock"))
        .accelerator("Cmd+Q")
        .build(handle)?;
    let app_submenu = SubmenuBuilder::new(handle, "MarkLock")
        .item(&about)
        .separator()
        .item(&prefs)
        .separator()
        .hide_with_text(ml("隐藏 MarkLock", "Hide MarkLock"))
        .separator()
        .item(&quit)
        .build()?;

    // 文件：整合应用内「主菜单」+ 库/工作目录开关（后两者按状态动态改标题，见 sync_native_menu）。
    // 带 ⌘ 的项用 MenuItemBuilder 显式声明 accelerator；macOS 上原生菜单在 AppKit 层拦截按键，
    // 不会与前端全局 keydown / CodeMirror keymap 双触发（前端处理器仍服务 Windows/Linux）。
    let new_file = MenuItemBuilder::with_id("menu:new-file", ml("新建文件", "New File")).accelerator("Cmd+N").build(handle)?;
    let open_file = MenuItemBuilder::with_id("menu:open-file", ml("打开文件…", "Open File…")).accelerator("Cmd+O").build(handle)?;
    let vault_open = MenuItemBuilder::with_id("menu:vault-open", ml("打开库", "Open Vault")).build(handle)?;
    let vault_close = MenuItemBuilder::with_id("menu:vault-close", ml("关闭所有库", "Close All Vaults")).enabled(false).build(handle)?;
    let workdir_toggle = MenuItemBuilder::with_id("menu:workdir-toggle", ml("打开工作目录", "Open Working Directory")).build(handle)?;
    let save = MenuItemBuilder::with_id("menu:save", ml("保存", "Save")).accelerator("Cmd+S").build(handle)?;
    let close_tab = MenuItemBuilder::with_id("menu:close-tab", ml("关闭页签", "Close Tab")).accelerator("Cmd+W").build(handle)?;
    let file_submenu = SubmenuBuilder::new(handle, ml("文件", "File"))
        .item(&new_file)
        .text("menu:new-vault", ml("新建加密库", "New Encrypted Vault"))
        .separator()
        .item(&open_file)
        .item(&vault_open)
        .item(&vault_close)
        .item(&workdir_toggle)
        .separator()
        .item(&save)
        .item(&close_tab)
        .separator()
        .text("menu:batch-encrypt", ml("批量加密文件…", "Batch Encrypt Files…"))
        .text("menu:import-dir", ml("导入目录到库…", "Import Directory to Vault…"))
        .text("menu:export-html", ml("导出 HTML…", "Export HTML…"))
        .build()?;

    // 编辑：保留系统剪贴板预定义项（⌘C/⌘V 等），否则覆盖默认菜单后这些快捷键失效。
    // 用 *_with_text 显式给出中文标题，避免预定义项按系统语言显示英文。
    let find = MenuItemBuilder::with_id("menu:find", ml("编辑器内搜索", "Find in Editor")).accelerator("Cmd+F").build(handle)?;
    let find_global = MenuItemBuilder::with_id("menu:find-global", ml("全局搜索", "Search Everywhere")).accelerator("Cmd+Shift+F").build(handle)?;
    let edit_submenu = SubmenuBuilder::new(handle, ml("编辑", "Edit"))
        .undo_with_text(ml("撤销", "Undo"))
        .redo_with_text(ml("重做", "Redo"))
        .separator()
        .cut_with_text(ml("剪切", "Cut"))
        .copy_with_text(ml("复制", "Copy"))
        .paste_with_text(ml("粘贴", "Paste"))
        .select_all_with_text(ml("全选", "Select All"))
        .separator()
        .item(&find)
        .item(&find_global)
        .build()?;

    let bold = MenuItemBuilder::with_id("menu:bold", ml("加粗", "Bold")).accelerator("Cmd+B").build(handle)?;
    let italic = MenuItemBuilder::with_id("menu:italic", ml("斜体", "Italic")).accelerator("Cmd+I").build(handle)?;
    let link = MenuItemBuilder::with_id("menu:link", ml("链接", "Link")).accelerator("Cmd+K").build(handle)?;
    let format_submenu = SubmenuBuilder::new(handle, ml("格式", "Format"))
        .text("menu:h1", ml("标题 1", "Heading 1"))
        .text("menu:h2", ml("标题 2", "Heading 2"))
        .text("menu:h3", ml("标题 3", "Heading 3"))
        .separator()
        .item(&bold)
        .item(&italic)
        .text("menu:strike", ml("删除线", "Strikethrough"))
        .text("menu:code", ml("行内代码", "Inline Code"))
        .text("menu:codeblock", ml("代码块", "Code Block"))
        .separator()
        .text("menu:quote", ml("引用", "Quote"))
        .text("menu:ul", ml("无序列表", "Bulleted List"))
        .text("menu:task", ml("任务列表", "Task List"))
        .text("menu:table", ml("表格", "Table"))
        .separator()
        .item(&link)
        .text("menu:image", ml("图片", "Image"))
        .text("menu:hr", ml("分割线", "Horizontal Line"))
        .build()?;

    // 视图：模式（单选勾选）+ 显示开关（勾选）用 CheckMenuItem，勾选态由前端 sync_native_menu 回写。
    let mk_check = |id: &str, text: String| -> tauri::Result<CheckMenuItem<tauri::Wry>> {
        CheckMenuItem::with_id(handle, id, text, true, false, None::<&str>)
    };
    let m_edit = mk_check("menu:mode-edit", ml("编辑模式", "Edit Mode"))?;
    let m_split = mk_check("menu:mode-split", ml("分屏", "Split"))?;
    let m_preview = mk_check("menu:mode-preview", ml("预览模式", "Preview Mode"))?;
    let c_tabs = mk_check("menu:toggle-tabs", ml("顶部页签", "Top Tabs"))?;
    let c_toolbar = mk_check("menu:toggle-toolbar", ml("格式工具栏", "Formatting Toolbar"))?;
    let c_statusbar = mk_check("menu:toggle-statusbar", ml("状态栏", "Status Bar"))?;
    let c_sidebar = CheckMenuItem::with_id(handle, "menu:toggle-sidebar", ml("显示侧边栏", "Show Sidebar"), true, false, Some("Cmd+J"))?;
    let c_openfiles = mk_check("menu:sec-open-files", ml("打开的文件", "Open Files"))?;
    let c_vaults = mk_check("menu:sec-vaults", ml("加密库", "Vaults"))?;
    let c_workdir = mk_check("menu:sec-workdir", ml("工作目录", "Working Directory"))?;
    let c_favorites = mk_check("menu:sec-favorites", ml("收藏", "Favorites"))?;
    let c_recent = mk_check("menu:sec-recent", ml("最近打开", "Recent"))?;
    let c_outline = mk_check("menu:sec-outline", ml("大纲", "Outline"))?;
    // 侧边栏子菜单：显示开关 + 各分组开关（库/工作目录开关已移到「文件」菜单）。
    let sidebar_submenu = SubmenuBuilder::new(handle, ml("侧边栏", "Sidebar"))
        .item(&c_sidebar)
        .separator()
        .item(&c_openfiles)
        .item(&c_vaults)
        .item(&c_workdir)
        .item(&c_favorites)
        .item(&c_recent)
        .item(&c_outline)
        .build()?;
    let lock_all = MenuItemBuilder::with_id("menu:lock-all", ml("全部锁定", "Lock All")).accelerator("Cmd+L").build(handle)?;
    let view_submenu = SubmenuBuilder::new(handle, ml("视图", "View"))
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

    // 窗口：最小化 / 全屏 + 「显示所有窗口」。
    // 「显示所有窗口」不用预定义 show_all（对应 AppKit unhideAllApplications:）：隐藏到托盘走的是
    // NSWindow 级 orderOut，并非 NSApp hide，预定义项唤不醒窗口且会因校验不通过而置灰；
    // 改为自定义项由 on_menu_event 直接唤醒主窗口（与托盘「打开 MarkLock」同语义）。
    let show_all_windows = MenuItemBuilder::with_id("menu:show-all-windows", ml("显示所有窗口", "Show All Windows")).build(handle)?;
    let window_submenu = SubmenuBuilder::new(handle, ml("窗口", "Window"))
        .minimize_with_text(ml("最小化", "Minimize"))
        .fullscreen_with_text(ml("全屏", "Full Screen"))
        .separator()
        .item(&show_all_windows)
        .build()?;

    // 帮助：项目主页（GitHub / Gitee）+ 反馈（跳 Issue 页）+ 检查更新（智能检测）/ 关于（置于底部）。
    // 外链项（主页/反馈）点击后由 App.vue 走 open_url 用系统浏览器打开（全局生效，不依赖当前路由）；
    // 「检查更新」不发外链，由 App.vue 拦截 menu:check-update 调前端智能检测（GitHub/Gitee 自动回落，发现新版再弹窗跳转发布页）；
    // 「关于」复用 menu:about 事件，App.vue 路由到应用内「关于」页（与 App 菜单顶部「关于 MarkLock」同源，允许重复 id）。
    let help_submenu = SubmenuBuilder::new(handle, ml("帮助", "Help"))
        .text("menu:help-github", ml("GitHub 主页", "GitHub Home"))
        .text("menu:help-gitee", ml("Gitee 主页（国内镜像）", "Gitee Home (Chinese mirror)"))
        .separator()
        .text("menu:feedback", ml("反馈与建议（GitHub）", "Feedback (GitHub)"))
        .text("menu:feedback-gitee", ml("反馈与建议（Gitee 国内镜像）", "Feedback (Gitee Chinese mirror)"))
        .separator()
        .text("menu:check-update", ml("检查更新", "Check for Updates"))
        .text("menu:about", ml("关于 MarkLock", "About MarkLock"))
        .build()?;

    let menu = MenuBuilder::new(handle)
        .items(&[
            &app_submenu,
            &file_submenu,
            &edit_submenu,
            &format_submenu,
            &view_submenu,
            &window_submenu,
            &help_submenu,
        ])
        .build()?;
    handle.set_menu(menu)?;

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
    Ok(())
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
        crypto::io::atomic_write(Path::new(&path), &bytes)?;
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
        use std::os::windows::process::CommandExt;
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
            // CREATE_NO_WINDOW：powershell 是控制台子系统程序，从本 GUI 进程 spawn 时默认会
            // 新建控制台窗口（.output() 仅重定向流、不抑制窗口），加此标志消除删除时的闪框。
            .creation_flags(0x0800_0000)
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

/// 快速启动一个本地应用 / 路径（便签底部快速启动条）。
///
/// 与 open_url 不同，这里接收的是用户自己在设置里配置的本地路径（应用、目录或文档），
/// 交给系统「默认方式」打开：macOS `open`、Windows `explorer`、Linux `xdg-open`。
/// 路径一律作为独立参数传入、不拼接 shell 字符串，避免命令注入；`.exe`/`.app` 会被直接运行，
/// 目录与文档则按系统关联打开。
#[tauri::command]
fn launch_local(path: String) -> Result<(), crypto::CryptoError> {
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open").arg(&path).spawn();
    #[cfg(target_os = "windows")]
    let res = std::process::Command::new("explorer").arg(&path).spawn();
    #[cfg(target_os = "linux")]
    let res = std::process::Command::new("xdg-open").arg(&path).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let res: std::io::Result<std::process::Child> = Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "当前平台不支持快速启动本地应用",
    ));
    res.map_err(|e| crypto::CryptoError::Io(e.to_string()))?;
    Ok(())
}

/// 快速启动项的显示名（macOS 走 NSWorkspace、Windows 走 PowerShell 取本地化应用名；其他平台回退 None，由前端用文件名兜底）。
#[tauri::command]
fn app_display_name(path: String) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        appinfo::display_name(&path)
    }
    #[cfg(target_os = "windows")]
    {
        appinfo_win::display_name(&path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        None
    }
}

/// 快速启动项的图标 data URL（`data:image/png;base64,…`）。
/// macOS 从 NSWorkspace、Windows 从 PowerShell(System.Drawing) 抽图标；其他平台回退 None（前端不显示图标）。
#[tauri::command]
fn app_icon(path: String) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        appinfo::icon_data_url(&path)
    }
    #[cfg(target_os = "windows")]
    {
        appinfo_win::icon_data_url(&path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        None
    }
}

/// Windows：查一个扩展名当前生效的关联 ProgId。
///
/// 先读 HKCU FileExts 的 UserChoice（用户显式选择，受系统哈希保护）；不存在时回退
/// `HKCR\.ext` 的默认 ProgId（安装包/本应用自注册写入的关联）。reg.exe 查询键不存在时
/// 返回码非 0 但进程正常退出，须以 status 而非 spawn 结果判断是否回退。
#[cfg(target_os = "windows")]
fn windows_assoc_progId(ext: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let read = |args: &[&str]| -> Option<String> {
        // reg.exe 是控制台子系统程序，加 CREATE_NO_WINDOW 避免查询关联时闪出控制台窗口。
        let output = std::process::Command::new("reg")
            .args(args)
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
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
    use std::os::windows::process::CommandExt;
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
            // reg.exe 是控制台子系统程序，加 CREATE_NO_WINDOW 避免注册关联时闪出控制台窗口。
            let ok = std::process::Command::new("reg")
                .args(&args)
                .creation_flags(0x0800_0000)
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
            // 标记位：自启启动时带上该参数，供 setup 判断是否隐藏主窗（前端再根据「自启时显示主窗」设置决定是否 show）。
            Some(vec!["--autostarted"]),
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
            set_close_to_tray,
            set_backup_enabled,
            restore_backup,
            has_backup,
            set_tray_enabled,
            set_tray_sticky_button,
            set_auto_start,
            show_sticky_note,
            hide_sticky_note,
            open_settings,
            show_main_window,
            sticky_engage,
            set_sticky_corner,
            set_hot_corner,
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
            launch_local,
            app_display_name,
            app_icon,
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
            sync_native_menu,
            set_native_menu_lang
        ])
        .setup(|app| {
            // Windows 冷启动：把命令行里的文件路径暂存，前端 mounted 后拉取打开。
            #[cfg(target_os = "windows")]
            buffer_open_paths(argv_file_paths());
            let handle = app.handle();

            // 开机自启启动（tauri-plugin-autostart 写入的 --autostarted 标记）：先以隐藏态创建主窗，
            // 前端载入后由 App.vue 根据「自启时显示主窗」与「托盘图标」设置决定是否 show；
            // 若托盘关闭则前端强制 show，避免「无窗口 + 无托盘」的不可达状态。手动启动（无此参数）时保持默认可见。
            let autostarted = std::env::args().any(|a| a == "--autostarted");

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

            if autostarted {
                builder = builder.visible(false);
            }

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
            install_native_menu(handle)?;

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

            // 系统托盘（全平台）：创建逻辑见 install_tray（set_tray_enabled 运行时开关共用）。
            install_tray(handle)?;

            // 热角轮询线程（全平台常驻；未支持平台光标读取恒 None 自然不触发）。
            hotcorner::start(handle.clone());

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != MAIN_WINDOW {
                return;
            }
            match event {
                // 关闭窗口（点红叉）：
                // - 开启「关闭最小化到托盘」时：隐藏窗口到托盘、程序后台常驻（页签与密钥内存态保留，
                //   自动锁定定时器照常运行），并通知前端给出一次性提示；
                // - 否则：保持原语义，通知前端判断未保存并弹窗确认，确认后由 quit_app 退出。
                // 注：Cmd+Q / 托盘「退出」仍走 close-requested → 退出确认，不受本开关影响。
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    if CLOSE_TO_TRAY.load(Ordering::SeqCst) {
                        let _ = window.hide();
                        let _ = window.emit("marklock://hidden-to-tray", ());
                    } else {
                        let _ = window.emit("marklock://close-requested", ());
                    }
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
            // 「显示所有窗口」：在 Rust 侧直接唤醒主窗口，不转前端（隐藏到托盘后窗口已 orderOut，
            // 与托盘「打开 MarkLock」同语义：取消最小化 + 显示 + 聚焦）。
            if id == "menu:show-all-windows" {
                if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = win.unminimize();
                    let _ = win.show();
                    let _ = win.set_focus();
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
            // macOS：程序运行中点击 Dock 图标 → 唤回主窗口（关闭到托盘是隐藏而非退出）。
            // 不再用 has_visible_windows 门控：便携便签可见时它会被误判为 true，
            // 导致主窗口从托盘无法恢复；点击应用图标应始终以主窗口为准。
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                if let Some(win) = app.get_webview_window(MAIN_WINDOW) {
                    let _ = win.unminimize();
                    let _ = win.show();
                    let _ = win.set_focus();
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
