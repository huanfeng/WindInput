//! 数据同步门面：在输入法进程内跑 `wind_sync_host` 的 [`Runner`]（设计 docs/design/sync-messaging.md §3a.2）。
//!
//! 与桌面 `sync.*` RPC 语义一致，全部用 JSON 字符串进出，绑定层原样透传：
//!
//! - 设置文件、同步状态、机密都在 `<状态目录>/sync/`（机密为 0600 文件，由宿主排除出系统备份）。
//! - 不监听文件、不按间隔自行调度（`on_demand`）：何时同步由宿主定，见 [`MobileSync::sync_if_due`]。
//! - 设备类别固定为 `mobile`：配置、主题、兼容规则只与别的手机互通，词库与方案与桌面共用。
//! - 同步写入文件后的重载走 [`InProcLink`]，直接调核心（与桌面 RPC 共用 `external_files`）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use wind_coordinator::external_files;
use wind_sync::{DeviceClass, KdfParams};
use wind_sync_host::secrets;
use wind_sync_host::{BackendFactory, Config, CoreLink, Input, NetFactory, Runner, Timing};
use wind_sync_ipc::{FromSync, Op, Status, SyncSettings, ToSync};

use crate::MobileCore;

/// 命令等回执的上限：口令加入含 KDF 与网络往返。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Runner 与门面之间共享的状态：最新状态帧、等回执的命令。
#[derive(Default)]
struct Shared {
    status: Mutex<Status>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
}

/// 进程内的 [`CoreLink`]：Runner 的状态 / 回执直接落到 [`Shared`]，重载请求直接调核心。
struct InProcLink {
    core: Arc<MobileCore>,
    shared: Arc<Shared>,
}

fn ids_param(params: &Value) -> Result<Option<Vec<String>>, String> {
    match params.get("ids") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "ids 须为字符串数组".to_string())
            })
            .collect::<Result<_, _>>()
            .map(Some),
        Some(_) => Err("ids 须为字符串数组".into()),
    }
}

fn path_param(params: &Value) -> Result<&str, String> {
    params
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "缺少参数 path".to_string())
}

impl CoreLink for InProcLink {
    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let c = self.core.coord.as_ref();
        let e = |e: &dyn std::fmt::Display| e.to_string();
        match method {
            "data.beginEdit" => {
                let s = external_files::begin_edit(c, path_param(&params)?).map_err(|x| e(&x))?;
                Ok(json!({
                    "path": s.path.to_string_lossy(),
                    "file": s.file,
                    "kind": s.kind.as_str(),
                }))
            }
            "data.endEdit" => {
                let r = external_files::end_edit(c, path_param(&params)?).map_err(|x| e(&x))?;
                // 数据问题（解析错误）是设置页词库窗的展示，同步不看它
                Ok(json!({ "changed": r.changed, "issues": [] }))
            }
            "data.editSessions" => Ok(Value::Array(
                external_files::edit_sessions(c)
                    .into_iter()
                    .map(|(file, d)| json!({ "file": file, "since_secs": d.as_secs() }))
                    .collect(),
            )),
            "dict.reload" => {
                let paths: Option<Vec<String>> =
                    params.get("paths").and_then(Value::as_array).map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    });
                let reloaded =
                    external_files::reload_dict_files(c, paths.as_deref()).map_err(|x| e(&x))?;
                Ok(json!({ "reloaded": reloaded }))
            }
            "config.reload" => {
                let failed = c.reload_user_config();
                Ok(json!({ "ok": !failed, "needsRestart": false }))
            }
            // 手机没有按应用的兼容规则表，可同步的 compat.toml 也不会被读取：成功的空操作。
            "compat.reload" => Ok(json!({ "ok": true })),
            "schema.reloadFiles" => {
                let ids = external_files::reload_schema_files(c, ids_param(&params)?);
                Ok(json!({ "ok": true, "reloaded": ids }))
            }
            "theme.reloadFiles" => {
                let (ids, applied) =
                    external_files::reload_theme_files(c, ids_param(&params)?, &|_| {})
                        .map_err(|x| e(&x))?;
                Ok(json!({ "ok": true, "reloaded": ids, "applied": applied }))
            }
            other => Err(format!("不允许的请求: {other}")),
        }
    }

    fn send(&self, msg: FromSync) {
        match msg {
            FromSync::Status(s) => *lock(&self.shared.status) = s,
            FromSync::CommandReply { id, result, error } => {
                if let Some(tx) = lock(&self.shared.pending).remove(&id) {
                    let _ = tx.send(match error {
                        Some(e) => Err(e),
                        None => Ok(result.unwrap_or_default()),
                    });
                }
            }
            FromSync::Request { .. } => {}
        }
    }
}

/// 进程内数据同步。构造即起 Runner 线程（空闲时只阻塞等输入），[`Self::shutdown`] 或丢弃时收尾。
pub struct MobileSync {
    settings_file: PathBuf,
    settings: Mutex<SyncSettings>,
    shared: Arc<Shared>,
    tx: Sender<Input>,
    handle: Mutex<Option<JoinHandle<()>>>,
    next_id: AtomicU64,
    /// 上次由 [`Self::sync_if_due`] 起一轮的时刻（unix 秒），失败后也据此限频。
    last_attempt: Mutex<Option<i64>>,
}

impl MobileSync {
    /// - `user_dir`：用户目录（与构造 [`MobileCore`] 的 `user_root` 同一个）
    /// - `state_dir`：本地状态目录；同步文件放在其下 `sync/`
    pub fn new(user_dir: &Path, state_dir: &Path, core: Arc<MobileCore>) -> Arc<MobileSync> {
        Self::with_parts(
            user_dir,
            state_dir,
            core,
            Box::new(NetFactory),
            KdfParams::DEFAULT,
        )
    }

    /// 可替换存储后端与 KDF 强度（测试用内存后端与快速 KDF）。
    pub fn with_parts(
        user_dir: &Path,
        state_dir: &Path,
        core: Arc<MobileCore>,
        factory: Box<dyn BackendFactory>,
        kdf: KdfParams,
    ) -> Arc<MobileSync> {
        let sync_dir = state_dir.join("sync");
        let settings_file = sync_dir.join("settings.toml");
        let settings = wind_sync_ipc::load_settings_file(&settings_file).unwrap_or_else(|e| {
            tracing::warn!("{e}，按默认值处理");
            SyncSettings::default()
        });
        let shared = Arc::new(Shared::default());
        let link = Arc::new(InProcLink {
            core,
            shared: shared.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let cfg = Config {
            userdata_dir: user_dir.to_path_buf(),
            secrets: secrets::default_store(&sync_dir),
            state_dir: sync_dir,
            settings: settings.clone(),
            link,
            factory,
            timing: Timing::default(),
            kdf,
            watch: false,
            class: DeviceClass::Mobile,
            on_demand: true,
        };
        let mut runner = Runner::new(cfg, tx.clone());
        *lock(&shared.status) = runner.status();
        let handle = std::thread::Builder::new()
            .name("wind-sync".into())
            .spawn(move || runner.run(rx))
            .map_err(|e| tracing::error!("同步线程启动失败: {e}"))
            .ok();
        Arc::new(MobileSync {
            settings_file,
            settings: Mutex::new(settings),
            shared,
            tx,
            handle: Mutex::new(handle),
            next_id: AtomicU64::new(1),
            last_attempt: Mutex::new(None),
        })
    }

    /// `{settings, installed, has_secret}`（同桌面 `sync.getSettings`）。绝不含机密。
    pub fn get_settings(&self) -> String {
        json!({
            "settings": &*lock(&self.settings),
            "installed": true,
            "has_secret": lock(&self.shared.status).has_secret,
        })
        .to_string()
    }

    /// 入参是 `SyncSettings` 的 JSON（可包一层 `{"settings": …}`）。校验失败返回原因，不落盘。
    pub fn set_settings(&self, json_str: &str) -> Result<String, String> {
        let v: Value =
            serde_json::from_str(json_str).map_err(|e| format!("同步设置格式不对: {e}"))?;
        let raw = v.get("settings").unwrap_or(&v);
        let s: SyncSettings =
            serde_json::from_value(raw.clone()).map_err(|e| format!("同步设置格式不对: {e}"))?;
        s.validate()?;
        wind_sync_ipc::save_settings_file(&self.settings_file, &s)?;
        *lock(&self.settings) = s.clone();
        self.tx
            .send(Input::Core(ToSync::Settings(s)))
            .map_err(|_| CLOSED.to_string())?;
        Ok(json!({ "ok": true }).to_string())
    }

    /// 同桌面 `sync.status`：`{installed, enabled, running, phase, last_ok, last_error, pending,
    /// conflicts, conflict_paths, has_secret, joined}`。
    pub fn status(&self) -> String {
        let enabled = lock(&self.settings).enabled;
        let st = lock(&self.shared.status).clone();
        let running = lock(&self.handle).is_some();
        let phase = if !enabled {
            "disabled".to_string()
        } else if st.phase.is_empty() {
            "starting".to_string()
        } else {
            st.phase.clone()
        };
        json!({
            "installed": true,
            "enabled": enabled,
            "running": running,
            "phase": phase,
            "last_ok": st.last_ok,
            "last_error": st.last_error,
            "pending": st.pending,
            "conflicts": st.conflicts,
            "conflict_paths": st.conflict_paths,
            "has_secret": st.has_secret,
            "joined": st.joined,
        })
        .to_string()
    }

    /// 同桌面 `sync.command`：`op` 为 `sync_now` / `test_connection` / `set_passphrase` / `import_key` /
    /// `export_key` / `reset_remote` / `set_secret` / `confirm_mass_delete` / `list_devices` /
    /// `remove_device`，`args_json` 为参数对象（空串当无参数）。阻塞到有回执（最多 60 秒），
    /// 宿主应在后台线程调用。回执 JSON 可能含密钥（`export_key`），不要记日志。
    pub fn command(&self, op: &str, args_json: &str) -> Result<String, String> {
        let op = Op::parse(op).ok_or_else(|| format!("未知的同步命令: {op}"))?;
        let args = if args_json.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(args_json).map_err(|e| format!("命令参数不是 JSON: {e}"))?
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        // 先登记再发送：回执可能比 send 返回得还早。
        lock(&self.shared.pending).insert(id, tx);
        if self
            .tx
            .send(Input::Core(ToSync::Command { id, op, args }))
            .is_err()
        {
            lock(&self.shared.pending).remove(&id);
            return Err(CLOSED.into());
        }
        match rx.recv_timeout(COMMAND_TIMEOUT) {
            Ok(r) => r.map(|v| v.to_string()),
            Err(_) => {
                lock(&self.shared.pending).remove(&id);
                Err("等待同步回应超时".into())
            }
        }
    }

    /// 已启用并配置好、距上次成功或上次尝试已超过 `min_interval_secs` 时，起一轮后台同步并立即返回 `true`；
    /// 其余情况（未配置、缺机密、正在同步、停在错误态、间隔未到）返回 `false`。
    /// 键盘弹出时调用，耗时可忽略。
    pub fn sync_if_due(&self, min_interval_secs: u64) -> bool {
        {
            let s = lock(&self.settings);
            if !(s.enabled && s.backend.is_some()) {
                return false;
            }
        }
        let st = lock(&self.shared.status).clone();
        if !matches!(st.phase.as_str(), "idle" | "error") {
            return false;
        }
        let now = now_unix();
        let mut attempt = lock(&self.last_attempt);
        let last = st.last_ok.max(*attempt).unwrap_or(0);
        if now.saturating_sub(last) < min_interval_secs as i64 {
            return false;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // 不等回执：被拒（缺机密等）会体现在状态里
        let sent = self
            .tx
            .send(Input::Core(ToSync::Command {
                id,
                op: Op::SyncNow,
                args: Value::Null,
            }))
            .is_ok();
        if sent {
            *attempt = Some(now);
        }
        sent
    }

    /// 停止同步线程（等进行中的一轮收手，最多几秒）。幂等；之后各调用返回「已关闭」。
    pub fn shutdown(&self) {
        let Some(h) = lock(&self.handle).take() else {
            return;
        };
        let _ = self.tx.send(Input::Core(ToSync::Shutdown));
        let _ = h.join();
    }
}

const CLOSED: &str = "同步已关闭";

impl Drop for MobileSync {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
