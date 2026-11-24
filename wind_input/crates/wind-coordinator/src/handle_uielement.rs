//! TSF UI-less（宿主自绘候选）：谁画候选、给宿主什么、宿主的操作怎么回来。
//!
//! 宿主以 `TF_TMAE_UIELEMENTENABLEDONLY` 激活或在 `BeginUIElement` 回 `pbShow=FALSE` 即接管
//! 候选绘制（全屏游戏、SDL / Unreal 等）。输入法这边把候选数据交给宿主（DLL 实现
//! `ITfCandidateListUIElement`，经 `CMD_UIELEMENT_QUERY` 从这里拉），并且不再弹自己的候选窗。
//!
//! 按 pid 记账而非「当前连接」：候选窗是服务进程的一个全局窗口，而接管绘制是某个进程的属性，
//! 显示时拿当前在输入的进程（[`Coordinator::focus_pid`]）来查。
//!
//! 与 host-render / `hide_candidate_window` 的区别：后者与本模块各在
//! [`Coordinator::notify_ui_update`] 里占一道早退，压住 `UpdateCandidates`；host-render 是换个
//! 地方画，`UpdateCandidates` 照发。本模块只不弹窗，候选状态照常演进。编码的有效归属
//! （[`Coordinator::preedit_in_app_effective`]）因此只把本模块的压制算进去，host-render 下
//! 编码栏照画，若也强制嵌入就成了两处重复。
//!
//! 设计与外部规范见 `docs/design/game-compat-tsf-uielement.md`。

use crate::coordinator::{Coordinator, State};
use std::sync::Mutex;

use tracing::{debug, info};
use wind_ipc::protocol::{
    UIELEMENT_ACTION_ABORT, UIELEMENT_ACTION_FINALIZE, UIELEMENT_ACTION_SET_PAGE,
    UIELEMENT_ACTION_SET_SELECTION, UiElementPage,
};

impl Coordinator {
    /// 消费一次 DLL 的 `CMD_UIELEMENT_STATE`：两张账一起写完，再统一刷一次 UI。
    ///
    /// 别拆成两次带副作用的写入：宿主从「声明接管」转成「只是读过」时，写完第一张账那一刻
    /// 两张都不命中，候选窗会闪出一帧。
    pub(crate) fn apply_uielement_state(&self, pid: u32, host_draws: bool, host_reads: bool) {
        if pid == 0 {
            return;
        }
        let draws_changed = self.record_uielement_pid(&self.uielement_host_pids, pid, host_draws);
        let reads_changed = self.record_uielement_pid(&self.uielement_reader_pids, pid, host_reads);
        if !draws_changed && !reads_changed {
            return;
        }
        let name = self.cached_proc_name((pid as u64) << 32);
        info!("uielement: pid={pid} name={name:?} host_draws={host_draws} host_reads={host_reads}");
        // 状态翻转要立刻体现：判定自绘时收掉已弹出的窗（首次组合的应答先于本报告到达），
        // 撤销时把候选重新弹出来。
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.notify_ui_update(&state);
    }

    /// 往一张 pid 账里写一笔，回是否改变了集合。不碰 UI，通知由调用方统一发
    /// （见 [`Self::apply_uielement_state`]）。
    fn record_uielement_pid(
        &self,
        account: &Mutex<std::collections::HashSet<u32>>,
        pid: u32,
        member: bool,
    ) -> bool {
        let mut set = account.lock().unwrap_or_else(|e| e.into_inner());
        if member {
            set.insert(pid)
        } else {
            set.remove(&pid)
        }
    }

    /// 记录某进程是否接管候选绘制。`host_draws=false` 即撤销（宿主 `Show(TRUE)` / 结束）。
    /// 只改声明账，读取账原样保留。
    ///
    /// 生产路径走 [`Self::apply_uielement_state`]；本方法供测试单独驱动一张账。
    #[cfg(test)]
    pub(crate) fn set_uielement_host_draws(&self, pid: u32, host_draws: bool) {
        let reads = self
            .uielement_reader_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&pid);
        self.apply_uielement_state(pid, host_draws, reads);
    }

    /// 记录某进程实际读走过候选串（`UIELEMENT_FLAG_HOST_READS`）。
    ///
    /// 与 [`Self::set_uielement_host_draws`] 分成两张账：那张是宿主的声明，这张是推断，
    /// 只有这张受 compat 规则 `host_drawn_candidates` 管。同样只给测试用。
    #[cfg(test)]
    pub(crate) fn set_uielement_host_reads(&self, pid: u32, host_reads: bool) {
        let draws = self
            .uielement_host_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&pid);
        self.apply_uielement_state(pid, draws, host_reads);
    }

    /// 进程退出/切走本输入法时清账。pid 复用时残留条目会让新进程首次候选被压一帧
    /// （DLL 的首次 `BeginUIElement` 会重报纠正），清掉就没有这一帧。
    pub(crate) fn clear_uielement_host_pid(&self, pid: u32) {
        if pid == 0 {
            return;
        }
        let removed = self
            .uielement_host_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
        // 两张账一起清：读取账残留时 pid 复用的新宿主会被直接判成自绘，且不一定有人纠正。
        let reader_removed = self
            .uielement_reader_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
        if removed || reader_removed {
            debug!("uielement: pid={pid} 清账 (draws={removed} reads={reader_removed})");
        }
    }

    /// 当前在输入的进程是否接管了候选绘制。
    ///
    /// 「当前进程」取 [`Coordinator::focus_pid`]（按键/焦点/激活三路都写），退而取
    /// `active_compat.pid`；两者任一命中即算——宁可多压一帧，也不要在独占全屏的游戏上弹窗。
    pub(crate) fn uielement_host_draws(&self) -> bool {
        let set = self
            .uielement_host_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.current_pid_in(&set).is_some()
    }

    /// 当前在输入的进程是否读走过我们的候选串（推断它在自绘）。取 pid 的口径同上。
    /// 生产判据用 [`Self::uielement_host_draws_by_inference`]（它还要查 compat 覆盖）。
    #[cfg(test)]
    pub(crate) fn uielement_host_reads(&self) -> bool {
        self.uielement_reader_pid().is_some()
    }

    /// 同上，但回命中的那个 pid，查 compat 覆盖时必须用它，见
    /// [`Self::uielement_host_draws_by_inference`]。
    fn uielement_reader_pid(&self) -> Option<u32> {
        let set = self
            .uielement_reader_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.current_pid_in(&set)
    }

    /// 「当前在输入的进程」在给定 pid 集合里的话，是哪一个。两张 UIElement 账共用同一
    /// 口径，免得一张改了另一张没改。
    ///
    /// 回 pid 而不是 `bool`：命中可能来自 `focus_pid`（按键来源）或 `active_compat.pid`
    /// （焦点事件），两者在游戏宿主上会分岔（常常一次 `focus_gained` 都不来，见
    /// `key_source_pid_alone_matches_host_draws`）。谁命中就得用谁查进程名。
    fn current_pid_in(&self, set: &std::collections::HashSet<u32>) -> Option<u32> {
        if set.is_empty() {
            return None;
        }
        let key_pid = self.focus_pid.load(std::sync::atomic::Ordering::Relaxed);
        if key_pid != 0 && set.contains(&key_pid) {
            return Some(key_pid);
        }
        let compat_pid = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pid;
        if compat_pid != 0 && set.contains(&compat_pid) {
            return Some(compat_pid);
        }
        None
    }

    /// 推断那条判据：宿主读走了候选串，且在 compat 里显式开启 `host_drawn_candidates = true`。
    /// opt-in 的依据见 `docs/design/game-compat-tsf-uielement.md` §1.4「推断被证伪，改成 opt-in」。
    ///
    /// 查名用命中的那个 pid（见 [`Self::current_pid_in`]），不能走 `active_process_name()`：
    /// 它只认 `active_compat.pid`，游戏宿主常靠 `focus_pid` 才命中，查到上一个进程会让 opt-in
    /// 规则静默失效。
    ///
    /// `process = "*"` 通配由 `AppCompat::get_rule` 合成；查不到进程名时 `get_rule` 不吃通配，
    /// 这里显式取通配那条。
    pub(crate) fn uielement_host_draws_by_inference(&self) -> bool {
        let Some(pid) = self.uielement_reader_pid() else {
            return false;
        };
        // 命中的是焦点应用时读预提取值（`ActiveLookups`），不查表。预提取按 active 槽的窗口，
        // 现查按 `focus_window_of_pid`；同一进程的旧实例只发 ime_activated 时两者可能不同，
        // 那时 active 槽才是正在输入的窗口。
        let prefetched = {
            let ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
            (ac.pid == pid).then_some(ac.lookups.host_drawn_candidates)
        };
        let rule = prefetched.unwrap_or_else(|| {
            let name = self.cached_proc_name((pid as u64) << 32);
            // 无名进程也认通配（`an_unnamed_process_still_honours_the_wildcard_rule`）。
            let key = if name.is_empty() {
                wind_config::app_compat::ANY_PROCESS
            } else {
                name.as_str()
            };
            // 游戏常常一次 focus_gained 都没有，窗口为空，只按进程名匹配。
            let win = self.focus_window_of_pid(pid);
            self.with_compat_rule(key, &win, |r| r.and_then(|r| r.host_drawn_candidates))
        });
        rule
            // 没有规则就不收窗：很多普通宿主也会读走候选串却不画。误判代价不对称——判成「在画」
            // 会一个候选框都没有，判成「没画」最多多一个框，故默认站在不收窗一侧。
            .unwrap_or(false)
    }

    /// 本进程的浮窗（候选窗 / 状态气泡 / 工具栏）是否该被压住。返回原因（供日志），
    /// `None` = 照常显示。命中任一判据即压。
    ///
    /// 判据一：宿主接管绘制（`host_draws`，`BeginUIElement` 回了 `pbShow=FALSE`，如 SDL2 游戏、
    /// Unreal）。不管是否独占全屏一律不弹：两头画，且经 SDL 拿不到文本光标，位置也是错的。
    /// Chromium/Electron/QQNT 走 IME-first 但 `pbShow=TRUE`，不算接管。
    ///
    /// 判据二：前台是 D3D 独占全屏（`fullscreen_exclusive_cached`），兜住不接管的游戏；无边框
    /// 全屏不在此列。独占判据会逐键抖动，判据一因此不能只当诊断，见设计文档 §4.5。
    ///
    /// 判据三：宿主没声明、却读走了候选串（CUAS 的 IMM32 桥替传统宿主画旧版候选窗）。这是推断
    /// 不是事实，故只有它受 compat `host_drawn_candidates` 管且为 opt-in，见
    /// [`Self::uielement_host_draws_by_inference`]。判据一二是可由程序判定的事实，不设覆盖（R1）。
    pub(crate) fn ui_suppressed_by_host(&self) -> Option<&'static str> {
        if self.uielement_host_draws() {
            return Some("uielement_host_draws");
        }
        if self.uielement_host_draws_by_inference() {
            return Some("uielement_host_reads");
        }
        if self
            .fullscreen_exclusive_cached
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Some("d3d_exclusive_fullscreen");
        }
        None
    }

    /// 给 DLL 的候选快照（`CMD_UIELEMENT_QUERY` 的应答），纯读。
    ///
    /// 只带当页：`GetCount` = 当页条数、页数恒 1、高亮为页内下标，与 Weasel / 微软拼音同形。
    /// 自绘宿主会把 `GetCount` 条全部画出，喂整条列表还会在独占全屏下冻死游戏，勿改回；
    /// 见设计文档 §4.3。翻页/移高亮后 DLL 重拉。
    pub(crate) fn uielement_page_snapshot(&self) -> UiElementPage {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pp = self.page_size(&state).max(1);
        let start = state.current_page * pp;
        let end = (start + pp).min(state.candidates.len());
        let items: Vec<String> = if start < end {
            state.candidates[start..end]
                .iter()
                .map(|c| self.cand_display_text(&state, c))
                .collect()
        } else {
            Vec::new()
        };
        // 高亮为页内下标（selected_index 本就是页内相对下标），钉在当页范围内。
        let selected = if items.is_empty() {
            0
        } else {
            state.selected_index.min(items.len() - 1) as u32
        };
        // 单页视图：页数恒 1、当前页恒 0（与 Weasel / 微软拼音一致）。
        UiElementPage {
            items,
            selected,
            page_size: pp as u32,
            current_page: 0,
        }
    }

    /// 宿主经 `ITfCandidateListUIElementBehavior` 发来的操作。
    ///
    /// 结果不在这里回：上屏/清组合经 push 管道回到 DLL（与鼠标点选同一条路），
    /// 高亮/翻页变化由 DLL 随后再拉一次快照得到。
    pub(crate) fn apply_uielement_action(&self, action: u32, arg: u32) {
        match action {
            UIELEMENT_ACTION_SET_SELECTION => {
                // 当页模型：宿主给的是页内下标（0..pageSize）。
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if self.set_highlight_page_local(&mut state, arg as usize) {
                    self.notify_ui_update(&state);
                }
            }
            UIELEMENT_ACTION_SET_PAGE => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let target = arg as usize;
                let mut changed = false;
                // 走既有的翻页原语而不是直接赋值：`page_next` 负责动态扩展候选与末页放宽，
                // 直接改 `current_page` 会跳过这两件事。
                while state.current_page < target && self.page_next(&mut state) {
                    changed = true;
                }
                while state.current_page > target && self.page_prev(&mut state) {
                    changed = true;
                }
                if changed {
                    self.notify_ui_update(&state);
                }
            }
            UIELEMENT_ACTION_FINALIZE => {
                // 定稿当前高亮 = 鼠标点选当页高亮项，走同一条 push 出口。
                // 取绝对下标、不经 `mouse_select`：后者把下标当 UI 回传的（展开态下是网格平铺
                // 下标）解释，而这里手里的是页内高亮。
                let idx = {
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.candidates.is_empty() {
                        return;
                    }
                    self.highlighted_global_index(&state)
                };
                let _ = self.select_candidate_at(idx);
            }
            UIELEMENT_ACTION_ABORT => {
                let act = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.candidates.is_empty() && state.input_buffer.is_empty() {
                        return;
                    }
                    self.cancel_session(&mut state)
                };
                // cancel_session 内部已 notify_ui_hide；这里补上「让宿主结束组合」那一半。
                // 它给出的是按键路径的应答形态，无按键上下文时统一推 ClearComposition——
                // 与托盘/菜单那条 `push_switch_commit` 的空文本分支同理。
                debug!("uielement: abort → {act:?}");
                self.push_server
                    .push_commit_to_active(&wind_ipc::codec::encode_clear_composition());
            }
            other => debug!("uielement: 未知动作 {other}（arg={arg}），忽略"),
        }
    }

    /// 把高亮移到当页页内下标（0..pageSize）；越界或当页无此条则不动。返回是否变化。
    fn set_highlight_page_local(&self, state: &mut State, local: usize) -> bool {
        let pp = self.page_size(state).max(1);
        if local >= pp {
            return false;
        }
        // 末页可能不足一页：该页内是否真有这条候选。
        let global = state.current_page * pp + local;
        if global >= state.candidates.len() {
            return false;
        }
        if state.selected_index == local {
            return false;
        }
        self.clear_hover();
        state.selected_index = local;
        true
    }
}

#[cfg(test)]
#[path = "handle_uielement_tests.rs"]
mod tests;
