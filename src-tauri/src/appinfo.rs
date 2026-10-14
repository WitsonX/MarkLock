//! macOS 应用「显示名 + 图标」读取（便携便签快速启动项用，仅 macOS 提供实现）。
//!
//! 快速启动选择的是一个 `.app` 包（或任意本地路径）。直接拿文件名当显示名既不准确（与
//! Finder/启动台里看到的本地化名称不一致），也没有图标。这里用 `NSWorkspace` 读取系统层面
//! 的真实显示名与图标：图标经 `TIFFRepresentation → NSBitmapImageRep → PNG` 转成
//! `data:image/png;base64,…` 交给前端，前端按 path 内存缓存、不持久化（避免 localStorage 膨胀）。
//!
//! 全部通过手工 FFI 声明 Objective-C / CoreFoundation 符号完成，不引入新 crate
//! （写法与 `clipboard` 模块一致；`objc_msgSend` 的地址用 `transmute` 转成带具体签名的函数指针后调用）。

#![cfg(target_os = "macos")]

use std::ffi::{c_char, c_void};

type Id = *const c_void;

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_msgSend();
    fn sel_registerName(name: *const c_char) -> Id;
    fn objc_getClass(name: *const c_char) -> Id;
}

// 显式链接 AppKit：主程序因 wry/tao 已加载它，但本模块的 `cargo test` 跑在纯 CLI 进程里，
// 不链则 `objc_getClass("NSWorkspace")` 返回 nil。链接后类在进程启动即可用（对主程序无害）。
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> Id;
    fn CFStringGetLength(cf: Id) -> isize;
    fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
    fn CFStringGetCString(cf: Id, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: *const c_void);
}

/// `kCFStringEncodingUTF8`
const UTF8: u32 = 0x0800_0100;
/// `NSBitmapImageFileTypePNG`
const PNG_TYPE: isize = 4;

/// 选择器按名字缓存：同一选择器反复 `sel_registerName` 属未定义行为，缓存后只注册一次。
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

unsafe fn m0(recv: Id, name: &'static str) -> Id {
    let f: extern "C" fn(Id, Id) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name))
}

unsafe fn m1(recv: Id, name: &'static str, arg: Id) -> Id {
    let f: extern "C" fn(Id, Id, Id) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name), arg)
}

unsafe fn m_sz(recv: Id, name: &'static str) -> usize {
    let f: extern "C" fn(Id, Id) -> usize = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name))
}

/// 带一个 `isize` + 一个对象参数：`representationUsingType:properties:`。
unsafe fn m_isize_obj(recv: Id, name: &'static str, a: isize, b: Id) -> Id {
    let f: extern "C" fn(Id, Id, isize, Id) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name), a, b)
}

/// 返回 void 的消息（`drain`）。
unsafe fn m_void0(recv: Id, name: &'static str) {
    let f: extern "C" fn(Id, Id) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name))
}

/// `NSSize`（两个 CGFloat），按值传给 `initWithSize:`。`#[repr(C)]` 保证与 C 的 HFA 传参一致。
#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    w: f64,
    h: f64,
}

/// 带一个 `NSSize`（结构体）参数、返回对象：`initWithSize:`。
unsafe fn m_size(recv: Id, name: &'static str, s: NSSize) -> Id {
    let f: extern "C" fn(Id, Id, NSSize) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name), s)
}

/// 带 `NSUInteger` 参数取回对象（`objectAtIndex:`）。
unsafe fn m_at(recv: Id, name: &'static str, idx: usize) -> Id {
    let f: extern "C" fn(Id, Id, usize) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(recv, sel(name), idx)
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

/// 命令可能运行在非主线程，AppKit 对象靠自动释放池回收；建池→执行→排空。
unsafe fn with_pool<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    let cname = std::ffi::CString::new("NSAutoreleasePool").unwrap_or_default();
    let cls = objc_getClass(cname.as_ptr());
    if cls.is_null() {
        return f();
    }
    let pool = m0(cls, "new");
    let out = f();
    if !pool.is_null() {
        m_void0(pool, "drain");
    }
    out
}

/// `NSWorkspace.sharedWorkspace`（失败返回 null）。
unsafe fn shared_workspace() -> Id {
    let cname = std::ffi::CString::new("NSWorkspace").unwrap_or_default();
    let cls = objc_getClass(cname.as_ptr());
    if cls.is_null() {
        return std::ptr::null();
    }
    m0(cls, "sharedWorkspace")
}

/// 取路径在系统层面（Finder）显示的本地化名称（中文系统即中文名）。读不到返回 `None`。
/// 用 `NSFileManager.displayNameAtPath:`——注意 `NSWorkspace` 并无 `displayNameForFile:`，误用会抛 unrecognized selector 崩溃。
pub fn display_name(path: &str) -> Option<String> {
    unsafe {
        with_pool(|| {
            let cname = std::ffi::CString::new("NSFileManager").unwrap_or_default();
            let cls = objc_getClass(cname.as_ptr());
            if cls.is_null() {
                return None;
            }
            let fm = m0(cls, "defaultManager");
            if fm.is_null() {
                return None;
            }
            let cs = std::ffi::CString::new(path).ok()?;
            let path_cf = CFStringCreateWithCString(std::ptr::null(), cs.as_ptr(), UTF8);
            if path_cf.is_null() {
                return None;
            }
            let ns = m1(fm, "displayNameAtPath:", path_cf);
            let out = cf_to_string(ns);
            CFRelease(path_cf);
            out
        })
    }
}

/// 取路径对应图标的 PNG data URL（`data:image/png;base64,…`）。读不到返回 `None`。
pub fn icon_data_url(path: &str) -> Option<String> {
    unsafe {
        with_pool(|| {
            let ws = shared_workspace();
            if ws.is_null() {
                return None;
            }
            let cs = std::ffi::CString::new(path).ok()?;
            let path_cf = CFStringCreateWithCString(std::ptr::null(), cs.as_ptr(), UTF8);
            if path_cf.is_null() {
                return None;
            }
            let img = m1(ws, "iconForFile:", path_cf);
            CFRelease(path_cf);
            if img.is_null() {
                return None;
            }
            // 选一个接近目标尺寸（>=128 中最小者，否则最大者）的表示，避免直接 TIFF 出全尺寸（可达 1MB+）。
            let target: usize = 128;
            let mut chosen: Id = std::ptr::null();
            let mut chosen_w: usize = 0;
            let mut best_ge: usize = usize::MAX;
            let mut largest: Id = std::ptr::null();
            let mut largest_w: usize = 0;
            let reps = m0(img, "representations");
            let count = if reps.is_null() { 0 } else { m_sz(reps, "count") };
            for i in 0..count {
                let r = m_at(reps, "objectAtIndex:", i);
                if r.is_null() {
                    continue;
                }
                let w = m_sz(r, "pixelsWide");
                if w == 0 {
                    continue;
                }
                if w > largest_w {
                    largest_w = w;
                    largest = r;
                }
                if w >= target && w < best_ge {
                    best_ge = w;
                    chosen = r;
                    chosen_w = w;
                }
            }
            if chosen.is_null() && !largest.is_null() {
                chosen = largest;
                chosen_w = largest_w;
            }

            // 用选中的表示重建一张按像素尺寸初始化的 NSImage，再取 TIFF→PNG；无可用表示则退回整图 TIFF。
            let src = if chosen.is_null() {
                img
            } else {
                let img_cname = std::ffi::CString::new("NSImage").unwrap_or_default();
                let img_cls = objc_getClass(img_cname.as_ptr());
                let alloc = m0(img_cls, "alloc");
                let sized = m_size(
                    alloc,
                    "initWithSize:",
                    NSSize {
                        w: chosen_w as f64,
                        h: chosen_w as f64,
                    },
                );
                if sized.is_null() {
                    img
                } else {
                    m1(sized, "addRepresentation:", chosen);
                    // alloc/init 归我们所有（+1），autorelease 交回自动释放池在排空时释放，避免泄漏
                    m0(sized, "autorelease")
                }
            };

            let tiff = m0(src, "TIFFRepresentation");
            if tiff.is_null() {
                return None;
            }
            let bmp_cname = std::ffi::CString::new("NSBitmapImageRep").unwrap_or_default();
            let bmp_cls = objc_getClass(bmp_cname.as_ptr());
            if bmp_cls.is_null() {
                return None;
            }
            let rep = m1(bmp_cls, "imageRepWithData:", tiff);
            if rep.is_null() {
                return None;
            }
            // properties 传 nil：PNG 无需额外编码参数
            let png = m_isize_obj(rep, "representationUsingType:properties:", PNG_TYPE, std::ptr::null());
            if png.is_null() {
                return None;
            }
            let bytes = m0(png, "bytes");
            let len = m_sz(png, "length");
            if bytes.is_null() || len == 0 {
                return None;
            }
            let slice = std::slice::from_raw_parts(bytes as *const u8, len);
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(slice);
            Some(format!("data:image/png;base64,{b64}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_is_localized() {
        let n = display_name("/System/Applications/Notes.app");
        eprintln!("display_name = {n:?}");
        assert!(n.is_some(), "应能读到本地化显示名");
    }

    #[test]
    fn icon_is_downscaled_png() {
        let d = icon_data_url("/System/Applications/Notes.app");
        let s = d.expect("应能读到图标 data URL");
        eprintln!("icon base64 len = {}", s.len());
        assert!(s.starts_with("data:image/png;base64,"));
        // 选中 ~128px 表示后 base64 应远小于全尺寸（全尺寸可达 ~860KB）；验证 NSSize 传参 ABI 生效。
        assert!(s.len() < 200_000, "图标过大，initWithSize 降尺寸未生效: {}", s.len());
    }
}
