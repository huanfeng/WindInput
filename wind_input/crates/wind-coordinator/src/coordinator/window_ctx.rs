//! 焦点窗口上下文：`compat.toml` 规则按窗口类名 / 标题匹配的协调器侧（P2，设计见
//! `docs/design/compat-window-match.md`「服务端判定」）。
//!
//! 三件事都收在这里，调用点只认这几个入口：
//! - 记账：`client_token → 最近一次 focus_gained 的窗口`（[`FocusWindow`]）。ime_activated、
//!   按 token 推 DLL 配置都沿用这张表；按 pid 查的路径取该 pid 最近一次获焦的那个窗口。
//! - 解析：有窗口上下文走 `AppCompat::resolve`，没有退回按进程名（`get_rule`）——两者在窗口
//!   为空时结果相同，后者只是省掉一次合成与缓存键的分配。
//! - 跨越判据：进入类字段（初始中英 / 标点 / 方案）只在「pid 变了，或命中的带窗口条件的进入类
//!   规则集合变了」时重算；带标题条件的规则不进这个集合，故仅标题变化永不重算。
//!
//! 标题由 FocusGained 变长段上报（P3），只在规则表里有标题规则时才有：DLL 收到
//! `CONFIG_KEY_COMPAT_TITLE_MATCH` 开着才采集，服务端这边也只在有标题规则时留下标题
//! （[`Coordinator::focus_window_of`]）。标题按用户数据处理：本模块的日志只记长度。

use super::*;
use std::hash::{Hash, Hasher};
use wind_config::app_compat::{AppCompatRule, ResolvedRule, RuleId, RuleKey, WindowCtx};

/// 一个焦点窗口的身份：顶层窗口类名 + 标题。拿不到的项为空串，带对应条件的规则一律不命中。
///
/// 相等 / `cache_hash` 比「类名 + 标题签名」，不比标题原文：签名 = 命中的标题规则集合
/// （`AppCompat::title_signature`），故仅标题变化而命中集合不变（浏览器换标签页）不算换窗口。
/// 签名下标只在同一张规则表内有意义：重载后至多多算一次「换了窗口」（多推一次，无害）。
///
/// `Debug` 手写：标题是用户数据，只输出长度。
#[derive(Clone, Default)]
pub(crate) struct FocusWindow {
    pub(crate) class: String,
    pub(crate) title: String,
    /// 命中的标题规则下标（升序）；没有可能命中的标题规则 / 无标题时为空。
    pub(crate) title_sig: Vec<usize>,
}

impl PartialEq for FocusWindow {
    fn eq(&self, other: &Self) -> bool {
        self.class == other.class && self.title_sig == other.title_sig
    }
}

impl Eq for FocusWindow {}

impl std::fmt::Debug for FocusWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FocusWindow")
            .field("class", &self.class)
            .field("title_len", &self.title.chars().count())
            .field("title_sig", &self.title_sig)
            .finish()
    }
}

impl FocusWindow {
    /// 只有类名（测试与无标题的场合）。焦点事件一律经 [`Coordinator::focus_window_of`]。
    #[cfg(test)]
    pub(crate) fn of_class(class: &str) -> Self {
        FocusWindow {
            class: class.to_string(),
            ..Default::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.class.is_empty() && self.title.is_empty()
    }

    /// `ActiveCompat` 的缓存键分量：(类名小写, 标题签名) 的哈希。规则按类名不区分大小写匹配，
    /// 类名大小写不同、或仅标题不同而命中集合相同的两次焦点必然解析出同一结果，不必重算。
    pub(crate) fn cache_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.class.to_lowercase().hash(&mut h);
        self.title_sig.hash(&mut h);
        h.finish()
    }

    pub(crate) fn ctx<'a>(&'a self, process: &'a str) -> WindowCtx<'a> {
        WindowCtx {
            process,
            class: &self.class,
            title: &self.title,
        }
    }
}

/// token 窗口表的上限：客户端断开时 bridge 不带 token，表靠容量淘汰最旧的一条。
const FOCUS_WINDOWS_CAP: usize = 256;

/// `client_token → 最近一次 focus_gained 的窗口`，外加 `active_compat` 当前按哪个窗口解析。
#[derive(Debug, Default)]
pub(crate) struct FocusWindows {
    /// 值的第二项是记录序号，按 pid 查时取序号最大（最近获焦）的那条。
    by_token: HashMap<u64, (FocusWindow, u64)>,
    seq: u64,
    /// `active_compat` 当前那份规则是按哪个窗口解析的（与 `ActiveCompat::window_hash` 同步写）。
    active: FocusWindow,
}

/// 「初始模式该按谁算」：上一次真正参与初始模式决策的宿主，见 `Coordinator::mode_scope`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModeScope {
    pub(crate) pid: u32,
    /// 该宿主是否配了初始状态规则（`initial_mode` / `initial_punct`）。
    pub(crate) has_rule: bool,
    /// 命中的带窗口条件的进入类规则（不含带标题条件的），见 [`entry_window_key`]。
    pub(crate) entry_windows: Vec<RuleId>,
    /// 推进本记录的那次焦点「看不见标题」（该进程可能命中标题规则，事件却没带标题）。
    /// 随后同一窗口带着标题来的那次焦点（DLL 收到采集开关后的补发）仍算切进来，见 [`focus_crossed`]。
    pub(crate) title_blind: bool,
    /// 推进本记录的那次焦点的窗口：规则表重载后按它重算 `entry_windows`
    /// （[`Coordinator::refresh_mode_scope_after_reload`]）。不能取「该 pid 最近获焦的窗口」：
    /// 作用域外的过渡窗口（explorer 任务栏）不推进本记录，却会成为最近的那个。
    pub(crate) window: FocusWindow,
}

/// 进入类跨越判据用的规则集合：`entry_window_matches` 去掉带标题条件的。
///
/// 去掉标题规则 = 「仅标题变化永不重算初始模式」（设计稿定稿决策）：同一个窗口的标题随浏览器
/// 标签页、编辑器文件不断变化，拿它当跨越会把用户手切的中英态反复冲掉。代价是标题规则里的进入类
/// 字段只在 pid / 类名集合跨越时随之生效。
pub(crate) fn entry_window_key(r: &ResolvedRule) -> Vec<RuleId> {
    r.entry_window_matches
        .iter()
        .filter(|id| id.title.is_empty())
        .cloned()
        .collect()
}

/// 焦点是否「切进来」（进入类字段该不该重算的前一半判据，另一半见 `should_reapply_initial`）。
///
/// pid 变了，或同一进程内命中的带窗口条件的进入类规则集合变了。pid 未知（0）一律不算。
pub(crate) fn entry_crossed(old: &ModeScope, new_pid: u32, new_entry: &[RuleId]) -> bool {
    new_pid != 0 && (old.pid != new_pid || old.entry_windows != new_entry)
}

/// 完整的「切进来」判据：[`entry_crossed`]，外加「标题迟到」——上一次推进模式归属的焦点看不见
/// 标题（[`ModeScope::title_blind`]），这次同进程、同类名的焦点带着标题来了，且命中了写进入类
/// 字段的标题规则。
///
/// 新进程首焦常早于 DLL 收到「采集标题」开关，补发的带标题焦点按 [`entry_crossed`] 只算「仅标题
/// 变化」，标题规则的 initial_mode 就永不生效。服务端不知道 DLL 何时收到开关，只能从帧内容判断；
/// 条件收得很窄，误判只剩「真没标题的窗口后来有了标题」，代价是那一次按规则重套初始状态。
pub(crate) fn focus_crossed(
    old: &ModeScope,
    new_pid: u32,
    resolved: &ResolvedRule,
    win: &FocusWindow,
) -> bool {
    entry_crossed(old, new_pid, &entry_window_key(resolved))
        || (old.title_blind
            && new_pid != 0
            && old.pid == new_pid
            && !win.title.is_empty()
            && old.window.class == win.class
            && resolved
                .entry_window_matches
                .iter()
                .any(|id| !id.title.is_empty()))
}

impl Coordinator {
    /// 焦点事件带来的窗口（FocusGained 的重型段与同步段 `get_current_mode` 共用，两段必须同源）。
    ///
    /// 该进程不可能命中标题规则时丢掉标题（`AppCompat::process_may_match_title`）：DLL 按开关
    /// 不采集，但开关推送与焦点事件之间有竞态（重载刚删光标题规则、DLL 还没收到关），此时留下标题
    /// 既无用又是用户数据。留下的标题算出标题签名，供窗口身份比较（见 [`FocusWindow`]）。
    pub(crate) fn focus_window_of(&self, process: &str, class: &str, title: &str) -> FocusWindow {
        let table = self.app_compat.lock().unwrap_or_else(|e| e.into_inner());
        if title.is_empty() || !table.process_may_match_title(process) {
            return FocusWindow {
                class: class.to_string(),
                ..Default::default()
            };
        }
        let title_sig = table.title_signature(&WindowCtx {
            process,
            class,
            title,
        });
        FocusWindow {
            class: class.to_string(),
            title: title.to_string(),
            title_sig,
        }
    }

    /// 本次焦点「看不见标题」：该进程可能命中标题规则，焦点事件却没带标题——多半是 DLL 还没收到
    /// 采集开关（新进程首焦与握手推送的竞态）。记进 [`ModeScope::title_blind`]，见 [`focus_crossed`]。
    pub(crate) fn focus_is_title_blind(&self, process: &str, win: &FocusWindow) -> bool {
        win.title.is_empty()
            && self
                .app_compat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .process_may_match_title(process)
    }

    /// 记下 `token` 这次获焦的窗口；与它上一次记录（没有 = 空窗口）不同时返回 true。
    ///
    /// 返回值驱动 DLL 配置重推：DLL 的占位字符 / 密码抑制 / 英文配对是按 token 持有的一份值，
    /// 同一 token（同一 TSF 实例）换到另一个窗口时规则可能变了，不重推 DLL 就一直用旧窗口的值。
    pub(crate) fn note_focus_window(&self, token: u64, win: FocusWindow) -> bool {
        if token == 0 {
            return false;
        }
        let mut t = self.focus_windows.lock().unwrap_or_else(|e| e.into_inner());
        t.seq += 1;
        let seq = t.seq;
        // 没有记录 ≡ 空窗口：握手时推过去的就是按空窗口算的值。
        let changed = match t.by_token.get(&token) {
            Some((prev, _)) => *prev != win,
            None => !win.is_empty(),
        };
        t.by_token.insert(token, (win, seq));
        if t.by_token.len() > FOCUS_WINDOWS_CAP
            && let Some(oldest) = t
                .by_token
                .iter()
                .min_by_key(|(_, (_, s))| *s)
                .map(|(k, _)| *k)
        {
            t.by_token.remove(&oldest);
        }
        changed
    }

    /// `token` 最近一次获焦的窗口；没记录过返回空窗口（不匹配任何窗口规则）。
    pub(crate) fn focus_window_of_token(&self, token: u64) -> FocusWindow {
        self.focus_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .by_token
            .get(&token)
            .map(|(w, _)| w.clone())
            .unwrap_or_default()
    }

    /// `token` 的窗口；它从没获焦过（只发了 ime_activated 的新实例）则退回该 pid 最近获焦的窗口。
    ///
    /// 只给「焦点应用是谁」的两处用（`update_active_compat` 的 active 槽、`apply_initial_mode`）。
    /// 推给 DLL 的配置不用它：那是按 token 持有的值，没获焦过的 token 不该套别的实例的窗口。
    pub(crate) fn focus_window_of_token_or_pid(&self, token: u64) -> FocusWindow {
        if let Some((w, _)) = self
            .focus_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .by_token
            .get(&token)
        {
            return w.clone();
        }
        self.focus_window_of_pid((token >> 32) as u32)
    }

    /// 清掉 `pid` 名下的全部窗口记录（PID 复用：上一任进程的窗口不属于新进程）。
    /// 唯一调用方 `revalidate_pid_name` 同此 cfg。
    #[cfg(any(windows, test))]
    pub(crate) fn forget_focus_windows_of_pid(&self, pid: u32) {
        self.focus_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .by_token
            .retain(|k, _| (*k >> 32) as u32 != pid);
    }

    /// `pid` 最近一次获焦的窗口（它名下各 token 里最新的那条）；没记录过返回空窗口。
    ///
    /// 给只知道 pid 的路径用（输入诊断上报、UIElement 推断、按 pid 的推送判定）。对刚获焦的
    /// 那个 token，它与 [`Self::focus_window_of_token`] 必然相同——密码抑制的两侧（服务端
    /// `apply_input_diag` 按 pid、推给 DLL 的按 token）因此对焦点实例算出同一个值。
    pub(crate) fn focus_window_of_pid(&self, pid: u32) -> FocusWindow {
        if pid == 0 {
            return FocusWindow::default();
        }
        self.focus_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .by_token
            .iter()
            .filter(|(k, _)| (**k >> 32) as u32 == pid)
            .max_by_key(|(_, (_, s))| *s)
            .map(|(_, (w, _))| w.clone())
            .unwrap_or_default()
    }

    /// `active_compat` 当前按哪个窗口解析（焦点应用的窗口上下文）。
    pub(crate) fn active_focus_window(&self) -> FocusWindow {
        self.focus_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .clone()
    }

    /// 只供 `update_active_compat` / 测试同步写：与 `ActiveCompat::window_hash` 同步。
    pub(super) fn set_active_focus_window(&self, win: FocusWindow) {
        self.focus_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active = win;
    }

    /// 按 (进程名, 窗口) 解析规则后交给 `f`。规则查表的唯一入口（[[commit_newline]] 与
    /// HostRender 白名单除外，它们按设计不支持窗口条件）。
    ///
    /// 进程名为空 ⇒ `None`（空名不吃通配，口径同 `AppCompat::get_rule`）。窗口为空 ⇒ 按进程名
    /// 查（不分配、不合成），与 `resolve` 空窗口的结果相同。
    pub(crate) fn with_compat_rule<R>(
        &self,
        process: &str,
        win: &FocusWindow,
        f: impl FnOnce(Option<&AppCompatRule>) -> R,
    ) -> R {
        if process.is_empty() {
            return f(None);
        }
        let table = self.app_compat.lock().unwrap_or_else(|e| e.into_inner());
        if win.is_empty() {
            return f(table.get_rule(process));
        }
        let resolved = table.resolve(&win.ctx(process));
        drop(table);
        f(resolved.rule.as_ref())
    }

    /// 按 (进程名, 窗口) 解析，连同命中的窗口规则集合一起返回（跨越判据要用）。
    pub(crate) fn resolve_compat(
        &self,
        process: &str,
        win: &FocusWindow,
    ) -> std::sync::Arc<ResolvedRule> {
        if process.is_empty() {
            return Default::default();
        }
        self.app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resolve(&win.ctx(process))
    }

    /// 焦点应用（`active_compat.pid` + 它的窗口）的规则。
    pub(crate) fn with_active_compat_rule<R>(
        &self,
        f: impl FnOnce(Option<&AppCompatRule>) -> R,
    ) -> R {
        let name = self.active_process_name();
        let win = self.active_focus_window();
        self.with_compat_rule(&name, &win, f)
    }

    /// pid → 进程名：先读缓存，没有再反查（推送路径用，不得用在 DLL 同步阻塞路径上）。
    pub(super) fn proc_name_or_lookup(&self, pid: u32) -> String {
        if pid == 0 {
            return String::new();
        }
        let cached = self
            .pid_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned();
        cached.unwrap_or_else(|| process_name(pid))
    }

    /// 指定 token（推给它的 DLL 配置）的规则：该 token 自己的窗口；没获焦过 = 无窗口上下文，
    /// 不匹配窗口规则。
    pub(crate) fn with_token_compat_rule<R>(
        &self,
        token: u64,
        f: impl FnOnce(Option<&AppCompatRule>) -> R,
    ) -> R {
        let name = self.proc_name_or_lookup((token >> 32) as u32);
        let win = self.focus_window_of_token(token);
        self.with_compat_rule(&name, &win, f)
    }

    /// 指定 pid 的规则：该 pid 最近一次获焦的窗口；没获焦过 = 不匹配窗口规则。
    pub(crate) fn with_pid_compat_rule<R>(
        &self,
        pid: u32,
        f: impl FnOnce(Option<&AppCompatRule>) -> R,
    ) -> R {
        let name = self.proc_name_or_lookup(pid);
        let win = self.focus_window_of_pid(pid);
        self.with_compat_rule(&name, &win, f)
    }

    /// 规则表重载后按新表重算 `mode_scope.entry_windows`（窗口取推进它的那次焦点）。
    ///
    /// 不重算的话，记录里留着按旧表算的集合，重载后在同一个窗口里的下一次 focus_gained
    /// 就会因「集合变了」被当成切进来，把用户手切的中英态按初始规则冲掉。
    /// `has_rule` 不动：它的刷新沿用既有口径（菜单改初始规则时刷 `active_compat`）。
    pub(crate) fn refresh_mode_scope_after_reload(&self) {
        let (pid, win) = {
            let m = self.mode_scope.lock().unwrap_or_else(|e| e.into_inner());
            (m.pid, m.window.clone())
        };
        if pid == 0 {
            return;
        }
        let name = self.cached_proc_name((pid as u64) << 32);
        let entry = entry_window_key(&self.resolve_compat(&name, &win));
        let mut m = self.mode_scope.lock().unwrap_or_else(|e| e.into_inner());
        if m.pid == pid {
            m.entry_windows = entry;
        }
    }

    /// 右键菜单为焦点应用写回 `fields` 时的目标规则身份（设计稿 P4）。
    ///
    /// 菜单显示的是焦点窗口的生效值；它来自「进程名 + 窗口条件」的规则时，写那条的身份
    /// （出厂规则则在用户层写同身份的覆盖），否则写纯进程键（进程名原样）。判据在
    /// `AppCompat::menu_writeback_target`。日志不含标题文本（标题按用户数据处理）。
    pub(crate) fn menu_writeback_target(&self, name: &str, fields: &[&str]) -> RuleKey {
        let win = self.active_focus_window();
        if name.is_empty() || win.is_empty() {
            return RuleKey::from(name);
        }
        let target = self
            .app_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .menu_writeback_target(&win.ctx(name), fields);
        match target {
            Some(id) => {
                tracing::debug!(
                    "菜单写回 {fields:?}: 生效值来自窗口规则（class={:?} title_len={}），写到该规则",
                    id.class,
                    id.title.chars().count()
                );
                RuleKey::from(&id)
            }
            None => RuleKey::from(name),
        }
    }

    /// 焦点换了窗口（同一 token 的窗口上下文变了）时，给该 token 重推按规则现算的 DLL 配置。
    ///
    /// 清单与 `revalidate_pid_name` 的补推一致：这几项都是「按目标客户端现算」的。握手推的值按
    /// 空窗口算（那时还没有焦点），故首次获焦带来类名时同样要推（`Chrome_WidgetWin_*` 的占位
    /// 字符就靠这一下才到 DLL）。
    pub(crate) fn repush_window_scoped_dll_config(&self, token: u64) {
        if token == 0 {
            return;
        }
        self.push_password_suppress_config(token);
        self.push_english_pair_config(token);
        self.push_pair_via_comp_config(token);
        self.push_composition_placeholder_config(token);
        self.push_space_before_digit_config(token);
        self.push_multiline_commit_config(token);
    }
}

#[cfg(test)]
#[path = "window_ctx_tests.rs"]
mod tests;
