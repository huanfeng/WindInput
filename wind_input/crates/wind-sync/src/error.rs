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
    #[error("另一台设备在使用相同设备号（{0}），请给本机改号")]
    DeviceIdConflict(String),
}

/// 大批量删除护栏触发时 `Halted` 消息的固定前缀（apps 侧据此识别并提示用户确认）。
pub const MASS_DELETION_PREFIX: &str = "检测到大批量删除";

impl SyncError {
    /// 大批量删除护栏：本轮要发布 `count` 个删除，而已同步文件共 `total` 个。
    /// 归入 `Halted`（需用户确认；确认后清除停止态并以 `allow_mass_delete` 重跑）。
    pub fn mass_deletion(count: usize, total: usize) -> Self {
        SyncError::Halted(format!(
            "{MASS_DELETION_PREFIX}：本次将删除 {count} 个文件（已同步共 {total} 个），\
             请确认本机用户目录完好后再继续"
        ))
    }

    pub fn is_mass_deletion(&self) -> bool {
        matches!(self, SyncError::Halted(m) if m.starts_with(MASS_DELETION_PREFIX))
    }

    /// 是否值得自动退避重试。认证失败、口令/密钥错误、远端格式不认识都要等用户处理。
    pub fn retryable(&self) -> bool {
        match self {
            SyncError::Backend(e) => e.retryable(),
            SyncError::Local(_) => true,
            _ => false,
        }
    }
}
