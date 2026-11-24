//! 文件小工具：唯一临时文件名、明文缓冲清零。

use std::path::{Path, PathBuf};

/// 与 `path` 同目录的临时文件：`.<原名>.<pid>-<随机>` + `suffix`。
/// 并发写同一目标（两个进程、两次重入）时各用各的临时文件，不会互相截断或误删。
pub fn tmp_sibling(path: &Path, suffix: &str) -> PathBuf {
    let r = wind_sync::crypto::random_bytes::<6>();
    let hex: String = r.iter().map(|b| format!("{b:02x}")).collect();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}-{hex}{suffix}", std::process::id()))
}

/// 清零缓冲（volatile 写，避免被优化掉）。用于机密明文用后即抹。
pub fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: b 是有效的 &mut u8。
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmp_names_are_unique_hidden_and_siblings() {
        let p = Path::new("/a/b/secrets.bin");
        let (a, b) = (tmp_sibling(p, ".tmp"), tmp_sibling(p, ".tmp"));
        assert_ne!(a, b);
        assert_eq!(a.parent(), p.parent());
        let n = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(n.starts_with(".secrets.bin.") && n.ends_with(".tmp"), "{n}");
        assert!(n.contains(&std::process::id().to_string()));
    }

    #[test]
    fn wipe_zeroes() {
        let mut v = b"secret".to_vec();
        wipe(&mut v);
        assert!(v.iter().all(|b| *b == 0));
    }
}
