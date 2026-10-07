//! macOS 剪贴板「文件引用」读取（仅 macOS 提供实现，其余平台由 `lib.rs` 走空桩）。
//!
//! 场景：粘贴图片/文件时 WKWebView 的 `clipboardData.files` 常拿不到磁盘路径
//! （从访达「拷贝文件」时甚至完全没有 File 对象，只在 `clipboardData.types` 里留一个
//! `Files` 标记）。这里直接读系统剪贴板的 `public.file-url` 类型兜底，返回真实磁盘路径。
//!
//! 全部通过手工 FFI 声明 Objective-C 运行时与 CoreFoundation 符号完成，不引入新 crate
//! （写法与 `screenlock` / `fileassoc` 模块一致；`objc_msgSend` 的地址用 `transmute` 转成
//! 带具体签名的函数指针后调用）。

#![cfg(target_os = "macos")]

use std::ffi::{c_char, c_void};
use std::path::PathBuf;

type Id = *const c_void;

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_msgSend();
    fn sel_registerName(name: *const c_char) -> Id;
    fn objc_getClass(name: *const c_char) -> Id;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> Id;
    fn CFStringGetLength(cf: Id) -> isize;
    fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
    fn CFStringGetCString(cf: Id, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFStringGetTypeID() -> usize;
    fn CFGetTypeID(cf: Id) -> usize;
    fn CFURLCreateWithString(alloc: *const c_void, url: Id, base: *const c_void) -> Id;
    fn CFURLCopyFileSystemPath(url: Id, style: u32) -> Id;
    fn CFRelease(cf: *const c_void);
}

/// `kCFStringEncodingUTF8`
const UTF8: u32 = 0x0800_0100;
/// `kCFURLPOSIXPathStyle`
const POSIX_STYLE: u32 = 0;

/// 选择器按名字缓存：同一选择器反复 `sel_registerName` 属未定义行为，缓存后只注册一次。
///
/// 缓存值以 `usize` 存储（裸指针非 `Send`/`Sync`，不能直接放进 `static`），取用时再转回指针。
fn sel(name: &'static str) -> Id {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<&'static str, usize>>> = OnceLock::new();
    let map = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.get(name) {
        return *s as Id;
    }
    let c = std::ffi::CString::new(name).unwrap_or_default();
    let s = unsafe { sel_registerName(c.as_ptr()) };
    guard.insert(name, s as usize);
    s
}

/// 把 `objc_msgSend` 的地址转成具体签名的函数指针后调用（0 个额外参数，返回对象指针）。
unsafe fn m0(recv: Id, name: &'static str) -> Id {
    let f: extern "C" fn(Id, Id) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name))
}

/// 同上，1 个对象参数。
unsafe fn m1(recv: Id, name: &'static str, arg: Id) -> Id {
    let f: extern "C" fn(Id, Id, Id) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name), arg)
}

/// 带 `usize` 参数（`objectAtIndex:`）取回对象。
unsafe fn m_at(recv: Id, name: &'static str, idx: usize) -> Id {
    let f: extern "C" fn(Id, Id, usize) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name), idx)
}

/// 返回 `NSUInteger` 的消息（`count` / `length`）。
unsafe fn m_sz(recv: Id, name: &'static str) -> usize {
    let f: extern "C" fn(Id, Id) -> usize = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name))
}

/// CFString → Rust String。
unsafe fn cf_to_string(cf: Id) -> Option<String> {
    if cf.is_null() {
        return None;
    }
    let len = CFStringGetLength(cf);
    if len <= 0 {
        return None;
    }
    let cap = CFStringGetMaximumSizeForEncoding(len, UTF8) + 1;
    if cap <= 0 {
        return None;
    }
    let mut buf: Vec<u8> = vec![0; cap as usize];
    if CFStringGetCString(cf, buf.as_mut_ptr() as *mut c_char, cap, UTF8) == 0 {
        return None;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr() as *const c_char) };
    Some(cstr.to_string_lossy().into_owned())
}

/// `file:///…` URL 字符串 → 本地绝对路径（CF 负责百分号解码）。
unsafe fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let cs = std::ffi::CString::new(url).ok()?;
    let cfstr = CFStringCreateWithCString(std::ptr::null(), cs.as_ptr(), UTF8);
    if cfstr.is_null() {
        return None;
    }
    let cfurl = CFURLCreateWithString(std::ptr::null(), cfstr, std::ptr::null());
    CFRelease(cfstr);
    if cfurl.is_null() {
        return None;
    }
    let path_cf = CFURLCopyFileSystemPath(cfurl, POSIX_STYLE);
    CFRelease(cfurl);
    if path_cf.is_null() {
        return None;
    }
    let out = cf_to_string(path_cf).map(PathBuf::from);
    CFRelease(path_cf);
    out
}

/// 读取剪贴板里的文件 URL（`public.file-url`），只返回真实存在的文件/目录路径。
///
/// 支持一次拷贝多个条目（访达多选拷贝）。顺序即剪贴板条目顺序；读不到时返回空列表。
pub fn clipboard_file_paths() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    unsafe {
        let cname = std::ffi::CString::new("NSPasteboard").unwrap_or_default();
        let cls = objc_getClass(cname.as_ptr());
        if cls.is_null() {
            return out;
        }
        // +[NSPasteboard generalPasteboard]（类方法：receiver 传类对象）
        let pb = m0(cls, "generalPasteboard");
        if pb.is_null() {
            return out;
        }
        let items = m0(pb, "pasteboardItems");
        if items.is_null() {
            return out;
        }
        let n = m_sz(items, "count");
        let tname = std::ffi::CString::new("public.file-url").unwrap_or_default();
        let type_str = CFStringCreateWithCString(std::ptr::null(), tname.as_ptr(), UTF8);
        if type_str.is_null() {
            return out;
        }
        for i in 0..n {
            let item = m_at(items, "objectAtIndex:", i);
            if item.is_null() {
                continue;
            }
            let data = m1(item, "dataForType:", type_str);
            if data.is_null() {
                continue;
            }
            let bytes = m0(data, "bytes");
            let len = m_sz(data, "length");
            if bytes.is_null() || len == 0 {
                continue;
            }
            let slice = std::slice::from_raw_parts(bytes as *const u8, len);
            // 该类型内容即 UTF-8 的 file URL 文本
            let url = String::from_utf8_lossy(slice).into_owned();
            if let Some(p) = file_url_to_path(&url) {
                if p.exists() {
                    if let Some(s) = p.to_str() {
                        out.push(s.to_string());
                    }
                }
            }
        }
        CFRelease(type_str);
    }
    out
}

// 消除未使用告警：这两个类型解析辅助函数在只读单个字符串的调用方保留备用。
#[allow(dead_code)]
unsafe fn is_nsstring(obj: Id) -> bool {
    !obj.is_null() && CFGetTypeID(obj) == CFStringGetTypeID()
}
