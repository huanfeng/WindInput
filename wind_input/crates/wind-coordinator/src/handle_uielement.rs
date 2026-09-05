//! TSF UI-less（宿主自绘候选）：谁画候选、给宿主什么、宿主的操作怎么回来。
//!
//! # 背景
//!
//! TSF 允许宿主接管候选列表的绘制：线程以 `TF_TMAE_UIELEMENTENABLEDONLY` 激活，或在
//! `ITfUIElementSink::BeginUIElement` 里回 `pbShow=FALSE`。全屏游戏、搜索框、游戏引擎
//! （SDL、Unreal 等）都走这条路——它们的画面里根本容不下别的进程弹的窗口：独占全屏下
//! 一弹就把游戏踢出独占态，处理不好的游戏直接崩。
//!
//! 输入法这边要做两件事：**把候选数据交给宿主**（DLL 实现 `ITfCandidateListUIElement`，
//! 数据经 `CMD_UIELEMENT_QUERY` 从这里拉），以及**不再弹自己的候选窗**（按 pid 记账，
//! 见 [`Coordinator::ui_suppressed_by_host`]）。
//!
//! # 为什么按 pid 记账而不是「当前连接」
//!
//! 候选窗是服务进程里的**一个**全局窗口，而「宿主接管绘制」是**某个进程**的属性：
//! 游戏接管了，切到记事本仍要弹。DLL 报告的是自己进程的状态，这里按 pid 存；显示时
//! 拿「当前在输入的进程」（[`Coordinator::focus_pid`]，按键/焦点/激活三路都写）来查。
//!
//! # 与 host-render / `hide_candidate_window` 的关系
//!
//! 三者都在 [`Coordinator::notify_ui_update`] 里压住 `UpdateCandidates`，但意图不同：
//! host-render 是「换个地方画」（数据仍走 SHM 到 DLL 的 band 窗口）、`hide_candidate_window`
//! 是用户开关、本模块是宿主接管。本模块只**不弹窗**，候选状态照常演进——空格上屏、
//! 数字选词、翻页全部照旧，宿主画的就是这份状态。
//!
//! 设计与外部规范摘要见 `docs/design/game-compat-tsf-uielement.md`。

use crate::coordinator::{Coordinator, State};
use tracing::{debug, info};
use wind_ipc::protocol::{
    UIELEMENT_ACTION_ABORT, UIELEMENT_ACTION_FINALIZE, UIELEMENT_ACTION_SET_PAGE,
    UIELEMENT_ACTION_SET_SELECTION, UiElementPage,
};

impl Coordinator {
    /// 记录某进程是否接管候选绘制。`host_draws=false` 即撤销（宿主 `Show(TRUE)` / 结束）。
    pub(crate) fn set_uielement_host_draws(&self, pid: u32, host_draws: bool) {
        if pid == 0 {
            return;
        }
        let changed = {
            let mut set = self
                .uielement_host_pids
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if host_draws {
                set.insert(pid)
            } else {
                set.remove(&pid)
            }
        };
        if changed {
            let name = self.cached_proc_name((pid as u64) << 32);
            info!("uielement: pid={pid} name={name:?} host_draws={host_draws}（宿主接管候选绘制）");
            // 状态翻转要立刻体现：接管时收掉已弹出的窗（首次组合的应答先于本报告到达），
            // 撤销时把候选重新弹出来。
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.notify_ui_update(&state);
        }
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
        if removed {
            debug!("uielement: pid={pid} 清账");
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
        if set.is_empty() {
            return false;
        }
        let key_pid = self.focus_pid.load(std::sync::atomic::Ordering::Relaxed);
        let compat_pid = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pid;
        (key_pid != 0 && set.contains(&key_pid)) || (compat_pid != 0 && set.contains(&compat_pid))
    }

    /// 本进程的浮窗（候选窗 / 状态气泡 / 工具栏）是否该被压住。返回原因（供日志），
    /// `None` = 照常显示。命中任一判据即压。
    ///
    /// **判据一：宿主接管绘制（host_draws）** —— 焦点进程在 `BeginUIElement` 回了
    /// `pbShow=FALSE`（真 UI-less 宿主：SDL2 游戏如 Dota 2、Unreal、ImeSharp）。宿主明确
    /// 声明「候选 UI 由我负责」，我们再弹自己的窗只会两头画；且我们经 SDL 拿不到文本光标
    /// （SDL 不经 TSF 上报），窗只能退到宿主窗口左下角，**位置是错的**（Dota 2 窗口模式实测
    /// `pos=100,1080`）。故**不管是不是独占全屏，只要宿主接管就一律不弹**，候选数据仍照喂
    /// （`ITfCandidateListUIElement` + 分页表），由宿主 / 读屏自绘。
    ///
    /// ⛔ 只对 `pbShow=FALSE` 的宿主生效：Chromium/Electron/QQNT 走 IME-first 调度但
    /// `pbShow=TRUE`（host_draws=false），照常弹我们的窗——别把它们误判成接管。
    ///
    /// **判据二：前台是 D3D 独占全屏**（`fullscreen_exclusive_cached`）—— 兜住**不接管**的
    /// 游戏（IMM32 桥接）。别的进程的窗盖上来会把游戏踢出独占态（闪黑、切分辨率、卡死）。
    /// 无边框全屏（Covering）不在此列。
    ///
    /// ★ 为什么两条都要、且 host_draws 不能只当诊断（2026-09-05 Dota 2 实测教训）：独占判据
    /// **逐键抖动**——同一个 Dota `SDL_app` 窗口，`SHQueryUserNotificationState` 一会儿回
    /// D3dExclusive、一会儿因矩形判据落到 Covering。v3 只压独占、host_draws 照弹，于是抖到
    /// Covering 的那一键弹了窗、把游戏踢出独占 → 黑屏 → 游戏重抢独占 → 下一键又判独占……
    /// **反馈环 = 持续跳黑屏卡死**。按 host_draws 无条件压窗后，独占判据抖不抖都不弹，环断开。
    ///
    /// 两条都**不设配置键**：都是可由程序判定的物理事实，按 config-design-rules R1 处理。
    pub(crate) fn ui_suppressed_by_host(&self) -> Option<&'static str> {
        if self.uielement_host_draws() {
            return Some("uielement_host_draws");
        }
        if self
            .fullscreen_exclusive_cached
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Some("d3d_exclusive_fullscreen");
        }
        None
    }

    /// 给 DLL 的候选快照（`CMD_UIELEMENT_QUERY` 的应答）。**纯读**。
    ///
    /// **只带当页**：`GetCount` = 当页条数、页数恒 1、高亮为页内下标。与 Weasel /
    /// 微软拼音同一形状。曾带「从 0 起至少 64 条」的前缀让宿主自己切页——Dota 2 这类
    /// 自绘候选的宿主会把 `GetCount` 条**全部**画出来（微软五笔在 Dota 2 里「所有页一次
    /// 显示、翻页崩游戏」正是这个形状，微软拼音则正常），故收敛为当页。翻页/上下移高亮
    /// 后 DLL 重拉，宿主看到的就是新的一页。
    pub(crate) fn uielement_page_snapshot(&self) -> UiElementPage {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pp = self.per_page(state.active).max(1);
        // **只带当页**（Weasel / 微软拼音 / 搜狗同形状）。★★★ 2026-09-05 Dota 2 实测教训：
        // v3 曾改成「整条列表」（想让 CUAS 桥认出可翻页列表、开宿主候选盒），实测彻底反了——
        // ① Dota 读走全部 100 条却**并不据此开候选盒**（getter 日志 GetString 0..99 全读、
        //    借边框下无候选窗）；② **独占全屏下读完 100 条即冻死游戏**（seq44 实测：借边框喂
        //    100 条正常、切独占喂 100 条第一键就冻）。这正是记忆里「微软五笔给整条列表在
        //    Dota 2 所有页一次显示、翻页崩」的同一形状。③ 用户实测微软五笔 / 搜狗的候选在
        //    Dota 2 里是**游戏自绘**（同一风格），差别只在它们**只喂当页**——所以只带当页
        //    既能让游戏画出候选、又不会崩。⛔⛔ 勿再改回整条列表。
        // 翻页 / 上下移高亮由我方按键驱动；换页后 DLL 重拉，宿主看到的就是新的一页。
        let start = state.current_page * pp;
        let end = (start + pp).min(state.candidates.len());
        let items: Vec<String> = if start < end {
            state.candidates[start..end]
                .iter()
                .map(|c| self.cand_s2t_text(&state, c))
                .collect()
        } else {
            Vec::new()
        };
        // 高亮为**页内**下标（selected_index 本就是页内相对下标），钉在当页范围内。
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
                // 当页模型：宿主给的是**页内**下标（0..pageSize）。
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
                let page_local = {
                    let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.candidates.is_empty() {
                        return;
                    }
                    state.selected_index
                };
                self.mouse_select(page_local);
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

    /// 把高亮移到当页**页内**下标（0..pageSize）；越界或当页无此条则不动。返回是否变化。
    fn set_highlight_page_local(&self, state: &mut State, local: usize) -> bool {
        let pp = self.per_page(state.active).max(1);
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
mod tests {
    use super::*;
    use std::sync::Arc;
    use wind_candidate::Candidate;
    use wind_config::Config;
    use wind_ui_types::UiCommand;

    fn coord() -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
        Coordinator::new_headless_with_ui(Config::default(), None)
    }

    fn fill(c: &Coordinator, n: usize) {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "ni".into();
        st.candidates = (0..n)
            .map(|i| Candidate {
                text: format!("c{i}"),
                ..Default::default()
            })
            .collect();
        st.current_page = 0;
        st.selected_index = 0;
        st.caret_x = 100;
        st.caret_y = 200;
        st.caret_height = 20;
    }

    /// 把焦点进程设成 `pid`（焦点事件那一路），并用 instant 首显档让 `UpdateCandidates`
    /// 立即下发（本组测试只关心「发没发」，不关心首显闸门）。
    fn focus_pid(c: &Coordinator, pid: u32) {
        c.focus_pid.store(0, std::sync::atomic::Ordering::Relaxed);
        *c.active_compat.lock().unwrap() = crate::coordinator::ActiveCompat {
            pid,
            first_show_mode: Some(wind_config::app_compat::FirstShowMode::Instant),
            ..Default::default()
        };
    }

    fn drain(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Vec<&'static str> {
        rx.try_iter()
            .map(|cmd| match cmd {
                UiCommand::UpdateCandidates { .. } => "update",
                UiCommand::HideCandidates => "hide",
                _ => "other",
            })
            .collect()
    }

    /// 宿主接管绘制（host_draws）压住我们的候选窗：真 UI-less 宿主（Dota 2/SDL2 回
    /// pbShow=FALSE）声明自绘，我们再弹会两头画、且位置错（SDL 不报光标）。数据仍照喂。
    /// ⛔ 不管是不是独占全屏都压——独占判据逐键抖动，只靠它挡不干净（见 ui_suppressed_by_host
    /// 的注释：v3 只压独占导致 Dota 2 跳黑屏卡死）。
    #[test]
    fn host_draws_suppresses_candidate_window() {
        let (c, rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 42);
        c.set_uielement_host_draws(42, true);
        assert!(c.uielement_host_draws(), "记账记录该进程接管");
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_draws"),
            "host_draws 即压窗，与全屏模式无关"
        );
        let _ = drain(&rx);
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        let got = drain(&rx);
        assert!(
            got.contains(&"hide") && !got.contains(&"update"),
            "接管态只发 Hide、不弹我们的窗: {got:?}"
        );

        // 撤销接管后恢复弹窗。
        c.set_uielement_host_draws(42, false);
        assert_eq!(c.ui_suppressed_by_host(), None, "撤销接管后恢复");
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        assert!(drain(&rx).contains(&"update"), "撤销接管应把候选弹回来");
    }

    /// D3D 独占全屏缓存位单独就能压住候选窗；无边框全屏（仅 fullscreen_cached）不压。
    #[test]
    fn exclusive_fullscreen_suppresses_but_covering_does_not() {
        let (c, rx) = coord();
        fill(&c, 3);
        focus_pid(&c, 1);
        c.fullscreen_cached
            .store(true, std::sync::atomic::Ordering::Relaxed);
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        assert!(drain(&rx).contains(&"update"), "无边框全屏照常显示");
        c.fullscreen_exclusive_cached
            .store(true, std::sync::atomic::Ordering::Relaxed);
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        let got = drain(&rx);
        assert!(got.contains(&"hide") && !got.contains(&"update"), "{got:?}");
        assert_eq!(c.ui_suppressed_by_host(), Some("d3d_exclusive_fullscreen"));
    }

    /// 快照只带当页：条数=当页条数、page_size=每页数、selected=**页内**下标、current_page 恒 0；
    /// 末页不足一页只带剩下的；空列表不 panic。⛔ 勿改回整条列表（Dota 2 独占全屏读完即冻死）。
    #[test]
    fn snapshot_is_current_page_only() {
        let (c, _rx) = coord();
        let pp = {
            let st = c.state.lock().unwrap();
            c.per_page(st.active)
        };
        fill(&c, pp * 3 + 2);
        {
            let mut st = c.state.lock().unwrap();
            st.current_page = 1;
            st.selected_index = 2;
        }
        let page = c.uielement_page_snapshot();
        assert_eq!(page.items.len(), pp, "只带当页那 pp 条");
        assert_eq!(
            page.items[0],
            format!("c{pp}"),
            "当页首条是绝对下标 pp 的候选"
        );
        assert_eq!(
            (page.selected, page.page_size as usize, page.current_page),
            (2, pp, 0),
            "selected 是页内下标、page_size=pp、current_page 恒 0"
        );

        // 末页不足一页：只带剩下的两条。
        {
            let mut st = c.state.lock().unwrap();
            st.current_page = 3;
            st.selected_index = 0;
        }
        assert_eq!(c.uielement_page_snapshot().items.len(), 2, "末页只带剩下的");

        fill(&c, 0);
        let empty = c.uielement_page_snapshot();
        assert!(empty.items.is_empty());
        assert_eq!(empty.selected, 0);
    }

    /// 按键来源 pid 单独就能对上接管记账并压窗（游戏宿主常没有 focus_gained，active_compat
    /// 停在上一个进程，「谁在输入」由每个按键的管道对端 pid 写 focus_pid）。
    #[test]
    fn key_source_pid_alone_matches_host_draws() {
        let (c, _rx) = coord();
        fill(&c, 3);
        focus_pid(&c, 1); // 焦点事件那一路记的是别的进程
        c.set_uielement_host_draws(42, true);
        assert!(!c.uielement_host_draws(), "焦点在 1，未命中 42");
        assert_eq!(c.ui_suppressed_by_host(), None, "未命中则不压");
        c.focus_pid.store(42, std::sync::atomic::Ordering::Relaxed);
        assert!(c.uielement_host_draws(), "按键来源切到 42 后命中记账");
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_draws"),
            "命中即压窗"
        );
    }

    /// 宿主操作：SetPage 走翻页原语；SetSelection 按**页内**下标落到当页；越界不动。
    #[test]
    fn actions_move_highlight_and_page() {
        let (c, rx) = coord();
        let pp = {
            let st = c.state.lock().unwrap();
            c.per_page(st.active)
        };
        fill(&c, pp * 4);
        focus_pid(&c, 1);
        let _ = drain(&rx);
        c.apply_uielement_action(UIELEMENT_ACTION_SET_PAGE, 2);
        {
            let st = c.state.lock().unwrap();
            assert_eq!((st.current_page, st.selected_index), (2, 0));
        }
        assert!(drain(&rx).contains(&"update"), "翻页须重绘");
        // SetSelection 给的是页内下标：落到当页（第 2 页）页内第 1 项。
        c.apply_uielement_action(UIELEMENT_ACTION_SET_SELECTION, 1);
        {
            let st = c.state.lock().unwrap();
            assert_eq!((st.current_page, st.selected_index), (2, 1));
        }
        assert!(drain(&rx).contains(&"update"), "高亮变化须重绘");

        c.apply_uielement_action(UIELEMENT_ACTION_SET_PAGE, 0);
        {
            let st = c.state.lock().unwrap();
            assert_eq!((st.current_page, st.selected_index), (0, 0));
        }
        // 越界：不动、不重绘。
        let _ = drain(&rx);
        c.apply_uielement_action(UIELEMENT_ACTION_SET_SELECTION, 10_000);
        assert!(drain(&rx).is_empty());
        {
            let st = c.state.lock().unwrap();
            assert_eq!((st.current_page, st.selected_index), (0, 0));
        }
    }

    /// Abort：清空会话并隐藏；空会话时无动作。
    #[test]
    fn abort_clears_session() {
        let (c, rx) = coord();
        fill(&c, 3);
        let _ = drain(&rx);
        c.apply_uielement_action(UIELEMENT_ACTION_ABORT, 0);
        {
            let st = c.state.lock().unwrap();
            assert!(st.candidates.is_empty() && st.input_buffer.is_empty());
        }
        assert!(drain(&rx).contains(&"hide"));
    }
}
