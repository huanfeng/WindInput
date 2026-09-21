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
//! 意图不同，**落点也不同**：`hide_candidate_window`（用户开关）与本模块（宿主接管）各在
//! [`Coordinator::notify_ui_update`] 里占一道早退，压住 `UpdateCandidates`；而 host-render
//! **不在那个函数里**——它是「换个地方画」，`UpdateCandidates` 照发，只是数据走 SHM 交给
//! 宿主进程内 DLL 的 band 窗口去渲染（`host_render_active()` 的生产消费点在
//! `message_handler.rs` 的伪终止事件那一处）。本模块只**不弹窗**，候选状态照常演进——
//! 空格上屏、数字选词、翻页全部照旧，宿主画的就是这份状态。
//!
//! 这条区别有实质后果：编码的**有效归属**（[`Coordinator::preedit_in_app_effective`]）只把
//! 本模块这道压制算进去。host-render 下候选窗的编码栏照画，若也强制嵌入就成了两处重复。
//!
//! 设计与外部规范摘要见 `docs/design/game-compat-tsf-uielement.md`。

use crate::coordinator::{Coordinator, State};
use std::sync::Mutex;

use tracing::{debug, info};
use wind_ipc::protocol::{
    UIELEMENT_ACTION_ABORT, UIELEMENT_ACTION_FINALIZE, UIELEMENT_ACTION_SET_PAGE,
    UIELEMENT_ACTION_SET_SELECTION, UiElementPage,
};

/// compat 里表示「所有应用」的进程名，目前只被 `host_drawn_candidates` 的回落查表认。
///
/// ⛔ 刻意**不**做成 `AppCompat::get_rule` 的通用通配：那会让 `process = "*"` 的一条规则
/// 把全部字段（初始中英、首显档、定位方式……）一次性套到每个应用头上，是个比本次要解决
/// 的问题大得多的语义变更。这里只给「推断收窗」这一条判据留一个全局关闭口。
pub(crate) const HOST_DRAWN_WILDCARD: &str = "*";

impl Coordinator {
    /// 消费一次 DLL 的 `CMD_UIELEMENT_STATE`：**两张账一起写完，再统一刷一次 UI**。
    ///
    /// ⚠ 别拆成两次带副作用的写入。DLL 报的是一整份 flags，拆开写会在过渡态露出半截
    /// 状态：宿主从「声明接管」转成「只是读过」（`Show(TRUE)` 之后闩仍在）时，先写完
    /// 声明账那一刻两张账都不命中，`notify_ui_update` 会把候选窗弹出来一帧，等第二张账
    /// 写完才收回去——用户看到候选窗闪一下。
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

    /// 往一张 pid 账里写一笔，回「这一笔是否改变了集合」。**不**碰 UI——通知由调用方
    /// 在两张账都写完之后统一发一次（见 [`Self::apply_uielement_state`]）。
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
    /// 生产路径走 [`Self::apply_uielement_state`]（DLL 一份 flags 一次写完）；本方法留给
    /// 测试单独驱动一张账，故 `cfg(test)`。
    #[cfg(test)]
    pub(crate) fn set_uielement_host_draws(&self, pid: u32, host_draws: bool) {
        let reads = self
            .uielement_reader_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&pid);
        self.apply_uielement_state(pid, host_draws, reads);
    }

    /// 记录某进程**实际读走过候选串**（`UIELEMENT_FLAG_HOST_READS`）。
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
        // 两张账一起清：读取账留着更危险——pid 复用后新宿主会被直接判成自绘，
        // 而它自己的首次 GetString 不一定会发生，没人来纠正。
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

    /// 当前在输入的进程是否**读走过**我们的候选串（推断它在自绘）。取 pid 的口径同上。
    /// 生产判据用 [`Self::uielement_host_draws_by_inference`]（它还要查 compat 覆盖）。
    #[cfg(test)]
    pub(crate) fn uielement_host_reads(&self) -> bool {
        self.uielement_reader_pid().is_some()
    }

    /// 同上，但回**命中的那个 pid**——查 compat 覆盖时必须用它，不能另取一次，见
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
    /// ⚠ 回的是 `Option<u32>` 而不是 `bool`：命中可能来自 `focus_pid`（按键来源）也可能
    /// 来自 `active_compat.pid`（焦点事件），**两者在游戏宿主上会分岔**——游戏常常没有
    /// 可编辑 TSF 上下文、`focus_gained` 一次都不来，`active_compat` 会停在上一个进程
    /// （既有测试 `key_source_pid_alone_matches_host_draws` 就钉的这个）。谁命中就得用谁
    /// 去查进程名，否则覆盖规则会落到别的进程头上。
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

    /// 推断那条判据：宿主读走了候选串，**且**在 compat 里被显式开启（`= true`）。
    ///
    /// ⚠ 2026-09-15 起是 **opt-in**：默认不据此收窗，理由见函数体末尾 `unwrap_or(false)`
    /// 处的长注释（三个反证样本 + 两种误判的代价不对称）。此前是默认开启、靠 compat
    /// 写 `false` 逐个关，那个方向在拿到反证后不成立了。
    ///
    /// 查名用**命中的那个 pid**（见 [`Self::current_pid_in`]）：不能图省事走
    /// `active_process_name()`，它只认 `active_compat.pid`，而游戏宿主恰恰常常靠
    /// `focus_pid` 才命中——那样写的话查到的是**上一个进程**的名字，用户给游戏配的
    /// opt-in 规则静默失效 ⇒ 不收窗 ⇒ 新枫之谷又变回两个候选框。
    ///
    /// （opt-out 时代这里的后果是反过来的：查错 pid ⇒ 关不掉推断 ⇒「两个候选框都没了」。
    /// 反转成 opt-in 之后，查错 pid 的代价从「彻底不能用」降成「多一个框」，但规则该生效
    /// 而不生效仍是 bug，判据不变。）
    ///
    /// 覆盖查两层：先查本进程名，再回落到通配规则 `process = "*"`。通配那层保留下来是给
    /// 反方向用的——某类宿主普遍需要收窗时可一行开到全局；opt-in 之后它不再是「故障半径
    /// 的配套」，因为默认已经不会出现「所有应用候选框都没了」。
    ///
    /// 查不到进程名时按默认（**不**收窗）走：查不到名字就等于查不到规则，而 opt-in 的前提
    /// 是「有人明确说过这个宿主要收」——查不出是谁，就谈不上它被指名过。
    pub(crate) fn uielement_host_draws_by_inference(&self) -> bool {
        let Some(pid) = self.uielement_reader_pid() else {
            return false;
        };
        let name = self.cached_proc_name((pid as u64) << 32);
        let table = self.app_compat.lock().unwrap_or_else(|e| e.into_inner());
        let by_process = if name.is_empty() {
            None
        } else {
            table.get_rule(&name).and_then(|r| r.host_drawn_candidates)
        };
        by_process
            .or_else(|| {
                table
                    .get_rule(HOST_DRAWN_WILDCARD)
                    .and_then(|r| r.host_drawn_candidates)
            })
            // ★★★ 没有规则 ⇒ **不**据此收窗（2026-09-15 反转，原为 `unwrap_or(true)`）。
            //
            // 「读走候选串 ⇒ 它在画」这条推断立案时自陈「至今没有反证样本」——那份日志里
            // 7 个宿主只有新枫之谷读过，另外 6 个一次候选都没出过，构不成负对照。反证样本
            // 2026-09-15 在靶机上一次到齐：**Notepad / Illustrator / EverEdit 三个宿主都把
            // 候选串整串读走（`GetString(0..4)`），却都不画候选窗**。同一台机器上新旧两版
            // DLL 的对照（旧版也记录到记事本读了 96 次、但不据此收窗）说明读取是这些宿主的
            // 常态行为，不是自绘的标志。
            //
            // 反转的是**默认值**而不是判据本身：新枫之谷那类宿主（CUAS 的 IMM32 桥替它读走
            // 候选、画出旧版系统候选窗）仍可经 compat 写 `host_drawn_candidates = true` 保住
            // 修复，见 data/compat.toml 的出厂规则。
            //
            // 为什么默认值该站在「不收窗」这一侧——**两种误判的代价不对称**：
            //   判错成「宿主在画」（实际没画）⇒ 两个候选框一个都没有，输入法完全不能用，
            //     且用户无从知道要去 compat 里配什么；
            //   判错成「宿主没画」（实际在画）⇒ 最多多出一个框，字照打、能用，配一行即可收。
            // 一个明确标着「是推断不是事实」的判据，不该把不可用那一侧设成默认。
            .unwrap_or(false)
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
    /// **判据三：宿主没声明、却把候选串读走了**（`UIELEMENT_FLAG_HOST_READS`）—— 已知的
    /// 读取者是 CUAS 的 IMM32 桥：传统宿主经 `ImmGetCandidateList` 取候选，由宿主或
    /// `DefWindowProc` 画出旧版候选窗。2026-09-11 新枫之谷实测就是这个形态——屏幕上两个
    /// 候选框，且游戏画的那个停在第一个码的候选上。读了就是在画，我们不必再画第二份；
    /// 宿主画的那个还贴着它自己的输入框（位置由它的 `ImmSetCandidateWindow` 决定），
    /// 比我们在全屏游戏里靠 caret 猜的位置准。
    ///
    /// ⚠ 判据一二是**事实**（宿主声明 / 物理独占），判据三是**推断**——读候选串的不一定
    /// 都在画。故只有它受 compat 规则 `host_drawn_candidates` 管，且 2026-09-15 起改成
    /// **opt-in**：写 `= true` 的宿主才据此收窗，不写一律照弹我们的窗。
    ///
    /// 反转的由来：立案时「只有真要画的宿主才会来读候选串」这条假设自陈没有反证样本，
    /// 而反证在靶机上一次到齐——Notepad / Illustrator / EverEdit 都整串读走候选却都不画。
    /// 详见 [`Self::uielement_host_draws_by_inference`]。
    ///
    /// 判据一二**不设任何覆盖**：都是可由程序判定的物理事实，按 R1 处理。
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
                .map(|c| self.cand_convert_text(&state, c))
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
    use wind_bridge::handler::{COMPOSITION_PLACEHOLDER, KeyAction, KeyEventData, MessageHandler};
    use wind_candidate::Candidate;
    use wind_config::{Config, PreeditDisplay};
    use wind_ipc::protocol::EVENT_KEY_DOWN;
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

    /// 把进程名登记进 `pid_names`，让 `active_process_name()` 查得到（compat 覆盖要用）。
    fn name_pid(c: &Coordinator, pid: u32, name: &str) {
        c.pid_names.lock().unwrap().insert(pid, name.to_string());
    }

    /// 装一份只含一条规则的 compat 表。
    fn compat_rule(c: &Coordinator, rule: wind_config::app_compat::AppCompatRule) {
        *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(vec![rule]);
    }

    /// ★★★ 默认不因「读走候选串」收窗：**普通应用一定要看得见候选框**。
    ///
    /// 这是 2026-09-15 的反转所钉的那一条。反证样本（同一台靶机、同一轮实测）：
    /// Notepad / Illustrator / EverEdit 都把候选串整串读走（`GetString(0..4)`），却都不画
    /// 候选窗 —— 读取是这些宿主的常态，不是自绘的标志。误判成「宿主在画」的代价是两个框
    /// 一个都没有、完全不能用，故默认值必须站在「照弹我们的窗」这一侧。
    ///
    /// 变异检验：把 `uielement_host_draws_by_inference` 末尾改回 `unwrap_or(true)` ⇒ 本条红。
    #[test]
    fn reading_our_candidates_does_not_by_itself_hide_the_window() {
        let (c, rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 42);
        name_pid(&c, 42, "notepad.exe"); // compat 里没有它的规则

        c.set_uielement_host_reads(42, true);
        assert!(!c.uielement_host_draws(), "它并没有声明接管——两张账不能混");
        // 前置对照：本条唯一的正向断言是 `None`，而「读取账根本没命中」同样得 `None`
        // ⇒ 不先钉住这一条，将来 set_uielement_host_reads 静默失效时本用例会继续绿。
        assert!(c.uielement_host_reads(), "前置：读取账必须真的命中");
        assert_eq!(
            c.ui_suppressed_by_host(),
            None,
            "没配规则的宿主读了候选串也照弹我们的窗"
        );
        let _ = drain(&rx);
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        assert!(drain(&rx).contains(&"update"), "普通应用必须看得见候选框");
    }

    /// 反过来：compat 显式写 `= true` 的宿主，读走候选串就收掉我们的窗。
    ///
    /// 2026-09-11 新枫之谷实测形态：CUAS 的 IMM32 桥替宿主读走候选、由宿主/DefWindowProc
    /// 画出旧版候选窗，屏幕上两个框。宿主画的那个贴着它自己的输入框，位置比我们靠 caret
    /// 猜的准（全屏游戏根本给不出 caret），所以该退的是我们。出厂 compat.toml 给
    /// MapleStory.exe 配的就是这一行 —— opt-in 化之后它是保住那份修复的唯一通路。
    #[test]
    fn a_host_opted_in_is_treated_as_drawing_them() {
        let (c, rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 42);
        name_pid(&c, 42, "maplestory.exe");
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: "MapleStory.exe".into(), // 大小写无关
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );

        c.set_uielement_host_reads(42, true);
        assert!(!c.uielement_host_draws(), "它并没有声明接管——两张账不能混");
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_reads"),
            "显式 opt-in 的宿主读走候选串即判定自绘"
        );
        let _ = drain(&rx);
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        let got = drain(&rx);
        assert!(
            got.contains(&"hide") && !got.contains(&"update"),
            "判定自绘后只发 Hide: {got:?}"
        );
    }

    fn key_event(vk: u32) -> KeyEventData {
        KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }
    }

    /// 敲一个键，返回它的**出口**动作。必须走 `handle_key_event_policed`——占位后处理
    /// 就挂在那个出口上，`handle_key_event` 一路看不到它。
    fn press(c: &Coordinator, vk: u32) -> KeyAction {
        c.handle_key_event_policed(&key_event(vk))
    }

    /// 敲一串字母，返回末键的出口动作。
    fn type_code(c: &Coordinator, code: &str) -> KeyAction {
        let mut last = KeyAction::Consumed;
        for ch in code.chars() {
            last = press(c, (ch.to_ascii_uppercase() as u32) & 0xFF);
        }
        last
    }

    /// 出口动作里的组合区文本。不是 `UpdateComposition` 就是构造出了问题，直接炸——
    /// 返回 `None` 再 `assert_ne!` 的话，形态变了会变成一条**永远成立**的断言。
    fn composition_of(action: &KeyAction) -> &str {
        match action {
            KeyAction::UpdateComposition { text, .. } => text.as_str(),
            other => panic!("期望组合区更新，实得 {other:?}"),
        }
    }

    /// ★★★ 候选窗被压住 ⇒ **强制嵌入编码**：非 app_inline 一律降级回 app_inline。
    ///
    /// # 修的是什么
    ///
    /// 非 app_inline 时真编码只走 `UiCommand::UpdateCandidates::preedit` 交给候选窗，
    /// 宿主组合区里换成占位空格（`with_composition_placeholder`）。而上面几条用例正说明
    /// 候选窗在压制态**根本不下发**（`notify_ui_update` 发完 `HideCandidates` 就 return），
    /// 交给宿主自绘的 `UiElementPage` 又不带编码串 —— 于是 UI-less 游戏里编码两条路全断、
    /// 一个字都看不见。占位存在的唯一理由是「别和候选窗的编码栏重复显示」，压制态下那条
    /// 理由本就不存在，占位是纯粹的信息丢失。
    ///
    /// # 护栏为什么落在按键出口上
    ///
    /// 判据函数返回什么不算数，**它被那个 if 读到**才算数：`preedit_uses_placeholder`
    /// 的唯一消费点是 `handle_key_event_policed` 出口那一步。故这里断言的是出口动作里的
    /// 组合区文本，不是判据的布尔值。
    ///
    /// # 反向对照不可省
    ///
    /// 开头那条「没有压制时照旧占位」是全组的对照：只测正向的话，判据整条恒 false
    /// （占位从此全局失效、连记事本都不占位了）也照样全绿。
    ///
    /// # ⚠️ 本条**不覆盖**生产上的首键
    ///
    /// 这里是「先压制、再打字」，而生产上的顺序常常是反的：`pbShow=FALSE` 在 `Show()` /
    /// `BeginUIElement` 才上报、`host_reads` 在宿主真读走候选串才上报，**两者都晚于第一次
    /// 候选出现**，也就是晚于首键的应答。所以 `candidate_top` + 这类宿主，本次激活的第一次
    /// 组合的**第一键**组合区仍是占位空格，第二键起才是真编码。不是数据损坏（组合整串替换，
    /// C++ 的去重比的是 text **和** caret，`" "/0` 与 `"ni"/2` 不相等，不会被跳过），是可
    /// 接受的一帧。以 `TF_TMAE_UIELEMENTENABLEDONLY` 激活的那一类宿主没有这一帧——它在
    /// `ActivateEx` 就知道，激活时即上报。
    ///
    /// 变异检验：删掉 `preedit_in_app_effective` 里那个 `ui_suppressed_by_host` 早退
    /// ⇒ 三条正向全红；把该函数改成无条件回 `false`（即恒占位）⇒ 开头的对照红。
    #[test]
    fn a_suppressed_host_gets_the_real_code_inline() {
        // 对照：同样 candidate_top、同样两键，只差没有任何压制来源。
        let (c, _rx) = coord();
        *c.preedit_display.lock().unwrap() = PreeditDisplay::CandidateTop;
        focus_pid(&c, 42);
        assert_eq!(
            c.ui_suppressed_by_host(),
            None,
            "前置：对照这一格必须真的没被压住"
        );
        assert_eq!(
            composition_of(&type_code(&c, "ni")),
            COMPOSITION_PLACEHOLDER,
            "没被压住时 candidate_top 照旧把编码换成占位——这是本组的鉴别力来源"
        );

        // 三个压制来源逐个走一遍：它们在「编码丢了」这件事上没有分别，判据收口在
        // `ui_suppressed_by_host`，故三条都得钉，漏一条就等于给那条留了退路。
        type Suppress = Box<dyn Fn(&Arc<Coordinator>)>;
        let sources: [(&str, Suppress); 3] = [
            (
                "宿主声明接管（pbShow=FALSE / UI-less 线程）",
                Box::new(|c: &Arc<Coordinator>| c.set_uielement_host_draws(42, true)),
            ),
            (
                "前台 D3D 独占全屏",
                Box::new(|c: &Arc<Coordinator>| {
                    c.fullscreen_exclusive_cached
                        .store(true, std::sync::atomic::Ordering::Relaxed)
                }),
            ),
            (
                "compat opt-in 的候选读取者",
                Box::new(|c: &Arc<Coordinator>| {
                    name_pid(c, 42, "maplestory.exe");
                    compat_rule(
                        c,
                        wind_config::app_compat::AppCompatRule {
                            process: "MapleStory.exe".into(),
                            host_drawn_candidates: Some(true),
                            ..Default::default()
                        },
                    );
                    c.set_uielement_host_reads(42, true);
                }),
            ),
        ];

        for (name, suppress) in sources {
            let (c, _rx) = coord();
            *c.preedit_display.lock().unwrap() = PreeditDisplay::CandidateTop;
            focus_pid(&c, 42);
            suppress(&c);
            assert!(
                c.ui_suppressed_by_host().is_some(),
                "前置：{name} 必须真的压住了候选窗，否则下面那条断言测的是别的东西"
            );
            assert_eq!(
                composition_of(&type_code(&c, "ni")),
                "ni",
                "{name}：候选窗被压住后编码必须原样写回宿主组合区"
            );
        }

        // 撤销压制即恢复用户配的显示方式：接管是**宿主**的属性，切回桌面应用不该还嵌着。
        let (c, _rx) = coord();
        *c.preedit_display.lock().unwrap() = PreeditDisplay::CandidateTop;
        focus_pid(&c, 42);
        c.set_uielement_host_draws(42, true);
        assert_eq!(
            composition_of(&type_code(&c, "ni")),
            "ni",
            "前置：接管态先拿到真编码"
        );
        c.set_uielement_host_draws(42, false);
        assert_eq!(
            composition_of(&press(&c, 'H' as u32)),
            COMPOSITION_PLACEHOLDER,
            "撤销接管后下一键就该回到占位——粘住的话用户切回记事本会看到编码显示两遍"
        );
    }

    /// ★ 顶码余码这条**出厂主路径**在压制态下也得把真编码交出去。
    ///
    /// `top_commit_mode` 出厂是 `direct_commit`（`data/config.toml`），于是压制态下用户在游戏
    /// 里打满码长的那一下，走的是 `CommitThenDeferComposition` 而不是上面两条钉的
    /// `UpdateComposition` —— 护栏此前**一条都没踩过这条路**。而
    /// `with_composition_placeholder` 确实也会改写这个变体的 `deferred_composition`
    /// （`wind-bridge/src/handler.rs`，那一支正是为「skce 顶码后快打 h」那次真机事故补的），
    /// 所以它与压制态的交叉点是真实可达的用户可见行为：漏掉的话，顶码之后那一截余码编码
    /// 在游戏里是隐形的。
    ///
    /// ⚠️ 本条**复述**了出口那一步（判据 + 改写函数），没有走 `handle_key_event_policed`：
    /// 真顶码要求输入超过方案码长上限，而 lib 单测一律无词库（本模块所有用例都是，带词库会
    /// 让它们在没有 `build_dev/data` 的 worktree 里静默跳过而计数照绿）。于是分工是——
    /// 「出口里那个 if 还在不在」由 [`a_suppressed_host_gets_the_real_code_inline`] 钉（它走
    /// 真出口），本条钉「同一条规则对顶码变体同样成立」。两条合起来才完整，删任何一条都留缺口。
    #[test]
    fn the_top_code_remainder_also_reaches_the_host() {
        // `suppress` 之外两侧完全同构，返回的是余码组合最终的模样。
        let run = |suppress: bool| -> String {
            let mut cfg = Config::default();
            cfg.ui.candidate.preedit_display = PreeditDisplay::CandidateTop.as_config().into();
            let (c, _rx) = Coordinator::new_headless_with_ui(cfg, None);
            focus_pid(&c, 42);
            if suppress {
                c.set_uielement_host_draws(42, true);
                assert!(
                    c.ui_suppressed_by_host().is_some(),
                    "前置：压制必须真的生效"
                );
            } else {
                assert_eq!(c.ui_suppressed_by_host(), None, "前置：对照那侧不该被压住");
            }
            let action = {
                let mut st = c.state.lock().unwrap();
                st.input_buffer = "h".into();
                st.input_buffer_cased = "h".into();
                st.preedit = "h".into();
                c.commit_top_text(
                    &mut st,
                    "aaaa",
                    "工".into(),
                    None,
                    "h",
                    wind_candidate::CandidateSource::CodeTable,
                )
            };
            // ↓ 这三行是 `handle_key_event_policed` 出口那一步的复述（见上面的 ⚠️）。
            let out = if c.preedit_uses_placeholder() {
                action.with_composition_placeholder()
            } else {
                action
            };
            match out {
                KeyAction::CommitThenDeferComposition {
                    commit_text,
                    deferred_composition,
                    ..
                } => {
                    assert_eq!(
                        commit_text, "工",
                        "顶出的正文是已承诺上屏的字，两侧都不许被改写"
                    );
                    deferred_composition
                }
                other => {
                    panic!("顶码 direct_commit 该产出 CommitThenDeferComposition，实得 {other:?}")
                }
            }
        };

        assert_eq!(
            run(false),
            COMPOSITION_PLACEHOLDER,
            "没被压住时余码照旧换占位——编码归候选窗画，这是本条的鉴别力来源"
        );
        assert_eq!(
            run(true),
            "h",
            "压住候选窗后余码必须原样交给宿主，否则顶码后那一截编码在游戏里是隐形的"
        );
    }

    /// ⛔ **用户自己关掉候选窗**（`ui.candidate.hide_window`）不在强制嵌入之列。
    ///
    /// 在「信息丢失」这个维度上它与压制态**完全同构**：`notify_ui_update` 里那两条早退
    /// （用户开关那条、`ui_suppressed_by_host` 那条）动作逐字一样——`clear_hover` +
    /// `HideCandidates` + `reset_first_show` + `return`，`UpdateCandidates` 同样不下发，
    /// 非 app_inline 时组合区同样只剩一个空格。所以这个排除**靠的不是「后果不同」**，
    /// 而是用户表达了几次意图：压制态下他只做过一次选择（编码放候选窗顶部），是环境把它
    /// 推翻的，他从没同意过「编码可以看不见」；而 `hide_window` 是第二次显式选择，
    /// 「关掉候选窗 + 编码归候选窗」这个组合本身就定义了盲打语境——那里「编码也看不见」
    /// 不是丢失，是这个模式的定义。
    ///
    /// 本条守的就是这个边界：谁将来「顺手补全」把 `hide_candidate_window` 也并进
    /// `ui_suppressed_by_host`，这里立刻红。对照 `wind-bridge` 那边
    /// `placeholder_keeps_literal_symbol_compositions` 守 `with_composition_placeholder` 的
    /// 变体边界——同一个道理，这条边界此前没人守。
    #[test]
    fn the_user_hiding_the_window_keeps_the_placeholder() {
        let mut cfg = Config::default();
        cfg.ui.candidate.hide_window = true;
        cfg.ui.candidate.preedit_display = PreeditDisplay::CandidateTop.as_config().into();
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        focus_pid(&c, 42);

        // 前置①：候选窗确实被用户关掉了（走可观测路径确认，不去读私有开关字段）。
        fill(&c, 5);
        let _ = drain(&rx);
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        let got = drain(&rx);
        assert!(
            got.contains(&"hide") && !got.contains(&"update"),
            "前置：用户关窗后候选窗不下发，本条才与压制态同构: {got:?}"
        );
        // 前置②：这不是宿主压制。两条前置缺一，下面那条断言就在测别的东西。
        assert_eq!(
            c.ui_suppressed_by_host(),
            None,
            "前置：用户开关不该被算成宿主压制"
        );

        // 清掉 fill 摆的局，让下面两键从空缓冲开始。
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer.clear();
            st.candidates.clear();
        }
        assert_eq!(
            composition_of(&type_code(&c, "ni")),
            COMPOSITION_PLACEHOLDER,
            "用户自己关的窗照旧占位——盲打语境下看不见编码是这个模式的定义，不是缺陷"
        );
    }

    /// 摆一个混输「同一串码两种编码形态」的局面：高亮在候选 0（拼音来源）时编码显示音节拆分
    /// `sa'a'a`，移到候选 1（码表来源）时显示原始码 `saaa`。
    ///
    /// 形态由 `effective_preedit_body` 纯读 `state` 算出（候选来源 + 三个 body 字段），**不碰
    /// 引擎也不碰词库**，所以这里手工摆盘是合法的，不是在绕过什么。
    fn fill_split_forms(c: &Coordinator) {
        use wind_candidate::CandidateSource;
        let mut st = c.state.lock().unwrap();
        st.active = None;
        st.input_buffer = "saaa".into();
        st.input_buffer_cased = "saaa".into();
        st.preedit_split_body = "sa'a'a".into();
        st.committed_text.clear();
        st.candidates = vec![
            Candidate {
                text: "萨阿阿".into(),
                source: CandidateSource::Pinyin,
                ..Default::default()
            },
            Candidate {
                text: "模式".into(),
                source: CandidateSource::CodeTable,
                ..Default::default()
            },
        ];
        st.current_page = 0;
        st.selected_index = 0;
        st.caret_x = 100;
        st.caret_y = 200;
        st.caret_height = 20;
        // 先落到「拆分形态」这一侧。不做这一步的话 `before` 是空串、形态变化恒成立，
        // 用例就测不出「变化才回传」那半个条件了。
        c.sync_preedit_to_highlight(&mut st);
        assert_eq!(st.preedit, "sa'a'a", "前提：高亮在拼音候选上时是拆分形态");
    }

    /// ★★★ 压制态下**高亮跟随**也得把编码回传宿主——抽访问器那一步真正修掉的就是这条。
    ///
    /// 混输方案下 ↑↓ 在五笔↔拼音候选间移动会切换编码形态（原始码 `saaa` ↔ 音节拆分
    /// `sa'a'a`）。`apply_session_action` 里那条回传 `UpdateComposition` 的分支此前**只读配置
    /// 原值**：压制态 + `candidate_top` 下判成「编码归候选窗画」⇒ 不回传，而候选窗在压制态又
    /// 根本不下发 ⇒ 游戏聊天框里的编码停在旧形态。换成 `preedit_in_app_effective()` 才跟上。
    ///
    /// ⚠ 这是**上一版改动造出来的**可见性，不是老 bug：改之前那一格组合区里恒是占位空格，
    /// 形态对不对都看不见。压制态一旦开始往组合区写真编码，所有写入点就都得跟着这条规则走
    /// ——这正是那个访问器存在的理由。
    ///
    /// 反向对照是同一副牌、只把压制撤掉：那时**不回传才是对的**（编码本就归候选窗画）。
    ///
    /// 变异检验：把 `apply_session_action` 里的 `preedit_in_app_effective()` 改回
    /// `preedit_display.lock()...in_app()` ⇒ 正向红、反向仍绿。
    #[test]
    fn highlight_follow_reaches_the_host_when_suppressed() {
        // ⚠️ 必须走 `press`（= 生产出口 `handle_key_event_policed`），不能图省事直接调
        // `apply_session_action`：出口那步还会做 `with_composition_placeholder`，而本条与
        // `a_suppressed_host_gets_the_real_code_inline` 守的是同一条规则的两半。绕过出口的话，
        // 哪天有人动了 `preedit_uses_placeholder` 的判据，回传的 `"saaa"` 会在出口被拍成占位
        // 空格，而这条测试照绿。
        let down = |c: &Arc<Coordinator>| press(c, wind_keys::keymap::VK_DOWN);

        // 反向对照：candidate_top、没有压制 ⇒ 编码归候选窗，不该回传组合串。
        let (c, _rx) = coord();
        *c.preedit_display.lock().unwrap() = PreeditDisplay::CandidateTop;
        focus_pid(&c, 42);
        fill_split_forms(&c);
        assert!(
            matches!(down(&c), KeyAction::Consumed),
            "没被压住时高亮移动只吞键、不回传组合串，编码由候选窗自己画——这是本条的鉴别力来源"
        );
        assert_eq!(
            c.state.lock().unwrap().selected_index,
            1,
            "前置：↓ 真的把高亮挪到了码表候选上（不然下面测的是导航坏了还是回传坏了分不清）"
        );

        // 正向：压住之后，形态一变就得把新编码写回宿主组合区。
        let (c, _rx) = coord();
        *c.preedit_display.lock().unwrap() = PreeditDisplay::CandidateTop;
        focus_pid(&c, 42);
        c.set_uielement_host_draws(42, true);
        fill_split_forms(&c);
        match down(&c) {
            KeyAction::UpdateComposition { text, caret_pos } => {
                assert_eq!(
                    text, "saaa",
                    "回传的必须是**新**形态（原始码），不是旧的拆分串"
                );
                assert_eq!(caret_pos, 4, "光标落在新编码末尾");
            }
            other => panic!("压住候选窗后，高亮移到码表候选必须把编码回传宿主，实得 {other:?}"),
        }
    }

    /// `host_drawn_candidates` 只管**推断**那条，管不着宿主的**声明**。
    ///
    /// 本条的鉴别力**全在后半段**：宿主一旦声明接管（`pbShow=FALSE`），无论本字段写什么
    /// 都得收 —— 声明是事实，不是推断，不该被一个推断开关否决。
    ///
    /// ⚠ 前半段（`Some(false)` ⇒ 照弹）**在本用例的构造下**鉴别力很弱：这里没有出厂层、
    /// 没有通配规则，`false` 与不写恰好同结果，那个 `None` 断言在 `get_rule` 整个失效时
    /// 照样成立。（`false` 与不写在一般情况下**并不同义** —— 它挡住出厂层继承、也压过
    /// `"*"` 通配，见 `AppCompatRule::host_drawn_candidates` 的字段文档；压过通配那条由
    /// `a_wildcard_rule_applies_everywhere_and_a_process_rule_wins` 钉着。）
    /// 真正钉 `= true` 生效的是
    /// `a_host_opted_in_is_treated_as_drawing_them`，钉查名落点的是
    /// `the_compat_rule_follows_the_key_source_pid`。
    #[test]
    fn compat_governs_the_inference_but_not_the_declaration() {
        let (c, rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 42);
        name_pid(&c, 42, "maplestory.exe");
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: "MapleStory.exe".into(), // 大小写无关
                host_drawn_candidates: Some(false),
                ..Default::default()
            },
        );

        c.set_uielement_host_reads(42, true);
        assert_eq!(c.ui_suppressed_by_host(), None, "显式 false ⇒ 照弹我们的窗");
        let _ = drain(&rx);
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        assert!(drain(&rx).contains(&"update"), "不启用推断时候选窗必须在");

        // 但宿主**声明**接管时，本开关管不着：宿主明说了不要我们的 UI。
        c.set_uielement_host_draws(42, true);
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_draws"),
            "声明是事实，不受 host_drawn_candidates 影响"
        );
    }

    /// ★★★ compat 规则必须落在「靠按键来源才命中的宿主」上——而那正是游戏宿主的常态。
    ///
    /// `focus_pid`（按键来源）与 `active_compat.pid`（焦点事件）在游戏上会分岔：游戏常常
    /// 没有可编辑 TSF 上下文、`focus_gained` 一次都不来，`active_compat` 停在上一个进程
    /// （既有测试 `key_source_pid_alone_matches_host_draws` 钉的就是这个）。
    /// 查覆盖时若另取一次 `active_compat.pid`，查到的是**上一个进程**的名字，游戏那条
    /// opt-in 规则静默失效 —— 新枫之谷又变回两个候选框。
    ///
    /// ⚠ 用 `Some(true)` 而不是 `Some(false)` 来钉：opt-in 化之后 `false` 与不写同义，
    /// 断言 `None` 在「查名落到了 explorer.exe」时**照样成立**，测试会绿着失去鉴别力。
    ///
    /// 变异检验：把 `uielement_host_draws_by_inference` 的查名换成 `active_process_name()`
    /// ⇒ 本条立刻红。（⛔ 调换 `current_pid_in` 两支的先后**不会**让本条红——这里读取账里
    /// 只有 42 一个 pid，`active_compat.pid=1` 压根不在集合里，两种顺序都落到 42。钉分支
    /// 先后的是 `the_key_source_pid_wins_when_both_pids_are_readers`，那条两个 pid 都在账里。）
    #[test]
    fn the_compat_rule_follows_the_key_source_pid() {
        let (c, _rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 1); // 焦点事件停在上一个进程
        name_pid(&c, 1, "explorer.exe");
        c.focus_pid.store(42, std::sync::atomic::Ordering::Relaxed); // 按键来源才是游戏
        name_pid(&c, 42, "maplestory.exe");
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: "MapleStory.exe".into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );

        c.set_uielement_host_reads(42, true);
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_reads"),
            "规则必须落到真正在输入的那个进程上，不能另取一次 active_compat.pid"
        );
    }

    /// 两个 pid 都命中且进程名不同时，用的必须是 `focus_pid`（按键来源）那一个。
    ///
    /// 这条顺序原本无关紧要（`current_pid_in` 只回 bool），H1 修复把它变成了**载荷**——
    /// 它现在决定拿谁的名字去查 compat。没有测试钉住的话，有人调换两个分支的先后
    /// 不会有任何一条变红，而后果是逃生口落到错误的进程上。
    /// 变异检验：把 `active_compat.pid` 那一支提到前面 ⇒ 本条立刻红。
    #[test]
    fn the_key_source_pid_wins_when_both_pids_are_readers() {
        let (c, _rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 1); // active_compat.pid = 1
        name_pid(&c, 1, "explorer.exe");
        c.focus_pid.store(42, std::sync::atomic::Ordering::Relaxed);
        name_pid(&c, 42, "maplestory.exe");
        c.set_uielement_host_reads(1, true); // 两个都在读取账里
        c.set_uielement_host_reads(42, true);
        // 只给按键来源那个 opt-in：生效 ⇒ 说明查名用的是它。
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: "maplestory.exe".into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );
        assert!(c.uielement_host_reads(), "前置：读取账必须真的命中");
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_reads"),
            "两个 pid 都命中时应取 focus_pid（按键来源）去查覆盖"
        );

        // 反向对照：把规则改挂到 active_compat.pid 那个名字上就**不该**生效——
        // 一正一反锁死方向，只有「查名用的是 focus_pid」能同时满足两条。
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: "explorer.exe".into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            c.ui_suppressed_by_host(),
            None,
            "规则挂在 active_compat.pid 的名字上不该生效"
        );
    }

    /// 通配规则 `process = "*"` 一行套到全局，而本进程自己的规则**优先于**通配。
    ///
    /// ⚠ 两半都用「与默认相反」的那个值来钉：opt-in 之后默认是不收窗，若第一半仍写
    /// `Some(false)` 断言 `None`，那条通配就算完全没被读到也照样绿。
    #[test]
    fn a_wildcard_rule_applies_everywhere_and_a_process_rule_wins() {
        let (c, _rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 42);
        name_pid(&c, 42, "some-unknown-host.exe");
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: crate::handle_uielement::HOST_DRAWN_WILDCARD.into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );
        c.set_uielement_host_reads(42, true);
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_reads"),
            "通配规则应套到没有自己规则的宿主上"
        );

        // 本进程自己的规则优先于通配：通配开、本进程显式关 ⇒ 照弹我们的窗。
        *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(vec![
            wind_config::app_compat::AppCompatRule {
                process: crate::handle_uielement::HOST_DRAWN_WILDCARD.into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
            wind_config::app_compat::AppCompatRule {
                process: "some-unknown-host.exe".into(),
                host_drawn_candidates: Some(false),
                ..Default::default()
            },
        ]);
        assert_eq!(c.ui_suppressed_by_host(), None, "本进程规则应压过通配");
    }

    /// 查不到进程名时按默认（**不**收窗）走，而通配规则**仍然管用**——受限/短命宿主
    /// 查不出名字，但「所有应用都收窗」这类需求仍要能一行套到它们头上。
    #[test]
    fn an_unnamed_process_still_honours_the_wildcard_rule() {
        let (c, _rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 42); // 不登记 pid_names
        c.set_uielement_host_reads(42, true);
        assert_eq!(
            c.ui_suppressed_by_host(),
            None,
            "查不到名字 ⇒ 查不到规则 ⇒ 按 opt-in 的默认照弹我们的窗"
        );
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: crate::handle_uielement::HOST_DRAWN_WILDCARD.into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_reads"),
            "通配对无名进程也要生效"
        );
    }

    /// 「声明接管」→「只是读过」的过渡不得把候选窗闪出来一帧。
    ///
    /// DLL 报的是一整份 flags，两张账必须写完再统一刷 UI；拆成两次带副作用的写入时，
    /// 声明账先被清掉的那一瞬两张账都不命中，`notify_ui_update` 会发一帧 `UpdateCandidates`。
    #[test]
    fn the_draws_to_reads_transition_does_not_flash_the_window() {
        let (c, rx) = coord();
        fill(&c, 5);
        focus_pid(&c, 42);
        name_pid(&c, 42, "maplestory.exe");
        // 本条钉的是 opt-in 宿主（新枫之谷那类）身上的过渡不变量，故先把规则配上：
        // 没有它，撤销声明后本就该把窗弹回来，「过渡不闪窗」这件事无从谈起。
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: "maplestory.exe".into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );
        // ⚠ 必须从「只有声明账」起步：若先报过一次 draws+reads，读取账里已经有这个 pid，
        // 拆开写也不会露出空窗，测试就成了永远绿的。可达路径是中间那次 0x5 上报丢了
        // （SendAsync 失败会留着 _uiElementStateSent 下次重报），core 直接收到 0x4。
        c.apply_uielement_state(42, true, false); // 只声明接管
        let _ = drain(&rx);
        c.apply_uielement_state(42, false, true); // 一步转成「只是读过」
        let got = drain(&rx);
        assert!(
            !got.contains(&"update"),
            "过渡期间不得弹出候选窗（哪怕只有一帧）: {got:?}"
        );
        assert_eq!(
            c.ui_suppressed_by_host(),
            Some("uielement_host_reads"),
            "撤销声明后仍由推断接手收窗"
        );
    }

    /// 读取账按 pid 记，只对「当前在输入的进程」生效——游戏读过，切到记事本仍要弹。
    #[test]
    fn the_reader_account_only_applies_to_the_process_being_typed_in() {
        let (c, _rx) = coord();
        fill(&c, 3);
        focus_pid(&c, 42);
        name_pid(&c, 42, "maplestory.exe");
        // opt-in 之后推断要靠规则才生效；不配这一行，下面两个断言都会是 None，
        // 「未命中 7」那条就分不清是作用域对了还是推断整个没开。
        compat_rule(
            &c,
            wind_config::app_compat::AppCompatRule {
                process: "maplestory.exe".into(),
                host_drawn_candidates: Some(true),
                ..Default::default()
            },
        );
        c.set_uielement_host_reads(7, true); // 别的进程读过
        assert_eq!(c.ui_suppressed_by_host(), None, "焦点在 42，未命中 7");
        focus_pid(&c, 7);
        name_pid(&c, 7, "maplestory.exe");
        assert_eq!(c.ui_suppressed_by_host(), Some("uielement_host_reads"));
    }

    /// 进程退出清账必须把**两张**账一起清：读取账留着，pid 复用后新宿主会一上来就被
    /// 判成自绘，而它自己的 GetString 不一定会发生，没人来纠正。
    #[test]
    fn clearing_a_pid_clears_the_reader_account_too() {
        let (c, _rx) = coord();
        fill(&c, 3);
        focus_pid(&c, 42);
        name_pid(&c, 42, "maplestory.exe");
        c.set_uielement_host_draws(42, true);
        c.set_uielement_host_reads(42, true);
        c.clear_uielement_host_pid(42);
        assert!(!c.uielement_host_draws(), "声明账未清");
        assert!(
            !c.uielement_host_reads(),
            "读取账未清——pid 复用后会误判新宿主"
        );
        assert_eq!(c.ui_suppressed_by_host(), None);
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
