//! MarkLock 加密核心。
//!
//! 模块划分：
//! - [`error`]  统一错误类型
//! - [`format`] `.mdl` 文件格式与常量
//! - [`keys`]   KEK 派生 / DEK 包裹·解包裹·重包裹
//! - [`file`]   `.mdl` 序列化 / 反序列化
//! - [`session`] 解锁会话与自动锁定计时
//! - [`vault`]  库级操作（创建 / 解锁 / 读写 / 改密码 / 锁定）

pub mod error;
pub mod file;
pub mod format;
pub mod keys;
pub mod session;
pub mod vault;

pub use error::CryptoError;
