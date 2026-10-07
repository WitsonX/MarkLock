//! `.mdl` 文件的序列化与反序列化。
//!
//! 磁盘布局：
//! ```text
//! [4 字节大端 header_len][header JSON 字节][nonce_b64 换行][ct_b64]
//! ```
//!
//! 头与数据区分离，头可独立读取（用于解锁前校验格式、KDF 参数），
//! 数据区密文只在真正需要解密时才整体加载。

use std::io::Read;

use super::error::{CryptoError, Result};
use super::format::{self, EncryptedBody, FileHeader, HEADER_LEN_BYTES};

/// 从字节序列化完整 `.mdl` 文件（头 + 数据区）。
pub fn serialize(header: &FileHeader, body: &EncryptedBody) -> Result<Vec<u8>> {
    let header_json = serde_json::to_vec(header)
        .map_err(|e| CryptoError::BadHeader(format!("序列化文件头失败: {e}")))?;
    let len = header_json.len();
    if len > u32::MAX as usize {
        return Err(CryptoError::BadHeader("文件头过大".into()));
    }
    let mut out = Vec::with_capacity(HEADER_LEN_BYTES + len + 1 + body.nonce.len() + 1 + body.ct.len());
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.extend_from_slice(&header_json);
    out.push(b'\n');
    out.extend_from_slice(body.nonce.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(body.ct.as_bytes());
    Ok(out)
}

/// 解析文件头（不解密数据区）。
pub fn parse_header(bytes: &[u8]) -> Result<(FileHeader, &[u8])> {
    if bytes.len() < HEADER_LEN_BYTES {
        return Err(CryptoError::BadHeader("文件过短，不是有效的 .mdl".into()));
    }
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let header_end = HEADER_LEN_BYTES + len;
    if bytes.len() < header_end {
        return Err(CryptoError::BadHeader("文件头长度越界".into()));
    }
    let header: FileHeader = serde_json::from_slice(&bytes[HEADER_LEN_BYTES..header_end])
        .map_err(|e| CryptoError::BadHeader(format!("文件头 JSON 解析失败: {e}")))?;

    // 校验格式标识与算法（兼容单文件 .mdl 与单文件库 .mdlb）
    if header.format != format::FORMAT && header.format != format::VAULT_FILE_FORMAT {
        return Err(CryptoError::Unsupported(format!(
            "format={}（期望 {} 或 {}）",
            header.format,
            format::FORMAT,
            format::VAULT_FILE_FORMAT
        )));
    }
    if header.cipher != "aes-256-gcm" {
        return Err(CryptoError::Unsupported(format!("cipher={}", header.cipher)));
    }
    if header.kdf.algo != format::KDF_ALGO {
        return Err(CryptoError::Unsupported(format!("kdf={}", header.kdf.algo)));
    }

    // 剩余部分是数据区
    Ok((header, &bytes[header_end..]))
}

/// 从数据区字节解析出 nonce + ct。
///
/// 数据区布局为 `\n<nonce>\n<ct>`（头后第一个 `\n` 是分隔符）。
pub fn parse_body(rest: &[u8]) -> Result<EncryptedBody> {
    let s = std::str::from_utf8(rest)
        .map_err(|_| CryptoError::BadHeader("数据区不是合法文本".into()))?;
    // 去掉头与数据区之间的换行符（若有）
    let s = s.strip_prefix('\n').unwrap_or(s);
    let mut lines = s.split('\n');
    let nonce = lines.next().ok_or_else(|| CryptoError::BadHeader("缺少 nonce".into()))?;
    let ct = lines.next().ok_or_else(|| CryptoError::BadHeader("缺少密文".into()))?;
    if nonce.is_empty() || ct.is_empty() {
        return Err(CryptoError::BadHeader("数据区为空".into()));
    }
    Ok(EncryptedBody {
        nonce: nonce.to_string(),
        ct: ct.to_string(),
    })
}

/// 从 reader 读整个文件字节。
#[allow(dead_code)]
pub fn read_all<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)
        .map_err(|e| CryptoError::Io(e.to_string()))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::format::{KdfParams, WrappedDek};

    fn sample_header() -> FileHeader {
        FileHeader {
            format: format::FORMAT.to_string(),
            kdf: KdfParams::default(),
            cipher: "aes-256-gcm".to_string(),
            wrapped_dek: WrappedDek {
                nonce: "nonce".into(),
                ct: "ct".into(),
            },
            hint: None,
        }
    }

    #[test]
    fn serialize_parse_roundtrip() {
        let h = sample_header();
        let body = EncryptedBody {
            nonce: "abc123".into(),
            ct: "def456".into(),
        };
        let bytes = serialize(&h, &body).unwrap();
        let (h2, rest) = parse_header(&bytes).unwrap();
        assert_eq!(h2.format, h.format);
        assert_eq!(h2.cipher, h.cipher);
        let body2 = parse_body(rest).unwrap();
        assert_eq!(body2.nonce, body.nonce);
        assert_eq!(body2.ct, body.ct);
    }

    #[test]
    fn wrong_format_rejected() {
        let mut h = sample_header();
        h.format = "mlk/0".into();
        let body = EncryptedBody { nonce: "a".into(), ct: "b".into() };
        let bytes = serialize(&h, &body).unwrap();
        assert!(matches!(
            parse_header(&bytes),
            Err(CryptoError::Unsupported(_))
        ));
    }
}
