//! 端到端加密（设计 §5）：DEK、口令封装、对象加解密、密钥校验、blob id。

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("口令错误")]
    BadPassphrase,
    #[error("密钥与远端不匹配")]
    BadKey,
    #[error("对象解密失败（损坏或被调换）")]
    Decrypt,
    #[error("格式错误: {0}")]
    Format(String),
}

pub const META_FORMAT: u32 = 1;
const OBJ_VERSION: u8 = 1;
const NONCE_LEN: usize = 24;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("系统随机数不可用");
    b
}

/// 数据密钥。不实现 Debug 明文输出。
#[derive(Clone, PartialEq, Eq)]
pub struct Dek([u8; 32]);

impl std::fmt::Debug for Dek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dek(..)")
    }
}

impl Dek {
    pub fn generate() -> Self {
        Dek(random_bytes::<32>())
    }
    /// 导出为 base64 字符串（用户自行备份）。
    pub fn to_base64(&self) -> String {
        B64.encode(self.0)
    }
    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        let v = B64
            .decode(s.trim())
            .map_err(|_| CryptoError::Format("密钥不是合法 base64".into()))?;
        let a: [u8; 32] = v
            .try_into()
            .map_err(|_| CryptoError::Format("密钥长度不对".into()))?;
        Ok(Dek(a))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    /// 出厂参数。
    pub const DEFAULT: KdfParams = KdfParams {
        m_kib: 65536,
        t: 3,
        p: 1,
    };
    /// 仅测试用，快。
    pub const FAST_TEST: KdfParams = KdfParams {
        m_kib: 8,
        t: 1,
        p: 1,
    };
    /// 远端 meta 是不可信输入：参数过大会被用来耗尽本机资源。
    fn validate(&self) -> Result<(), CryptoError> {
        if self.m_kib < 8
            || self.m_kib > 1 << 20
            || self.t == 0
            || self.t > 16
            || self.p == 0
            || self.p > 16
        {
            return Err(CryptoError::Format("KDF 参数越界".into()));
        }
        Ok(())
    }
}

fn derive_kek(pass: &str, salt: &[u8], kdf: &KdfParams) -> Result<[u8; 32], CryptoError> {
    kdf.validate()?;
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32))
        .map_err(|e| CryptoError::Format(format!("KDF 参数: {e}")))?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pass.as_bytes(), salt, &mut out)
        .map_err(|e| CryptoError::Format(format!("KDF: {e}")))?;
    Ok(out)
}

fn seal_with(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let c = XChaCha20Poly1305::new(key.into());
    let nonce: [u8; NONCE_LEN] = random_bytes();
    let ct = c
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .expect("XChaCha20Poly1305 加密不会失败");
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    out.push(OBJ_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

fn open_with(key: &[u8; 32], aad: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if data.len() < 1 + NONCE_LEN + 16 || data[0] != OBJ_VERSION {
        return Err(CryptoError::Decrypt);
    }
    let c = XChaCha20Poly1305::new(key.into());
    c.decrypt(
        XNonce::from_slice(&data[1..1 + NONCE_LEN]),
        Payload {
            msg: &data[1 + NONCE_LEN..],
            aad,
        },
    )
    .map_err(|_| CryptoError::Decrypt)
}

/// 对象类型；与 id 一起进 AAD，防止把 A 的密文放到 B 的键下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjKind {
    Manifest,
    Blob,
}

fn aad(kind: ObjKind, id: &str) -> Vec<u8> {
    let k = match kind {
        ObjKind::Manifest => "manifest",
        ObjKind::Blob => "blob",
    };
    format!("windinput-sync/v1/{k}/{id}").into_bytes()
}

pub fn seal(dek: &Dek, kind: ObjKind, id: &str, plain: &[u8]) -> Vec<u8> {
    seal_with(&dek.0, &aad(kind, id), plain)
}

pub fn open(dek: &Dek, kind: ObjKind, id: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    open_with(&dek.0, &aad(kind, id), data)
}

/// 内容的 SHA-256（小写十六进制）。
pub fn content_hash(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(dek: &Dek, msg: &[u8]) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(&dek.0).expect("HMAC 接受任意长度密钥");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// blob id = base32(HMAC(DEK, sha256(明文)))，小写无填充。入参为 [`content_hash`] 的十六进制。
pub fn blob_id(dek: &Dek, hash_hex: &str) -> String {
    let raw = data_encoding::HEXLOWER
        .decode(hash_hex.as_bytes())
        .unwrap_or_else(|_| hash_hex.as_bytes().to_vec());
    data_encoding::BASE32_NOPAD
        .encode(&hmac(dek, &raw))
        .to_lowercase()
}

fn key_check(dek: &Dek) -> String {
    B64.encode(hmac(dek, b"windinput-sync/v1/key-check"))
}

/// `meta.json`：明文，含 KDF 参数、盐、口令封装的 DEK、密钥校验值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub format: u32,
    pub kdf_alg: String,
    pub kdf: KdfParams,
    pub salt: String,
    pub wrapped_dek: String,
    pub check: String,
}

const WRAP_AAD: &[u8] = b"windinput-sync/v1/wrap";

impl Meta {
    /// 用口令封装 DEK（随机新盐）。改口令也走这里。
    pub fn new(dek: &Dek, pass: &str, kdf: KdfParams) -> Result<Meta, CryptoError> {
        let salt: [u8; 16] = random_bytes();
        let kek = derive_kek(pass, &salt, &kdf)?;
        Ok(Meta {
            format: META_FORMAT,
            kdf_alg: "argon2id".into(),
            kdf,
            salt: B64.encode(salt),
            wrapped_dek: B64.encode(seal_with(&kek, WRAP_AAD, &dek.0)),
            check: key_check(dek),
        })
    }

    pub fn unlock(&self, pass: &str) -> Result<Dek, CryptoError> {
        let salt = B64
            .decode(&self.salt)
            .map_err(|_| CryptoError::Format("盐".into()))?;
        let kek = derive_kek(pass, &salt, &self.kdf)?;
        let wrapped = B64
            .decode(&self.wrapped_dek)
            .map_err(|_| CryptoError::Format("wrapped_dek".into()))?;
        let raw = open_with(&kek, WRAP_AAD, &wrapped).map_err(|_| CryptoError::BadPassphrase)?;
        let a: [u8; 32] = raw.try_into().map_err(|_| CryptoError::BadPassphrase)?;
        let dek = Dek(a);
        if !self.check_dek(&dek) {
            return Err(CryptoError::BadKey);
        }
        Ok(dek)
    }

    pub fn check_dek(&self, dek: &Dek) -> bool {
        self.check == key_check(dek)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("Meta 可序列化")
    }

    pub fn from_json(b: &[u8]) -> Result<Meta, CryptoError> {
        let m: Meta = serde_json::from_slice(b)
            .map_err(|e| CryptoError::Format(format!("meta.json: {e}")))?;
        if m.format != META_FORMAT {
            return Err(CryptoError::Format(format!(
                "不认识的格式版本 {}",
                m.format
            )));
        }
        if m.kdf_alg != "argon2id" {
            return Err(CryptoError::Format("不认识的 KDF".into()));
        }
        m.kdf.validate()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_aad_binding() {
        let d = Dek::generate();
        let c = seal(&d, ObjKind::Blob, "a", b"hello");
        assert_eq!(open(&d, ObjKind::Blob, "a", &c).unwrap(), b"hello");
        assert!(open(&d, ObjKind::Blob, "b", &c).is_err());
        assert!(open(&d, ObjKind::Manifest, "a", &c).is_err());
        assert!(open(&Dek::generate(), ObjKind::Blob, "a", &c).is_err());
        let mut bad = c.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open(&d, ObjKind::Blob, "a", &bad).is_err());
    }

    #[test]
    fn nonce_is_random() {
        let d = Dek::generate();
        assert_ne!(
            seal(&d, ObjKind::Blob, "a", b"x"),
            seal(&d, ObjKind::Blob, "a", b"x")
        );
    }

    #[test]
    fn meta_unlock_and_wrong_pass() {
        let d = Dek::generate();
        let m = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
        let m2 = Meta::from_json(&m.to_json()).unwrap();
        assert_eq!(m2.unlock("pw").unwrap(), d);
        assert_eq!(m2.unlock("nope"), Err(CryptoError::BadPassphrase));
        assert!(m2.check_dek(&d));
        assert!(!m2.check_dek(&Dek::generate()));
    }

    #[test]
    fn same_pass_different_salt() {
        let d = Dek::generate();
        let a = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
        let b = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.wrapped_dek, b.wrapped_dek);
    }

    #[test]
    fn dek_base64_roundtrip_and_blob_id_stable() {
        let d = Dek::generate();
        assert_eq!(Dek::from_base64(&d.to_base64()).unwrap(), d);
        assert!(Dek::from_base64("abc").is_err());
        let h = content_hash(b"x");
        assert_eq!(blob_id(&d, &h), blob_id(&d, &h));
        assert_ne!(blob_id(&d, &h), blob_id(&Dek::generate(), &h));
    }

    #[test]
    fn hostile_kdf_params_rejected() {
        let d = Dek::generate();
        let mut m = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
        m.kdf.m_kib = u32::MAX;
        assert!(Meta::from_json(&m.to_json()).is_err());
    }
}
