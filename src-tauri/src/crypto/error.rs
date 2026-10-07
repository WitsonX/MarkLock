//! MarkLock 加密核心错误类型

use std::fmt;

/// 加密核心的统一错误类型。
///
/// 对外（Tauri 命令）会序列化为字符串，前端拿到的始终是可读的中文/英文消息，
/// 不泄露内部细节。密钥材料错误尤其要避免把内存内容暴露进错误里。
#[derive(Debug)]
pub enum CryptoError {
    /// 密码错误 / KEK 无法解开 DEK（认证失败）
    WrongPassword,
    /// 文件头格式非法或字段缺失
    BadHeader(String),
    /// 密文被篡改（GCM 认证失败）
    IntegrityFailed,
    /// 未知的加密算法 / KDF 参数（格式版本不兼容）
    Unsupported(String),
    /// 尚未解锁（会话不存在或已锁定）
    NotUnlocked,
    /// 库/文件 IO 错误
    Io(String),
    /// 密码强度不足
    WeakPassword(String),
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CryptoError::WrongPassword => write!(f, "密码错误"),
            CryptoError::BadHeader(msg) => write!(f, "文件头格式错误: {msg}"),
            CryptoError::IntegrityFailed => write!(f, "密文完整性校验失败，文件可能被篡改"),
            CryptoError::Unsupported(msg) => write!(f, "不支持的格式: {msg}"),
            CryptoError::NotUnlocked => write!(f, "尚未解锁或已自动锁定"),
            CryptoError::Io(msg) => write!(f, "IO 错误: {msg}"),
            CryptoError::WeakPassword(msg) => write!(f, "密码强度不足: {msg}"),
        }
    }
}

impl std::error::Error for CryptoError {}

impl serde::Serialize for CryptoError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Tauri 命令统一返回类型：`Result<T, CryptoError>`。
pub type Result<T> = std::result::Result<T, CryptoError>;
