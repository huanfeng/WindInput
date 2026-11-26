//! 远端存储抽象。真实后端在 `net`（feature `net`）；这里是 trait 与带故障注入的内存后端。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// 后端错误分类：决定上层是否重试。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("not found: {0}")]
    NotFound(String),
    /// 凭据被拒；不重试，等用户处理。
    #[error("auth: {0}")]
    Auth(String),
    /// 网络抖动、限流、5xx：可退避重试。
    #[error("transient: {0}")]
    Transient(String),
    /// 远端格式不认识等：不重试。
    #[error("fatal: {0}")]
    Fatal(String),
}

impl BackendError {
    pub fn retryable(&self) -> bool {
        matches!(self, BackendError::Transient(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjMeta {
    pub key: String,
    pub size: u64,
    /// 修改时间（unix 秒）；后端给不出则为 None（回收时按「不够老」处理）。
    pub modified: Option<u64>,
}

/// 服务端给出的空间用量（WebDAV 配额、官方服务的计费口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderUsage {
    pub used: u64,
    /// 总容量；服务端不给或不限量为 None。
    pub quota: Option<u64>,
}

/// 键为 `/` 分隔的相对路径。删除不存在的键视为成功。
pub trait Backend {
    fn list(&self, prefix: &str) -> Result<Vec<ObjMeta>, BackendError>;
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError>;
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError>;
    fn delete(&self, key: &str) -> Result<(), BackendError>;

    /// 单个对象的元信息；不存在返回 `None`。
    /// 默认用 `list(key)` 过滤出精确等于 key 的项（兼容自定义后端）；真实后端应覆盖为单对象请求。
    fn stat(&self, key: &str) -> Result<Option<ObjMeta>, BackendError> {
        Ok(self.list(key)?.into_iter().find(|o| o.key == key))
    }

    /// 连通性与读写权限检查：写入、读回并删除一个探测对象。
    /// 凭据错误返回 `Auth`，网络/超时返回 `Transient`，可据此给用户不同提示。
    fn test_connection(&self) -> Result<(), BackendError> {
        const PROBE: &str = ".wind-sync-probe";
        self.put(PROBE, b"ok")?;
        match self.get(PROBE)? {
            Some(v) if v == b"ok" => {}
            _ => return Err(BackendError::Fatal("探测对象读回不一致".into())),
        }
        self.delete(PROBE)
    }

    /// 服务端的空间用量；给不出（不支持）返回 `Ok(None)`。
    fn usage(&self) -> Result<Option<ProviderUsage>, BackendError> {
        Ok(None)
    }
}

#[derive(Default)]
struct MemInner {
    objs: BTreeMap<String, (Vec<u8>, u64)>,
    now: u64,
    puts: u64,
    /// 各方法调用次数（含失败的）；list 另记前缀。
    calls: Calls,
    /// (还剩几次 put 后失败, 错误)：计到 1 时这一次 put 失败，且只失败一次。
    fail_put: Option<(u64, BackendError)>,
    fail_all: Option<BackendError>,
    provider: Option<ProviderUsage>,
}

/// 各方法的调用计数。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Calls {
    pub list: u64,
    pub get: u64,
    pub put: u64,
    pub delete: u64,
    pub stat: u64,
    /// 每次 `list` 的前缀，按调用顺序。
    pub list_prefixes: Vec<String>,
}

/// 内存后端；`clone` 共享同一份存储（模拟多台设备连同一个远端）。
#[derive(Clone, Default)]
pub struct MemBackend {
    inner: Arc<Mutex<MemInner>>,
}

impl MemBackend {
    pub fn new() -> Self {
        Self::default()
    }
    fn g(&self) -> std::sync::MutexGuard<'_, MemInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// 之后写入对象的修改时间。
    pub fn set_now(&self, now: u64) {
        self.g().now = now;
    }
    /// 从现在起第 `n` 次（1 起）put 失败一次。
    pub fn fail_put_nth(&self, n: u64, err: BackendError) {
        self.g().fail_put = Some((n, err));
    }
    /// 所有操作都返回该错误，传 None 取消。
    pub fn fail_all(&self, err: Option<BackendError>) {
        self.g().fail_all = err;
    }
    /// `usage()` 的返回值。
    pub fn set_provider_usage(&self, u: Option<ProviderUsage>) {
        self.g().provider = u;
    }
    /// 方法调用计数快照。
    pub fn calls(&self) -> Calls {
        self.g().calls.clone()
    }
    /// 计数清零（测试里隔开准备阶段与被测阶段）。
    pub fn reset_calls(&self) {
        self.g().calls = Calls::default();
    }
    pub fn put_count(&self) -> u64 {
        self.g().puts
    }
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        self.g()
            .objs
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }
    /// 绕过故障注入直接读写（测试篡改用）。
    pub fn raw_get(&self, key: &str) -> Option<Vec<u8>> {
        self.g().objs.get(key).map(|v| v.0.clone())
    }
    pub fn raw_put(&self, key: &str, bytes: Vec<u8>) {
        let now = self.g().now;
        self.g().objs.insert(key.to_string(), (bytes, now));
    }
}

impl Backend for MemBackend {
    fn list(&self, prefix: &str) -> Result<Vec<ObjMeta>, BackendError> {
        let mut g = self.g();
        g.calls.list += 1;
        g.calls.list_prefixes.push(prefix.to_string());
        if let Some(e) = &g.fail_all {
            return Err(e.clone());
        }
        Ok(g.objs
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, (v, t))| ObjMeta {
                key: k.clone(),
                size: v.len() as u64,
                modified: Some(*t),
            })
            .collect())
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError> {
        let mut g = self.g();
        g.calls.get += 1;
        if let Some(e) = &g.fail_all {
            return Err(e.clone());
        }
        Ok(g.objs.get(key).map(|v| v.0.clone()))
    }
    fn stat(&self, key: &str) -> Result<Option<ObjMeta>, BackendError> {
        let mut g = self.g();
        g.calls.stat += 1;
        if let Some(e) = &g.fail_all {
            return Err(e.clone());
        }
        Ok(g.objs.get(key).map(|(v, t)| ObjMeta {
            key: key.to_string(),
            size: v.len() as u64,
            modified: Some(*t),
        }))
    }
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        let mut g = self.g();
        g.calls.put += 1;
        if let Some(e) = &g.fail_all {
            return Err(e.clone());
        }
        g.puts += 1;
        if let Some((n, e)) = g.fail_put.take() {
            if n <= 1 {
                return Err(e);
            }
            g.fail_put = Some((n - 1, e));
        }
        let now = g.now;
        g.objs.insert(key.to_string(), (bytes.to_vec(), now));
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        let mut g = self.g();
        g.calls.delete += 1;
        if let Some(e) = &g.fail_all {
            return Err(e.clone());
        }
        g.objs.remove(key);
        Ok(())
    }
    fn usage(&self) -> Result<Option<ProviderUsage>, BackendError> {
        let g = self.g();
        if let Some(e) = &g.fail_all {
            return Err(e.clone());
        }
        Ok(g.provider)
    }
}

/// 列举时无视前缀、把全部对象都返回的后端（测试用）：模拟实现有误或被篡改的服务端。
#[cfg(test)]
pub struct ListIgnoresPrefix(pub MemBackend);

#[cfg(test)]
impl Backend for ListIgnoresPrefix {
    fn list(&self, _prefix: &str) -> Result<Vec<ObjMeta>, BackendError> {
        self.0.list("")
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, BackendError> {
        self.0.get(key)
    }
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), BackendError> {
        self.0.put(key, bytes)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.0.delete(key)
    }
}
