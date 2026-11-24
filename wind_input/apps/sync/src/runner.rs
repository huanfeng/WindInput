//! 同步进程的主循环：调度、命令处理、状态上报（设计 §3.2、§3.3、§7）。
//!
//! 单线程跑业务：同一时刻只有一轮同步或一条命令。输入（core 的帧、文件事件）都经一个 channel 进来；
//! core 对本进程请求的回执由读线程直接交给 [`crate::link::PipeLink`]，不经这里——
//! 同步进行中也要能收到回执，否则 `data.beginEdit` 会自己等死自己。

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wind_sync::crypto::{Dek, KdfParams, Meta, content_hash};
use wind_sync::scope::{ScopeFlags, in_scope};
use wind_sync::state::backoff_secs;
use wind_sync::{Backend, SyncCtx, SyncError, SyncState, manifest, remote, sync_once};
use wind_sync_ipc::{FromSync, Op, Scope, Status, SyncSettings, ToSync};

use crate::backends::BackendFactory;
use crate::errors::{backend_error, crypto_error, sync_error};
use crate::link::CoreLink;
use crate::local::{Echo, FsLocal, lock};
use crate::log::{log, now_unix};
use crate::secrets::{SecretStore, Secrets};
use crate::statefile;
use crate::watch::{self, SharedScope};

/// 主循环的输入。
#[derive(Debug)]
pub enum Input {
    /// core 发来的帧（`Reply` 由读线程处理，不会出现在这里）。
    Core(ToSync),
    /// 范围内的文件事件（相对用户目录路径）。
    Fs(String),
    /// stdin 关闭：core 已退出。
    Eof,
}

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// 启动后首次同步。
    pub first_delay: Duration,
    /// 文件变化去抖。
    pub debounce: Duration,
    /// 设置 / 机密变更后到下一轮的等待。
    pub settings_delay: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            first_delay: Duration::from_secs(30),
            debounce: Duration::from_secs(10),
            settings_delay: Duration::from_secs(2),
        }
    }
}

pub struct Config {
    pub userdata_dir: std::path::PathBuf,
    pub state_dir: std::path::PathBuf,
    pub settings: SyncSettings,
    pub link: Arc<dyn CoreLink>,
    pub secrets: Box<dyn SecretStore>,
    pub factory: Box<dyn BackendFactory>,
    pub timing: Timing,
    pub kdf: KdfParams,
    /// 是否监听文件变化（测试可关）。
    pub watch: bool,
}

pub fn flags_of(s: &Scope) -> ScopeFlags {
    ScopeFlags {
        config: s.config,
        schemas: s.schemas,
        themes: s.themes,
        dicts: s.dicts,
    }
}

pub struct Runner {
    cfg: Config,
    settings: SyncSettings,
    secrets: Secrets,
    secret_error: Option<String>,
    state: SyncState,
    echo: Echo,
    scope: SharedScope,
    cached: Option<Box<dyn Backend>>,
    syncing: bool,
    force_now: bool,
    debounce_until: Option<Instant>,
    periodic_at: Instant,
    conflicts: Vec<String>,
    last_sent: Option<Status>,
    _watch: Option<watch::Watch>,
}

fn interval(s: &SyncSettings) -> Duration {
    Duration::from_secs(u64::from(s.interval_min.max(1)) * 60)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

impl Runner {
    pub fn new(cfg: Config, tx: Sender<Input>) -> Runner {
        let (secrets, secret_error) = match Secrets::load(&*cfg.secrets) {
            Ok(s) => (s, None),
            Err(e) => {
                log!("{e}");
                (Secrets::default(), Some(e))
            }
        };
        let state = statefile::load(&cfg.state_dir);
        let settings = cfg.settings.clone();
        let scope = SharedScope::new(flags_of(&settings.scope));
        let watch = cfg.watch.then(|| {
            watch::start(&cfg.userdata_dir, scope.clone(), tx)
                .map_err(|e| log!("{e}；仅靠定时同步"))
                .ok()
        });
        let periodic_at = Instant::now() + cfg.timing.first_delay;
        Runner {
            cfg,
            settings,
            secrets,
            secret_error,
            state,
            echo: Arc::new(Mutex::new(Default::default())),
            scope,
            cached: None,
            syncing: false,
            force_now: false,
            debounce_until: None,
            periodic_at,
            conflicts: Vec::new(),
            last_sent: None,
            _watch: watch.flatten(),
        }
    }

    // ── 状态 ───────────────────────────────────────────────

    fn configured(&self) -> bool {
        self.settings.enabled && self.settings.backend.is_some()
    }

    fn ready(&self) -> bool {
        self.secrets.password.is_some() && self.secrets.dek.is_some()
    }

    /// 能自动调度：已配置、机密齐全、没停在错误态。
    fn runnable(&self) -> bool {
        self.configured() && self.ready() && self.state.retry.halted.is_none()
    }

    pub fn status(&self) -> Status {
        let phase = if !self.configured() {
            "not_configured"
        } else if !self.ready() {
            "needs_setup"
        } else if self.syncing {
            "syncing"
        } else if self.state.retry.halted.is_some() {
            "halted"
        } else if self.state.retry.last_error.is_some() {
            "error"
        } else {
            "idle"
        };
        Status {
            phase: phase.into(),
            last_ok: self.state.last_ok.map(|t| t as i64),
            last_error: self
                .secret_error
                .clone()
                .or_else(|| self.state.retry.last_error.clone()),
            pending: self.state.pending.len() as u32,
            conflicts: self.conflicts.len() as u32,
            conflict_paths: self
                .conflicts
                .iter()
                .take(wind_sync_ipc::MAX_CONFLICT_PATHS)
                .cloned()
                .collect(),
            has_secret: self.ready(),
            joined: self.secrets.dek.is_some(),
        }
    }

    fn report(&mut self) {
        let s = self.status();
        if self.last_sent.as_ref() != Some(&s) {
            self.cfg.link.send(FromSync::Status(s.clone()));
            self.last_sent = Some(s);
        }
    }

    fn save_state(&self) {
        if let Err(e) = statefile::save(&self.cfg.state_dir, &self.state) {
            log!("{e}");
        }
    }

    fn save_secrets(&mut self) -> Result<(), String> {
        self.secrets.save(&*self.cfg.secrets)?;
        self.secret_error = None;
        Ok(())
    }

    // ── 主循环 ─────────────────────────────────────────────

    /// 下一次要跑同步的时刻；不可调度返回 None。
    fn next_wake(&self, now: Instant) -> Option<Instant> {
        if !self.runnable() {
            return None;
        }
        if self.force_now {
            return Some(now);
        }
        let mut t = self.periodic_at;
        if let Some(d) = self.debounce_until {
            t = t.min(d);
        }
        // 退避：不早于上次失败后约定的重试时刻
        let wait = self.state.retry.next_at.saturating_sub(now_unix());
        Some(t.max(now + Duration::from_secs(wait)))
    }

    /// 阻塞运行直到 `Shutdown` / stdin EOF。
    pub fn run(&mut self, rx: Receiver<Input>) {
        self.report();
        loop {
            let now = Instant::now();
            let wake = self.next_wake(now);
            if wake.is_some_and(|w| w <= now) {
                self.sync_round();
                continue;
            }
            let timeout = wake.map_or(Duration::from_secs(3600), |w| w - now);
            match rx.recv_timeout(timeout) {
                Ok(Input::Core(ToSync::Shutdown)) | Ok(Input::Eof) => break,
                Ok(i) => self.handle(i),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.save_state();
        log!("同步进程退出");
    }

    fn handle(&mut self, i: Input) {
        match i {
            Input::Core(ToSync::Settings(s)) => self.on_settings(s),
            Input::Core(ToSync::Command { id, op, args }) => {
                let r = self.command(op, &args);
                self.cfg.link.send(FromSync::command_reply(id, r));
                self.report();
            }
            Input::Fs(p) => self.on_fs(&p),
            Input::Core(_) | Input::Eof => {}
        }
    }

    fn on_settings(&mut self, s: SyncSettings) {
        if s == self.settings {
            return;
        }
        self.scope.set(flags_of(&s.scope));
        self.settings = s;
        self.cached = None;
        // 用户改了设置：解除停止态，稍后按新设置跑一轮
        self.state.retry.clear_halt();
        self.save_state();
        self.kick();
        self.report();
    }

    /// 稍后（`settings_delay`）跑一轮。
    fn kick(&mut self) {
        self.periodic_at = Instant::now() + self.cfg.timing.settings_delay;
    }

    /// 文件事件：与已同步内容或我们刚写的内容一致的（回声）忽略。
    fn on_fs(&mut self, rel: &str) {
        if !self.runnable() || !in_scope(rel, &flags_of(&self.settings.scope)) {
            return;
        }
        let hash = std::fs::read(self.abs(rel))
            .ok()
            .map(|b| content_hash(&b))
            .unwrap_or_default();
        if lock(&self.echo).get(rel) == Some(&hash) {
            return;
        }
        let synced = self
            .state
            .files
            .get(rel)
            .and_then(|e| e.value())
            .unwrap_or_default();
        if synced == hash {
            return;
        }
        self.debounce_until = Some(Instant::now() + self.cfg.timing.debounce);
    }

    fn abs(&self, rel: &str) -> std::path::PathBuf {
        rel.split('/')
            .fold(self.cfg.userdata_dir.clone(), |p, s| p.join(s))
    }

    // ── 同步 ───────────────────────────────────────────────

    fn sync_round(&mut self) {
        let force = std::mem::take(&mut self.force_now);
        self.debounce_until = None;
        self.syncing = true;
        self.report();
        let r = self.try_sync(force);
        self.syncing = false;
        let now = Instant::now();
        match r {
            Ok(skipped) => {
                self.periodic_at = now + interval(&self.settings);
                if skipped {
                    log!("处于退避等待期，本轮跳过");
                }
            }
            Err(msg) => {
                log!("同步失败: {msg}");
                // 重试按退避；停止态不排期（runnable 为假）
                let backoff = Duration::from_secs(backoff_secs(self.state.retry.failures));
                self.periodic_at = now + backoff.min(interval(&self.settings));
            }
        }
        self.save_state();
        self.report();
    }

    /// 取缓存的后端，没有就按当前设置与凭据构建。用完要放回 `self.cached`。
    fn take_backend(&mut self) -> Result<Box<dyn Backend>, String> {
        match self.cached.take() {
            Some(b) => Ok(b),
            None => {
                let pw = self.secrets.password.clone().unwrap_or_default();
                self.cfg.factory.build(&self.settings, &pw)
            }
        }
    }

    /// Ok(true) = 退避中跳过；Err = 用户可读的失败原因。
    fn try_sync(&mut self, force: bool) -> Result<bool, String> {
        let dek = self.dek()?;
        let now = now_unix();
        let scope = flags_of(&self.settings.scope);
        let mut local = FsLocal::new(
            self.cfg.userdata_dir.clone(),
            scope,
            self.cfg.link.clone(),
            self.echo.clone(),
        );
        let mut ctx = SyncCtx::new(scope, now);
        ctx.force = force;
        ctx.gc = true;
        let backend = self.take_backend()?;
        let result = sync_once(&*backend, &mut local, &mut self.state, &dek, &ctx);
        self.cached = Some(backend);
        self.conflicts = local.conflict_files();
        match result {
            Ok(rep) => {
                if rep.skipped {
                    return Ok(true);
                }
                for w in &rep.warnings {
                    log!("警告: {w}");
                }
                self.state.last_ok = Some(now);
                self.state.retry.last_error = None;
                log!(
                    "同步完成：上传 {}，下载 {}，合并 {}，冲突 {}",
                    rep.uploaded.len(),
                    rep.downloaded.len(),
                    rep.merged.len(),
                    rep.conflicts.len()
                );
                Ok(false)
            }
            Err(e) => {
                let msg = sync_error(&e);
                // 状态里存可读文案：重启后设置页直接展示
                if self.state.retry.halted.is_some() && !matches!(e, SyncError::Halted(_)) {
                    self.state.retry.halted = Some(msg.clone());
                }
                self.state.retry.last_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    fn dek(&self) -> Result<Dek, String> {
        let b64 = self.secrets.dek.as_deref().ok_or("本机还没有同步密钥")?;
        Dek::from_base64(b64).map_err(|e| crypto_error(&e))
    }

    // ── 命令 ───────────────────────────────────────────────

    fn command(&mut self, op: Op, args: &Value) -> Result<Value, String> {
        match op {
            Op::SetSecret => self.cmd_set_secret(args),
            Op::TestConnection => self.cmd_test_connection(args),
            Op::SetPassphrase => self.cmd_set_passphrase(args),
            Op::ImportKey => self.cmd_import_key(args),
            Op::ExportKey => self.cmd_export_key(),
            Op::SyncNow => self.cmd_sync_now(),
            Op::ResetRemote => self.cmd_reset_remote(args),
        }
    }

    /// 用户处理完问题（改凭据 / 口令 / 密钥）后：解除停止态、丢旧连接、稍后同步。
    fn after_user_fix(&mut self) {
        self.state.retry.clear_halt();
        self.cached = None;
        self.save_state();
        self.kick();
    }

    fn cmd_set_secret(&mut self, args: &Value) -> Result<Value, String> {
        match args.get("password") {
            Some(Value::String(p)) => self.secrets.password = Some(p.clone()),
            Some(Value::Null) => self.secrets.password = None,
            _ => return Err("缺少参数 password（字符串；null 表示清除）".into()),
        }
        self.save_secrets()?;
        self.after_user_fix();
        Ok(json!({ "ok": true }))
    }

    /// 不经缓存、可带临时密码地建后端：测试连接与改口令都用它。
    fn fresh_backend(&self, password: Option<&str>) -> Result<Box<dyn Backend>, String> {
        if self.settings.backend.is_none() {
            return Err("尚未配置存储后端".into());
        }
        let pw = password
            .map(str::to_string)
            .or_else(|| self.secrets.password.clone())
            .ok_or("尚未保存存储凭据（密码或访问密钥）")?;
        self.cfg.factory.build(&self.settings, &pw)
    }

    fn read_meta(b: &dyn Backend) -> Result<Option<Meta>, String> {
        match b
            .get(&manifest::meta_key())
            .map_err(|e| backend_error(&e))?
        {
            None => Ok(None),
            Some(raw) => Meta::from_json(&raw)
                .map(Some)
                .map_err(|e| crypto_error(&e)),
        }
    }

    fn cmd_test_connection(&mut self, args: &Value) -> Result<Value, String> {
        let b = self.fresh_backend(str_arg(args, "password"))?;
        b.test_connection().map_err(|e| backend_error(&e))?;
        let meta = Self::read_meta(&*b)?;
        let joined = match (&meta, self.dek()) {
            (Some(m), Ok(d)) => m.check_dek(&d),
            _ => false,
        };
        Ok(json!({ "ok": true, "remote_initialized": meta.is_some(), "joined": joined }))
    }

    /// DEK 变了说明换了同步库：旧的清单基线对新远端没有意义，重建（设备号也换新）。
    fn adopt_dek(&mut self, dek: &Dek) -> Result<(), String> {
        let b64 = dek.to_base64();
        let changed = self.secrets.dek.as_deref() != Some(b64.as_str());
        self.secrets.dek = Some(b64);
        self.save_secrets()?;
        if changed {
            self.state = SyncState::new_random();
        }
        self.after_user_fix();
        Ok(())
    }

    /// 远端无 meta → 初始化；有 meta：已加入（持有匹配的 DEK 或给了旧口令）→ 改口令，否则用口令加入。
    fn cmd_set_passphrase(&mut self, args: &Value) -> Result<Value, String> {
        let pass = str_arg(args, "passphrase").unwrap_or_default();
        if pass.is_empty() {
            return Err("口令不能为空".into());
        }
        let b = self.fresh_backend(None)?;
        let kdf = self.cfg.kdf;
        let se = |e: SyncError| sync_error(&e);
        let (dek, mode) = match Self::read_meta(&*b)? {
            None => (remote::init_remote(&*b, pass, kdf).map_err(se)?, "init"),
            Some(meta) => {
                let old = str_arg(args, "old_passphrase").filter(|s| !s.is_empty());
                let held = self.dek().ok().filter(|d| meta.check_dek(d));
                if let Some(old) = old {
                    remote::change_passphrase(&*b, old, pass, kdf).map_err(se)?;
                    (
                        remote::join_with_passphrase(&*b, pass).map_err(se)?,
                        "change",
                    )
                } else if let Some(d) = held {
                    remote::rewrap_passphrase(&*b, &d, pass, kdf).map_err(se)?;
                    (d, "change")
                } else {
                    (remote::join_with_passphrase(&*b, pass).map_err(se)?, "join")
                }
            }
        };
        self.adopt_dek(&dek)?;
        log!("口令设置完成（{mode}）");
        Ok(json!({ "ok": true, "mode": mode }))
    }

    fn cmd_import_key(&mut self, args: &Value) -> Result<Value, String> {
        let key = str_arg(args, "key").unwrap_or_default();
        if key.trim().is_empty() {
            return Err("密钥不能为空".into());
        }
        let b = self.fresh_backend(None)?;
        let dek = remote::join_with_key(&*b, key).map_err(|e| sync_error(&e))?;
        self.adopt_dek(&dek)?;
        Ok(json!({ "ok": true }))
    }

    fn cmd_export_key(&self) -> Result<Value, String> {
        let dek = self
            .dek()
            .map_err(|_| "本机尚未加入加密同步，没有可导出的密钥".to_string())?;
        Ok(json!({ "key": dek.to_base64() }))
    }

    fn cmd_sync_now(&mut self) -> Result<Value, String> {
        if !self.configured() {
            return Err("同步未启用或尚未配置存储后端".into());
        }
        if self.secrets.password.is_none() {
            return Err("尚未保存存储凭据".into());
        }
        if self.secrets.dek.is_none() {
            return Err("尚未设置同步口令或导入密钥".into());
        }
        self.state.retry.clear_halt();
        self.cached = None;
        self.force_now = true;
        Ok(json!({ "queued": true }))
    }

    /// 清空远端后以本机为准重新上传。需 `confirm: true` 与新口令；本机有 DEK 则沿用（已导出的密钥仍有效）。
    fn cmd_reset_remote(&mut self, args: &Value) -> Result<Value, String> {
        if args.get("confirm").and_then(Value::as_bool) != Some(true) {
            return Err("重置远端会清空其中全部同步数据，须明确确认（confirm=true）".into());
        }
        let pass = str_arg(args, "passphrase").unwrap_or_default();
        if pass.is_empty() {
            return Err("重置后需要设置新口令".into());
        }
        let b = self.fresh_backend(None)?;
        let dek = self.dek().unwrap_or_else(|_| Dek::generate());
        remote::reset_remote(&*b, &dek, pass, self.cfg.kdf).map_err(|e| sync_error(&e))?;
        self.secrets.dek = Some(dek.to_base64());
        self.save_secrets()?;
        // 远端已空：旧基线作废，所有本地文件当作新增上传
        self.state = SyncState::new_random();
        self.after_user_fix();
        log!("远端已重置");
        Ok(json!({ "ok": true }))
    }
}
