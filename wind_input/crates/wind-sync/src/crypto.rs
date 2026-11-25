//! 端到端加密（设计 §5）：DEK（即同步密钥）、对象加解密、密钥校验、blob id。

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("同步密钥不对")]
    BadKey,
    #[error("远端同步库格式不受支持，请重置远端")]
    UnsupportedMeta,
    #[error("对象解密失败（损坏或被调换）")]
    Decrypt,
    #[error("格式错误: {0}")]
    Format(String),
}

pub const META_FORMAT: u32 = 2;
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
    /// 本机保存用的 base64（密钥库内部格式，不面向用户）。
    pub fn to_base64(&self) -> String {
        B64.encode(self.0)
    }
    /// 同步密钥：base32 小写无填充（52 字符），每 4 字符一组用 `-` 连接。
    pub fn to_key_string(&self) -> String {
        let raw = data_encoding::BASE32_NOPAD.encode(&self.0).to_lowercase();
        raw.as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).expect("base32 为 ASCII"))
            .collect::<Vec<_>>()
            .join("-")
    }
    /// 解析同步密钥：忽略大小写、空白与 `-`。
    pub fn from_key_string(s: &str) -> Result<Self, CryptoError> {
        let norm: String = s
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let v = data_encoding::BASE32_NOPAD
            .decode(norm.as_bytes())
            .map_err(|_| CryptoError::Format("同步密钥含非法字符".into()))?;
        let a: [u8; 32] = v
            .try_into()
            .map_err(|_| CryptoError::Format("同步密钥长度不对".into()))?;
        Ok(Dek(a))
    }
    /// 本机保存用的 base64（密钥库内部格式，不面向用户）。
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

/// 子密钥用途；每种用途对应一个固定的 HKDF `info` 标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// 文件同步对象加密与 blob id。
    Files,
    /// 设备名册里的设备信息对象。
    DevInfo,
    /// 剪贴板消息。
    Clip,
    /// 图片 / 文件传输。
    Xfer,
    /// 局域网直连的预共享密钥。
    Lan,
}

impl Purpose {
    pub fn info(self) -> &'static [u8] {
        match self {
            Purpose::Files => b"windinput-sync/files",
            Purpose::DevInfo => b"windinput-sync/devinfo",
            Purpose::Clip => b"windinput-sync/clip",
            Purpose::Xfer => b"windinput-sync/xfer",
            Purpose::Lan => b"windinput-sync/lan",
        }
    }
}

/// 由 DEK 经 HKDF-SHA256 派生的用途子密钥。
#[derive(Clone, PartialEq, Eq)]
pub struct SubKey([u8; 32]);

impl std::fmt::Debug for SubKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubKey(..)")
    }
}

impl SubKey {
    /// 原始字节：仅供子密钥的最终使用方（如局域网握手）取用。
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

/// 能做对象加解密的密钥（子密钥或按用途的新类型；不含 DEK 与 [`LanPsk`]）。
pub trait ObjKey {
    fn sub(&self) -> &SubKey;
}

impl ObjKey for SubKey {
    fn sub(&self) -> &SubKey {
        self
    }
}

macro_rules! typed_key {
    ($(#[$m:meta])* $name:ident, $purpose:expr, $method:ident, obj) => {
        typed_key!(@def $(#[$m])* $name, $purpose, $method);
        impl ObjKey for $name {
            fn sub(&self) -> &SubKey {
                &self.0
            }
        }
    };
    ($(#[$m:meta])* $name:ident, $purpose:expr, $method:ident) => {
        typed_key!(@def $(#[$m])* $name, $purpose, $method);
    };
    (@def $(#[$m:meta])* $name:ident, $purpose:expr, $method:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(SubKey);

        impl $name {
            pub fn expose(&self) -> &[u8; 32] {
                self.0.expose()
            }
        }

        impl Dek {
            pub fn $method(&self) -> $name {
                $name(self.derive($purpose))
            }
        }
    };
}

typed_key!(
    /// 文件同步（清单、blob）。
    FilesKey,
    Purpose::Files,
    files_key,
    obj
);
typed_key!(
    /// 设备信息对象。
    DevInfoKey,
    Purpose::DevInfo,
    devinfo_key,
    obj
);
typed_key!(
    /// 剪贴板消息（P3）。
    ClipKey,
    Purpose::Clip,
    clip_key,
    obj
);
typed_key!(
    /// 图片 / 文件传输（P5）。
    XferKey,
    Purpose::Xfer,
    xfer_key,
    obj
);
typed_key!(
    /// 局域网预共享密钥（P4）：不用于对象加密。
    LanPsk,
    Purpose::Lan,
    lan_psk
);

impl Dek {
    /// HKDF-SHA256 派生用途子密钥（无盐，`info` 为用途标签）。
    pub fn derive(&self, purpose: Purpose) -> SubKey {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.0)
            .expand(purpose.info(), &mut out)
            .expect("32 字节在 HKDF 输出上限内");
        SubKey(out)
    }
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
    DevInfo,
}

fn aad(kind: ObjKind, id: &str) -> Vec<u8> {
    let k = match kind {
        ObjKind::Manifest => "manifest",
        ObjKind::Blob => "blob",
        ObjKind::DevInfo => "devinfo",
    };
    format!("windinput-sync/v1/{k}/{id}").into_bytes()
}

pub fn seal(key: &impl ObjKey, kind: ObjKind, id: &str, plain: &[u8]) -> Vec<u8> {
    seal_with(&key.sub().0, &aad(kind, id), plain)
}

pub fn open(
    key: &impl ObjKey,
    kind: ObjKind,
    id: &str,
    data: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    open_with(&key.sub().0, &aad(kind, id), data)
}

/// 内容的 SHA-256（小写十六进制）。
pub fn content_hash(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8; 32], msg: &[u8]) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// blob id = base32(HMAC(files 子密钥, sha256(明文)))，小写无填充。入参为 [`content_hash`] 的十六进制。
pub fn blob_id(key: &FilesKey, hash_hex: &str) -> String {
    let raw = data_encoding::HEXLOWER
        .decode(hash_hex.as_bytes())
        .unwrap_or_else(|_| hash_hex.as_bytes().to_vec());
    data_encoding::BASE32_NOPAD
        .encode(&hmac(key.expose(), &raw))
        .to_lowercase()
}

fn key_check(dek: &Dek) -> String {
    B64.encode(hmac(&dek.0, b"windinput-sync/v1/key-check"))
}

/// `meta.json`：明文，只含格式版本与同步密钥的校验值（密钥本身不上传）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub format: u32,
    pub check: String,
}

impl Meta {
    pub fn new(dek: &Dek) -> Meta {
        Meta {
            format: META_FORMAT,
            check: key_check(dek),
        }
    }

    pub fn verify(&self, dek: &Dek) -> bool {
        self.check == key_check(dek)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("Meta 可序列化")
    }

    /// 旧口令格式（含 `kdf_alg` 等）或未知版本一律报「不受支持」。
    pub fn from_json(b: &[u8]) -> Result<Meta, CryptoError> {
        let m: Meta = serde_json::from_slice(b).map_err(|_| CryptoError::UnsupportedMeta)?;
        if m.format != META_FORMAT {
            return Err(CryptoError::UnsupportedMeta);
        }
        Ok(m)
    }
}

#[cfg(test)]
#[path = "crypto_tests.rs"]
mod tests;
