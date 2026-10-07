//! 加密格式常量与 `.mdl` 文件头结构。
//!
//! ## 文件格式（与 `README.md` 一致）
//!
//! `.mdl` 文件 = 明文 JSON 头 + 密文数据区：
//!
//! ```text
//! ┌─ 文件头（明文 JSON，定长前缀） ──────────────┐
//! │  format : "mlk/1"                           │
//! │  kdf    : argon2id, m, t, p, salt(b64)      │
//! │  cipher : "aes-256-gcm"                     │
//! │  wrapped_dek : { nonce, ct }   (b64)        │
//! ├─ 数据区（密文）─────────────────────────────┤
//! │  nonce(b64) + AES-256-GCM(markdown 源码)    │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! 头用「4 字节大端长度 + JSON 字节」的方式前缀，方便直接切出数据区，
//! 不依赖 JSON 自身的边界检测。

use serde::{Deserialize, Serialize};

/// 文件格式标识
pub const FORMAT: &str = "mlk/1";

/// 当前版本使用的 KDF 名称
pub const KDF_ALGO: &str = "argon2id";

/// 密码派生的内存成本（KiB，即 64 MiB）
pub const ARGON2_M: u32 = 64 * 1024;
/// 迭代次数
pub const ARGON2_T: u32 = 3;
/// 并行度
pub const ARGON2_P: u32 = 4;
/// 输出 KEK 长度（32 字节 = AES-256）
pub const KEK_LEN: usize = 32;
/// salt 长度
#[allow(dead_code)]
pub const SALT_LEN: usize = 16;
/// GCM nonce 长度（96 位）
pub const NONCE_LEN: usize = 12;
/// DEK 长度（32 字节 = AES-256）
#[allow(dead_code)]
pub const DEK_LEN: usize = 32;

/// 头前缀的 4 字节大端长度字段
pub const HEADER_LEN_BYTES: usize = 4;

/// Argon2id KDF 参数（序列化进文件头，跨版本可追溯）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub algo: String,
    /// 内存成本（KiB）
    pub m: u32,
    /// 迭代次数
    pub t: u32,
    /// 并行度
    pub p: u32,
    /// salt，base64（无 padding）
    pub salt: String,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            algo: KDF_ALGO.to_string(),
            m: ARGON2_M,
            t: ARGON2_T,
            p: ARGON2_P,
            salt: String::new(),
        }
    }
}

/// 被 KEK 包裹的 DEK（`AES-256-GCM(wrapped) -> DEK`）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedDek {
    /// GCM nonce，base64
    pub nonce: String,
    /// 密文（含认证 tag），base64
    pub ct: String,
}

/// `.mdl` 文件头（明文 JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHeader {
    pub format: String,
    pub kdf: KdfParams,
    pub cipher: String,
    pub wrapped_dek: WrappedDek,
    /// 可选：密码提示（仅本地可见，绝不存放密码本身）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// 数据区：nonce + 密文（base64），与头分开放便于流式处理
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedBody {
    pub nonce: String,
    pub ct: String,
}

/// 库（目录）元数据文件名。
pub const VAULT_META_FILE: &str = "vault.json";

/// 单文件加密库（`.mdlb` 文件）的格式标识（区别于 `.mdl` 单文件的 `mlk/1`）。
pub const VAULT_FILE_FORMAT: &str = "mlk-vault/1";

/// 库校验探针的固定明文。
pub const VAULT_PROBE: &str = "mlk-vault-probe-v1";

/// 库元数据（明文 JSON，存于库目录根）。
///
/// 作用：让「库=目录」解锁时无需先解密某个 .mdl，而是直接校验探针。
/// 库内所有 .mdl 文件复用此 KDF 参数（同一 salt ⇒ 同一主密码派生同一 KEK），
/// 实现「多文件共享一个主密码」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultMeta {
    pub format: String,
    pub kdf: KdfParams,
    pub cipher: String,
    /// 校验探针：用 KEK 包裹的一段固定明文（如 "mlk-probe"），
    /// 解锁时解开它即证明密码正确。
    pub probe: WrappedDek,
    /// 可选：密码提示
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl VaultMeta {
    /// 新建库元数据：用给定主密码派生 KEK，并用 KEK 包裹校验探针。
    pub fn new(password: &str, hint: Option<String>) -> crate::crypto::error::Result<Self> {
        let kdf = crate::crypto::keys::new_kdf_params();
        let salt = b64::decode(&kdf.salt)?;
        let kek = crate::crypto::keys::derive_kek(password, &salt, kdf.m, kdf.t, kdf.p)?;
        // 用 KEK 包裹探针（复用 WrappedDek 结构，明文是固定探针字符串）
        let probe = kek.seal_probe()?;
        Ok(Self {
            format: FORMAT.to_string(),
            kdf,
            cipher: "aes-256-gcm".to_string(),
            probe,
            hint,
        })
    }
}

/// base64 工具（无 padding，URL-safe 与标准均可，统一用标准 + 去 padding）
pub mod b64 {
    use base64::Engine;

    const ENGINE: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD_NO_PAD;

    pub fn encode(bytes: &[u8]) -> String {
        ENGINE.encode(bytes)
    }

    pub fn decode(s: &str) -> crate::crypto::error::Result<Vec<u8>> {
        ENGINE
            .decode(s)
            .map_err(|_| crate::crypto::error::CryptoError::BadHeader("base64 解码失败".into()))
    }
}
