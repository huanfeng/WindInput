//! 数据同步的纯逻辑（设计 docs/design/data-sync.md）：无网络、无异步、无文件监听。
//!
//! - [`backend`]：远端存储 trait 与带故障注入的内存后端
//! - [`crypto`]：DEK、口令封装、对象加解密、blob id、`meta.json`
//! - [`devices`]：设备名册（`devices/<dev>/info`）
//! - [`manifest`]：每设备清单与远端布局
//! - [`plan`]：逐文件裁决；[`merge`]：三方合并；[`scope`]：同步范围
//! - [`engine`]：一次同步的编排；[`remote`]：初始化/加入/改口令/回收
//! - [`state`]：可序列化的本机状态与退避

pub mod backend;
pub mod crypto;
pub mod devices;
pub mod engine;
pub mod error;
pub mod local;
pub mod manifest;
pub mod merge;
#[cfg(feature = "net")]
pub mod net;
pub mod plan;
pub mod remote;
pub mod scope;
pub mod state;

pub use backend::{Backend, BackendError, MemBackend, ObjMeta};
pub use crypto::{Dek, KdfParams};
pub use engine::{SyncCtx, SyncReport, sync_once};
pub use error::SyncError;
pub use local::{LocalError, LocalStore, MemLocal};
#[cfg(feature = "net")]
pub use net::{S3Backend, S3Config, WebDavBackend, WebDavConfig};
pub use scope::{ScopeFlags, in_scope};
pub use state::SyncState;
