//! 库级加密操作：把密钥原语组合成可用的业务流程。
//!
//! 库有两种形态，统一走 [`SessionStore`] 维护解锁会话：
//! - **库 = 目录**：目录根有 `vault.json`（存 KDF 参数 + 校验探针），
//!   目录内所有 `.mdl` 文件共享同一个主密码（同一 salt ⇒ 同一 KEK）。
//! - **库 = 单个 `.mdl` 文件**：文件头自带 KDF 参数与 wrapped_dek，独立密码。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::error::{CryptoError, Result};
use super::file;
use super::format::{self, EncryptedBody, FileHeader, VaultMeta};
use super::keys::{self, Dek, Kek};
use super::session::{SessionStore, UnlockedVault, DEFAULT_AUTO_LOCK};

/// 密码强度检查（与前端强度条大致对齐，Rust 侧做兜底校验）。
fn check_password_strength(pwd: &str) -> Result<()> {
    if pwd.chars().count() < 1 {
        return Err(CryptoError::WeakPassword("密码不能为空".into()));
    }
    Ok(())
}

// ==================== 单文件 .mdl ====================

/// 创建一个新的加密文件（`.mdl`）。
///
/// 返回写入的字节。调用方决定落盘路径。
pub fn create_encrypted(
    password: &str,
    plaintext: &str,
    hint: Option<String>,
) -> Result<Vec<u8>> {
    check_password_strength(password)?;

    // 生成 salt 与 KDF 参数（salt 必须与派生 KEK 用的一致，并写入文件头）
    let kdf_params = keys::new_kdf_params();
    let salt = format::b64::decode(&kdf_params.salt)?;
    let kek = keys::derive_kek(password, &salt, kdf_params.m, kdf_params.t, kdf_params.p)?;
    let dek = Dek(*keys::generate_dek());

    // 用 KEK 包裹 DEK
    let wrapped_dek = dek.wrap(&kek)?;

    let header = FileHeader {
        format: format::FORMAT.to_string(),
        kdf: kdf_params,
        cipher: "aes-256-gcm".to_string(),
        wrapped_dek,
        hint,
    };

    // 用 DEK 加密正文
    let (nonce, ct) = keys::encrypt_body(&dek, plaintext.as_bytes())?;
    let body = EncryptedBody { nonce, ct };

    file::serialize(&header, &body)
}

/// 解锁一个加密文件：读文件、派生 KEK、解开 DEK 并校验密码正确性，
/// 成功后在会话表中登记（供后续读写、自动锁定使用）。
///
/// `vault_id` 是会话标识（通常用文件绝对路径）。`auto_lock` 传入 None 用默认 5 分钟。
pub fn unlock_file(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    password: &str,
    auto_lock: Option<Duration>,
) -> Result<()> {
    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    let (header, _rest) = file::parse_header(&bytes)?;

    let salt = format::b64::decode(&header.kdf.salt)?;
    let kek = keys::derive_kek(&password, &salt, header.kdf.m, header.kdf.t, header.kdf.p)?;

    // 用 KEK 解开 DEK，失败即密码错误（GCM 认证失败）
    let _dek = Dek::unwrap(&header.wrapped_dek, &kek)?;

    store.insert(
        vault_id.to_string(),
        UnlockedVault {
            kek,
            kdf: header.kdf,
            dek: None,
            last_activity: std::time::Instant::now(),
            auto_lock: auto_lock.unwrap_or(DEFAULT_AUTO_LOCK),
        },
    );
    Ok(())
}

// ==================== 库 = 目录 ====================

/// 创建一个新的加密库（目录）。
///
/// 在 `dir` 下创建 `vault.json` 元数据（KDF 参数 + 校验探针）。
/// `dir` 若不存在会创建。
pub fn create_vault_dir(
    dir: &Path,
    password: &str,
    hint: Option<String>,
) -> Result<()> {
    check_password_strength(password)?;
    fs::create_dir_all(dir).map_err(|e| CryptoError::Io(e.to_string()))?;

    let meta = VaultMeta::new(password, hint)?;
    let json = serde_json::to_vec_pretty(&meta)
        .map_err(|e| CryptoError::BadHeader(format!("序列化库元数据失败: {e}")))?;
    let meta_path = dir.join(format::VAULT_META_FILE);
    fs::write(&meta_path, json).map_err(|e| CryptoError::Io(e.to_string()))
}

/// 解锁一个加密库目录。
///
/// 读取 `vault.json`，用密码派生 KEK 并解开校验探针验证密码，
/// 成功后登记会话（vault_id 通常为目录绝对路径）。
pub fn unlock_vault_dir(
    store: &SessionStore,
    vault_id: &str,
    dir: &Path,
    password: &str,
    auto_lock: Option<Duration>,
) -> Result<()> {
    let meta = read_vault_meta(dir)?;

    let salt = format::b64::decode(&meta.kdf.salt)?;
    let kek = keys::derive_kek(&password, &salt, meta.kdf.m, meta.kdf.t, meta.kdf.p)?;

    // 解开校验探针，失败即密码错误
    let plain = kek.open_probe(&meta.probe)?;
    if plain != format::VAULT_PROBE.as_bytes() {
        return Err(CryptoError::WrongPassword);
    }

    store.insert(
        vault_id.to_string(),
        UnlockedVault {
            kek,
            kdf: meta.kdf,
            dek: None,
            last_activity: std::time::Instant::now(),
            auto_lock: auto_lock.unwrap_or(DEFAULT_AUTO_LOCK),
        },
    );
    Ok(())
}

/// 读取库元数据（不校验密码）。
pub fn read_vault_meta(dir: &Path) -> Result<VaultMeta> {
    let meta_path = dir.join(format::VAULT_META_FILE);
    let json = fs::read(&meta_path).map_err(|e| CryptoError::Io(e.to_string()))?;
    serde_json::from_slice(&json)
        .map_err(|e| CryptoError::BadHeader(format!("库元数据解析失败: {e}")))
}

/// 在已解锁的库目录内创建一个新的加密文件。
///
/// 复用库的 KEK 与 KDF（同一 salt），新文件的 DEK 由库 KEK 包裹。
/// `rel_path` 是相对库根的文件路径（如 `项目/产品方案.md`），确保父目录存在。
pub fn create_file_in_vault(
    store: &SessionStore,
    vault_id: &str,
    rel_path: &str,
    plaintext: &str,
) -> Result<()> {
    store.with_vault(vault_id, |vault| {
        // 复用库的 salt 派生参数，DEK 由库 KEK 包裹
        let dek = Dek(*keys::generate_dek());
        let wrapped_dek = dek.wrap(&vault.kek)?;

        let header = FileHeader {
            format: format::FORMAT.to_string(),
            kdf: vault.kdf.clone(),
            cipher: "aes-256-gcm".to_string(),
            wrapped_dek,
            hint: None,
        };
        let (nonce, ct) = keys::encrypt_body(&dek, plaintext.as_bytes())?;
        let body = EncryptedBody { nonce, ct };
        let bytes = file::serialize(&header, &body)?;

        // 拼接库根路径：vault_id 即目录绝对路径
        let mut full = PathBuf::from(vault_id);
        full.push(rel_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|e| CryptoError::Io(e.to_string()))?;
        }
        fs::write(&full, bytes).map_err(|e| CryptoError::Io(e.to_string()))
    })
}

/// 在已解锁的库目录内创建一个空文件夹。
///
/// `rel_path` 是相对库根的目录路径（如 `项目/文档`），会递归创建父目录。
pub fn create_folder_in_vault(
    store: &SessionStore,
    vault_id: &str,
    rel_path: &str,
) -> Result<()> {
    store.with_vault(vault_id, |_vault| {
        let mut full = PathBuf::from(vault_id);
        full.push(rel_path);
        fs::create_dir_all(&full).map_err(|e| CryptoError::Io(e.to_string()))
    })
}

/// 删除库内的一个文件或空/非空目录（`rel_path` 相对库根）。
///
/// 安全校验：拒绝删除库根、`vault.json`、以及任何试图跳出库根的路径。
/// 删除只是文件系统操作，不涉及解密（密文随文件一起删除）。
pub fn delete_in_vault(
    store: &SessionStore,
    vault_id: &str,
    rel_path: &str,
) -> Result<()> {
    let full = resolve_vault_path(vault_id, rel_path)?;
    // 禁止删除库根或元数据文件
    if full == PathBuf::from(vault_id) {
        return Err(CryptoError::Io("不能删除库根目录".into()));
    }
    if full.file_name().and_then(|n| n.to_str()) == Some(format::VAULT_META_FILE) {
        return Err(CryptoError::Io("不能删除库元数据文件".into()));
    }
    store.with_vault(vault_id, |_vault| {
        if full.is_dir() {
            fs::remove_dir_all(&full).map_err(|e| CryptoError::Io(e.to_string()))
        } else {
            fs::remove_file(&full).map_err(|e| CryptoError::Io(e.to_string()))
        }
    })
}

/// 重命名 / 移动库内的文件或目录（`old_rel` → `new_rel`，均相对库根）。
///
/// 移动与重命名本质都是改路径，密文不变。若目标父目录不存在则自动创建。
pub fn rename_in_vault(
    store: &SessionStore,
    vault_id: &str,
    old_rel: &str,
    new_rel: &str,
) -> Result<()> {
    let src = resolve_vault_path(vault_id, old_rel)?;
    let dst = resolve_vault_path(vault_id, new_rel)?;
    // 禁止移动库根 / 元数据文件
    if src == PathBuf::from(vault_id) {
        return Err(CryptoError::Io("不能移动库根目录".into()));
    }
    if src.file_name().and_then(|n| n.to_str()) == Some(format::VAULT_META_FILE) {
        return Err(CryptoError::Io("不能移动库元数据文件".into()));
    }
    store.with_vault(vault_id, |_vault| {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| CryptoError::Io(e.to_string()))?;
        }
        fs::rename(&src, &dst).map_err(|e| CryptoError::Io(e.to_string()))
    })
}

/// 把相对库根的路径解析为绝对路径，并确保它不会跳出库根目录（防路径穿越）。
fn resolve_vault_path(vault_id: &str, rel_path: &str) -> Result<PathBuf> {
    let rel = rel_path.trim();
    // 拒绝绝对路径或含 `..` 的路径，杜绝路径穿越
    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err(CryptoError::Io("路径超出库范围".into()));
    }
    if rel.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(CryptoError::Io("路径超出库范围".into()));
    }
    let root = PathBuf::from(vault_id);
    let full = root.join(rel);
    Ok(full)
}

/// 读取并解密一个已解锁文件的正文。
///
/// 对目录库：用库 KEK 解开该文件头里的 wrapped_dek（文件头的 salt 应与库一致）。
/// 对单文件库：同上，但会话 KEK 即文件自身的 KEK。
pub fn read_file(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    let (header, rest) = file::parse_header(&bytes)?;
    let body = file::parse_body(rest)?;

    store.with_vault(vault_id, |vault| {
        // 从会话中的 KEK 解出 DEK
        let dek = Dek::unwrap(&header.wrapped_dek, &vault.kek)?;
        let plain = keys::decrypt_body(&dek, &body.nonce, &body.ct)?;
        String::from_utf8(plain).map_err(|_| CryptoError::BadHeader("正文不是合法 UTF-8".into()))
    })
}

/// 加密并写回一个文件（覆盖原路径）。
///
/// 复用库既有的 KEK/DEK，不重新派生。要求已解锁。
pub fn write_file(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    plaintext: &str,
) -> Result<()> {
    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    let (header, _rest) = file::parse_header(&bytes)?;

    let out = store.with_vault(vault_id, |vault| {
        let dek = Dek::unwrap(&header.wrapped_dek, &vault.kek)?;
        let (nonce, ct) = keys::encrypt_body(&dek, plaintext.as_bytes())?;
        let body = EncryptedBody { nonce, ct };
        file::serialize(&header, &body)
    })?;

    fs::write(path, out).map_err(|e| CryptoError::Io(e.to_string()))
}

/// 修改库主密码：用旧密码验证，用新密码重包裹 DEK 并写回。
///
/// 对目录库：重包裹 `vault.json` 的探针 + 目录内所有 `.mdl` 的 wrapped_dek。
/// 对单文件：只重包裹该文件的 wrapped_dek。
/// 关键：只重包裹 DEK，**不重加密正文**。
pub fn change_password(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    old_password: &str,
    new_password: &str,
) -> Result<()> {
    check_password_strength(new_password)?;

    // 判断是目录库还是单文件库
    if path.is_dir() {
        change_vault_dir_password(store, vault_id, path, old_password, new_password)
    } else {
        change_file_password(store, vault_id, path, old_password, new_password)
    }
}

/// 改单文件主密码（重包裹该文件 DEK）。
fn change_file_password(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    old_password: &str,
    new_password: &str,
) -> Result<()> {
    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    let (mut header, rest) = file::parse_header(&bytes)?;

    let salt = format::b64::decode(&header.kdf.salt)?;
    let old_kek = keys::derive_kek(&old_password, &salt, header.kdf.m, header.kdf.t, header.kdf.p)?;
    let dek = Dek::unwrap(&header.wrapped_dek, &old_kek)?;

    // 用新密码派生新 KEK，重包裹 DEK
    let new_kek = keys::derive_kek(&new_password, &salt, header.kdf.m, header.kdf.t, header.kdf.p)?;
    header.wrapped_dek = dek.rewrap(&new_kek)?;

    // 更新会话（KEK 换成新的，沿用原自动锁定时长）
    store.insert(
        vault_id.to_string(),
        UnlockedVault {
            kek: new_kek,
            kdf: header.kdf.clone(),
            dek: None,
            last_activity: std::time::Instant::now(),
            auto_lock: store.current_auto_lock(vault_id),
        },
    );

    // 重新序列化：头变了，数据区原样保留
    let body = file::parse_body(rest)?;
    let out = file::serialize(&header, &body)?;
    fs::write(path, out).map_err(|e| CryptoError::Io(e.to_string()))
}

/// 改目录库主密码：重包裹探针 + 所有 .mdl 文件的 wrapped_dek。
fn change_vault_dir_password(
    store: &SessionStore,
    vault_id: &str,
    dir: &Path,
    old_password: &str,
    new_password: &str,
) -> Result<()> {
    let meta = read_vault_meta(dir)?;

    let salt = format::b64::decode(&meta.kdf.salt)?;
    let old_kek = keys::derive_kek(&old_password, &salt, meta.kdf.m, meta.kdf.t, meta.kdf.p)?;
    // 先验证旧密码
    let probe_plain = old_kek.open_probe(&meta.probe)?;
    if probe_plain != format::VAULT_PROBE.as_bytes() {
        return Err(CryptoError::WrongPassword);
    }

    let new_kek = keys::derive_kek(&new_password, &salt, meta.kdf.m, meta.kdf.t, meta.kdf.p)?;

    // 1) 重包裹探针
    let new_probe = new_kek.seal_probe()?;

    // 2) 遍历目录内所有 .mdl，重包裹各自的 DEK
    rewrite_dir_deks(dir, &old_kek, &new_kek)?;

    // 3) 写回新元数据
    let new_meta = VaultMeta {
        format: meta.format,
        kdf: meta.kdf,
        cipher: meta.cipher,
        probe: new_probe,
        hint: meta.hint,
    };
    let json = serde_json::to_vec_pretty(&new_meta)
        .map_err(|e| CryptoError::BadHeader(format!("序列化库元数据失败: {e}")))?;
    fs::write(dir.join(format::VAULT_META_FILE), json)
        .map_err(|e| CryptoError::Io(e.to_string()))?;

    // 4) 更新会话（沿用原自动锁定时长）
    store.insert(
        vault_id.to_string(),
        UnlockedVault {
            kek: new_kek,
            kdf: new_meta.kdf,
            dek: None,
            last_activity: std::time::Instant::now(),
            auto_lock: store.current_auto_lock(vault_id),
        },
    );
    Ok(())
}

/// 递归遍历目录内所有加密文件，用旧 KEK 解出 DEK、新 KEK 重包裹并写回。
///
/// 库内所有普通文件（除 `vault.json`）都是加密的 `.mdl` 内容，
/// 与扩展名无关（用户可能用 `.md` / `.txt` 等作为 markdown 文件）。
fn rewrite_dir_deks(dir: &Path, old_kek: &Kek, new_kek: &Kek) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| CryptoError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CryptoError::Io(e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == format::VAULT_META_FILE {
            continue;
        }
        if path.is_dir() {
            rewrite_dir_deks(&path, old_kek, new_kek)?;
        } else {
            let bytes = fs::read(&path).map_err(|e| CryptoError::Io(e.to_string()))?;
            let (mut header, rest) = file::parse_header(&bytes)?;
            let dek = Dek::unwrap(&header.wrapped_dek, old_kek)?;
            header.wrapped_dek = dek.rewrap(new_kek)?;
            let body = file::parse_body(rest)?;
            let out = file::serialize(&header, &body)?;
            fs::write(&path, out).map_err(|e| CryptoError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

// ==================== 单文件库（.mdlb） ====================

/// 单文件库的明文文件树节点（含正文内容）。
///
/// 整个库就是一个 `.mdlb` 文件，明文是一棵文件树的 JSON 序列化，
/// 用「全库共享的 DEK」整体加密。文件名、目录结构、正文全部密文。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlainNode {
    /// 名称（文件或文件夹名）
    pub name: String,
    /// 相对库根的路径（如 `项目/方案.md`）
    pub path: String,
    /// 是否目录
    pub is_dir: bool,
    /// 文件正文（仅文件节点；目录节点为空）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    /// 子节点（仅目录节点）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PlainNode>,
}

/// 把明文树转换为对外的 `FsNode` 视图（不含正文内容）。
fn to_fs_node(n: &PlainNode) -> FsNode {
    FsNode {
        name: n.name.clone(),
        path: n.path.clone(),
        is_dir: n.is_dir,
        is_vault: false,
        children: n.children.iter().map(to_fs_node).collect(),
    }
}

/// 单文件库头结构：复用 `.mdl` 的 FileHeader 布局，
/// 但 `wrapped_dek` 包裹的是「全库共享的 DEK」，`format` 用 `mlk-vault/1` 区分。
/// 这里直接复用 `FileHeader`，靠 `format` 字段区分类型。

/// 从 `.mdlb` 文件字节解出整棵明文树。
fn decrypt_plain_tree(dek: &Dek, bytes: &[u8]) -> Result<Vec<PlainNode>> {
    let (_header, rest) = file::parse_header(bytes)?;
    let body = file::parse_body(rest)?;
    let plain = keys::decrypt_body(dek, &body.nonce, &body.ct)?;
    serde_json::from_slice(&plain)
        .map_err(|e| CryptoError::BadHeader(format!("库内容解析失败: {e}")))
}

/// 创建单文件加密库（`.mdlb`），返回落盘字节。
///
/// 生成全库共享 DEK，用主密码派生的 KEK 包裹，明文为空的文件树。
pub fn create_file_vault(password: &str, hint: Option<String>) -> Result<Vec<u8>> {
    check_password_strength(password)?;

    let kdf_params = keys::new_kdf_params();
    let salt = format::b64::decode(&kdf_params.salt)?;
    let kek = keys::derive_kek(password, &salt, kdf_params.m, kdf_params.t, kdf_params.p)?;
    let dek = Dek(*keys::generate_dek());
    let wrapped_dek = dek.wrap(&kek)?;

    let header = FileHeader {
        format: format::VAULT_FILE_FORMAT.to_string(),
        kdf: kdf_params,
        cipher: "aes-256-gcm".to_string(),
        wrapped_dek,
        hint,
    };

    let empty_tree: Vec<PlainNode> = Vec::new();
    let plain_json = serde_json::to_vec(&empty_tree)
        .map_err(|e| CryptoError::BadHeader(format!("序列化库树失败: {e}")))?;
    let (nonce, ct) = keys::encrypt_body(&dek, &plain_json)?;
    let body = EncryptedBody { nonce, ct };
    file::serialize(&header, &body)
}

/// 解锁单文件库（`.mdlb`）：验证密码、解出全库 DEK 并登记会话。
pub fn unlock_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    password: &str,
    auto_lock: Option<Duration>,
) -> Result<()> {
    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    let (header, _rest) = file::parse_header(&bytes)?;
    if header.format != format::VAULT_FILE_FORMAT {
        return Err(CryptoError::Unsupported(format!(
            "format={}（不是单文件库）",
            header.format
        )));
    }

    let salt = format::b64::decode(&header.kdf.salt)?;
    let kek = keys::derive_kek(password, &salt, header.kdf.m, header.kdf.t, header.kdf.p)?;
    let dek = Dek::unwrap(&header.wrapped_dek, &kek)?;

    // 用解出的 DEK 试解密一次，校验整体完整性（密码错误会在此失败）
    let _tree = decrypt_plain_tree(&dek, &bytes)?;

    store.insert(
        vault_id.to_string(),
        UnlockedVault {
            kek,
            kdf: header.kdf,
            dek: Some(dek),
            last_activity: std::time::Instant::now(),
            auto_lock: auto_lock.unwrap_or(DEFAULT_AUTO_LOCK),
        },
    );
    Ok(())
}

/// 取会话中的全库 DEK（仅单文件库）。
fn vault_dek(vault: &UnlockedVault) -> Result<&Dek> {
    vault
        .dek
        .as_ref()
        .ok_or_else(|| CryptoError::BadHeader("该库不是单文件库".into()))
}

/// 读取单文件库的明文树（要求已解锁）。
fn read_plain_tree(store: &SessionStore, vault_id: &str, path: &Path) -> Result<Vec<PlainNode>> {
    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    store.with_vault(vault_id, |vault| {
        let dek = vault_dek(vault)?;
        decrypt_plain_tree(dek, &bytes)
    })
}

/// 把明文树加密写回 `.mdlb` 文件（整体重写）。
fn write_plain_tree(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    tree: &[PlainNode],
) -> Result<()> {
    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    let (header, _rest) = file::parse_header(&bytes)?;
    let out = store.with_vault(vault_id, |vault| {
        let dek = vault_dek(vault)?;
        let plain_json = serde_json::to_vec(tree)
            .map_err(|e| CryptoError::BadHeader(format!("序列化库树失败: {e}")))?;
        let (nonce, ct) = keys::encrypt_body(dek, &plain_json)?;
        let body = EncryptedBody { nonce, ct };
        file::serialize(&header, &body)
    })?;
    fs::write(path, out).map_err(|e| CryptoError::Io(e.to_string()))
}

/// 递归排序 `FsNode` 列表：目录在前，文件在后，各自按名称（忽略大小写）排序。
///
/// 与 `list_dir_inner` 的排序规则保持一致，供明文树（单文件库）等
/// 按插入顺序存储的来源在出口处统一排序。
fn sort_fs_nodes(nodes: &mut Vec<FsNode>) {
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    for n in nodes.iter_mut() {
        sort_fs_nodes(&mut n.children);
    }
}

/// 列出单文件库的目录树（不含内容）。
pub fn list_file_vault_tree(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
) -> Result<Vec<FsNode>> {
    let tree = read_plain_tree(store, vault_id, path)?;
    let mut nodes: Vec<FsNode> = tree.iter().map(to_fs_node).collect();
    // 明文树按插入顺序存储，这里补上与目录库一致的排序（目录优先 + 名称升序）
    sort_fs_nodes(&mut nodes);
    Ok(nodes)
}

/// 在树中递归查找节点（按相对路径）。
fn find_node<'a>(nodes: &'a [PlainNode], rel: &str) -> Option<&'a PlainNode> {
    for n in nodes {
        if n.path == rel {
            return Some(n);
        }
        if let Some(found) = find_node(&n.children, rel) {
            return Some(found);
        }
    }
    None
}

/// 在树中递归查找节点（可变借用）。
fn find_node_mut<'a>(nodes: &'a mut [PlainNode], rel: &str) -> Option<&'a mut PlainNode> {
    for n in nodes {
        if n.path == rel {
            return Some(n);
        }
        if let Some(found) = find_node_mut(&mut n.children, rel) {
            return Some(found);
        }
    }
    None
}

/// 规范化相对路径（去首尾 /，统一分隔符为 /）。
fn norm_rel(rel: &str) -> String {
    rel.trim()
        .trim_matches('/')
        .replace('\\', "/")
}

/// 读单文件库内一个文件的正文。
pub fn read_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    rel: &str,
) -> Result<String> {
    let tree = read_plain_tree(store, vault_id, path)?;
    let rel = norm_rel(rel);
    let node = find_node(&tree, &rel).ok_or_else(|| CryptoError::Io("文件不存在".into()))?;
    if node.is_dir {
        return Err(CryptoError::Io("目标是文件夹".into()));
    }
    Ok(node.content.clone())
}

/// 写回单文件库内一个文件的正文。
pub fn write_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    rel: &str,
    content: &str,
) -> Result<()> {
    let mut tree = read_plain_tree(store, vault_id, path)?;
    let rel = norm_rel(rel);
    let node = find_node_mut(&mut tree, &rel)
        .ok_or_else(|| CryptoError::Io("文件不存在".into()))?;
    if node.is_dir {
        return Err(CryptoError::Io("目标是文件夹".into()));
    }
    node.content = content.to_string();
    write_plain_tree(store, vault_id, path, &tree)
}

/// 在单文件库内创建文件（含父目录自动创建）。
pub fn create_file_in_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    rel_path: &str,
    content: &str,
) -> Result<()> {
    let mut tree = read_plain_tree(store, vault_id, path)?;
    let rel = norm_rel(rel_path);
    if rel.is_empty() {
        return Err(CryptoError::Io("路径不能为空".into()));
    }
    if find_node(&tree, &rel).is_some() {
        return Err(CryptoError::Io("同名文件或文件夹已存在".into()));
    }
    // 确保父目录存在
    if let Some(parent) = rel.rsplit_once('/').map(|(p, _)| p.to_string()) {
        ensure_dir(&mut tree, &parent)?;
    }
    insert_node(&mut tree, PlainNode {
        name: rel.rsplit('/').next().unwrap_or(&rel).to_string(),
        path: rel.clone(),
        is_dir: false,
        content: content.to_string(),
        children: Vec::new(),
    })?;
    write_plain_tree(store, vault_id, path, &tree)
}

/// 在单文件库内创建文件夹（含父目录自动创建）。
pub fn create_folder_in_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    rel_path: &str,
) -> Result<()> {
    let mut tree = read_plain_tree(store, vault_id, path)?;
    let rel = norm_rel(rel_path);
    if rel.is_empty() {
        return Err(CryptoError::Io("路径不能为空".into()));
    }
    ensure_dir(&mut tree, &rel)?;
    write_plain_tree(store, vault_id, path, &tree)
}

/// 确保某条目录路径存在（递归创建缺失的文件夹节点）。
fn ensure_dir(nodes: &mut Vec<PlainNode>, rel: &str) -> Result<()> {
    if rel.is_empty() {
        return Ok(());
    }
    let mut current: &mut Vec<PlainNode> = nodes;
    let mut prefix = String::new();
    for seg in rel.split('/') {
        prefix = if prefix.is_empty() {
            seg.to_string()
        } else {
            format!("{prefix}/{seg}")
        };
        // 在当前层找同名节点
        let existing = current.iter().find(|n| n.name == seg);
        match existing {
            Some(n) if n.is_dir => {
                // 需要推进到该目录的 children
                let idx = current.iter().position(|x| x.path == prefix).unwrap();
                current = &mut current[idx].children;
            }
            Some(_) => return Err(CryptoError::Io(format!("'{prefix}' 已是文件"))),
            None => {
                current.push(PlainNode {
                    name: seg.to_string(),
                    path: prefix.clone(),
                    is_dir: true,
                    content: String::new(),
                    children: Vec::new(),
                });
                let idx = current.len() - 1;
                current = &mut current[idx].children;
            }
        }
    }
    Ok(())
}

/// 按相对路径把节点插入到正确位置（父目录需已存在）。
fn insert_node(nodes: &mut Vec<PlainNode>, node: PlainNode) -> Result<()> {
    if let Some((parent, _name)) = node.path.rsplit_once('/') {
        let parent_node = find_node_mut(nodes, parent)
            .ok_or_else(|| CryptoError::Io("父目录不存在".into()))?;
        parent_node.children.push(node);
    } else {
        nodes.push(node);
    }
    Ok(())
}

/// 删除单文件库内的文件或文件夹。
pub fn delete_in_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    rel_path: &str,
) -> Result<()> {
    let mut tree = read_plain_tree(store, vault_id, path)?;
    let rel = norm_rel(rel_path);
    if rel.is_empty() {
        return Err(CryptoError::Io("不能删除库根".into()));
    }
    if !remove_node(&mut tree, &rel) {
        return Err(CryptoError::Io("目标不存在".into()));
    }
    write_plain_tree(store, vault_id, path, &tree)
}

/// 从树中移除指定路径的节点，返回是否找到。
fn remove_node(nodes: &mut Vec<PlainNode>, rel: &str) -> bool {
    if let Some(idx) = nodes.iter().position(|n| n.path == rel) {
        nodes.remove(idx);
        return true;
    }
    for n in nodes {
        if remove_node(&mut n.children, rel) {
            return true;
        }
    }
    false
}

/// 重命名 / 移动单文件库内的文件或文件夹。
///
/// 移动后子树内所有节点的 path 前缀都要更新。
pub fn rename_in_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    old_rel: &str,
    new_rel: &str,
) -> Result<()> {
    let mut tree = read_plain_tree(store, vault_id, path)?;
    let old = norm_rel(old_rel);
    let new = norm_rel(new_rel);
    if old.is_empty() {
        return Err(CryptoError::Io("不能移动库根".into()));
    }
    if old == new {
        return Ok(());
    }
    // 目标不能已存在
    if find_node(&tree, &new).is_some() {
        return Err(CryptoError::Io("目标位置已有同名文件或文件夹".into()));
    }
    // 目标父目录需存在（移动场景）
    if let Some(parent) = new.rsplit_once('/').map(|(p, _)| p.to_string()) {
        if find_node(&tree, &parent).map(|n| !n.is_dir).unwrap_or(true) {
            ensure_dir(&mut tree, &parent)?;
        }
    }
    // 取出节点
    let idx = tree.iter().position(|n| n.path == old);
    let node = if let Some(i) = idx {
        tree.remove(i)
    } else if let Some(n) = take_node(&mut tree, &old) {
        n
    } else {
        return Err(CryptoError::Io("源不存在".into()));
    };
    // 更新路径前缀并放回
    let mut new_node = node;
    rewrite_paths(&mut new_node, &old, &new);
    insert_node(&mut tree, new_node)?;
    write_plain_tree(store, vault_id, path, &tree)
}

/// 从嵌套树中取出指定路径节点（用于非顶层节点）。
fn take_node(nodes: &mut Vec<PlainNode>, rel: &str) -> Option<PlainNode> {
    if let Some(idx) = nodes.iter().position(|n| n.path == rel) {
        return Some(nodes.remove(idx));
    }
    for n in nodes {
        if let Some(found) = take_node(&mut n.children, rel) {
            return Some(found);
        }
    }
    None
}

/// 递归重写子树所有节点的 path（把 old 前缀替换为 new）。
fn rewrite_paths(node: &mut PlainNode, old: &str, new: &str) {
    if node.path == old {
        node.path = new.to_string();
        node.name = new.rsplit('/').next().unwrap_or(new).to_string();
    } else if node.path.starts_with(&format!("{old}/")) {
        node.path = format!("{new}{}", &node.path[old.len()..]);
    }
    for child in &mut node.children {
        rewrite_paths(child, old, new);
    }
}

/// 修改单文件库主密码：重包裹全库 DEK，无需重加密正文。
pub fn change_file_vault_password(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    old_password: &str,
    new_password: &str,
) -> Result<()> {
    check_password_strength(new_password)?;

    let bytes = fs::read(path).map_err(|e| CryptoError::Io(e.to_string()))?;
    let (mut header, rest) = file::parse_header(&bytes)?;
    if header.format != format::VAULT_FILE_FORMAT {
        return Err(CryptoError::Unsupported(format!(
            "format={}（不是单文件库）",
            header.format
        )));
    }

    let salt = format::b64::decode(&header.kdf.salt)?;
    let old_kek = keys::derive_kek(old_password, &salt, header.kdf.m, header.kdf.t, header.kdf.p)?;
    let dek = Dek::unwrap(&header.wrapped_dek, &old_kek)?;

    let new_kek = keys::derive_kek(new_password, &salt, header.kdf.m, header.kdf.t, header.kdf.p)?;
    header.wrapped_dek = dek.rewrap(&new_kek)?;

    store.insert(
        vault_id.to_string(),
        UnlockedVault {
            kek: new_kek,
            kdf: header.kdf.clone(),
            dek: Some(dek),
            last_activity: std::time::Instant::now(),
            auto_lock: store.current_auto_lock(vault_id),
        },
    );

    let body = file::parse_body(rest)?;
    let out = file::serialize(&header, &body)?;
    fs::write(path, out).map_err(|e| CryptoError::Io(e.to_string()))
}

// ==================== 目录浏览 ====================

/// 目录树节点（用于前端文件树展示）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FsNode {
    /// 名称
    pub name: String,
    /// 相对库根（或绝对路径）的路径
    pub path: String,
    /// 是否目录
    pub is_dir: bool,
    /// 是否加密库（.mdl 文件 / 含 vault.json 的目录）
    pub is_vault: bool,
    /// 子节点（仅目录）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FsNode>,
}

/// 列出目录内容（不递归，一层）。
///
/// 用于"工作目录"视图：展示操作系统目录，普通文件夹可展开，
/// 库（`.mdlb` 目录 / `.mdl` 文件）作为原子节点。
pub fn list_dir(dir: &Path) -> Result<Vec<FsNode>> {
    list_dir_inner(dir, false, false)
}

/// 列出已解锁库的目录树（递归）。
///
/// 用于"打开的库"视图：展示库内完整树，库内所有文件都是加密内容。
pub fn list_vault_tree(
    store: &SessionStore,
    vault_id: &str,
    dir: &Path,
) -> Result<Vec<FsNode>> {
    // 先校验会话存在且未过期
    store.with_vault(vault_id, |_v| Ok(()))?;
    list_dir_inner(dir, true, true)
}

/// `recursive`：是否递归子目录；`in_vault`：是否处于库上下文
/// （true 时所有普通文件都视为加密内容，false 时按扩展名识别库）。
fn list_dir_inner(dir: &Path, recursive: bool, in_vault: bool) -> Result<Vec<FsNode>> {
    let mut nodes = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| CryptoError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| CryptoError::Io(e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // 跳过库元数据文件
        if name == format::VAULT_META_FILE {
            continue;
        }
        let is_dir = path.is_dir();
        let is_vault = if in_vault {
            // 库内：目录若含 vault.json 是子库（罕见），普通文件都是加密内容
            if is_dir {
                path.join(format::VAULT_META_FILE).exists()
            } else {
                true
            }
        } else if is_dir {
            path.join(format::VAULT_META_FILE).exists()
        } else {
            // 单文件加密：`.mdl` 为单文件密文，`.mdlb` 为单文件库
            matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("mdl") | Some("mdlb")
            )
        };

        let children = if recursive && is_dir && !is_vault {
            list_dir_inner(&path, true, in_vault)?
        } else {
            Vec::new()
        };

        nodes.push(FsNode {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir,
            is_vault,
            children,
        });
    }
    // 目录在前，文件在后，各自按名称排序
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(nodes)
}

// ==================== 锁定 ====================

/// 拖入/打开路径的探测结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PathInfo {
    /// 是否目录
    pub is_dir: bool,
    /// 是否加密库（`.mdl` 文件 / `.mdlb` 文件 / 含 vault.json 的目录）
    pub is_vault: bool,
    /// 是否为单文件库（`.mdlb`），区别于单文件 `.mdl`
    pub is_file_vault: bool,
    /// 文件位于某个库目录内时，为该库目录路径
    /// （库内文件无论扩展名都是密文，需先解锁所属库）
    pub parent_vault: Option<String>,
}

/// 探测一个路径的类型，供拖入/系统打开时的路由：
/// 目录库→解锁；普通目录→挂工作目录；`.mdl`→解锁单文件；
/// 库内文件→解锁所属库；其余→明文打开。
pub fn inspect_path(path: &Path) -> Result<PathInfo> {
    let is_dir = path.is_dir();
    let ext = if is_dir {
        None
    } else {
        path.extension().and_then(|e| e.to_str()).map(|s| s.to_string())
    };
    let is_vault = if is_dir {
        path.join(format::VAULT_META_FILE).exists()
    } else {
        ext.as_deref() == Some("mdl") || ext.as_deref() == Some("mdlb")
    };
    let is_file_vault = !is_dir && ext.as_deref() == Some("mdlb");
    // 文件向上逐级查找所属库目录（支持库内子目录里的文件）
    let mut parent_vault = None;
    if !is_dir {
        let mut cur = path.parent();
        while let Some(p) = cur {
            if p.join(format::VAULT_META_FILE).is_file() {
                parent_vault = Some(p.to_string_lossy().to_string());
                break;
            }
            cur = p.parent();
        }
    }
    Ok(PathInfo {
        is_dir,
        is_vault,
        is_file_vault,
        parent_vault,
    })
}

/// 锁定一个库（清零内存中的密钥）。
pub fn lock_vault(store: &SessionStore, vault_id: &str) {
    store.lock(vault_id);
}

/// 锁定全部库。
pub fn lock_all_vaults(store: &SessionStore) {
    store.lock_all();
}

/// 回收所有过期的会话（自动锁定计时到点）。
pub fn reap_expired(store: &SessionStore) -> Vec<String> {
    store.reap_expired()
}

/// 已解锁库数量。
pub fn unlocked_count(store: &SessionStore) -> usize {
    store.unlocked_count()
}

// ==================== 全库搜索 ====================

/// 单文件搜索上限（超过视为大文件，跳过不搜）。
const PLAIN_SEARCH_MAX_FILE: u64 = 2 * 1024 * 1024;
/// 整体搜索结果上限（防刷屏 / 防卡顿）。
const SEARCH_MAX_HITS: usize = 100;
/// 每个文件的命中上限。
const SEARCH_HITS_PER_FILE: usize = 3;

/// 参与明文搜索的文本类扩展名白名单。
fn is_plain_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "md" | "markdown"
            | "txt"
            | "json"
            | "js"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "jsx"
            | "vue"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "less"
            | "py"
            | "rs"
            | "go"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "sh"
            | "yml"
            | "yaml"
            | "toml"
            | "ini"
            | "cfg"
            | "xml"
            | "csv"
            | "log"
    )
}

/// 一条搜索结果（命中某个已解锁库 / 工作目录明文文件的文件名或内容）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// 所属库 id（目录库=目录路径，单文件库=.mdlb 文件路径；明文命中=工作目录根）
    pub vault_id: String,
    /// 库显示名（目录名 / 文件名去扩展名；明文命中=「工作目录」）
    pub vault_name: String,
    /// 文件的相对路径（明文命中=相对工作目录根）
    pub rel_path: String,
    /// 命中上下文片段（内容命中=截取匹配点前后若干字符；文件名命中=相对路径）
    pub snippet: String,
    /// 是否为工作目录明文文件命中（前端据此走明文打开而非解密读取）
    pub plain: bool,
    /// 是否为文件名/路径命中（前端据此标注「文件名」徽章）
    pub name_hit: bool,
}

/// 从匹配点截取上下文片段（前后各约 30 字符，替换换行为空格）。
///
/// `pos` 是 **char 索引**（与 [`search_content`] 的匹配口径一致）。
fn make_snippet(content: &str, pos: usize) -> String {
    let chars: Vec<char> = content.chars().collect();
    let start = pos.saturating_sub(30);
    let end = (pos + 30).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    s = s.replace(['\n', '\r', '\t'], " ");
    if start > 0 {
        s.insert_str(0, "…");
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

/// 在单个文件内容里查找所有匹配点，产出命中片段。
///
/// 匹配在 **char 级**进行（先把查询与内容转 char 数组），彻底规避
/// byte 偏移与 char 索引混用导致中文多字节场景下片段截错位的问题。
/// 限每个文件 [`SEARCH_HITS_PER_FILE`] 条。
fn search_content(
    query: &str,
    content: &str,
    vault_id: &str,
    vault_name: &str,
    rel_path: &str,
    plain: bool,
) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let lower: Vec<char> = content.to_lowercase().chars().collect();
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let n = q.len();
    if n == 0 {
        return hits;
    }
    let mut i = 0usize;
    while i + n <= lower.len() {
        if lower[i..i + n] == q[..] {
            hits.push(SearchHit {
                vault_id: vault_id.to_string(),
                vault_name: vault_name.to_string(),
                rel_path: rel_path.to_string(),
                snippet: make_snippet(content, i),
                plain,
                name_hit: false,
            });
            if hits.len() >= SEARCH_HITS_PER_FILE {
                break;
            }
            i += n;
        } else {
            i += 1;
        }
    }
    hits
}

/// 查询是否命中路径/文件名（大小写不敏感子串匹配）。
fn name_matches(rel_path: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    !q.is_empty() && rel_path.to_lowercase().contains(&q)
}

/// 单个文件单元的搜索：文件名命中产出一条 `name_hit` 结果，内容命中照常产出；
/// 两者都命中时只出内容命中（结果行本身已显示文件名，避免重复）。
/// `content` 为 None 表示无法读取/解密，此时仅参与文件名匹配。
fn search_file_unit(
    query: &str,
    rel: &str,
    content: Option<&str>,
    vault_id: &str,
    vault_name: &str,
    plain: bool,
) -> Vec<SearchHit> {
    let content_hits = content
        .map(|c| search_content(query, c, vault_id, vault_name, rel, plain))
        .unwrap_or_default();
    if content_hits.is_empty() && name_matches(rel, query) {
        return vec![SearchHit {
            vault_id: vault_id.to_string(),
            vault_name: vault_name.to_string(),
            rel_path: rel.to_string(),
            snippet: rel.to_string(),
            plain,
            name_hit: true,
        }];
    }
    content_hits
}

/// 从库 id 提取显示名（目录名 / 文件名去 .mdlb 扩展名）。
fn vault_display_name(vault_id: &str) -> String {
    Path::new(vault_id)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .map(|n| n.trim_end_matches(".mdlb").to_string())
        .unwrap_or_else(|| vault_id.to_string())
}

/// 递归遍历目录库内所有文件，返回命中的结果。
fn search_dir_vault(
    store: &SessionStore,
    vault_id: &str,
    dir: &Path,
    query: &str,
) -> Result<Vec<SearchHit>> {
    let mut out = Vec::new();
    let vault_name = vault_display_name(vault_id);
    search_dir_inner(store, vault_id, dir, &vault_name, query, &mut out)?;
    Ok(out)
}

fn search_dir_inner(
    store: &SessionStore,
    vault_id: &str,
    dir: &Path,
    vault_name: &str,
    query: &str,
    out: &mut Vec<SearchHit>,
) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| CryptoError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CryptoError::Io(e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == format::VAULT_META_FILE {
            continue;
        }
        if path.is_dir() {
            search_dir_inner(store, vault_id, &path, vault_name, query, out)?;
        } else {
            let rel = path
                .strip_prefix(vault_id)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| name.clone());
            let rel = rel.replace('\\', "/");
            // 解密单个文件，出错（如非加密文件、损坏）则仅参与文件名匹配
            let content = read_file(store, vault_id, &path).ok();
            out.extend(search_file_unit(
                query,
                &rel,
                content.as_deref(),
                vault_id,
                vault_name,
                false,
            ));
        }
    }
    Ok(())
}

/// 遍历单文件库（.mdlb）的明文树，返回命中的结果。
fn search_file_vault(
    store: &SessionStore,
    vault_id: &str,
    path: &Path,
    query: &str,
) -> Result<Vec<SearchHit>> {
    let tree = read_plain_tree(store, vault_id, path)?;
    let vault_name = vault_display_name(vault_id);
    let mut out = Vec::new();
    fn walk(nodes: &[PlainNode], vault_id: &str, vault_name: &str, query: &str, out: &mut Vec<SearchHit>) {
        for n in nodes {
            if n.is_dir {
                walk(&n.children, vault_id, vault_name, query, out);
            } else {
                out.extend(search_file_unit(
                    query,
                    &n.path,
                    Some(&n.content),
                    vault_id,
                    vault_name,
                    false,
                ));
            }
        }
    }
    walk(&tree, vault_id, &vault_name, query, &mut out);
    Ok(out)
}

/// 递归搜索工作目录内的明文文本文件（文件名 + 内容）。
///
/// 规则：
/// - 跳过隐藏项（`.` 开头，如 `.DS_Store`、`.git`）与库元数据；
/// - 跳过加密文件（`.mdl` / `.mdlb`，密文不可读；已解锁库由库搜索覆盖）；
/// - 白名单文本扩展名直接读，无扩展名的小文件尝试按 UTF-8 解码；
/// - 超过 2MB / 二进制等读不了内容的文件仍参与文件名匹配；
/// - 整体命中数到达上限即停。
pub fn search_plain_dir(root: &Path, query: &str) -> Result<Vec<SearchHit>> {
    let q = query.trim();
    if q.is_empty() || !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let vault_name = "工作目录".to_string();
    let vault_id = root.to_string_lossy().to_string();
    search_plain_inner(root, &vault_id, &vault_name, q, &mut out)?;
    Ok(out)
}

fn search_plain_inner(
    root: &Path,
    vault_id: &str,
    vault_name: &str,
    query: &str,
    out: &mut Vec<SearchHit>,
) -> Result<()> {
    if out.len() >= SEARCH_MAX_HITS {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|e| CryptoError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CryptoError::Io(e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // 隐藏文件/目录与库元数据跳过
        if name.starts_with('.') || name == format::VAULT_META_FILE {
            continue;
        }
        if path.is_dir() {
            // 库目录（含 vault.json）整体跳过：库内文件虽用 .md/.txt 等普通扩展名命名，
            // 实为加密内容，不应被明文搜索卷入——否则锁定的库也会冒出库内文件名命中。
            // 已解锁的库由 search_vaults 统一解密覆盖，此处无需重复搜索。
            if path.join(format::VAULT_META_FILE).exists() {
                continue;
            }
            search_plain_inner(&path, vault_id, vault_name, query, out)?;
        } else {
            let rel = path
                .strip_prefix(Path::new(vault_id))
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| name.clone());
            let rel = rel.replace('\\', "/");
            let emit_name_only = |out: &mut Vec<SearchHit>| {
                out.extend(search_file_unit(query, &rel, None, vault_id, vault_name, true))
            };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase());
            // 加密文件是密文，跳过内容（已解锁库会由库搜索覆盖）；文件名照常匹配
            if ext.as_deref() == Some("mdl") || ext.as_deref() == Some("mdlb") {
                emit_name_only(out);
                continue;
            }
            // 大文件跳过内容，仅文件名匹配
            let ok_size = fs::metadata(&path)
                .map(|m| m.len() <= PLAIN_SEARCH_MAX_FILE)
                .unwrap_or(false);
            if !ok_size {
                emit_name_only(out);
                continue;
            }
            // 白名单文本扩展名直接读；无扩展名的小文件尝试 UTF-8 解码；
            // 未知扩展名（如 .png/.pdf 等二进制）跳过内容，仅文件名匹配
            let is_text = match &ext {
                Some(e) => is_plain_text_ext(e),
                None => true,
            };
            if !is_text {
                emit_name_only(out);
                continue;
            }
            let content = match fs::read(&path)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
            {
                Some(c) if !c.contains('\0') => c,
                // 读不了 / 非 UTF-8 / 含 NUL：仅文件名匹配
                _ => {
                    emit_name_only(out);
                    continue;
                }
            };
            out.extend(search_file_unit(
                query,
                &rel,
                Some(&content),
                vault_id,
                vault_name,
                true,
            ));
            if out.len() >= SEARCH_MAX_HITS {
                break;
            }
        }
    }
    Ok(())
}

/// 统一搜索入口：已解锁库（加密内容解密后匹配）+ 工作目录（明文匹配）。
pub fn search_all(store: &SessionStore, query: &str, workdirs: &[String]) -> Result<Vec<SearchHit>> {
    let mut out = search_vaults(store, query)?;
    for d in workdirs {
        if let Ok(hits) = search_plain_dir(Path::new(d), query) {
            out.extend(hits);
        }
        if out.len() >= SEARCH_MAX_HITS {
            break;
        }
    }
    out.truncate(SEARCH_MAX_HITS);
    Ok(out)
}

/// 搜索所有已解锁库的文件内容，返回匹配结果。
///
/// 覆盖两类库：目录库（递归解密每个文件）与单文件库（遍历解密后的明文树）。
/// 单个文件解密失败会跳过，不影响其它文件。
pub fn search_vaults(store: &SessionStore, query: &str) -> Result<Vec<SearchHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let ids = store.unlocked_ids();
    let mut out = Vec::new();
    for id in ids {
        // 判断库类型：单文件库的会话持有全库 DEK（dek 为 Some）
        let is_file_vault = store.with_vault(&id, |v| Ok(v.dek.is_some()))?;
        if is_file_vault {
            if let Ok(hits) = search_file_vault(store, &id, Path::new(&id), q) {
                out.extend(hits);
            }
        } else {
            if let Ok(hits) = search_dir_vault(store, &id, Path::new(&id), q) {
                out.extend(hits);
            }
        }
    }
    Ok(out)
}

// ==================== 批量加密 / 目录导入 ====================

/// 批量加密结果：每个源文件独立加密为 `.mdl`；单个失败不影响其余。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchEncryptResult {
    /// 生成的 `.mdl` 绝对路径列表
    pub encrypted: Vec<String>,
    /// 移动到「原文件」子目录后的绝对路径列表
    pub moved: Vec<String>,
    /// 失败明细：`(原路径, 错误消息)`
    pub failed: Vec<(String, String)>,
}

/// 目录导入到库的结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportResult {
    /// 成功导入的相对库根路径（如 `项目/方案.md`）
    pub imported: Vec<String>,
    /// 被跳过的相对路径或原因（隐藏 / vault.json / 目标已存在）
    pub skipped: Vec<String>,
    /// 失败明细：`(相对路径, 错误消息)`
    pub failed: Vec<(String, String)>,
}

/// 参与批量导入的文本类扩展名白名单（与前端 `openFileDialog` 保持一致）。
fn is_import_text_ext(ext_lower: &str) -> bool {
    matches!(ext_lower, "md" | "markdown" | "mdown" | "mkd" | "txt")
}

/// 为源文件挑选一个尚未被占用的 `.mdl` 目标路径：同名冲突时追加 ` (1)` / ` (2)` …
fn pick_mdl_target(src: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名".into());
    let parent: PathBuf = src
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut candidate = parent.join(format!("{stem}.mdl"));
    let mut i = 1usize;
    while candidate.exists() {
        candidate = parent.join(format!("{stem} ({i}).mdl"));
        i += 1;
    }
    candidate
}

/// 批量加密多个明文文件为独立的 `.mdl`（同批次共用一次 Argon2id 派生）。
///
/// 每个文件仍是**独立单文件**（`format=mlk/1`），只是 wrapped_dek 用同一个 KEK 包裹；
/// 这与「目录库内文件」模型一致，只是这里没有 `vault.json`。
/// `move_originals=true` 时把原文件移到其**父目录**下的 `原文件/` 子目录。
pub fn batch_encrypt(
    paths: &[String],
    password: &str,
    move_originals: bool,
) -> Result<BatchEncryptResult> {
    check_password_strength(password)?;

    // 整个批次共用一份 KDF 参数与 KEK：Argon2id 单次约 1s，N 次派生会拖死 UI。
    let kdf = keys::new_kdf_params();
    let salt = format::b64::decode(&kdf.salt)?;
    let kek = keys::derive_kek(password, &salt, kdf.m, kdf.t, kdf.p)?;

    let mut out = BatchEncryptResult {
        encrypted: Vec::new(),
        moved: Vec::new(),
        failed: Vec::new(),
    };

    for p in paths {
        let src = Path::new(p);
        // 读原文（必须 UTF-8）
        let content = match fs::read(src) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    out.failed.push((p.clone(), "文件不是合法 UTF-8".into()));
                    continue;
                }
            },
            Err(e) => {
                out.failed.push((p.clone(), format!("读取失败: {e}")));
                continue;
            }
        };

        // 每文件独立 DEK，用批次 KEK 包裹
        let dek = Dek(*keys::generate_dek());
        let enc = (|| -> Result<PathBuf> {
            let wrapped_dek = dek.wrap(&kek)?;
            let header = FileHeader {
                format: format::FORMAT.to_string(),
                kdf: kdf.clone(),
                cipher: "aes-256-gcm".to_string(),
                wrapped_dek,
                hint: None,
            };
            let (nonce, ct) = keys::encrypt_body(&dek, content.as_bytes())?;
            let body = EncryptedBody { nonce, ct };
            let bytes = file::serialize(&header, &body)?;
            let target = pick_mdl_target(src);
            fs::write(&target, &bytes).map_err(|e| CryptoError::Io(format!("写入失败: {e}")))?;
            Ok(target)
        })();
        let target = match enc {
            Ok(t) => t,
            Err(e) => {
                out.failed.push((p.clone(), e.to_string()));
                continue;
            }
        };
        out.encrypted.push(target.to_string_lossy().to_string());

        // 可选：把原文件移到同目录下的「原文件」子目录；移动失败不影响加密结果，仅不加入 moved 列表
        if move_originals {
            let parent = src.parent().unwrap_or(src);
            let orig_dir = parent.join("原文件");
            if fs::create_dir_all(&orig_dir).is_err() {
                continue;
            }
            let dest = match src.file_name() {
                Some(n) => orig_dir.join(n),
                None => continue,
            };
            if fs::rename(src, &dest).is_ok() {
                out.moved.push(dest.to_string_lossy().to_string());
            }
        }
    }

    Ok(out)
}

/// 递归收集源目录下所有可导入的文本文件：返回 `(相对根路径, 绝对路径)` 列表（以 / 分隔）。
/// 跳过隐藏文件/目录、`vault.json`；非白名单扩展名的文件也跳过但不记入 skipped（对用户透明）。
fn collect_import_files(root: &Path, cur: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = fs::read_dir(cur) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if name == format::VAULT_META_FILE {
            continue;
        }
        if p.is_dir() {
            collect_import_files(root, &p, out);
        } else {
            let ext_ok = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| is_import_text_ext(&s.to_ascii_lowercase()))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(root) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                out.push((rel_str, p));
            }
        }
    }
}

/// 把一个源目录里的文本文件（含子目录结构）加密导入到目标库。
///
/// 目标库必须已解锁（会话存在）；根据 `vault_id` 是否为目录自动区分目录库 / 单文件库。
/// 目标已存在同 rel 路径时**跳过**（不覆盖），保证幂等与安全。
pub fn import_dir_to_vault(
    store: &SessionStore,
    vault_id: &str,
    src_dir: &Path,
) -> Result<ImportResult> {
    // 先校验会话存在，未解锁直接 NotUnlocked
    store.with_vault(vault_id, |_| Ok(()))?;

    // 源目录不得是加密库
    if src_dir.join(format::VAULT_META_FILE).is_file() {
        return Err(CryptoError::BadHeader("源目录是加密库，不能作为导入源".into()));
    }

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    collect_import_files(src_dir, src_dir, &mut entries);

    let vault_path = Path::new(vault_id);
    let is_dir_vault = vault_path.is_dir();

    let mut out = ImportResult {
        imported: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };

    if is_dir_vault {
        for (rel, abs) in entries {
            let target = vault_path.join(&rel);
            if target.exists() {
                out.skipped.push(format!("目标已存在: {rel}"));
                continue;
            }
            let content = match fs::read_to_string(&abs) {
                Ok(s) => s,
                Err(e) => {
                    out.failed.push((rel, format!("读取失败: {e}")));
                    continue;
                }
            };
            if let Err(e) = create_file_in_vault(store, vault_id, &rel, &content) {
                out.failed.push((rel, e.to_string()));
                continue;
            }
            out.imported.push(rel);
        }
    } else {
        // 单文件库：读整棵明文树 → 批量插入 PlainNode → 一次性加密写回
        let mut tree = read_plain_tree(store, vault_id, vault_path)?;
        for (rel, abs) in entries {
            if find_node(&tree, &rel).is_some() {
                out.skipped.push(format!("目标已存在: {rel}"));
                continue;
            }
            let content = match fs::read_to_string(&abs) {
                Ok(s) => s,
                Err(e) => {
                    out.failed.push((rel, format!("读取失败: {e}")));
                    continue;
                }
            };
            if let Some(parent) = rel.rsplit_once('/').map(|(p, _)| p.to_string()) {
                if let Err(e) = ensure_dir(&mut tree, &parent) {
                    out.failed.push((rel, e.to_string()));
                    continue;
                }
            }
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            if let Err(e) = insert_node(
                &mut tree,
                PlainNode {
                    name,
                    path: rel.clone(),
                    is_dir: false,
                    content,
                    children: Vec::new(),
                },
            ) {
                out.failed.push((rel, e.to_string()));
                continue;
            }
            out.imported.push(rel);
        }
        // 无导入项时不重写文件树，避免无意义加解密
        if !out.imported.is_empty() {
            write_plain_tree(store, vault_id, vault_path, &tree)?;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_unlock_read_write_cycle() {
        let dir = std::env::temp_dir().join("marklock_test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.mdl");

        let bytes = create_encrypted("password123", "# hello\nworld", None).unwrap();
        fs::write(&path, &bytes).unwrap();

        let store = SessionStore::new();
        unlock_file(&store, "v", &path, "password123", None).unwrap();

        let plain = read_file(&store, "v", &path).unwrap();
        assert_eq!(plain, "# hello\nworld");

        write_file(&store, "v", &path, "# updated").unwrap();
        assert_eq!(read_file(&store, "v", &path).unwrap(), "# updated");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn wrong_password_rejected() {
        let dir = std::env::temp_dir().join("marklock_test2");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("t.mdl");
        let bytes = create_encrypted("password123", "x", None).unwrap();
        fs::write(&path, &bytes).unwrap();

        let store = SessionStore::new();
        assert!(unlock_file(&store, "v", &path, "wrong", None).is_err());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn change_password_rewraps_dek() {
        let dir = std::env::temp_dir().join("marklock_test3");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("t.mdl");
        let bytes = create_encrypted("oldpass123", "secret", None).unwrap();
        fs::write(&path, &bytes).unwrap();

        let store = SessionStore::new();
        unlock_file(&store, "v", &path, "oldpass123", None).unwrap();
        change_password(&store, "v", &path, "oldpass123", "newpass456").unwrap();

        // 新密码能读，旧密码失败
        assert_eq!(read_file(&store, "v", &path).unwrap(), "secret");

        let store2 = SessionStore::new();
        assert!(unlock_file(&store2, "v2", &path, "oldpass123", None).is_err());
        assert!(unlock_file(&store2, "v3", &path, "newpass456", None).is_ok());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn directory_vault_full_cycle() {
        let dir = std::env::temp_dir().join("marklock_vault_dir");
        let _ = fs::remove_dir_all(&dir);
        create_vault_dir(&dir, "vaultpass123", None).unwrap();

        let store = SessionStore::new();
        let vault_id = dir.to_string_lossy().to_string();

        // 错误密码应失败
        assert!(unlock_vault_dir(&store, &vault_id, &dir, "wrongpass", None).is_err());

        // 正确密码解锁
        unlock_vault_dir(&store, &vault_id, &dir, "vaultpass123", None).unwrap();

        // 在库内创建文件
        create_file_in_vault(&store, &vault_id, "notes.md", "# 第一篇").unwrap();
        create_file_in_vault(&store, &vault_id, "子目录/deep.md", "深层内容").unwrap();

        // 读取
        let f1 = dir.join("notes.md");
        assert_eq!(read_file(&store, &vault_id, &f1).unwrap(), "# 第一篇");

        // 列树
        let tree = list_vault_tree(&store, &vault_id, &dir).unwrap();
        assert!(!tree.is_empty());

        // 改密码
        change_password(&store, &vault_id, &dir, "vaultpass123", "newvault789").unwrap();
        assert_eq!(read_file(&store, &vault_id, &f1).unwrap(), "# 第一篇");

        // 新密码能重新解锁，旧密码失败
        let store2 = SessionStore::new();
        assert!(unlock_vault_dir(&store2, "v", &dir, "vaultpass123", None).is_err());
        assert!(unlock_vault_dir(&store2, "v2", &dir, "newvault789", None).is_ok());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_folder_support() {
        let dir = std::env::temp_dir().join("marklock_vault_folder");
        let _ = fs::remove_dir_all(&dir);
        create_vault_dir(&dir, "vaultpass123", None).unwrap();

        let store = SessionStore::new();
        let vault_id = dir.to_string_lossy().to_string();
        unlock_vault_dir(&store, &vault_id, &dir, "vaultpass123", None).unwrap();

        // 新建文件夹（含多级）
        create_folder_in_vault(&store, &vault_id, "项目/文档").unwrap();

        // 在新建文件夹内创建文件
        create_file_in_vault(&store, &vault_id, "项目/文档/方案.md", "# 方案").unwrap();

        // 读取该文件（复用库 KEK，无需再次输入密码）
        let deep = dir.join("项目").join("文档").join("方案.md");
        assert_eq!(read_file(&store, &vault_id, &deep).unwrap(), "# 方案");

        // 列树应递归包含嵌套文件夹与文件
        let tree = list_vault_tree(&store, &vault_id, &dir).unwrap();
        let project = tree.iter().find(|n| n.name == "项目").expect("应有 项目 文件夹");
        assert!(project.is_dir);
        assert!(!project.children.is_empty());
        let docs = project.children.iter().find(|n| n.name == "文档").expect("应有 文档 子文件夹");
        assert!(docs.is_dir);
        assert!(docs.children.iter().any(|n| n.name == "方案.md"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_rename_move_delete() {
        let dir = std::env::temp_dir().join("marklock_vault_manage");
        let _ = fs::remove_dir_all(&dir);
        create_vault_dir(&dir, "vaultpass123", None).unwrap();

        let store = SessionStore::new();
        let vault_id = dir.to_string_lossy().to_string();
        unlock_vault_dir(&store, &vault_id, &dir, "vaultpass123", None).unwrap();

        // 建文件与文件夹
        create_file_in_vault(&store, &vault_id, "a.md", "# A").unwrap();
        create_folder_in_vault(&store, &vault_id, "sub").unwrap();

        // 重命名（同目录）
        rename_in_vault(&store, &vault_id, "a.md", "b.md").unwrap();
        assert!(dir.join("b.md").exists());
        assert!(!dir.join("a.md").exists());

        // 移动（移动到子目录）
        rename_in_vault(&store, &vault_id, "b.md", "sub/b.md").unwrap();
        assert!(dir.join("sub").join("b.md").exists());
        // 移动后内容不变（密文原样，可读回）
        assert_eq!(read_file(&store, &vault_id, &dir.join("sub").join("b.md")).unwrap(), "# A");

        // 删除文件
        delete_in_vault(&store, &vault_id, "sub/b.md").unwrap();
        assert!(!dir.join("sub").join("b.md").exists());

        // 删除目录（含内容）
        create_file_in_vault(&store, &vault_id, "sub/x.md", "# X").unwrap();
        delete_in_vault(&store, &vault_id, "sub").unwrap();
        assert!(!dir.join("sub").exists());

        // 拒绝删除库根 / 元数据 / 路径穿越
        assert!(delete_in_vault(&store, &vault_id, "").is_err() || delete_in_vault(&store, &vault_id, ".").is_err());
        assert!(delete_in_vault(&store, &vault_id, "vault.json").is_err());
        assert!(delete_in_vault(&store, &vault_id, "../escape").is_err());
        assert!(rename_in_vault(&store, &vault_id, "b.md", "../escape.md").is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_vault_full_cycle() {
        let dir = std::env::temp_dir().join("marklock_file_vault");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.mdlb");

        // 创建单文件库
        let bytes = create_file_vault("vaultpass123", None).unwrap();
        fs::write(&path, &bytes).unwrap();
        let vault_id = path.to_string_lossy().to_string();

        let store = SessionStore::new();
        // 错误密码失败
        assert!(unlock_file_vault(&store, "v", &path, "wrongpass", None).is_err());
        // 正确密码解锁
        unlock_file_vault(&store, &vault_id, &path, "vaultpass123", None).unwrap();

        // 建文件（含嵌套文件夹）
        create_file_in_file_vault(&store, &vault_id, &path, "笔记/第一篇.md", "# 你好").unwrap();
        create_file_in_file_vault(&store, &vault_id, &path, "根文件.md", "root").unwrap();

        // 读回（复用全库 DEK，无需再输密码）
        assert_eq!(
            read_file_vault(&store, &vault_id, &path, "笔记/第一篇.md").unwrap(),
            "# 你好"
        );

        // 列树：文件名/目录结构都来自解密后的树
        let tree = list_file_vault_tree(&store, &vault_id, &path).unwrap();
        let notes = tree.iter().find(|n| n.name == "笔记").expect("应有 笔记 文件夹");
        assert!(notes.is_dir);
        assert!(notes.children.iter().any(|n| n.name == "第一篇.md"));

        // 写回
        write_file_vault(&store, &vault_id, &path, "根文件.md", "updated").unwrap();
        assert_eq!(read_file_vault(&store, &vault_id, &path, "根文件.md").unwrap(), "updated");

        // 重命名/移动
        rename_in_file_vault(&store, &vault_id, &path, "根文件.md", "笔记/移动.md").unwrap();
        assert!(read_file_vault(&store, &vault_id, &path, "根文件.md").is_err());
        assert_eq!(read_file_vault(&store, &vault_id, &path, "笔记/移动.md").unwrap(), "updated");

        // 删除
        delete_in_file_vault(&store, &vault_id, &path, "笔记/移动.md").unwrap();
        assert!(read_file_vault(&store, &vault_id, &path, "笔记/移动.md").is_err());

        // 改密码
        change_file_vault_password(&store, &vault_id, &path, "vaultpass123", "newpass456").unwrap();
        assert_eq!(
            read_file_vault(&store, &vault_id, &path, "笔记/第一篇.md").unwrap(),
            "# 你好"
        );

        // 新密码能重新解锁，旧密码失败
        let store2 = SessionStore::new();
        assert!(unlock_file_vault(&store2, "x", &path, "vaultpass123", None).is_err());
        assert!(unlock_file_vault(&store2, "y", &path, "newpass456", None).is_ok());

        // 磁盘上只有一个文件，且文件名不含库内文件名（整包密文）
        let disk_files: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(disk_files, vec!["notes.mdlb".to_string()]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_dir_search_finds_chinese() {
        let dir = std::env::temp_dir().join("marklock_plain_search");
        let _ = fs::remove_dir_all(&dir);
        let sub = dir.join("子目录");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("说明.md"), "# MarkLock — 快速上手\n\nTauri 2 桌面壳").unwrap();
        fs::write(sub.join("note.txt"), "hello world").unwrap();
        fs::write(dir.join(".DS_Store"), "binaryjunk").unwrap();
        fs::write(dir.join("加密库.mdlb"), b"\x00\x01binary").unwrap();
        fs::write(dir.join("vault.json"), b"{}").unwrap();

        let hits = search_plain_dir(&dir, "快速上手").unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].plain);
        assert_eq!(hits[0].rel_path, "说明.md");
        assert_eq!(hits[0].vault_name, "工作目录");
        assert!(hits[0].snippet.contains("快速上手"));

        // 大小写不敏感
        let hits = search_plain_dir(&dir, "HELLO").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel_path, "子目录/note.txt");

        // 无匹配
        assert!(search_plain_dir(&dir, "不存在的内容xyz").unwrap().is_empty());

        // 文件名命中（内容不含查询词）：note.txt 正文为 hello world，查询 note 仅命中文件名
        let hits = search_plain_dir(&dir, "note").unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].name_hit);
        assert_eq!(hits[0].rel_path, "子目录/note.txt");

        // 文件夹名命中：子目录下的文件 rel_path 含目录名
        let hits = search_plain_dir(&dir, "子目").unwrap();
        assert!(hits.iter().any(|h| h.name_hit && h.rel_path == "子目录/note.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_dir_skips_nested_vault_dir() {
        // 工作目录下嵌套的库目录（含 vault.json）应被整体跳过，
        // 即使库内文件用 .md/.txt 等普通扩展名命名，也不得出现在明文搜索结果里。
        let dir = std::env::temp_dir().join("marklock_plain_skip_vault");
        let _ = fs::remove_dir_all(&dir);
        let vault = dir.join("机密库");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join(format::VAULT_META_FILE), b"{}").unwrap();
        fs::write(vault.join("密码.md"), "top secret content").unwrap();
        // 同级普通明文目录应正常参与搜索
        let normal = dir.join("笔记");
        fs::create_dir_all(&normal).unwrap();
        fs::write(normal.join("todo.md"), "buy milk").unwrap();

        // 搜库内内容：不得命中被跳过的库目录
        assert!(search_plain_dir(&dir, "secret").unwrap().is_empty());
        assert!(search_plain_dir(&dir, "密码").unwrap().is_empty());
        // 搜普通目录内容：正常命中
        let hits = search_plain_dir(&dir, "milk").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rel_path, "笔记/todo.md");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_vault_path_traversal_rejected() {
        let dir = std::env::temp_dir().join("marklock_file_vault_pt");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v.mdlb");
        fs::write(&path, create_file_vault("pw", None).unwrap()).unwrap();
        let vault_id = path.to_string_lossy().to_string();

        let store = SessionStore::new();
        unlock_file_vault(&store, &vault_id, &path, "pw", None).unwrap();

        // 拒绝库根 / 空路径
        assert!(delete_in_file_vault(&store, &vault_id, &path, "").is_err());
        assert!(rename_in_file_vault(&store, &vault_id, &path, "", "x.md").is_err());
        // 拒绝不存在的源
        assert!(delete_in_file_vault(&store, &vault_id, &path, "不存在.md").is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
