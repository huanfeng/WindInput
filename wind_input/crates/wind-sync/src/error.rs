use crate::backend::BackendError;
use crate::crypto::CryptoError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("后端: {0}")]
    Backend(#[from] BackendError),
    #[error("加密: {0}")]
    Crypto(#[from] CryptoError),
    #[error("本地存储: {0}")]
    Local(String),
    #[error("远端未初始化")]
    NotInitialized,
    #[error("远端已初始化")]
    AlreadyInitialized,
    #[error("同步已停止，等待用户处理: {0}")]
    Halted(String),
}

impl SyncError {
    /// 是否值得自动退避重试。认证失败、口令/密钥错误、远端格式不认识都要等用户处理。
    pub fn retryable(&self) -> bool {
        match self {
            SyncError::Backend(e) => e.retryable(),
            SyncError::Local(_) => true,
            _ => false,
        }
    }
}
