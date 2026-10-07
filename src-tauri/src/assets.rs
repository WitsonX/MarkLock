//! 资源（图片/文件）落盘辅助：把粘贴或拖入的文件放进「同级 `.dat` 目录」，
//! 负责建目录、重名自动加序号、二进制写入与整目录递归复制。
//!
//! 与加密核心无关，纯文件系统操作，故所有平台均编译。返回的最终绝对路径由前端
//! 拼成 markdown 链接插入正文；预览再通过 asset 协议读取这些文件。
//! 错误统一复用 [`crypto::CryptoError::Io`]，与其它命令一致按字符串上抛。

use std::path::{Path, PathBuf};

use crate::crypto::CryptoError;

type Result<T> = std::result::Result<T, CryptoError>;

fn io_err(e: std::io::Error) -> CryptoError {
    CryptoError::Io(e.to_string())
}

/// 只取路径的最后一段，去掉任何目录成分（防止名字里带 `/` 逃出目标目录）。
fn sanitize_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "未命名".to_string();
    }
    // 同时按两种分隔符切，取末段（Windows 粘贴名可能带反斜杠）
    let last = trimmed
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(trimmed);
    // 进一步剔除连续点开头（`..`）等非法段
    let cleaned = last.trim_matches('.');
    if cleaned.is_empty() {
        "未命名".to_string()
    } else {
        cleaned.to_string()
    }
}

/// 在 `dir` 下为 `name` 求一个不冲突的路径：已存在则在扩展名前追加 `-1`、`-2`…
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str());
    for i in 1..=9999u32 {
        let candidate = match ext {
            Some(e) if !e.is_empty() => format!("{stem}-{i}.{e}"),
            _ => format!("{stem}-{i}"),
        };
        let cand_path = dir.join(&candidate);
        if !cand_path.exists() {
            return cand_path;
        }
    }
    // 极端情况下用时间戳兜底
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    dir.join(format!("{stem}-{ts}"))
}

/// 递归复制一个目录（`src` → `dst`），已存在的同名文件按 unique 规则改名。
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(io_err)?;
    let entries = std::fs::read_dir(src).map_err(io_err)?;
    for entry in entries {
        let entry = entry.map_err(io_err)?;
        let name = entry.file_name();
        let src_child = entry.path();
        let dst_child = dst.join(&name);
        if src_child.is_dir() {
            copy_dir_all(&src_child, &dst_child)?;
        } else {
            let final_dst = if dst_child.exists() {
                let n = name.to_string_lossy().to_string();
                unique_path(dst, &n)
            } else {
                dst_child
            };
            std::fs::copy(&src_child, &final_dst).map_err(io_err)?;
        }
    }
    Ok(())
}

/// 把一个源路径（文件或目录）复制进 `dest_dir`，重名自动加序号，返回最终绝对路径。
///
/// 用于「拖入 → 复制到资源目录」。目录走递归复制。
pub fn copy_into(dest_dir: &str, src: &str) -> Result<String> {
    let src_path = Path::new(src);
    let name = src_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| CryptoError::Io("源路径无文件名".into()))?;
    let name = sanitize_name(&name);
    std::fs::create_dir_all(dest_dir).map_err(io_err)?;
    let dir = Path::new(dest_dir);
    if src_path.is_dir() {
        // 目录：整体复制，重名则在目录名后加序号
        let target = unique_path(dir, &name);
        copy_dir_all(src_path, &target)?;
        return target
            .to_str()
            .map(|s| s.to_string())
            .ok_or_else(|| CryptoError::Io("路径含非法字符".into()));
    }
    let target = unique_path(dir, &name);
    std::fs::copy(src_path, &target).map_err(io_err)?;
    target
        .to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| CryptoError::Io("路径含非法字符".into()))
}

/// 把 base64 二进制内容写入 `dest_dir/<name>`（重名自动加序号），返回最终绝对路径。
///
/// 用于「粘贴截图/剪贴板图片」：WKWebView 能给出图片字节但没有磁盘路径，前端读出
/// base64 交给这里落盘。`base64_data` 允许是完整 data URL（`data:…;base64,XXXX`），
/// 此时按第一个逗号剥离前面的 metadata（data URL 的 metadata 段不含逗号）。
pub fn write_bytes(dest_dir: &str, name: &str, base64_data: &str) -> Result<String> {
    use base64::Engine;
    let payload = match base64_data.strip_prefix("data:") {
        Some(rest) => match rest.find(',') {
            Some(idx) => &rest[idx + 1..],
            None => base64_data,
        },
        None => base64_data,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| CryptoError::Io(format!("base64 解码失败: {e}")))?;
    let name = sanitize_name(name);
    std::fs::create_dir_all(dest_dir).map_err(io_err)?;
    let target = unique_path(Path::new(dest_dir), &name);
    std::fs::write(&target, &bytes).map_err(io_err)?;
    target
        .to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| CryptoError::Io("路径含非法字符".into()))
}

/// 读取任意本地文件为 data URL（`data:<mime;base64,…>`），供导出 HTML 内联图片等资源。
///
/// MIME 按扩展名推断，未知统一 `application/octet-stream`。
pub fn read_data_url(path: &str) -> Result<String> {
    use base64::Engine;
    let bytes = std::fs::read(path).map_err(io_err)?;
    let mime = match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
    {
        Some(ref e) if e == "png" => "image/png",
        Some(ref e) if e == "jpg" || e == "jpeg" => "image/jpeg",
        Some(ref e) if e == "gif" => "image/gif",
        Some(ref e) if e == "webp" => "image/webp",
        Some(ref e) if e == "bmp" => "image/bmp",
        Some(ref e) if e == "svg" => "image/svg+xml",
        Some(ref e) if e == "avif" => "image/avif",
        Some(ref e) if e == "ico" => "image/x-icon",
        Some(ref e) if e == "tiff" || e == "tif" => "image/tiff",
        _ => "application/octet-stream",
    };
    Ok(format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_dirs() {
        assert_eq!(sanitize_name("/a/b/c.png"), "c.png");
        assert_eq!(sanitize_name(".."), "未命名");
        assert_eq!(sanitize_name(""), "未命名");
    }

    #[test]
    fn unique_adds_suffix_on_collision() {
        let dir = std::env::temp_dir().join(format!("marklock-asset-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p1 = unique_path(&dir, "x.png");
        std::fs::write(&p1, b"1").unwrap();
        let p2 = unique_path(&dir, "x.png");
        assert_ne!(p1, p2);
        assert_eq!(p2.file_name().unwrap(), "x-1.png");
        std::fs::write(&p2, b"2").unwrap();
        let p3 = unique_path(&dir, "x.png");
        assert_eq!(p3.file_name().unwrap(), "x-2.png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_bytes_strips_dataurl_prefix() {
        let dir = std::env::temp_dir().join(format!("marklock-asset-wb-{}", std::process::id()));
        // "hi" -> aGk=
        let out = write_bytes(&dir.to_string_lossy(), "note.txt", "data:text/plain;base64,aGk=").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "hi");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_data_url_encodes_extension_mime() {
        let dir = std::env::temp_dir().join(format!("marklock-asset-rd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        std::fs::write(&p, b"hi").unwrap();
        let url = read_data_url(&p.to_string_lossy()).unwrap();
        assert_eq!(url, "data:image/png;base64,aGk=");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
