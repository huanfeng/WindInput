//! 候选窗首显闸门：延迟首显判定与释放 + 共享兜底 timer。
//!
//! coordinator 的**子模块**（非平级）：首显族重度读写父模块私有字段
//!（pending_first_show / candidate_shown / caret_cache_verified 等），子模块对父
//! 私有项可见，平级模块则须放开字段可见性——封装以此为界。
//!（自 coordinator.rs 平移，纯搬运。）

use super::*;

impl Coordinator {
    /// 复位首显延迟状态（候选窗隐藏 / 组合结束）：下次新组合重新延迟首显，并作废未触发的兜底 timer。
    pub(crate) fn reset_first_show(&self) {
        self.first_show_was_provisional
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.first_show_extended
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
        *self
            .composition_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = (0, 0, false);
    }

    /// 推迟首次显示候选窗：标记 pending 并启动兜底 timer。token 比对使后续按键的 arm 自动作废
    /// 旧 timer。handle_caret_pending 握手会把 wait 档延到 600ms（应对 OnLayoutChange burst 慢的应用）。
    pub(super) fn arm_pending_first_show(&self) {
        // ★ 首帧信任门：`fast` 的短兜底建立在「手里的坐标 ≈ 当前插入点」之上，而焦点刚到达 /
        // 用户刚移动过光标时这个前提不成立（见 `caret_cache_verified`）。此时拿旧坐标首显
        // 必然是一次可见的错位加一次跳，「快」反而有害，让位给长兜底等权威坐标。
        //
        // ⚠⚠ **长等待一旦开始就不因后续按键重置**，这是本门能否成立的关键：闸门在候选窗
        // 显示前对**每一个字母**都会调到这里（`is_first_frame` 一直为真），而
        // `arm_pending_first_show_with_timeout` 每次都 bump token 重新计时。若照常重置，
        // 用户多打几个字母就把这段等待反复推后，长兜底静默退化回短兜底、错位照旧——正是
        // 「兜底超时长于组合寿命 ⇒ 永不到期」那个死结的镜像。Excel 建单元格编辑上下文要
        // 558ms，其间用户往往已经敲了三五个字母。
        //
        // 反过来，长等待到期后就**不再续**（`pending` 被 fire 消费掉，`extended` 保持置位），
        // 用旧坐标首显仍优于候选窗一直不出现。
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

    /// 当前焦点应用**实际生效**的首显档位：per-app 规则优先，未配则回落全局
    /// `ui.candidate.first_show_mode`（认不出的值再回落到枚举默认档）。
    ///
    /// ★ 全局默认档可配之后，「档位是什么」就有了两个来源，必须收成一个函数：判据一旦
    /// 分散在各消费点（本仓有 5 条首显通路），改一处漏一处的表现是「某条路仍按旧档位
    /// 走」，而首显逻辑的错都只表现为位置或时机不对，没有任何报错。
    ///
    /// ⚠ 先取 `active_compat` 的值并**释放锁**再读配置：`rt()` 内部另有锁，两把锁在此
    /// 嵌套会引入一个新的持有序。
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
    /// 缓存（`state.caret_x/y`）是兜底首显的坐标来源：`reset_first_show` 每次上屏都会把
    /// `composition_start` 清掉，于是下一轮的兜底走的正是这份缓存（见 `notify_ui_update`
    /// 里 `in_app && cs.2` 那个三元）。它若陈旧，候选窗就钉在原地。
    ///
    /// 实测（2026-09-02 记事本 + 五笔长按 d）：连续快速上屏时宿主的 `OnLayoutChange` 被
    /// 50ms debounce 压住，整段**一条权威 caret_update 都不来**，缓存停在 456px 之外，
    /// 每轮兜底都用它首显。试探坐标虽是 reflow 前的（偏差 ~30px），但远好过那份旧值。
    ///
    /// 两类不收：
    /// - 退化帧（`h<=0`）：宿主尚未 reflow 的空 rect，收了会污染缓存。
    /// - 配了 `stale_probe_guard` 的宿主：它们组合期间上报的 rect 可能停在**上一次组合**
    ///   的位置（微信实测差 136~419px），收进缓存等于把陈旧值扩散到兜底路径上。
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

    /// 本次 arm **实际**会用的超时值。
    ///
    /// 存在的唯一理由是给首显闸门的日志用：闸门原本直接打印 `first_show_fallback_ms()`，
    /// 而信任门命中时真正 arm 的是长兜底——日志说 25ms、实际等 600ms，排查时会被带偏。
    /// **判据分散在两处（一处算日志、一处定行为）就必然分叉**，故收敛到同一个函数。
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
            // 用的既然是旧坐标，就必须按「非权威」记账，否则随后到达的权威坐标会被 3px 常规容差
            // 判成需要校正而跳一下——兜底路径本来就是抖动最容易被看见的地方。
            // 置位在 has_content 内：没真显示就不该留下"用过非权威坐标"的账。
            self.first_show_was_provisional
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.show_authorized
                .store(true, std::sync::atomic::Ordering::Relaxed);
            debug!("first_show 兜底 timer 到期 → 用现有坐标首显（非权威，享放宽容差）");
            self.notify_ui_update(&state);
        }
    }
}

// ———————————————— 首显兜底 timer（进程内共享单线程）————————————————

/// 首显兜底的共享定时器：只保留**最近一次** arm 的待触发任务。
///
/// 此前每次 arm 都 `thread::spawn` 一个线程去 `sleep`，靠 token 让被取代的那些醒来后自行
/// 放弃——日志实测一小时创建两千余个线程。既然 token 已经保证「只有最新一次有效」，被作废
/// 的任务就没有理由继续占着线程；改成覆盖式待办后语义反而更直白：待办本身只有一个。
///
/// 本线程只做「等到点 + 回调」，**绝不在此执行可能阻塞的调用**（如前台窗口探测）——
/// 一次慢调用就会拖垮兜底的 150ms 时限。需要后台跑阻塞探测的场景另行处理。
struct FirstShowTimer {
    /// `(到期时刻, token, 协调器弱引用)`；`None` = 空闲。
    pending: Mutex<Option<(std::time::Instant, u64, std::sync::Weak<Coordinator>)>>,
    cv: std::sync::Condvar,
}

static FIRST_SHOW_TIMER: std::sync::OnceLock<Arc<FirstShowTimer>> = std::sync::OnceLock::new();

/// 取共享定时器，首次调用时懒启动其线程。
fn first_show_timer() -> &'static Arc<FirstShowTimer> {
    FIRST_SHOW_TIMER.get_or_init(|| {
        let timer = Arc::new(FirstShowTimer {
            pending: Mutex::new(None),
            cv: std::sync::Condvar::new(),
        });
        let worker = timer.clone();
        let _ = std::thread::Builder::new()
            .name("first-show-timer".into())
            .spawn(move || worker.run());
        timer
    })
}

impl FirstShowTimer {
    /// 覆盖式登记：新的 arm 直接顶掉旧的（与原先"旧线程靠 token 自行作废"等价）。
    fn arm(&self, deadline: std::time::Instant, token: u64, coord: std::sync::Weak<Coordinator>) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some((deadline, token, coord));
        self.cv.notify_one();
    }

    fn run(&self) {
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let deadline = match guard.as_ref() {
                Some((d, _, _)) => *d,
                None => {
                    // 空闲：睡到下一次 arm
                    guard = self.cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
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
            let Some((_, token, coord)) = guard.take() else {
                continue;
            };
            // 回调期间释放锁，否则回调里若触发新的 arm 会自锁
            drop(guard);
            if let Some(c) = coord.upgrade() {
                c.fire_pending_first_show(token);
            }
            guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(test)]
mod first_show_timer_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn idle_timer() -> FirstShowTimer {
        FirstShowTimer {
            pending: Mutex::new(None),
            cv: std::sync::Condvar::new(),
        }
    }

    /// 覆盖式登记：这是取代「spawn 多个线程靠 token 自行作废」的等价语义——
    /// 待办任何时刻只有一个，且必须是最近一次 arm 的那个。
    #[test]
    fn arm_replaces_previous_pending() {
        let t = idle_timer();
        let dead = std::sync::Weak::<Coordinator>::new();
        let base = Instant::now();

        t.arm(base + Duration::from_secs(10), 1, dead.clone());
        t.arm(base + Duration::from_secs(20), 2, dead.clone());
        t.arm(base + Duration::from_secs(30), 3, dead);

        let g = t.pending.lock().unwrap();
        let (deadline, token, _) = g.as_ref().expect("应有待办");
        assert_eq!(*token, 3, "只应保留最近一次 arm 的 token");
        assert_eq!(
            *deadline,
            base + Duration::from_secs(30),
            "到期时刻也应随最近一次 arm 更新"
        );
    }

    /// 线程真的会在到期后回调；且协调器已释放时安全跳过（不 panic）。
    #[test]
    fn fires_after_deadline_and_tolerates_dead_coordinator() {
        let t = Arc::new(idle_timer());
        let worker = t.clone();
        std::thread::spawn(move || worker.run());

        t.arm(
            Instant::now() + Duration::from_millis(30),
            7,
            std::sync::Weak::<Coordinator>::new(), // upgrade 必失败，走"协调器已没了"分支
        );

        // 到期后待办应被取走（说明线程确实醒来处理了），且不 panic
        std::thread::sleep(Duration::from_millis(200));
        assert!(t.pending.lock().unwrap().is_none(), "到期后待办应已被消费");
    }
}
