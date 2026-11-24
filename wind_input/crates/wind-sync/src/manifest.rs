//! 每设备清单与远端布局（设计 §4）。

use crate::crypto::{self, CryptoError, FilesKey, ObjKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ROOT: &str = "windinput-sync/v1";
/// 每设备保留的清单代数。
pub const KEEP_GENS: u64 = 2;

pub fn meta_key() -> String {
    format!("{ROOT}/meta.json")
}
pub fn devices_prefix() -> String {
    format!("{ROOT}/devices/")
}
pub fn blobs_prefix() -> String {
    format!("{ROOT}/blobs/")
}
pub fn blob_key(id: &str) -> String {
    format!("{ROOT}/blobs/{id}")
}
pub fn manifest_key(dev: &str, gen_: u64) -> String {
    format!("{ROOT}/devices/{dev}/manifest.{gen_}")
}

/// 远端键是否安全：非空、无空段、无 `.`/`..` 段、无反斜杠与控制字符。
/// 后端列出的键不可信（恶意或出错的服务端），删除/读取前要校验。
pub fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains('\\')
        && !key.chars().any(char::is_control)
        && key
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// 设备号只允许字母数字与 `-_`：它会拼进远端键。
pub fn valid_device_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `devices/<dev>/manifest.<gen>` → (dev, gen)。
pub fn parse_manifest_key(key: &str) -> Option<(String, u64)> {
    let rest = key.strip_prefix(&devices_prefix())?;
    let (dev, file) = rest.split_once('/')?;
    let g = file.strip_prefix("manifest.")?.parse().ok()?;
    valid_device_id(dev).then(|| (dev.to_string(), g))
}

/// 清单条目。`hash` 为内容 SHA-256（删除时为空）；`base` 是本设备上次同步时的 hash；
/// `lamport`/`device` 是产生该版本的变更的逻辑时钟与设备（被领养时保持原值）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub hash: String,
    pub base: Option<String>,
    pub deleted: bool,
    pub lamport: u64,
    pub device: String,
    /// 本条版本合并/保存为冲突副本时已吸收的其它版本的 hash：持有这些版本的旧条目
    /// 不再被当作并发修改。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub absorbed: Vec<String>,
}

impl Entry {
    /// 该版本的内容 hash；删除为 None。
    pub fn value(&self) -> Option<&str> {
        (!self.deleted).then_some(self.hash.as_str())
    }
    /// 全序键：lamport 大者新，同值按设备号。
    pub fn order(&self) -> (u64, &str) {
        (self.lamport, self.device.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub device: String,
    pub gen_: u64,
    pub lamport: u64,
    pub files: BTreeMap<String, Entry>,
}

fn obj_id(dev: &str, gen_: u64) -> String {
    format!("{dev}/{gen_}")
}

impl Manifest {
    pub fn encode(&self, key: &FilesKey) -> Vec<u8> {
        let json = serde_json::to_vec(self).expect("Manifest 可序列化");
        crypto::seal(
            key,
            ObjKind::Manifest,
            &obj_id(&self.device, self.gen_),
            &json,
        )
    }

    /// 解密并核对内部的设备号与代数与键一致。
    pub fn decode(
        key: &FilesKey,
        dev: &str,
        gen_: u64,
        data: &[u8],
    ) -> Result<Manifest, CryptoError> {
        let json = crypto::open(key, ObjKind::Manifest, &obj_id(dev, gen_), data)?;
        let m: Manifest =
            serde_json::from_slice(&json).map_err(|e| CryptoError::Format(format!("清单: {e}")))?;
        if m.device != dev || m.gen_ != gen_ {
            return Err(CryptoError::Decrypt);
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        let mut files = BTreeMap::new();
        files.insert(
            "config.toml".into(),
            Entry {
                hash: "ab".into(),
                base: None,
                deleted: false,
                lamport: 3,
                device: "d1".into(),
                absorbed: vec![],
            },
        );
        Manifest {
            device: "d1".into(),
            gen_: 4,
            lamport: 3,
            files,
        }
    }

    #[test]
    fn encode_decode_and_swap_rejected() {
        let d = crate::crypto::Dek::generate().files_key();
        let m = sample();
        let c = m.encode(&d);
        assert_eq!(Manifest::decode(&d, "d1", 4, &c).unwrap(), m);
        assert!(Manifest::decode(&d, "d2", 4, &c).is_err());
        assert!(Manifest::decode(&d, "d1", 5, &c).is_err());
    }

    #[test]
    fn key_parse() {
        assert_eq!(
            parse_manifest_key(&manifest_key("dev-1", 7)),
            Some(("dev-1".into(), 7))
        );
        assert_eq!(
            parse_manifest_key("windinput-sync/v1/devices/a/other"),
            None
        );
        assert_eq!(
            parse_manifest_key("windinput-sync/v1/devices/a b/manifest.1"),
            None
        );
    }
}
