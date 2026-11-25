//! 机密存储（设计 §3.3）：DEK + WebDAV 密码 / S3 secret 合存一份 `secrets.bin`。
//!
//! 存储层只管字节，由平台实现保护：
//! - Windows：DPAPI（当前用户）封装后写文件。
//! - macOS：内容进 Keychain 通用密码项，文件里只留条目名。
//! - 其它（Linux 等）：0600 文件，创建时就带权限，不先写后 chmod。

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "secrets.bin";

/// 平台机密存储：读 / 写 / 删一份不透明字节。
pub trait SecretStore: Send {
    fn read(&self) -> io::Result<Option<Vec<u8>>>;
    fn write(&self, bytes: &[u8]) -> io::Result<()>;
    fn delete(&self) -> io::Result<()>;
}

/// 本机持有的机密。`Debug` 不输出内容。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secrets {
    /// DEK 的 base64。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dek: Option<String>,
    /// WebDAV 密码或 S3 secret access key。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// 保存 `password` 时所对应存储位置的指纹（不含机密）；启动时与当前设置比对，对不上就丢弃密码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Secrets {{ dek: {}, password: {} }}",
            self.dek.is_some(),
            self.password.is_some()
        )
    }
}

impl Secrets {
    pub fn is_empty(&self) -> bool {
        self.dek.is_none() && self.password.is_none()
    }

    /// 读不出来（损坏、解不开）按错误返回，不当作「没有」：静默当空会让用户以为要重新设置。
    pub fn load(store: &dyn SecretStore) -> Result<Secrets, String> {
        match store.read().map_err(|e| format!("读取机密存储失败: {e}"))? {
            None => Ok(Secrets::default()),
            Some(mut b) => {
                let r = serde_json::from_slice(&b).map_err(|e| format!("机密存储内容损坏: {e}"));
                crate::fsutil::wipe(&mut b);
                r
            }
        }
    }

    pub fn save(&self, store: &dyn SecretStore) -> Result<(), String> {
        if self.is_empty() {
            return store.delete().map_err(|e| format!("清除机密存储失败: {e}"));
        }
        let mut b = serde_json::to_vec(self).map_err(|e| format!("序列化机密失败: {e}"))?;
        let r = store.write(&b).map_err(|e| format!("写机密存储失败: {e}"));
        crate::fsutil::wipe(&mut b);
        r
    }
}

/// 同目录临时文件 + rename；`mode` 在 Unix 上于创建时设定。
fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| io::Error::other("无父目录"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = crate::fsutil::tmp_sibling(path, ".tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut f = opts.open(&tmp)?;
    let r = f
        .write_all(bytes)
        .and_then(|()| f.sync_all())
        .and_then(|()| {
            drop(f);
            std::fs::rename(&tmp, path)
        });
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

fn read_opt(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn remove_opt(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 0600 文件（Linux 与其它平台；其余平台的单测也走它）。
pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    pub fn new(path: PathBuf) -> Self {
        FileSecretStore { path }
    }
}

impl SecretStore for FileSecretStore {
    fn read(&self) -> io::Result<Option<Vec<u8>>> {
        read_opt(&self.path)
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        write_atomic(&self.path, bytes, true)
    }
    fn delete(&self) -> io::Result<()> {
        remove_opt(&self.path)
    }
}

/// 本平台的默认机密存储，文件位于 `<state_dir>/secrets.bin`。
pub fn default_store(state_dir: &Path) -> Box<dyn SecretStore> {
    let path = state_dir.join(FILE_NAME);
    #[cfg(windows)]
    return Box::new(dpapi::DpapiSecretStore::new(path));
    #[cfg(target_os = "macos")]
    return Box::new(keychain::KeychainSecretStore::new(path));
    #[cfg(not(any(windows, target_os = "macos")))]
    Box::new(FileSecretStore::new(path))
}

#[cfg(windows)]
mod dpapi {
    use super::*;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;

    /// DPAPI 当前用户封装；密文写文件。
    pub struct DpapiSecretStore {
        path: PathBuf,
    }

    impl DpapiSecretStore {
        pub fn new(path: PathBuf) -> Self {
            DpapiSecretStore { path }
        }
    }

    fn blob_of(b: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: b.len() as u32,
            pbData: b.as_ptr() as *mut u8,
        }
    }

    /// `wipe_source`：系统缓冲区装的是明文（解密结果）时，释放前先抹掉。
    fn take(out: CRYPT_INTEGER_BLOB, wipe_source: bool) -> Vec<u8> {
        // SAFETY: 系统返回的缓冲区，长度由 cbData 给出；拷出后用 LocalFree 释放。
        unsafe {
            let src = std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize);
            let v = src.to_vec();
            if wipe_source {
                crate::fsutil::wipe(src);
            }
            let _ = LocalFree(HLOCAL(out.pbData as _));
            v
        }
    }

    impl SecretStore for DpapiSecretStore {
        fn read(&self) -> io::Result<Option<Vec<u8>>> {
            let Some(enc) = read_opt(&self.path)? else {
                return Ok(None);
            };
            let input = blob_of(&enc);
            let mut out = CRYPT_INTEGER_BLOB::default();
            // SAFETY: input 指向 enc，调用期间有效；out 由系统分配，下面 take 负责释放。
            unsafe {
                CryptUnprotectData(
                    &input,
                    None,
                    None,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
            .map_err(|e| io::Error::other(format!("DPAPI 解密失败（换了用户或机器？）: {e}")))?;
            Ok(Some(take(out, true)))
        }

        fn write(&self, bytes: &[u8]) -> io::Result<()> {
            let input = blob_of(bytes);
            let mut out = CRYPT_INTEGER_BLOB::default();
            // SAFETY: 同上。
            unsafe {
                CryptProtectData(
                    &input,
                    PCWSTR::null(),
                    None,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
            .map_err(|e| io::Error::other(format!("DPAPI 加密失败: {e}")))?;
            write_atomic(&self.path, &take(out, false), false)
        }

        fn delete(&self) -> io::Result<()> {
            remove_opt(&self.path)
        }
    }
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::*;
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const SERVICE: &str = "com.windinput.sync";

    /// 内容存 Keychain 通用密码项；`path` 文件里只有条目名。
    pub struct KeychainSecretStore {
        path: PathBuf,
    }

    impl KeychainSecretStore {
        pub fn new(path: PathBuf) -> Self {
            KeychainSecretStore { path }
        }

        fn entry(&self) -> io::Result<Option<String>> {
            Ok(read_opt(&self.path)?
                .and_then(|b| String::from_utf8(b).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()))
        }
    }

    fn err(e: security_framework::base::Error) -> io::Error {
        io::Error::other(format!("Keychain: {e}"))
    }

    impl SecretStore for KeychainSecretStore {
        fn read(&self) -> io::Result<Option<Vec<u8>>> {
            match self.entry()? {
                None => Ok(None),
                Some(name) => get_generic_password(SERVICE, &name).map(Some).map_err(err),
            }
        }

        fn write(&self, bytes: &[u8]) -> io::Result<()> {
            let name = match self.entry()? {
                Some(n) => n,
                None => {
                    let r = wind_sync::crypto::random_bytes::<8>();
                    let hex: String = r.iter().map(|b| format!("{b:02x}")).collect();
                    format!("secrets-{hex}")
                }
            };
            set_generic_password(SERVICE, &name, bytes).map_err(err)?;
            write_atomic(&self.path, name.as_bytes(), true)
        }

        fn delete(&self) -> io::Result<()> {
            if let Some(name) = self.entry()? {
                let _ = delete_generic_password(SERVICE, &name);
            }
            remove_opt(&self.path)
        }
    }
}

#[cfg(test)]
#[path = "secrets_tests.rs"]
mod tests;
