//! 加密图片/附件：让 `.mdl` 单加密文件与 `.mdlb` 单文件库的插图不再以明文落盘。
//!
//! 策略（方案C）：
//! - **落盘加密**：附件复用 `.mdl` 的 `mlk/1` 容器格式，为每张图生成独立附件 DEK、
//!   用会话 KEK 包裹后加密二进制，写进容器同级的 `.dat` 目录，磁盘名为 `逻辑名.mdat`。
//! - **解锁解密到缓存**：解锁时把 `.dat` 里的 `.mdat` 解密到应用缓存目录
//!   `<cache_root>/<hash(vault_id)>/<dat_dir_name>/<逻辑名>`，预览走缓存明文。
//! - **锁定清缓存**：锁定/退出/自动锁定时删除该库的缓存目录，磁盘不残留明文。
//!
//! `.mdl`（会话 dek=None）与 `.mdlb`（会话 dek=Some）都持有 KEK，附件统一走 KEK 路径。

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use super::error::{CryptoError, Result};
use super::file;
use super::format::{self, EncryptedBody, FileHeader, KdfParams};
use super::io;
use super::keys::{self, Dek, Kek};
use super::session::SessionStore;

/// 附件密文文件的扩展名后缀：逻辑名 `xxx.png` → 磁盘名 `xxx.png.mdat`。
pub const ASSET_EXT: &str = "mdat";

/// 附件目录后缀：文档 `note.mdlb` 同级放 `note.dat`（与前端 `assetAnchorInfo` 同规则）。
pub const DAT_DIR_EXT: &str = "dat";

/// 文档（`.mdl` / `.mdlb`）同级的附件目录绝对路径。
/// 改主密码时按它定位需要重新包裹的附件；目录不存在也没关系（调用方按 `is_dir` 判空）。
pub fn sibling_dat_dir(doc_path: &Path) -> PathBuf {
    let stem = doc_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dir_name = format!("{stem}.{DAT_DIR_EXT}");
    match doc_path.parent() {
        Some(p) => p.join(dir_name),
        None => PathBuf::from(dir_name),
    }
}

/// 由 vault_id 派生确定性的缓存子目录名（十六进制，无需新依赖）。
fn vault_cache_dir(cache_root: &Path, vault_id: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    vault_id.hash(&mut h);
    cache_root.join(format!("{:016x}", h.finish()))
}

/// 只取路径最后一段并剔除 `.`/`..`，防止名字带分隔符逃出目标目录。
fn sanitize_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "未命名".to_string();
    }
    let last = trimmed
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(trimmed);
    let cleaned = last.trim_matches('.');
    if cleaned.is_empty() {
        "未命名".to_string()
    } else {
        cleaned.to_string()
    }
}

/// 在 `dir` 下为逻辑名 `name` 求一个不冲突的名字（以 `name.mdat` 是否存在判断），
/// 冲突时在扩展名前追加 `-1`、`-2`…，返回不含 `.mdat` 的逻辑名。
fn unique_logical_name(dir: &Path, name: &str) -> String {
    let taken = |logical: &str| dir.join(format!("{logical}.{ASSET_EXT}")).exists();
    if !taken(name) {
        return name.to_string();
    }
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|s| s.to_str());
    for i in 1..=9999u32 {
        let candidate = match ext {
            Some(e) if !e.is_empty() => format!("{stem}-{i}.{e}"),
            _ => format!("{stem}-{i}"),
        };
        if !taken(&candidate) {
            return candidate;
        }
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{stem}-{ts}")
}

/// 用会话 KEK 加密一段二进制，产出 `mlk/1` 容器字节（逻辑同 `create_encrypted`，明文换成二进制）。
pub fn encrypt_asset(kek: &Kek, kdf: &KdfParams, bytes: &[u8]) -> Result<Vec<u8>> {
    let dek = Dek(*keys::generate_dek());
    let wrapped_dek = dek.wrap(kek)?;
    let header = FileHeader {
        format: format::FORMAT.to_string(),
        kdf: kdf.clone(),
        cipher: "aes-256-gcm".to_string(),
        wrapped_dek,
        hint: None,
    };
    let (nonce, ct) = keys::encrypt_body(&dek, bytes)?;
    let body = EncryptedBody { nonce, ct };
    file::serialize(&header, &body)
}

/// 用会话 KEK 解开一个附件容器，返回明文二进制。
pub fn decrypt_asset(kek: &Kek, bytes: &[u8]) -> Result<Vec<u8>> {
    let (header, rest) = file::parse_header(bytes)?;
    let body = file::parse_body(rest)?;
    let dek = Dek::unwrap(&header.wrapped_dek, kek)?;
    keys::decrypt_body(&dek, &body.nonce, &body.ct)
}

/// 改主密码后同步重包裹同级 `.dat` 里的所有附件，返回重包裹的数量。
///
/// **必须随改密码一起调用**：附件的小 DEK 是用会话 KEK 包裹的，改密码会把会话 KEK 换成
/// 新派生的那把，若不动附件头，历史图片就永久解不开了（且失败很安静——只是图不显示）。
/// 这里只换头部的 `wrapped_dek`，密文数据区原样搬过去，不解密图片本身，开销与图片大小无关。
///
/// 顺序上先全部算完新字节再写盘：万一某个文件写失败，改密流程会整体报错中止，
/// 容器仍是旧 KEK 的，未写成功的附件也仍是旧 KEK 的，不会出现"两边钥匙不同"。
/// 用旧 KEK 解不开的条目跳过（本就锁不上这把钥匙，不是本库刚改密的对象）。
pub fn rewrap_assets(dat_dir: &Path, old_kek: &Kek, new_kek: &Kek) -> Result<usize> {
    if !dat_dir.is_dir() {
        return Ok(0);
    }
    let mdat_suffix = format!(".{ASSET_EXT}");
    let mut pending: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for entry in fs::read_dir(dat_dir).map_err(|e| CryptoError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CryptoError::Io(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if io::is_sidecar(&name) || !name.ends_with(&mdat_suffix) {
            continue;
        }
        let path = entry.path();
        let bytes = match io::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let (mut header, rest) = match file::parse_header(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let dek = match Dek::unwrap(&header.wrapped_dek, old_kek) {
            Ok(d) => d,
            Err(_) => continue,
        };
        header.wrapped_dek = dek.rewrap(new_kek)?;
        // 数据区不重新加密：按原 nonce/ct 序列化回去
        let body = file::parse_body(rest)?;
        pending.push((path, file::serialize(&header, &body)?));
    }
    let done = pending.len();
    for (path, out) in pending {
        io::atomic_write(&path, &out)?;
    }
    Ok(done)
}

/// 加密写入一个附件到 `dest_dir/<逻辑名>.mdat`，返回不含 `.mdat` 的逻辑名（供拼 markdown 链接）。
pub fn write_encrypted_asset(
    store: &SessionStore,
    vault_id: &str,
    dest_dir: &str,
    name: &str,
    bytes: &[u8],
) -> Result<String> {
    let name = sanitize_name(name);
    let dir = Path::new(dest_dir);
    fs::create_dir_all(dir).map_err(|e| CryptoError::Io(e.to_string()))?;
    let logical = unique_logical_name(dir, &name);
    let out = store.with_vault(vault_id, |vault| encrypt_asset(&vault.kek, &vault.kdf, bytes))?;
    io::atomic_write(&dir.join(format!("{logical}.{ASSET_EXT}")), &out)?;
    Ok(logical)
}

/// 一次解密到缓存的结果：缓存根目录 + 未能解密的附件数。
///
/// `failed` 要透到前端提示：附件解不开只会表现为"图不显示"，不报错的话用户无从知道
/// 是该重新导入图片，还是文件真的坏了。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Materialized {
    /// 该库的附件缓存根目录（前端 assetBaseDir）
    pub cache_dir: String,
    /// 本轮解密失败的附件数量（其余照常可用）
    pub failed: usize,
}

/// 缓存里的明文是否已经不旧于源密文（用于跳过重复解密）。
///
/// 粘贴 / 拖入每次都重新解密整个 `.dat` 会随图片数量线性变慢；`.mdat` 由原子写落盘，
/// 时间戳即写入时刻，缓存文件是其后解密写出的，故「缓存不旧于源」即可安全跳过。
fn cache_is_fresh(src: &Path, cached: &Path) -> bool {
    match (src.metadata(), cached.metadata()) {
        (Ok(a), Ok(b)) => match (a.modified(), b.modified()) {
            (Ok(am), Ok(bm)) => bm >= am,
            _ => false,
        },
        _ => false,
    }
}

/// 解锁后把 `.dat` 里的附件解密到缓存，返回该库的缓存根目录（前端 assetBaseDir）。
///
/// 缓存布局：`<cache_root>/<hash(vault_id)>/<dat_dir_name>/<逻辑名>`，与 markdown 里
/// 相对链接 `dat_dir_name/逻辑名` 拼接后精确命中。历史遗留的明文图片（无同名 `.mdat`）
/// 直接复制到缓存，保证向后兼容。
///
/// 单个附件读失败/解不开只跳过它自己（计入 `failed`），不让整库预览跟着崩：
/// 会话丢失或某个文件损坏时，此前会让整个目录返回 Err，前端静默 catch 之后所有图片一起消失。
pub fn materialize_assets(
    store: &SessionStore,
    vault_id: &str,
    dat_dir: &str,
    cache_root: &str,
    dat_dir_name: &str,
) -> Result<Materialized> {
    let vault_cache = vault_cache_dir(Path::new(cache_root), vault_id);
    let target_dir = vault_cache.join(dat_dir_name);
    fs::create_dir_all(&target_dir).map_err(|e| CryptoError::Io(e.to_string()))?;

    let report = |failed: usize| Materialized {
        cache_dir: vault_cache.to_string_lossy().to_string(),
        failed,
    };

    let dat = Path::new(dat_dir);
    if !dat.is_dir() {
        return Ok(report(0));
    }
    // 会话缺失（未解锁/已过期/已锁定）是状态问题而非附件损坏：整体报 NotUnlocked，
    // 前端 catch 后静默跳过；否则每条密文都会走解密失败，被误报成「N 个附件无法解密」
    if !store.contains(vault_id) {
        return Err(CryptoError::NotUnlocked);
    }
    let mut failed = 0usize;
    let mdat_suffix = format!(".{ASSET_EXT}");
    for entry in fs::read_dir(dat).map_err(|e| CryptoError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CryptoError::Io(e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // 跳过原子写盘旁路文件与隐藏文件
        if io::is_sidecar(&name) || name.starts_with('.') {
            continue;
        }
        if let Some(logical) = name.strip_suffix(&mdat_suffix) {
            let cached = target_dir.join(logical);
            if cache_is_fresh(&path, &cached) {
                continue;
            }
            // 单条解不开（损坏/异主密码）只跳过它自己并计入 failed；会话缺失已在上面整体拦截
            let plain = store.with_vault(vault_id, |vault| {
                let bytes = io::read(&path).map_err(|e| CryptoError::Io(e.to_string()))?;
                decrypt_asset(&vault.kek, &bytes)
            });
            match plain {
                Ok(bytes) => {
                    if fs::write(&cached, &bytes).is_err() {
                        failed += 1;
                    }
                }
                Err(_) => failed += 1,
            }
        } else if !dat.join(format!("{name}.{ASSET_EXT}")).exists() {
            // 历史明文图片：无对应 .mdat 时直接复制（兼容旧数据）
            let cached = target_dir.join(&name);
            if cache_is_fresh(&path, &cached) {
                continue;
            }
            if fs::copy(&path, &cached).is_err() {
                failed += 1;
            }
        }
    }
    Ok(report(failed))
}

/// 删除某库的附件缓存目录（锁定/退出/自动锁定时调用；幂等，忽略不存在）。
pub fn clear_asset_cache(vault_id: &str, cache_root: &str) {
    let dir = vault_cache_dir(Path::new(cache_root), vault_id);
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::{derive_kek_default, generate_salt};
    use crate::crypto::session::{UnlockedVault, DEFAULT_AUTO_LOCK};
    use std::time::Instant;

    fn uniq_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("marklock_asset_{tag}_{}_{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup_store(id: &str) -> SessionStore {
        let salt = generate_salt();
        let kek = derive_kek_default("pw", &salt).unwrap();
        let kdf = KdfParams {
            salt: format::b64::encode(&salt),
            ..Default::default()
        };
        let store = SessionStore::new();
        store.insert(
            id.to_string(),
            UnlockedVault {
                kek,
                kdf,
                dek: None,
                last_activity: Instant::now(),
                auto_lock: DEFAULT_AUTO_LOCK,
            },
        );
        store
    }

    #[test]
    fn encrypt_decrypt_roundtrip_and_tamper() {
        let salt = generate_salt();
        let kek = derive_kek_default("pw", &salt).unwrap();
        let kdf = KdfParams {
            salt: format::b64::encode(&salt),
            ..Default::default()
        };
        let img = vec![0x89u8, 0x50, 0x4E, 0x47, 0x00, 0xFF, 0x10];
        let ct = encrypt_asset(&kek, &kdf, &img).unwrap();
        assert_eq!(decrypt_asset(&kek, &ct).unwrap(), img);

        // 篡改密文数据区应解密失败
        let mut bad = ct.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        assert!(decrypt_asset(&kek, &bad).is_err());
    }

    #[test]
    fn write_materialize_clear_cycle() {
        let id = "vault-id-a";
        let store = setup_store(id);
        let dat = uniq_dir("dat");
        let cache = uniq_dir("cache");

        let img = b"\x89PNG-fake-bytes".to_vec();
        let logical = write_encrypted_asset(&store, id, &dat.to_string_lossy(), "pic.png", &img).unwrap();
        assert_eq!(logical, "pic.png");
        // 磁盘上是 .mdat 密文，且非明文
        let disk = dat.join("pic.png.mdat");
        assert!(disk.exists());
        assert_ne!(fs::read(&disk).unwrap(), img);

        // 重名自动加序号
        let logical2 = write_encrypted_asset(&store, id, &dat.to_string_lossy(), "pic.png", &img).unwrap();
        assert_eq!(logical2, "pic-1.png");

        // materialize 到缓存
        let root = materialize_assets(&store, id, &dat.to_string_lossy(), &cache.to_string_lossy(), "note.dat").unwrap();
        let cached = Path::new(&root.cache_dir).join("note.dat").join("pic.png");
        assert_eq!(fs::read(&cached).unwrap(), img);
        assert_eq!(root.failed, 0);

        // 清缓存后目录消失
        clear_asset_cache(id, &cache.to_string_lossy());
        assert!(!Path::new(&root.cache_dir).exists());

        let _ = fs::remove_dir_all(&dat);
        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn materialize_copies_legacy_plaintext() {
        let id = "vault-id-b";
        let store = setup_store(id);
        let dat = uniq_dir("legacy_dat");
        let cache = uniq_dir("legacy_cache");
        // 历史明文图片（无 .mdat）
        fs::write(dat.join("old.png"), b"legacy-plain").unwrap();

        let root = materialize_assets(&store, id, &dat.to_string_lossy(), &cache.to_string_lossy(), "n.dat").unwrap();
        let cached = Path::new(&root.cache_dir).join("n.dat").join("old.png");
        assert_eq!(fs::read(&cached).unwrap(), b"legacy-plain");

        let _ = fs::remove_dir_all(&dat);
        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn rewrap_assets_follows_password_change() {
        let dat = uniq_dir("rewrap_dat");
        let salt_old = generate_salt();
        let old_kek = derive_kek_default("pw", &salt_old).unwrap();
        let kdf = KdfParams {
            salt: format::b64::encode(&salt_old),
            ..Default::default()
        };
        let salt_new = generate_salt();
        let new_kek = derive_kek_default("pw2", &salt_new).unwrap();

        let img = b"\x89PNG-rewrap".to_vec();
        fs::write(dat.join("pic.png.mdat"), encrypt_asset(&old_kek, &kdf, &img).unwrap()).unwrap();
        // 不是容器结构的条目应被跳过，而不是让改密整体失败
        fs::write(dat.join("junk.png.mdat"), b"not-an-mlk-container").unwrap();

        assert_eq!(rewrap_assets(&dat, &old_kek, &new_kek).unwrap(), 1);

        let after = fs::read(dat.join("pic.png.mdat")).unwrap();
        assert_eq!(decrypt_asset(&new_kek, &after).unwrap(), img);
        assert!(decrypt_asset(&old_kek, &after).is_err());
        // 跳过的那条原样留在磁盘上
        assert_eq!(fs::read(dat.join("junk.png.mdat")).unwrap(), b"not-an-mlk-container");

        let _ = fs::remove_dir_all(&dat);
    }

    #[test]
    fn materialize_skips_broken_asset_and_keeps_the_rest() {
        let id = "vault-id-c";
        let store = setup_store(id);
        let dat = uniq_dir("broken_dat");
        let cache = uniq_dir("broken_cache");

        let img = b"\x89PNG-good".to_vec();
        write_encrypted_asset(&store, id, &dat.to_string_lossy(), "good.png", &img).unwrap();
        fs::write(dat.join("broken.png.mdat"), b"garbage").unwrap();

        let r = materialize_assets(&store, id, &dat.to_string_lossy(), &cache.to_string_lossy(), "n.dat").unwrap();
        let good = Path::new(&r.cache_dir).join("n.dat").join("good.png");
        assert_eq!(fs::read(&good).unwrap(), img);
        assert_eq!(r.failed, 1);
        assert!(!Path::new(&r.cache_dir).join("n.dat").join("broken.png").exists());

        // 第二轮：缓存已是最新的条目直接跳过，不再重复解密重写
        let first_mtime = fs::metadata(&good).unwrap().modified().unwrap();
        let r2 = materialize_assets(&store, id, &dat.to_string_lossy(), &cache.to_string_lossy(), "n.dat").unwrap();
        assert_eq!(fs::metadata(&good).unwrap().modified().unwrap(), first_mtime);
        // 坏的那条依旧解不开，数量不变
        assert_eq!(r2.failed, 1);

        let _ = fs::remove_dir_all(&dat);
        let _ = fs::remove_dir_all(&cache);
    }
}
