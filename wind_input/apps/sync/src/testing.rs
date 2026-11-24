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
    /// `data.editSessions` 回报的「用户正在编辑」的文件（相对数据目录）。
    /// 回「等待核心回应超时」的 `"方法 path"`。
    pub timeout: Mutex<Vec<String>>,
    /// 只失败一次的 `"方法 path"`（用后移除）。
    pub fail_once: Mutex<Vec<String>>,
    pub user_sessions: Mutex<Vec<String>>,
    /// 每次请求到达时先执行（模拟 core 处理请求时顺带改文件等副作用）。
    #[allow(clippy::type_complexity)]
    pub on_call: Mutex<Option<Box<dyn Fn(&str, &Value) + Send>>>,
}

impl FakeCore {
    /// 请求序列，形如 `data.beginEdit pinyin/user.dict.yaml` / `config.reload`。
    /// 只读查询 `data.editSessions` 不列入。
    pub fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m != "data.editSessions")
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
        if let Some(f) = self.on_call.lock().unwrap().as_ref() {
            f(method, &params);
        }
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        if self.timeout.lock().unwrap().contains(&key) {
            return Err(crate::link::TIMEOUT_MSG.into());
        }
        {
            let mut once = self.fail_once.lock().unwrap();
            if let Some(i) = once.iter().position(|k| *k == key) {
                once.remove(i);
                return Err("偶发失败".into());
            }
        }
        if self.fail.lock().unwrap().contains(&key) {
            return Err("只读模式".into());
        }
        if method == "data.editSessions" {
            let v: Vec<Value> = self
                .user_sessions
                .lock()
                .unwrap()
                .iter()
                .map(|f| json!({ "file": f, "since_secs": 1 }))
                .collect();
            return Ok(Value::Array(v));
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
