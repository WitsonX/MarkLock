//! 密钥管理：KEK 派生、DEK 包裹 / 解包裹 / 重包裹。
//!
//! 密钥链路：
//! ```text
//! 主密码 + salt ──Argon2id──▶ KEK（32B）
//!                                  │  AES-256-GCM
//!                                  ▼
//!                          wrapped_dek ──▶ DEK（32B，每文件独立随机）
//!                                  │  AES-256-GCM
//!                                  ▼
//!                           markdown 源码（密文）
//! ```
//!
//! 改主密码只需用新 KEK 重包裹 DEK（`rekey`），不重加密正文。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::Argon2;
use zeroize::{Zeroize, Zeroizing};

use super::error::{CryptoError, Result};
use super::format::{self, KdfParams, WrappedDek, ARGON2_M, ARGON2_P, ARGON2_T, KEK_LEN, NONCE_LEN};

/// KEK（密钥加密密钥），32 字节。实现 Zeroize，保证 drop 时内存清零。
#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct Kek([u8; KEK_LEN]);

impl Kek {
    #[allow(dead_code)]
    pub fn as_bytes(&self) -> &[u8; KEK_LEN] {
        &self.0
    }

    /// 用 KEK 包裹一段明文（这里用于包裹 DEK）。
    fn seal(&self, plaintext: &[u8]) -> Result<WrappedDek> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let nonce_bytes = rand::random::<[u8; NONCE_LEN]>();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| CryptoError::IntegrityFailed)?;
        Ok(WrappedDek {
            nonce: format::b64::encode(&nonce_bytes),
            ct: format::b64::encode(&ct),
        })
    }

    /// 用 KEK 解开包裹的 DEK。
    fn open(&self, wrapped: &WrappedDek) -> Result<Vec<u8>> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let nonce_bytes = format::b64::decode(&wrapped.nonce)?;
        if nonce_bytes.len() != NONCE_LEN {
            return Err(CryptoError::BadHeader("DEK nonce 长度非法".into()));
        }
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = format::b64::decode(&wrapped.ct)?;
        cipher
            .decrypt(nonce, ct.as_slice())
            .map_err(|_| CryptoError::WrongPassword)
    }

    /// 用 KEK 包裹库校验探针（固定明文）。
    pub fn seal_probe(&self) -> Result<WrappedDek> {
        self.seal(crate::crypto::format::VAULT_PROBE.as_bytes())
    }

    /// 用 KEK 解开库校验探针，返回明文（用于验证密码正确性）。
    pub fn open_probe(&self, probe: &WrappedDek) -> Result<Vec<u8>> {
        self.open(probe)
    }
}

/// 从主密码 + salt 派生 KEK（Argon2id）。
///
/// 密码用 `Zeroizing` 包裹，函数返回后自动清零，不在内存中残留。
pub fn derive_kek(password: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<Kek> {
    if salt.len() < 8 {
        return Err(CryptoError::BadHeader("salt 过短".into()));
    }
    let mut out = Zeroizing::new([0u8; KEK_LEN]);
    let params = argon2::Params::new(m, t, p, Some(KEK_LEN))
        .map_err(|_| CryptoError::BadHeader("Argon2 参数非法".into()))?;
    let argon = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let pwd = Zeroizing::new(password.as_bytes().to_vec());
    argon
        .hash_password_into(pwd.as_slice(), salt, out.as_mut_slice())
        .map_err(|_| CryptoError::BadHeader("Argon2 派生失败".into()))?;
    let mut kek = [0u8; KEK_LEN];
    kek.copy_from_slice(out.as_ref());
    Ok(Kek(kek))
}

/// 用默认参数从主密码派生 KEK。
#[allow(dead_code)]
pub fn derive_kek_default(password: &str, salt: &[u8]) -> Result<Kek> {
    derive_kek(password, salt, ARGON2_M, ARGON2_T, ARGON2_P)
}

/// 生成随机 DEK（32 字节）。
pub fn generate_dek() -> Zeroizing<[u8; 32]> {
    Zeroizing::new(rand::random::<[u8; 32]>())
}

/// DEK（文件数据密钥），内存清零。
#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct Dek(pub [u8; 32]);

impl Dek {
    #[allow(dead_code)]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 用 KEK 包裹当前 DEK。
    pub fn wrap(&self, kek: &Kek) -> Result<WrappedDek> {
        kek.seal(&self.0)
    }

    /// 用 KEK 解出 DEK。
    pub fn unwrap(wrapped: &WrappedDek, kek: &Kek) -> Result<Dek> {
        let bytes = kek.open(wrapped)?;
        if bytes.len() != 32 {
            return Err(CryptoError::BadHeader("DEK 长度非法".into()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Dek(arr))
    }

    /// 重包裹：用新 KEK 重新包裹 DEK（改主密码时调用，无需重加密正文）。
    pub fn rewrap(&self, new_kek: &Kek) -> Result<WrappedDek> {
        new_kek.seal(&self.0)
    }
}

/// 用 DEK 加密明文（返回 nonce + ct）。
pub fn encrypt_body(dek: &Dek, plaintext: &[u8]) -> Result<(String, String)> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&dek.0));
    let nonce_bytes = rand::random::<[u8; NONCE_LEN]>();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| CryptoError::IntegrityFailed)?;
    Ok((format::b64::encode(&nonce_bytes), format::b64::encode(&ct)))
}

/// 用 DEK 解密正文（认证失败即密文被篡改）。
pub fn decrypt_body(dek: &Dek, nonce_b64: &str, ct_b64: &str) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&dek.0));
    let nonce_bytes = format::b64::decode(nonce_b64)?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(CryptoError::BadHeader("数据 nonce 长度非法".into()));
    }
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = format::b64::decode(ct_b64)?;
    cipher
        .decrypt(nonce, ct.as_slice())
        .map_err(|_| CryptoError::IntegrityFailed)
}

/// 生成随机 salt。
pub fn generate_salt() -> [u8; 16] {
    rand::random::<[u8; 16]>()
}

/// 构造默认 KDF 参数（含随机 salt）。
pub fn new_kdf_params() -> KdfParams {
    KdfParams {
        algo: format::KDF_ALGO.to_string(),
        m: ARGON2_M,
        t: ARGON2_T,
        p: ARGON2_P,
        salt: format::b64::encode(&generate_salt()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kek_derive_and_dek_roundtrip() {
        let salt = generate_salt();
        let kek = derive_kek_default("correct horse battery staple", &salt).unwrap();

        let dek = Dek(*generate_dek());
        let wrapped = dek.wrap(&kek).unwrap();
        let opened = Dek::unwrap(&wrapped, &kek).unwrap();
        assert_eq!(opened.0, dek.0);
    }

    #[test]
    fn wrong_password_fails() {
        let salt = generate_salt();
        let kek = derive_kek_default("right-password", &salt).unwrap();
        let wrong = derive_kek_default("wrong-password", &salt).unwrap();

        let dek = Dek(*generate_dek());
        let wrapped = dek.wrap(&kek).unwrap();
        assert!(Dek::unwrap(&wrapped, &wrong).is_err());
    }

    #[test]
    fn body_roundtrip_and_tamper() {
        let dek = Dek(*generate_dek());
        let (nonce, ct) = encrypt_body(&dek, b"hello markdown").unwrap();
        let plain = decrypt_body(&dek, &nonce, &ct).unwrap();
        assert_eq!(plain, b"hello markdown");

        // 篡改一个字节应失败
        let mut bad = format::b64::decode(&ct).unwrap();
        bad[0] ^= 0xff;
        let bad_ct = format::b64::encode(&bad);
        assert!(decrypt_body(&dek, &nonce, &bad_ct).is_err());
    }
}
