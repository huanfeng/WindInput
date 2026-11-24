//! 测试替身：记录请求与发出帧的假 core、固定返回内存后端的工厂。
//! 放在库里（而非 `cfg(test)`）是因为集成测试也要用。

use std::sync::Mutex;

use serde_json::{Value, json};
use wind_sync::{Backend, MemBackend};
use wind_sync_ipc::{FromSync, SyncSettings};

use crate::backends::BackendFactory;
use crate::link::CoreLink;

/// 假 core：请求默认全部成功；`fail` 里列出的 `"方法 path"`（无 path 参数时仅方法名）会失败。
#[derive(Default)]
pub struct FakeCore {
    pub calls: Mutex<Vec<(String, Value)>>,
    pub fail: Mutex<Vec<String>>,
    pub sent: Mutex<Vec<FromSync>>,
}

impl FakeCore {
    /// 请求序列，形如 `data.beginEdit pinyin/user.dict.yaml` / `config.reload`。
    pub fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(m, p)| match p.get("path").and_then(Value::as_str) {
                Some(x) => format!("{m} {x}"),
                None => m.clone(),
            })
            .collect()
    }
    pub fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }
    pub fn fail_on(&self, key: &str) {
        self.fail.lock().unwrap().push(key.to_string());
    }
}

impl CoreLink for FakeCore {
    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let key = match params.get("path").and_then(Value::as_str) {
            Some(p) => format!("{method} {p}"),
            None => method.to_string(),
        };
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        if self.fail.lock().unwrap().contains(&key) {
            return Err("只读模式".into());
        }
        Ok(json!({}))
    }
    fn send(&self, m: FromSync) {
        self.sent.lock().unwrap().push(m);
    }
}

/// 总是返回同一个（共享存储的）内存后端，忽略设置与凭据。
pub struct MemFactory(pub MemBackend);

impl BackendFactory for MemFactory {
    fn build(&self, _: &SyncSettings, _: &str) -> Result<Box<dyn Backend>, String> {
        Ok(Box::new(self.0.clone()))
    }
}
