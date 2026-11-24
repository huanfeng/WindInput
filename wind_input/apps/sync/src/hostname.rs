//! 本机主机名（不引入新依赖）：Windows 读 `COMPUTERNAME`，Unix 调 `gethostname`。

const FALLBACK: &str = "未命名设备";

pub fn host_name() -> String {
    raw()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| FALLBACK.into())
}

#[cfg(windows)]
fn raw() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(unix)]
fn raw() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: 缓冲区有效且长度如实传入；失败返回非 0。
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

#[cfg(not(any(windows, unix)))]
fn raw() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_name_is_never_empty() {
        assert!(!super::host_name().is_empty());
    }
}
