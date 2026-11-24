//! 把同步库的错误转成用户读得懂的中文，区分「认证失败」「网络不可达」「口令错误」「远端格式不认识」。

use wind_sync::crypto::CryptoError;
use wind_sync::{BackendError, SyncError};

pub fn backend_error(e: &BackendError) -> String {
    match e {
        BackendError::Auth(_) => "认证失败：用户名、密码或访问密钥不正确，或没有读写权限".into(),
        BackendError::Transient(d) => {
            format!("网络不可达或服务暂时不可用，稍后自动重试（{d}）")
        }
        BackendError::NotFound(d) => format!("找不到远端目录或对象，请检查地址与路径（{d}）"),
        BackendError::Fatal(d) => format!("远端格式不认识或服务返回异常（{d}）"),
    }
}

pub fn crypto_error(e: &CryptoError) -> String {
    match e {
        CryptoError::BadPassphrase => "口令错误".into(),
        CryptoError::BadKey => "密钥与远端不匹配：请确认密钥来自同一个同步库".into(),
        CryptoError::Decrypt => "远端数据无法解密：已损坏，或密钥与远端不匹配".into(),
        CryptoError::Format(d) => format!("远端格式不认识（{d}）"),
    }
}

pub fn sync_error(e: &SyncError) -> String {
    match e {
        SyncError::Backend(b) => backend_error(b),
        SyncError::Crypto(c) => crypto_error(c),
        SyncError::Local(d) => format!("本地文件处理失败：{d}"),
        SyncError::NotInitialized => "远端尚未初始化：请先在一台设备上设置同步口令".into(),
        SyncError::AlreadyInitialized => "远端已经初始化过，请用口令或密钥加入".into(),
        SyncError::Halted(d) => d.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_are_distinguishable() {
        let a = sync_error(&BackendError::Auth("HTTP 401".into()).into());
        let n = sync_error(&BackendError::Transient("超时".into()).into());
        let p = sync_error(&CryptoError::BadPassphrase.into());
        let f = sync_error(&BackendError::Fatal("HTTP 405".into()).into());
        assert!(a.starts_with("认证失败"));
        assert!(n.starts_with("网络不可达"));
        assert_eq!(p, "口令错误");
        assert!(f.starts_with("远端格式不认识"));
    }
}
