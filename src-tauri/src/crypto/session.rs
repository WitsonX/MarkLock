//! 解锁会话：内存中持有已解锁库的 KEK/DEK，并负责自动锁定计时。
//!
//! 安全要点：
//! - 密钥只在解锁后的内存中存在，落盘的一律是包裹后的 DEK 与密文。
//! - 锁定（`lock`）时对 KEK/DEK 调用 `zeroize` 清零。
//! - 自动锁定由 `touch()` 刷新时间戳实现；超时未触摸则视为需要锁定。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::error::{CryptoError, Result};
use super::format::KdfParams;
use super::keys::{Dek, Kek};

/// 默认自动锁定超时：5 分钟。
pub const DEFAULT_AUTO_LOCK: Duration = Duration::from_secs(5 * 60);

/// 一个已解锁的库会话。
///
/// 库=目录（或单个 .mdl 文件）。解锁后持有 KEK 与 KDF 参数，
/// 目录内 / 该文件的所有数据都由此 KEK 解开各自的 DEK。
pub struct UnlockedVault {
    /// 该库的 KEK（可解出任意文件 / 探针的 DEK）
    pub kek: Kek,
    /// KDF 参数（salt 等），用于改主密码时重派生
    pub kdf: KdfParams,
    /// 单文件库（.mdlb）的全库共享 DEK；目录库为 None（各文件独立 DEK）。
    pub dek: Option<Dek>,
    /// 最近一次活动的时刻（用于自动锁定计时）
    pub last_activity: Instant,
    /// 自动锁定超时
    pub auto_lock: Duration,
}

impl UnlockedVault {
    /// 刷新活动时间戳（用户操作时调用）。
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// 是否已超过自动锁定超时。
    pub fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.last_activity) >= self.auto_lock
    }
}

/// 会话存储：库标识（如文件路径）→ 解锁会话。
pub struct SessionStore {
    vaults: Mutex<HashMap<String, UnlockedVault>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            vaults: Mutex::new(HashMap::new()),
        }
    }

    /// 插入或更新一个解锁会话。
    pub fn insert(&self, id: String, vault: UnlockedVault) {
        let mut m = self.vaults.lock().unwrap();
        m.insert(id, vault);
    }

    /// 以闭包方式访问会话（持锁期间执行，避免引用逃逸）。
    pub fn with_vault<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut UnlockedVault) -> Result<T>,
    ) -> Result<T> {
        let mut m = self.vaults.lock().unwrap();
        let vault = m.get_mut(id).ok_or(CryptoError::NotUnlocked)?;
        let now = Instant::now();
        if vault.expired(now) {
            // 过期即清零密钥并移除
            m.remove(id);
            return Err(CryptoError::NotUnlocked);
        }
        vault.touch();
        f(vault)
    }

    /// 检查并锁定过期的会话（返回被锁定的库 id 列表）。
    pub fn reap_expired(&self) -> Vec<String> {
        let mut m = self.vaults.lock().unwrap();
        let now = Instant::now();
        let expired: Vec<String> = m
            .iter()
            .filter(|(_, v)| v.expired(now))
            .map(|(k, _)| k.clone())
            .collect();
        for id in &expired {
            m.remove(id);
        }
        expired
    }

    /// 主动锁定某个库（清零密钥）。
    pub fn lock(&self, id: &str) {
        let mut m = self.vaults.lock().unwrap();
        m.remove(id);
    }

    /// 锁定全部。
    pub fn lock_all(&self) {
        let mut m = self.vaults.lock().unwrap();
        m.clear();
    }

    /// 当前已解锁的库数量。
    pub fn unlocked_count(&self) -> usize {
        self.vaults.lock().unwrap().len()
    }

    /// 查询某会话当前的自动锁定超时；无会话时返回默认值（改密码重建会话时沿用）。
    pub fn current_auto_lock(&self, id: &str) -> Duration {
        self.vaults
            .lock()
            .unwrap()
            .get(id)
            .map(|v| v.auto_lock)
            .unwrap_or(DEFAULT_AUTO_LOCK)
    }

    /// 所有已解锁库的 id 列表（用于全库搜索等遍历场景）。
    pub fn unlocked_ids(&self) -> Vec<String> {
        self.vaults.lock().unwrap().keys().cloned().collect()
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::format::KdfParams;
    use crate::crypto::keys::{generate_dek, generate_salt, derive_kek_default};

    fn sample_vault(auto_lock: Duration) -> UnlockedVault {
        let kek = derive_kek_default("pw", &generate_salt()).unwrap();
        let _dek = generate_dek();
        UnlockedVault {
            kek,
            kdf: KdfParams::default(),
            dek: None,
            last_activity: Instant::now(),
            auto_lock,
        }
    }

    #[test]
    fn lock_clears_session() {
        let store = SessionStore::new();
        store.insert("v1".into(), sample_vault(Duration::from_secs(60)));
        assert_eq!(store.unlocked_count(), 1);
        store.lock("v1");
        assert_eq!(store.unlocked_count(), 0);
    }

    #[test]
    fn expired_vault_is_reaped() {
        let store = SessionStore::new();
        let mut v = sample_vault(Duration::from_millis(1));
        v.last_activity = Instant::now() - Duration::from_secs(10);
        store.insert("v1".into(), v);
        assert_eq!(store.reap_expired(), vec!["v1".to_string()]);
        assert_eq!(store.unlocked_count(), 0);
    }

    #[test]
    fn never_lock_uses_max_duration() {
        // 「不自动锁定」映射为 Duration::MAX：任意时刻都不得被判为过期
        let store = SessionStore::new();
        let mut v = sample_vault(Duration::MAX);
        v.last_activity = Instant::now() - Duration::from_secs(3600);
        store.insert("v1".into(), v);
        assert!(store.with_vault("v1", |_| Ok(())).is_ok());
        assert!(store.reap_expired().is_empty());
    }
}
