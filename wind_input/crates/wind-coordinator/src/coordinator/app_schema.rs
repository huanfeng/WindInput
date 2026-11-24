//! 按应用方案（compat.toml `[[apps]] schema`）。设计见 `docs/design/per-app-overrides.md`
//! 「① ② 按应用方案」。三件事：
//!
//! 1. 切入：焦点跨进程切入时（与 initial_mode 同一个 `crossed` 判据）按规则算目标方案，
//!    不同就轻量切换——只换引擎与方案绑定的运行期资源，不写 `schema.active`、不归位中文、
//!    不弹状态气泡。见 [`Coordinator::apply_app_schema_on_focus`]。
//! 2. 手切分流：所有手切入口汇到 `finish_user_schema_switch`，经
//!    [`Coordinator::route_manual_schema_switch`]：规则应用内只改本应用方案（`@remember`
//!    另记记忆表）、不写盘；无规则应用照旧改全局。
//! 3. 全局方案取 [`AppSchemaState::global`]，不是 `rt().config.schema.active`：手切只写磁盘，
//!    内存 config 只在 `reload_user_config` 时整份重读，读它拿到的是启动时的旧值。
//!
//! 本模块的任何函数都不得进 `get_current_mode`（DLL 同步阻塞路径）：切方案可能要构建引擎、
//! 起后台线程。
//!
//! # 锁序（焦点线程、`app-schema-load` 后台线程、设置页 RPC、菜单线程并发调用）
//!
//! 1. [`AppSchemaState::switch_lock`] 是最外层：「比对/检查 + 轻量切换」整段持有它。
//!    持有任何别的锁时不得去取它；持有它期间可取下面任何一把。
//! 2. `Coordinator::schema_toggle_origin` 与 [`AppSchemaState::parked_toggle`] 是叶子锁，
//!    永不嵌套——一边 take 出值、放锁，再取另一边写入；两段代码嵌套顺序相反即死锁。
//! 3. `remembered` → `state_writer` 内部锁：写记忆表时持 `remembered` 调 `schedule`
//!    （见 [`Coordinator::remember_app_schema`]）；写盘闭包在 writer 线程上跑、不碰
//!    `remembered`，故不成环。
//! 4. 其余（`global` / `pending_load` / `warned_invalid`）只在单独一句里取、取完即放。

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use wind_config::app_compat::AppSchema;

/// 按应用方案的运行时态。独立成结构体：五个量只服务这一个功能，散在 `Coordinator` 的
/// 百来个字段里既难找也难看出它们之间的约束。
pub(crate) struct AppSchemaState {
    /// 「全局方案」：最近一次全局手切（无规则应用里手切）的结果，启动取引擎的活跃方案，
    /// `reload_user_config` 重建方案集后取引擎新的活跃方案（即磁盘上的 `schema.active`）。
    ///
    /// 不能读 `rt().config.schema.active`，理由见模块文档第 3 条。
    global: Mutex<String>,
    /// `@remember` 应用的记忆表（进程名小写 → 方案 id），`state.toml` `app_schemas` 的内存镜像。
    remembered: Mutex<HashMap<String, String>>,
    /// 切方案意图的代际：每次焦点切入重算、每次手切都 +1。后台冷加载完成时比对它——
    /// 不等 ⇒ 期间焦点已离开或用户已手切，这次延后切换作废。
    ///
    /// 用代际而不是比对 pid / 进程名：「切走又切回同一个应用」在 pid 上与「从未离开」同形，
    /// 而那期间用户可能已经在别处手切过全局方案。
    intent_gen: AtomicU64,
    /// 被自动切换挂起的往返热键去程记录：`(送达的方案 id, 记录)`。见
    /// [`Coordinator::switch_schema_light`] 里「往返记录」一段。
    parked_toggle: Mutex<Option<(String, SchemaToggleOrigin)>>,
    /// 最近一次冷方案后台加载的线程句柄，只供测试等它完成（生产不 join）。
    pending_load: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 自动切换的互斥：[`Coordinator::apply_app_schema_on_focus`] 的「比较 + 切换」与
    /// [`Coordinator::finish_deferred_app_schema`] 的「检查意图代际 + 切换」都整段持有它。
    ///
    /// 没有它时后者是 check-then-act：后台线程比对代际通过之后、真正切换之前，焦点线程
    /// 可以完成下一次切入（代际 +1、切到新目标），随后后台线程把方案切回旧目标。
    /// 锁序：最外层，见模块文档。
    switch_lock: Mutex<()>,
    /// 已报过「固定 id 不在 available」的 `(进程名, id)`：每次切入/手切都会问规则，不去重
    /// 就是同一条 WARN 刷满日志。available 热重载后仍不在的组合也不再重报（配置没变）。
    warned_invalid: Mutex<std::collections::HashSet<(String, String)>>,
}

impl AppSchemaState {
    pub(crate) fn new(global: String, remembered: HashMap<String, String>) -> Self {
        Self {
            global: Mutex::new(global),
            remembered: Mutex::new(remembered),
            intent_gen: AtomicU64::new(0),
            parked_toggle: Mutex::new(None),
            pending_load: Mutex::new(None),
            switch_lock: Mutex::new(()),
            warned_invalid: Mutex::new(std::collections::HashSet::new()),
        }
    }
}

/// 轻量切换时对未上屏编码的处置，见 [`Coordinator::switch_schema_light`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingInput {
    /// 只丢弃，不上屏、不推任何东西给宿主。焦点切入用：此刻 active token 已指向新宿主，
    /// 缓冲里若还有旧宿主的编码，按 `commit_on_switch` 上屏就会把 A 应用的码打进 B 应用。
    Discard,
    /// 与手切同一条策略（`keys.commit_on_switch`：开则上屏原码，关则丢弃）。只给冷方案
    /// 加载完成那条路用：那时焦点仍在该应用（意图代际未变），缓冲属于它。
    CommitPerPolicy,
}

/// 经 available 校验后的按应用方案规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppSchemaRule {
    Fixed(String),
    Remember,
}

impl Coordinator {
    /// 该进程生效中的方案规则：未配、或固定的 id 不在当时的 `schema.available` 内
    /// ⇒ `None`（视为未配置并 WARN）。
    ///
    /// available 校验放在这里而不是解析层：available 可热重载，按当时的值判才能随之生效；
    /// 且启动预热只覆盖 available，放行任意 id 等于允许焦点路径上同步冷构建。
    ///
    /// 规则按 (进程, 窗口) 解析；记忆表（`@remember`）仍按进程名记。
    pub(crate) fn app_schema_rule(&self, proc: &str, win: &FocusWindow) -> Option<AppSchemaRule> {
        let raw = self.with_compat_rule(proc, win, |r| match r?.app_schema()? {
            AppSchema::Remember => Some(None),
            AppSchema::Fixed(id) => Some(Some(id.to_string())),
        })?;
        let Some(raw) = raw else {
            return Some(AppSchemaRule::Remember);
        };
        if self.engine_mgr.available_schemas().contains(&raw) {
            Some(AppSchemaRule::Fixed(raw))
        } else {
            let first = self
                .app_schema
                .warned_invalid
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((proc.to_string(), raw.clone()));
            if first {
                warn!(
                    "compat.toml: {proc} 的 schema = \"{raw}\" 不在 schema.available 中，按未配置处理"
                );
            }
            None
        }
    }

    /// 当前的全局方案，见 [`AppSchemaState::global`]。
    pub(crate) fn global_schema(&self) -> String {
        self.app_schema
            .global
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 记忆表里该进程的方案；记录的 id 已不在 available ⇒ 当没记过（不清理：方案可能被重新启用）。
    fn remembered_schema(&self, proc: &str) -> Option<String> {
        let id = self
            .app_schema
            .remembered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(proc)
            .cloned()?;
        self.engine_mgr
            .available_schemas()
            .contains(&id)
            .then_some(id)
    }

    /// 焦点在该进程时应当使用的方案：固定 → 规则值；`@remember` → 记忆表，无记录用全局；
    /// 无规则 → 全局。
    pub(crate) fn app_schema_target(&self, proc: &str, win: &FocusWindow) -> String {
        match self.app_schema_rule(proc, win) {
            Some(AppSchemaRule::Fixed(id)) => id,
            Some(AppSchemaRule::Remember) => self
                .remembered_schema(proc)
                .unwrap_or_else(|| self.global_schema()),
            None => self.global_schema(),
        }
    }

    /// 焦点跨进程切入（或菜单改了规则、热重载重建了方案集）时，把活跃方案对齐到该进程的
    /// 目标方案。调用方负责「何时该算」（`crossed` 判据在 `handle_focus_gained`）。
    ///
    /// 目标方案没加载好（开机预热未完）⇒ 不阻塞：保持当前方案，后台加载，完成时若
    /// 意图代际未变（焦点没离开、用户没手切）再切。焦点路径上同步构建一个大词库会卡住宿主。
    ///
    /// 同步切换时缓冲只丢弃（[`PendingInput::Discard`]）：焦点切入那一刻 active token 已是
    /// 新宿主，上屏会把旧应用的码打进新应用。菜单改规则、热重载对齐也走这里，那两处缓冲
    /// 本就已被菜单/重载清掉，丢弃与上屏同效。
    pub(crate) fn apply_app_schema_on_focus(&self, proc: &str, win: &FocusWindow) {
        // 「比较 + 切换」整段互斥，见 `AppSchemaState::switch_lock`。代际 +1 也放在锁内：
        // 后台线程持锁检查时看到的代际与它随后切换时一致。
        let _switch = self
            .app_schema
            .switch_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let generation = self.app_schema.intent_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let target = self.app_schema_target(proc, win);
        if target.is_empty() || target == self.engine_mgr.active_schema_id() {
            return;
        }
        if self.engine_mgr.is_loaded(&target) {
            self.switch_schema_light(&target, proc, PendingInput::Discard);
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        debug!("按应用方案: {proc} 的目标 {target} 尚未加载，保持当前方案、后台加载");
        let proc = proc.to_string();
        let win = win.clone();
        let spawned = std::thread::Builder::new()
            .name("app-schema-load".into())
            .spawn(move || {
                let Some(c) = weak.upgrade() else {
                    return;
                };
                if !c.engine_mgr.ensure_schema(&target) {
                    warn!("按应用方案: {target} 加载失败，{proc} 保持当前方案");
                    return;
                }
                c.finish_deferred_app_schema(generation, &proc, &win, &target);
            });
        match spawned {
            Ok(h) => {
                *self
                    .app_schema
                    .pending_load
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(h);
            }
            Err(e) => warn!("按应用方案: 起后台加载线程失败: {e}"),
        }
    }

    /// 冷方案加载完成后的收尾：意图代际未变才切，且按此刻的规则重算目标——加载期间
    /// 菜单可能改过规则，拿旧目标会切到一个已被撤销的方案。
    ///
    /// 「检查 + 切换」整段持 `switch_lock`：否则检查通过后、切换之前焦点线程可以完成下一次
    /// 切入，本函数再把方案切回已作废的旧目标。
    pub(crate) fn finish_deferred_app_schema(
        &self,
        generation: u64,
        proc: &str,
        win: &FocusWindow,
        target: &str,
    ) {
        let _switch = self
            .app_schema
            .switch_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.app_schema.intent_gen.load(Ordering::SeqCst) != generation {
            debug!("按应用方案: {target} 加载完成，但焦点已离开 {proc} 或期间手切过，不切换");
            return;
        }
        if self.app_schema_target(proc, win) != target {
            return;
        }
        self.switch_schema_light(target, proc, PendingInput::CommitPerPolicy);
    }

    /// 轻量切换：换引擎 + 方案绑定的运行期同步，是 `finish_user_schema_switch` 去掉
    /// 「写 `schema.active` / 归位中文与取消大写 / 状态气泡」之后的部分。
    ///
    /// 不归位中文：中英态归 initial_mode 那套管，两者正交；自动切换强行改中英会与用户在
    /// 该应用里配的 `initial_mode = "english"` 打架。不弹气泡：焦点气泡另有
    /// `ui.status.show_on_focus` 管，再弹一次就是每次切应用都闪。
    ///
    /// # 往返记录（`toggle_schema:<id>`）
    ///
    /// 引擎切方案会让 `schema_generation` +1、往返键去程记录失效；对自动切换这是误伤（A 应用
    /// 往返去了英文、切走再切回后往返键没反应）。故自动切走时把仍有效的记录挂起，之后自动
    /// 切换落回同一方案时按新代际恢复；任何手切都丢弃挂起记录（见
    /// [`Self::route_manual_schema_switch`]）。两段都是 take 出值、放锁再取另一把，
    /// `schema_toggle_origin` 与 `parked_toggle` 永不同时持有。
    ///
    /// `pending` 决定未上屏编码怎么处置，见 [`PendingInput`]。
    pub(crate) fn switch_schema_light(&self, target: &str, proc: &str, pending: PendingInput) {
        let from = self.engine_mgr.active_schema_id();
        let generation = self.engine_mgr.schema_generation();
        let still_valid = self
            .schema_toggle_origin
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_if(|r| r.landing.generation == generation);
        if let Some(rec) = still_valid {
            *self
                .app_schema
                .parked_toggle
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some((from.clone(), rec));
        }
        // 编码处置必须早于 switch_schema：上屏记账取的是切换前的方案。
        let commit = match pending {
            PendingInput::CommitPerPolicy => self.take_input_on_schema_switch(),
            PendingInput::Discard => {
                self.discard_input_on_schema_switch();
                SwitchCommit::default()
            }
        };
        if !self.engine_mgr.switch_schema(target) {
            self.push_switch_commit(&commit);
            return;
        }
        self.sync_chaizi_assets();
        self.invalidate_aux_code_table();
        self.notify_ui_hide();
        self.push_state_update();
        self.notify_toolbar();
        self.push_switch_commit(&commit);
        let resumed = self
            .app_schema
            .parked_toggle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_if(|(id, _)| id == target);
        if let Some((_, mut rec)) = resumed {
            rec.landing.generation = self.engine_mgr.schema_generation();
            *self
                .schema_toggle_origin
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(rec);
        }
        info!("按应用方案: {proc} {from} -> {target}");
    }

    /// 丢弃未上屏的编码与一切独占模式缓冲，不上屏、不记上屏统计、不推任何东西给宿主。
    /// 与失焦清输入同一组动作（`reset_exclusive_modes` 已含主缓冲、候选、preedit、逐步
    /// 转换前缀）。旧宿主里残留的组合由它自己的 DLL 在失焦时结束，不归这里管。
    fn discard_input_on_schema_switch(&self) {
        self.terminate_auto_phrase("switch_schema");
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.exit_assoc(&mut state, crate::handle_assoc::AssocExit::ModeSwitch);
        self.reset_exclusive_modes(&mut state);
    }

    /// 手切收尾的分流（`finish_user_schema_switch` 调）：返回 `true` = 当前焦点应用有方案
    /// 规则，本次手切只属于该应用，调用方不得写 `schema.active`；`false` = 全局手切，
    /// 全局方案随之更新，调用方照旧持久化。
    ///
    /// 固定应用的手切是临时覆盖：不记任何东西，离开再回来由规则重算、自然恢复规则值。
    pub(crate) fn route_manual_schema_switch(&self, schema_id: &str) -> bool {
        // 手切就是新的意图：作废在途的冷加载（否则加载完会把用户刚切的方案切掉），
        // 丢弃挂起的往返记录（它描述的是手切之前的往返）。
        self.app_schema.intent_gen.fetch_add(1, Ordering::SeqCst);
        self.app_schema
            .parked_toggle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let proc = self.active_process_name();
        match self.app_schema_rule(&proc, &self.active_focus_window()) {
            Some(AppSchemaRule::Fixed(_)) => {
                info!("按应用方案: {proc} 内手切到 {schema_id}（临时，不写 schema.active）");
                true
            }
            Some(AppSchemaRule::Remember) => {
                self.remember_app_schema(&proc, schema_id);
                info!("按应用方案: {proc} 内手切到 {schema_id}，记入记忆表");
                true
            }
            None => {
                *self
                    .app_schema
                    .global
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = schema_id.to_string();
                false
            }
        }
    }

    /// 写记忆表并经 `state_writer` 落盘。闭包带整张表（覆盖语义，见 `state_writer` 模块文档）。
    ///
    /// 持 `remembered` 锁调 `schedule`，保证「登记顺序 = 快照顺序」：放锁后再登记，并发时
    /// 旧快照可能后登记而覆盖新表。不成环：`schedule` 只取 writer 自己的 `pending` 锁。
    pub(crate) fn remember_app_schema(&self, proc: &str, schema_id: &str) {
        let mut m = self
            .app_schema
            .remembered
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        m.insert(proc.to_string(), schema_id.to_string());
        let snapshot = m.clone();
        self.state_writer.schedule("app_schemas", move |rs| {
            rs.app_schemas = snapshot.clone();
        });
    }

    /// 该进程在记忆表里有没有有效记录（菜单设 `@remember` 时用来决定要不要播种）。
    pub(crate) fn has_remembered_schema(&self, proc: &str) -> bool {
        self.remembered_schema(proc).is_some()
    }

    /// `reload_user_config` 重建方案集之后调：引擎的活跃方案此刻已被重置为磁盘上的
    /// `schema.active`，即最新的全局方案——据此更新 [`AppSchemaState::global`]，再把当前
    /// 焦点应用对齐回它的目标方案。
    ///
    /// 不做后一步的话，焦点在固定五笔的应用里时，设置页随便保存一项方案段的设置，
    /// 活跃方案就被冲回全局，直到切走再切回来。
    pub(crate) fn resync_app_schema_after_reload(&self) {
        *self
            .app_schema
            .global
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.engine_mgr.active_schema_id();
        let proc = self.active_process_name();
        self.apply_app_schema_on_focus(&proc, &self.active_focus_window());
    }

    /// 设置页「设为当前」（`schema.setActive` RPC）直接切了引擎：那是全局意图，全局方案跟随。
    /// 不跟随的话，下一次焦点切入无规则应用会把它切回旧的全局方案。
    pub(crate) fn note_global_schema_set(&self, schema_id: &str) {
        self.app_schema.intent_gen.fetch_add(1, Ordering::SeqCst);
        *self
            .app_schema
            .global
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = schema_id.to_string();
    }

    /// 测试用：等最近一次冷方案后台加载（及其延后切换）结束。
    #[doc(hidden)]
    pub fn debug_wait_app_schema_load(&self) {
        let h = self
            .app_schema
            .pending_load
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(h) = h {
            let _ = h.join();
        }
    }

    /// 测试用：当前的全局方案与记忆表快照。
    #[doc(hidden)]
    pub fn debug_app_schema_state(&self) -> (String, HashMap<String, String>) {
        (
            self.global_schema(),
            self.app_schema
                .remembered
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        )
    }
}

#[cfg(test)]
#[path = "app_schema_tests.rs"]
mod tests;
