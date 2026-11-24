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
    /// `process = "*"` 通配规则由 `AppCompat::get_rule` 统一合成（本进程规则逐字段叠在通配之上；
    /// 查不到进程名时 `get_rule` 不吃通配，这里显式取通配那条）。通配是给反方向用的——某类宿主普遍需要收窗时可一行开到
    /// 全局；opt-in 之后它不再是「故障半径的配套」，因为默认已经不会出现「所有应用候选框都没了」。
    ///
    /// 查不到进程名时按默认（**不**收窗）走：查不到名字就等于查不到规则，而 opt-in 的前提
    /// 是「有人明确说过这个宿主要收」——查不出是谁，就谈不上它被指名过。
    pub(crate) fn uielement_host_draws_by_inference(&self) -> bool {
        let Some(pid) = self.uielement_reader_pid() else {
            return false;
        };
        // 命中的就是焦点应用：读焦点变化 / 规则重载时预提取好的值（`ActiveLookups`），不查表。
        //
        // 窗口来源与下面现查那支不同：预提取按 active 槽的窗口（焦点实例这一刻的窗口），现查按
        // `focus_window_of_pid`（该 pid 名下最近一次 focus_gained 的窗口）。焦点实例刚获焦时
        // 两者相同；只在同一进程的旧实例只发 ime_activated、没再 focus_gained 时可能不同，那时
        // active 槽才是正在输入的那个窗口。PID 复用时 `revalidate_pid_name` 把两边一起清空。
        let prefetched = {
            let ac = self.active_compat.lock().unwrap_or_else(|e| e.into_inner());
            (ac.pid == pid).then_some(ac.lookups.host_drawn_candidates)
        };
        let rule = prefetched.unwrap_or_else(|| {
            let name = self.cached_proc_name((pid as u64) << 32);
            // `get_rule("")` 不吃通配（空名口径）；本判据历来对无名进程也认通配
            // （`an_unnamed_process_still_honours_the_wildcard_rule`），故显式取通配那条。
            let key = if name.is_empty() {
                wind_config::app_compat::ANY_PROCESS
            } else {
                name.as_str()
            };
            // 窗口取该 pid 最近一次获焦的那个：游戏常常一次 focus_gained 都没有 ⇒ 空窗口 ⇒
            // 只按进程名（窗口规则不命中），与引入窗口条件之前一致。
            let win = self.focus_window_of_pid(pid);
            self.with_compat_rule(key, &win, |r| r.and_then(|r| r.host_drawn_candidates))
        });
        rule
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
        let pp = self.page_size(&state).max(1);
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
                .map(|c| self.cand_display_text(&state, c))
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

    /// 把高亮移到当页**页内**下标（0..pageSize）；越界或当页无此条则不动。返回是否变化。
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
