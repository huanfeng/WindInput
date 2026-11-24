//! 候选窗首显闸门：延迟首显判定与释放 + 共享兜底 timer。
//!
//! coordinator 的子模块（非平级）：首显族重度读写父模块私有字段
//!（pending_first_show / candidate_shown / caret_cache_verified 等），做成子模块才不必放开可见性。
//! 宿主行为背景见 `docs/architecture/caret-position-host-compat-notes.md`。

use super::*;

impl Coordinator {
    /// 候选窗已在显示时，解开组合起点的两把锁（`composition_start` 与 `locked_rect_anchor`），
    /// 不动显示状态（`shown_anchor` / `caret_baseline` 保留）。
    ///
    /// 用于联想接龙：候选窗全程不隐藏、`reset_first_show` 不会被调到，而宿主每次上屏后新建的
    /// 占位组合起点已右移；不解锁则候选窗钉在第一次的位置。解锁后下一帧重新锁定并 reshow，
    /// 其间候选窗留在原处（`hold_anchor`）。
    ///
    /// 窗口没显示时不解：那是首显流程，锁由 `reset_first_show` 管，本帧的组合起点可能正是
    /// 首显要用的坐标。
    pub(crate) fn unpin_composition_anchor_if_shown(&self) {
        if !*self
            .candidate_shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            return;
        }
        *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        *self
            .locked_rect_anchor
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
    }

    /// 两把组合起点锁是否都还锁着（仅测试用，另一半见 `debug_mark_coords_ready`）。
    #[cfg(test)]
    pub(crate) fn debug_anchor_locks(&self) -> (bool, bool) {
        (
            self.composition_start
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .2,
            self.locked_rect_anchor
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .2,
        )
    }

    /// 把两把锁都置成锁着，并标记候选窗已在显示（仅测试用）。
    #[cfg(test)]
    pub(crate) fn debug_lock_anchors(&self, shown: bool) {
        *self
            .candidate_shown
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = shown;
        *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (100, 200, true);
        *self
            .locked_rect_anchor
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (100, 200, true);
    }

    /// 复位首显延迟状态（候选窗隐藏 / 组合结束）：下次新组合重新延迟首显，并作废未触发的兜底 timer。
    pub(crate) fn reset_first_show(&self) {
        // 这里刻意不清 `caret_cache_verified`：上屏后缓存虽陈旧，但清掉会让下一组合走长兜底，
        // 而长按时组合寿命短于兜底超时，兜底被本函数反复作废、候选窗永远停在原地。接受上屏后
        // 一次性的小幅抖动。见 docs/architecture/caret-position-host-compat-notes.md 的「已否定的方向」。
        self.first_show_was_provisional
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.first_show_extended
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.first_show_after_commit
            .store(false, std::sync::atomic::Ordering::Relaxed);
        *self
            .candidate_shown
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = false;
        *self
            .pending_first_show
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = false;
        let mut t = self
            .pending_first_show_token
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *t = t.wrapping_add(1);
        drop(t);
        // 组合结束：复位组合起点锚定，下一组合重新锁定首个有效 compStart。
        self.tsf_frame_seen_in_composition
            .store(false, std::sync::atomic::Ordering::Relaxed);
        *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        // 矩形那条路是另一把锁（见字段注释），漏清会让候选窗钉在上一组合的位置。
        *self
            .locked_rect_anchor
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        // pre_reflow 基准只描述本轮组合，留着会让下一轮 probe 拿错基准比较。
        *self
            .last_pre_reflow_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        *self
            .pre_reflow_comp_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        // 显示锚点同理，不清会让下一轮的非坐标重绘复用上一轮的位置。
        *self.shown_anchor.lock().unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
        *self
            .caret_baseline
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
    }

    /// 推迟首次显示候选窗：标记 pending 并启动兜底 timer。token 比对使后续按键的 arm 自动作废
    /// 旧 timer。handle_caret_pending 握手会把 wait 档延到 600ms（应对 OnLayoutChange burst 慢的应用）。
    pub(super) fn arm_pending_first_show(&self) {
        // 首帧信任门：`fast` 的短兜底以「缓存坐标 ≈ 当前插入点」为前提，焦点刚到达或光标刚移动
        // 时不成立（见 `caret_cache_verified`），此时改走长兜底等权威坐标。
        //
        // 长等待一旦开始就不因后续按键重置：首显前每个字母都会调到这里，若照常 bump token，
        // 多打几个字母就把等待反复推后（Excel 建编辑上下文要半秒多）。长等待到期后也不再续，
        // 用旧坐标首显仍优于不出现。
        if self.first_show_needs_long_wait() {
            let already_waiting = *self
                .pending_first_show
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                && self
                    .first_show_extended
                    .load(std::sync::atomic::Ordering::Relaxed);
            if already_waiting {
                debug!("first_show 闸门 → 保持长兜底计时（坐标缓存仍未验证，不因本次按键重置）");
                return;
            }
            debug!(
                "first_show 闸门 → 坐标缓存未经当前插入点验证，改 arm {FIRST_SHOW_LONG_FALLBACK_MS}ms 长兜底"
            );
            self.first_show_extended
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.arm_pending_first_show_with_timeout(FIRST_SHOW_LONG_FALLBACK_MS);
            return;
        }
        self.arm_pending_first_show_with_timeout(self.first_show_fallback_ms());
    }

    /// 当前焦点应用实际生效的首显档位：per-app 规则优先，未配则回落全局
    /// `ui.candidate.first_show_mode`（认不出的值回落枚举默认档）。
    ///
    /// 各首显通路都须经此函数取档位，判据分散会让某条路静默按旧档位走。
    /// 先取 `active_compat` 并释放锁再读配置：`rt()` 内部另有锁，嵌套会引入新的持有序。
    pub(crate) fn effective_first_show_mode(&self) -> wind_config::app_compat::FirstShowMode {
        let per_app = self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first_show_mode;
        per_app.unwrap_or_else(|| {
            wind_config::app_compat::FirstShowMode::from_config(
                &self.rt().config.ui.candidate.first_show_mode,
            )
            .unwrap_or_default()
        })
    }

    /// 把一帧**不用于显示决策**的试探坐标收进缓存，返回是否真的收下。
    ///
    /// 缓存（`state.caret_x/y`）是兜底首显的坐标来源（`composition_start` 每次上屏都被清掉）。
    /// 连续快速上屏时可能一条权威 caret_update 都不来（C 类宿主），试探坐标虽是 reflow 前的，
    /// 也远好过陈旧缓存。
    ///
    /// 两类不收：退化帧（`h<=0`）；配了 `stale_probe_guard` 的宿主（组合期 rect 可能停在上一次
    /// 组合的位置）。
    pub(super) fn absorb_probe_coords(&self, data: &CaretData) -> bool {
        if data.height <= 0 {
            return false;
        }
        if self
            .active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stale_probe_guard
        {
            return false;
        }
        // 与 handle_caret_update 同口径：state 里存的是变换后的值
        let mut probe = *data;
        self.apply_caret_compat(&mut probe);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.caret_x = probe.x;
        state.caret_y = probe.y;
        state.caret_height = probe.height;
        state.caret_source = probe.source;
        // 缓存不再是组合前的空闲上报；不清位会让 idle_anchor 逃生口把 probe 坐标当按键前光标
        // 用，并锁成组合起点。
        self.caret_cache_is_idle_report
            .store(false, std::sync::atomic::Ordering::Relaxed);
        true
    }

    pub(super) fn first_show_mode_is_fast(&self) -> bool {
        self.effective_first_show_mode() == wind_config::app_compat::FirstShowMode::Fast
    }

    /// 首帧信任门是否命中：`fast` 档且坐标缓存未经当前插入点验证。
    pub(super) fn first_show_needs_long_wait(&self) -> bool {
        self.first_show_mode_is_fast()
            && !self
                .caret_cache_verified
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 该宿主的组合期试探坐标（probe）不可信、整条不收（`stale_probe_guard`）。
    ///
    /// 它同时是「能否拿组合前的空闲上报抢跑首显」的判据：probe 可信的宿主应等 probe（它是本次
    /// 组合现测的，空闲上报可能滞后一拍）；不可信的宿主空闲上报是唯一可信来源，抢跑无损失。
    /// 判据是结构性的（按宿主），不是逐帧的位置启发式，见
    /// docs/architecture/caret-position-host-compat-notes.md 的「已否定的方向」。
    pub(super) fn probe_is_untrusted(&self) -> bool {
        self.active_compat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stale_probe_guard
    }

    /// 缓存里装的是本次组合开始前、宿主经 TSF 主动上报的空闲光标位置，可直接当首显锚点。
    ///
    /// 首显闸门的第三个逃生口，补 `coords_ready` 在连打时的死角：组合起点每次上屏被清掉，
    /// 重新锁定要等的权威 caret_update 比 fast 兜底慢。兜底到期用的本就是这份缓存，等也白等。
    ///
    /// 两个标志缺一不可：`caret_cache_is_idle_report` 答缓存的出身（组合前空闲上报，而非 probe
    /// 或焦点事件携带的坐标）；`caret_cache_verified` 答它够不够格直接定位（限 TSF 通道、未被
    /// 切窗口 / 点击作废）。
    pub(super) fn caret_cache_is_fresh_idle_report(&self) -> bool {
        self.caret_cache_is_idle_report
            .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .caret_cache_verified
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 本次 arm 实际会用的超时值，供首显闸门打日志，保证日志与行为同源。
    pub(super) fn planned_first_show_timeout_ms(&self) -> u64 {
        if self.first_show_needs_long_wait() {
            FIRST_SHOW_LONG_FALLBACK_MS
        } else {
            self.first_show_fallback_ms()
        }
    }

    /// 本档位等不到坐标时的兜底超时。fast 档取远小于 wait 的值，理由见
    /// `fast_first_show_fallback_ms` 的字段注释（150ms 会让 fast 在 Word/记事本上退化成 wait）。
    pub(super) fn first_show_fallback_ms(&self) -> u64 {
        if self.first_show_mode_is_fast() {
            self.rt().config.ui.candidate.fast_first_show_fallback_ms
        } else {
            150
        }
    }

    pub(super) fn arm_pending_first_show_with_timeout(&self, ms: u64) {
        *self
            .pending_first_show
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = true;
        let token = {
            let mut t = self
                .pending_first_show_token
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *t = t.wrapping_add(1);
            *t
        };
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        first_show_timer().arm(
            std::time::Instant::now() + std::time::Duration::from_millis(ms),
            token,
            weak,
        );
    }

    /// 兜底 timer 到期回调。由共享定时器线程调用。
    pub(super) fn fire_pending_first_show(&self, token: u64) {
        // token/pending 校验：被新按键的 arm 取代、或已被首显/隐藏消费 → 放弃本次兜底。
        {
            let pending = *self
                .pending_first_show
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let tok = *self
                .pending_first_show_token
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !pending || tok != token {
                return;
            }
        }
        *self
            .pending_first_show
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = false;
        // 兜底超时：reflow 坐标迟迟未到，用当前 state 强制首显（坐标可能为按键前旧值，
        // 属慢应用降级，仍优于候选窗一直不显示）。
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let has_content = !state.candidates.is_empty()
            || !state.input_buffer.is_empty()
            || self.mode_indicator_text(&state).is_some();
        if has_content {
            // 用的是旧坐标，按「非权威」记账，随后的权威坐标才享放宽容差、不跳一下；
            // 没真显示就不记。上屏刚前移过插入点时旧坐标确定陈旧，不记（见字段注释）。
            let after_commit = self
                .first_show_after_commit
                .swap(false, std::sync::atomic::Ordering::Relaxed);
            self.first_show_was_provisional
                .store(!after_commit, std::sync::atomic::Ordering::Relaxed);
            self.show_authorized
                .store(true, std::sync::atomic::Ordering::Relaxed);
            debug!("first_show 兜底 timer 到期 → 用现有坐标首显（非权威，享放宽容差）");
            self.notify_ui_update(&state);
        }
    }
}

// ———————————————— 首显兜底 timer（进程内共享单线程）————————————————

/// 一条待办：`(到期时刻, token, 协调器弱引用)`。
type TimerEntry = (std::time::Instant, u64, std::sync::Weak<Coordinator>);

/// 覆盖式定时器：每个协调器只保留最近一次 arm 的待办，单线程服务。首显兜底与焦点气泡的锚点
/// 超时（`status_placement`）各用一个实例，互不顶替。
///
/// 按协调器分槽（`Weak::ptr_eq`）：同进程多个协调器（并行测试）共用一个槽会互相顶掉待办。
/// 本线程只做「等到点 + 回调」，不得执行可能阻塞的调用，否则会拖垮兜底时限。
struct OneShotTimer {
    /// 各协调器的待办（每个协调器至多一条）；空 = 空闲。
    pending: Mutex<Vec<TimerEntry>>,
    cv: std::sync::Condvar,
    /// 到期回调（token 校验由回调自己做）。
    fire: fn(&Coordinator, u64),
}

static FIRST_SHOW_TIMER: std::sync::OnceLock<Arc<OneShotTimer>> = std::sync::OnceLock::new();
static FOCUS_TIP_TIMER: std::sync::OnceLock<Arc<OneShotTimer>> = std::sync::OnceLock::new();

/// 懒启动一个定时器线程。
fn spawn_timer(name: &str, fire: fn(&Coordinator, u64)) -> Arc<OneShotTimer> {
    let timer = Arc::new(OneShotTimer {
        pending: Mutex::new(Vec::new()),
        cv: std::sync::Condvar::new(),
        fire,
    });
    let worker = timer.clone();
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || worker.run());
    timer
}

/// 取首显兜底的共享定时器，首次调用时懒启动其线程。
fn first_show_timer() -> &'static Arc<OneShotTimer> {
    FIRST_SHOW_TIMER
        .get_or_init(|| spawn_timer("first-show-timer", Coordinator::fire_pending_first_show))
}

/// 焦点气泡锚点超时的登记入口（`status_placement` 用）：到期回调
/// [`Coordinator::fire_focus_tip_timeout`]。
pub(super) fn arm_focus_tip_timer(
    deadline: std::time::Instant,
    token: u64,
    coord: std::sync::Weak<Coordinator>,
) {
    FOCUS_TIP_TIMER
        .get_or_init(|| spawn_timer("focus-tip-timer", Coordinator::fire_focus_tip_timeout))
        .arm(deadline, token, coord);
}

impl OneShotTimer {
    /// 覆盖式登记：同一协调器新的 arm 顶掉它旧的那条，别的协调器的待办不受影响。
    fn arm(&self, deadline: std::time::Instant, token: u64, coord: std::sync::Weak<Coordinator>) {
        let mut g = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|(_, _, w)| !w.ptr_eq(&coord));
        g.push((deadline, token, coord));
        drop(g);
        self.cv.notify_one();
    }

    fn run(&self) {
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            // 最早到期的那条
            let Some((idx, deadline)) = guard
                .iter()
                .enumerate()
                .min_by_key(|(_, (d, _, _))| *d)
                .map(|(i, (d, _, _))| (i, *d))
            else {
                // 空闲：睡到下一次 arm
                guard = self.cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                continue;
            };
            let now = std::time::Instant::now();
            if now < deadline {
                // 等待期间可能被新的 arm 顶掉，醒来后重新取 deadline 判断
                let (g, _) = self
                    .cv
                    .wait_timeout(guard, deadline - now)
                    .unwrap_or_else(|e| e.into_inner());
                guard = g;
                continue;
            }
            let (_, token, coord) = guard.swap_remove(idx);
            // 回调期间释放锁，否则回调里若触发新的 arm 会自锁
            drop(guard);
            if let Some(c) = coord.upgrade() {
                (self.fire)(&c, token);
            }
            guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(test)]
#[path = "first_show_first_show_timer_tests.rs"]
mod first_show_timer_tests;
