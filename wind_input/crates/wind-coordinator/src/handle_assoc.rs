//! 联想：上屏之后按刚上屏的内容给出下一批候选。
//!
//! ## ★★★ 联想 = 正常输入态，只是没有输入缓冲
//!
//! 这是本模块最重要的一句话，也是**推翻过一版实现**才得到的。
//!
//! 联想候选就住在 `state.candidates` 里，`state.active` 仍是 `None`——于是主输入路的
//! 既有能力**天然全部适用**：翻页、上下移高亮、二三候选键、数字选词、空格选高亮、
//! Esc 取消、鼠标点选，一行接线都不用写，因为那些分支的门槛本来就只是「候选非空」。
//!
//! 曾经的实现另立了一份 `assoc: Vec<Candidate>` 加 `assoc_selected`，理由是「塞进
//! `candidates` 会改变全 crate 28 处『候选非空』判断的语义」。那个担心本身没错，
//! **但结论反了**：那 28 处里绝大多数正是我们**希望**生效的（翻页要能翻、选词要能选）。
//! 独立字段换来的是展示要投影、翻页要投影、高亮要投影，外加一整套专用按键闸门——
//! 把一件事做成了两件，而且做出来的那套还比原来的少功能。
//!
//! 真正需要区分的只有一件事：**凡以「输入码」为 key 的加工必须跳过联想候选**
//! （词频记账、自动造词、码表调序——联想没有码）。那用 [`CandidateSource::Assoc`]
//! 一个来源标记就够了，与短语「有文本无码位、恒不记词频」是同一个先例。
//!
//! 于是**只剩三处**需要主动接：退格与回车（见 [`Coordinator::assoc_enter`] 与
//! [`Coordinator::assoc_backspace`]）、
//! 空缓冲模式激活的让位（见 `try_activate_mode`）、以及标点不顶屏（见
//! `commit_highlight_then_char` 与标点臂里的同款守卫）。
//!
//! ## ★★★ 联想态必须挂一个占位组合
//!
//! 「上屏之后还能收到按键」这件事，走了三步才对：
//!
//! 1. 补 `has_input_session`（Rust `key_gate.rs`）—— 只管到 Android，桌面走 C++ 那份。
//! 2. 让宿主的 `_hasCandidates` 镜像在上屏后保持真（协议 flags bit4）—— **仍然失败**，
//!    因为它由服务端应答**异步**回填，赢不了下一次 `OnTestKeyDown` 的竞速。
//!    真机日志同一行里 `composing=0 candidates=1 inputSession=0`：判定取的是 0，
//!    日志打出来已经是 1。该位已随之废弃（`BinaryProtocol.h` 里标了勿复用）。
//! 3. **挂占位组合**（见 [`ASSOC_COMPOSITION`]）—— `HasActiveComposition()` 是 TSF
//!    组合对象的**同步**状态，没有那个竞速窗口。特殊模式 / 临拼 / 临英一直可靠，
//!    正是因为它们都挂着组合。
//!
//! 教训有两条。其一，**一份判据有两处实现**（`key_gate.rs` 开头就写着「C++ TSF 尚未
//! 迁移」），补一处不够。其二，**「状态最终会变成真」不等于「判定时它是真」**——
//! 异步回填的镜像态永远赢不了同一拍的同步判定，这类缺陷只在按键够快时露头。
//!
//! ⚠️ 挂组合的代价落在**退格与回车**上：它们的既有分支在「缓冲空 + 无已转换段」时给
//! `PassThrough`，会把占位组合悬在宿主里。其余键要么替换组合（字母走
//! `UpdateComposition`）、要么结束组合（选词/标点走 `InsertText`、Esc 走
//! `cancel_session`），都不需要额外接。
//!
//! ## 为什么状态机与候选生成分家
//!
//! 候选**生成**在 `wind-assoc`（纯函数、可原生测试）；何时**展示与退出**在这里，
//! 因为它依赖宿主事件（失焦、切窗、鼠标点击）——那是协调器才有的信息。
//! `AssocConfig::mode` 也因此由本模块消费，而不是由 `wind_assoc::associate` 消费。

use crate::coordinator::{Coordinator, State};
use wind_assoc::{
    AssocConfig, AssocContext, AssocHit, AssocKind, AssocMode, AssocProvider, AssocSource,
};
use wind_bridge::handler::{COMPOSITION_PLACEHOLDER, KeyAction};
use wind_candidate::{Candidate, CandidateSource};

/// 联想态**显式退出**的原因。只用于日志，不参与控制流。
///
/// ⚠️ 这里只列「主动调 [`Coordinator::exit_assoc`] 的路径」，**不是退出联想的全部方式**
/// ——绝大多数退出是隐式的：联想候选就住在 `state.candidates` 里，谁清空/重填了候选
/// （敲字母、选词、Esc、失焦…）联想就自然没了，那些路径本来就在做该做的事，
/// 不需要也不该再报一次「我退出了联想」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssocExit {
    /// 退格 / 回车——两个「终结性」键，落回既有分支会给 `PassThrough` 悬空组合，
    /// 故自己收尾（见 [`Coordinator::assoc_enter`] 与 [`Coordinator::assoc_backspace`]）。
    Dismiss,
    /// 空格且 `space_commits = false`：不选联想，出空格。
    NonSelectKey,
    /// 模式切换（中英 / CapsLock / 系统切换）。
    ModeSwitch,
    /// 自动隐藏计时到期（`hide_after_ms`）。
    Timeout,
}

/// 联想窗自动隐藏的单槽定时器。
///
/// 与首显那个 timer 同构但**必须是独立实例**：两者都是单槽，而联想态与首显等待会同时
/// 存在（刚上屏时候选窗正等宿主 reflow 后的权威坐标，此刻也正要起自动隐藏计时），
/// 共用一个槽等于互相取消。
struct AssocHideTimer {
    pending: std::sync::Mutex<Option<(std::time::Instant, u64, std::sync::Weak<Coordinator>)>>,
    cv: std::sync::Condvar,
}

static ASSOC_HIDE_TIMER: std::sync::OnceLock<std::sync::Arc<AssocHideTimer>> =
    std::sync::OnceLock::new();

fn assoc_hide_timer() -> &'static std::sync::Arc<AssocHideTimer> {
    ASSOC_HIDE_TIMER.get_or_init(|| {
        let timer = std::sync::Arc::new(AssocHideTimer {
            pending: std::sync::Mutex::new(None),
            cv: std::sync::Condvar::new(),
        });
        let worker = timer.clone();
        let _ = std::thread::Builder::new()
            .name("assoc-hide-timer".into())
            .spawn(move || worker.run());
        timer
    })
}

impl AssocHideTimer {
    /// 覆盖式登记：新的 arm 顶掉旧的（旧的靠 token 比对自行作废）。
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
                    guard = self.cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
            };
            let now = std::time::Instant::now();
            if now < deadline {
                // 等待期间可能被新的 arm 顶掉，醒来后重新取 deadline 判断。
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
            // 回调期间释放锁：回调里会取 state 锁，也可能再次 arm。
            drop(guard);
            if let Some(c) = coord.upgrade() {
                c.fire_assoc_hide(token);
            }
            guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl State {
    /// 当前这批候选是不是联想来的。
    ///
    /// # ★★★ 真相源就是候选自己，没有第二份状态
    ///
    /// 曾经有过一份独立的 `assoc: Vec<Candidate>` 加一个 `assoc_selected`，于是
    /// 「联想态」与「正常候选态」成了两套并行的东西：展示要投影、翻页要投影、
    /// 高亮要投影，按键还得先过一道专用闸门才轮到既有分支。**那是把一件事做成了两件。**
    ///
    /// 联想其实就是**正常输入态、只是没有输入缓冲**——候选放在 `candidates` 里，
    /// 于是翻页、上下移高亮、二三候选键、数字选词、鼠标点选、Esc 取消**全部自动可用**，
    /// 一行接线都不用写。区分只需要一个只读判据，而它天然不会与候选本身失步。
    pub(crate) fn assoc_active(&self) -> bool {
        self.candidates
            .first()
            .is_some_and(|c| c.source == CandidateSource::Assoc)
    }
}

/// 联想态挂在宿主里的**占位组合**内容。
///
/// # 为什么必须有一个真的组合
///
/// TSF 侧「这个键要不要转发给输入法」的判据是
/// `HasActiveComposition() || _hasCandidates || …`。联想态下文本已提交、组合已结束，
/// 于是**只剩 `_hasCandidates` 一条路**——而它是由服务端应答**异步**回填的，赢不了
/// 下一次 `OnTestKeyDown` 的竞速。真机日志里抓到过铁证，同一行里：
///
/// ```text
/// vk=0x57 composing=0 candidates=1 inputSession=0 eaten=1
/// ```
///
/// `candidates` 是打日志时**现读**的、`inputSession` 是这次判定**开头**取的：
/// 判定用的是 0，日志打出来已经是 1。退格/Esc 就这样被宿主自己吃掉，服务端永远
/// 收不到——现象正是「联想窗关不掉」。
///
/// `HasActiveComposition()` 则是 TSF 组合对象的**同步**状态，没有这个窗口。
/// 这也正是特殊模式 / 临拼 / 临英一直可靠的原因：它们都挂着组合。
///
/// # 为什么是那个占位空格
///
/// 直接复用非嵌入模式的既有约定 [`COMPOSITION_PLACEHOLDER`]——空格 **且光标落在它
/// 前面**。两半缺一不可：只放空格不移光标，用户看到插入点凭空右移一格，很突兀
/// （2026-08-16 用户反馈）。光标那一半由 C++ 侧按「组合内容恰为占位符」判定。
///
/// 要给用户看的「联想输入」标识在 `state.preedit` 里，走候选窗自己的编码栏，
/// **不流进宿主**——而且只在非嵌入模式给：嵌入模式下候选窗本就没有编码栏，
/// 凭空多一栏会让窗口高度一跳。
pub(crate) const ASSOC_COMPOSITION: &str = COMPOSITION_PLACEHOLDER;

/// 本平台是否取用 `[mobile.*]` 覆盖段。
///
/// ★ 这是**整个联想里唯一一处平台判断**，平台差异全部落在配置文件的
/// `[mobile.association]` 上。
///
/// ⛔ 别把差异改成「值域哨兵」（让 `kind` / `mode` 取 `"auto"`、由本模块按平台解释）：
/// 那会把平台知识塞进值域，设置界面被迫列一个语义空洞的「自动」选项，而这里也要退化成
/// 一组 `platform_default_*` 函数——一处判断变成每个字段一处。
///
/// ⚠️ 判据是**协调器被编译进哪个宿主**，不是运行时探测。
const fn use_mobile_overrides() -> bool {
    cfg!(target_os = "android")
}

/// 词语联想的取数源：词库里以上文为前缀的更长的词。
///
/// ★ 显示整词、上屏只补剩余部分——`AssocHit::commit` 在这里填好，下游不再现算。
struct PrefixWords<'a> {
    mgr: &'a wind_engine::EngineManager,
    schema: String,
}

impl AssocProvider for PrefixWords<'_> {
    fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
        if limit == 0 || self.schema.is_empty() {
            return Vec::new();
        }
        self.mgr
            .assoc_prefix_words(&self.schema, ctx.text, limit)
            .into_iter()
            .map(|(word, weight)| {
                // 上文必是它的前缀（`assoc_prefix_words` 的后置条件），故 strip 恒成功；
                // 兜底成整词只是不让一个不该发生的情况变成 panic。
                let commit = word.strip_prefix(ctx.text).unwrap_or(&word).to_string();
                AssocHit {
                    text: word,
                    commit: Some(commit),
                    source: AssocSource::Prefix,
                    score: weight as i64,
                }
            })
            .collect()
    }
}

impl Coordinator {
    /// 退出联想态。已不在联想态时是空操作（返回 false）。
    ///
    /// 调用点遍布各条按键分派臂，故**必须幂等且极廉价**——绝大多数按键并不在联想态下发生。
    pub(crate) fn exit_assoc(&self, state: &mut State, why: AssocExit) -> bool {
        if !state.assoc_active() {
            return false;
        }
        tracing::debug!(reason = ?why, n = state.candidates.len(), "退出联想态");
        state.candidates.clear();
        state.preedit.clear();
        self.reset_candidate_view(state);
        // 作废未触发的自动隐藏计时。不作废的话，它会在**下一轮**联想里提前把窗收掉——
        // 现象是「有时联想刚出来就没了」，且只在两次上屏间隔短于 hide_after_ms 时复现。
        self.arm_assoc_hide(0);
        true
    }

    /// `[input.association]`（移动端再叠 `[mobile.association]`）的运行时视图（热重载快照）。
    pub(crate) fn assoc_config(&self) -> AssocConfig {
        let rt = self.rt();
        let mobile = use_mobile_overrides().then(|| &rt.config.mobile.association);
        AssocConfig::from_config(&rt.config.input.association, mobile)
    }

    /// 按刚上屏的文本生成联想候选并进入联想态。返回是否真的进入了。
    ///
    /// # 调用契约
    ///
    /// 调用前编码缓冲与常规候选**必须已清空**（互斥不变式，见
    /// [`State::debug_assert_assoc_invariant`]）。上屏路径上这件事由
    /// `reset_pinyin_composition` 完成，故本函数只该在它之后调用。
    pub(crate) fn maybe_enter_assoc(&self, state: &mut State, text: &str) -> bool {
        let cfg = self.assoc_config();
        if cfg.kind == AssocKind::Off {
            return false;
        }
        let ctx = AssocContext {
            text,
            // 上屏这一瞬间上文必然连续——正是刚刚打进去的那段字。
            // 断链只发生在失焦/切窗/点击等**别的**入口，那些路径直接调 `exit_assoc`。
            boundary_broken: false,
        };
        let punct = wind_assoc::punct::PunctRules;
        let prefix = PrefixWords {
            mgr: &self.engine_mgr,
            // ⚠️ **不能直接用 `active_schema_id()`**：混输方案自己没有词库，拿它的 id
            // 建出来的反查索引是空表，词语联想一条也出不来且完全静默。真机上活跃方案
            // 恰恰常是混输。解析规则见 `assoc_word_schema`。
            schema: self.engine_mgr.assoc_word_schema(),
        };
        // ⚠️ 四个源里目前接了两个。History（个人搭配，需新建 redb 表）与 Bigram
        // （词→后继表，需离线蒸馏）尚无 provider ⇒ `associate` 对它们 `continue`，
        // 配额顺延。这是有意的分期，不是漏接。
        //
        // 档位过滤在 `AssocConfig::source_enabled` 里，**不在这里**：这里一律登记全部
        // 已实现的源，由 kind 决定谁能出场。两处都做过滤会让「词语联想为什么没标点」
        // 这类问题有两个答案。
        let providers: &[(AssocSource, &dyn AssocProvider)] =
            &[(AssocSource::Prefix, &prefix), (AssocSource::Punct, &punct)];
        let hits = wind_assoc::associate(&ctx, &cfg, providers);
        if hits.is_empty() {
            return false;
        }
        tracing::debug!(n = hits.len(), kind = ?cfg.kind, mode = ?cfg.mode, "进入联想态");
        // ★ 填进 **`candidates`**（而不是另建一份列表）——联想就是「没有输入缓冲的正常
        // 候选态」。放在这里，翻页 / 上下移高亮 / 二三候选键 / 数字选词 / 鼠标点选 / Esc
        // 取消全部自动可用，因为那些分支的门槛本来就只是「候选非空」。
        state.candidates = hits
            .into_iter()
            .map(|h| Candidate {
                // 显示整词，上屏可能只补剩余部分（词语联想）。
                commit_override: h.commit.filter(|c| *c != h.text),
                text: h.text,
                // 唯一的身份标记：凡以「输入码」为 key 的加工（词频 / 造词 / 调序）
                // 据此跳过。见 `CandidateSource::Assoc`。
                source: CandidateSource::Assoc,
                ..Default::default()
            })
            .collect();
        self.reset_candidate_view(state);
        // 编码栏标识「联想输入」。联想候选与普通候选长得一模一样，没有标识时用户分不清
        // 候选窗为什么还开着、自己是不是还在打字。
        //
        // ★ **只在非嵌入模式给**。嵌入模式下编码在宿主里、候选窗**本来就没有编码栏**，
        // 这时凭空多一栏会让候选窗高度一跳——而联想是上屏后自动弹的，用户没做任何操作
        // 就看见窗口变高又变矮，比没有标识更烦（2026-08-16 用户反馈）。
        //
        // 嵌入模式下这个标识就此没有落点，是**有意放弃**而不是漏做：那一栏不存在，
        // 没有不打扰的地方可放。
        //
        // ⚠️ 无论哪种模式，这个字符串都**绝不流进宿主 composition**——宿主拿到的恒是
        // [`ASSOC_COMPOSITION`]（占位空格）。真写进去就是把「联想输入」四个字塞进用户的文档。
        let inline = self
            .preedit_display
            .lock()
            .map(|m| m.in_app())
            .unwrap_or(true);
        state.preedit = if inline {
            String::new()
        } else {
            self.rt().config.input.association.hint.clone()
        };
        self.arm_assoc_hide(cfg.hide_after_ms);
        true
    }

    /// 起自动隐藏计时。`ms == 0` = 不自动隐藏（仍会 bump generation 作废旧计时）。
    ///
    /// # 为什么不复用首显那个共享 timer
    ///
    /// 它是**单槽**的（「新的 arm 直接顶掉旧的」）。而联想态与首显等待**会同时存在**：
    /// 刚上屏时候选窗要重新首显（等宿主 reflow 后的权威坐标），此刻正好也要起自动隐藏。
    /// 共用一个槽等于两者互相取消——表现为「联想窗有时压根不出现」或「有时永远不消失」，
    /// 且取决于两次 arm 的先后，是最难复现的那类。
    fn arm_assoc_hide(&self, ms: u64) {
        let token = {
            let mut t = self
                .assoc_hide_token
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *t = t.wrapping_add(1);
            *t
        };
        if ms == 0 {
            return;
        }
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        assoc_hide_timer().arm(
            std::time::Instant::now() + std::time::Duration::from_millis(ms),
            token,
            weak,
        );
    }

    /// 自动隐藏计时到期。token 不匹配 = 期间已有新的联想/退出，本次作废。
    pub(crate) fn fire_assoc_hide(&self, token: u64) {
        {
            let t = *self
                .assoc_hide_token
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if t != token {
                return;
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.assoc_active() {
            return;
        }
        tracing::debug!("联想窗自动隐藏计时到期");
        self.exit_assoc(&mut state, AssocExit::Timeout);
        drop(state);
        // ★★★ 宿主那边的占位组合**没人收**——本函数跑在定时器线程上，没有待应答的按键
        // 可以搭载收口动作，而服务端→TSF 的 push 通道里没有一条能结束组合。留下标记，
        // 由下一次按键在 [`Self::adopt_orphaned_placeholder`] 里收口。
        //
        // 不留标记的后果（2026-09-03 真机日志实证，记事本 pid 70300）：超时 5.8 秒后
        // 按回车，TSF 侧仍是 `composing=1 candidates=1 inputSession=1`，键被判「有会话」
        // 吃下并转发；服务端此刻已不在联想态，回落到 `PassThrough` ⇒ `eaten=0` 的
        // 「吃了再吐」翻转 ⇒ 不补发 `WM_KEYDOWN` 的宿主直接丢键，且组合继续悬着，
        // 被宿主 finalize 后在文档里留下那个占位空格。
        self.assoc_placeholder_orphaned
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.notify_ui_hide();
    }

    /// 宿主组合已由别的路径终止 ⇒ 孤儿不复存在，撤掉标记。
    ///
    /// 不撤的后果只是下一次 `PassThrough` 多发一次收口（`EndComposition` 此时是空操作，
    /// 键照常重放），不致命；但那次多余的重放会让日志与真实意图对不上。
    pub(crate) fn clear_orphaned_placeholder(&self) {
        self.assoc_placeholder_orphaned
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// 按键处理的唯一出口上，把孤儿占位组合**搭上这一次按键**收掉。
    ///
    /// # 为什么落在这个单点上
    ///
    /// 「谁来收组合」这件事只有按键应答一条通道，而上屏/取消路径有 40+ 个返回点。
    /// `handle_key_event_policed` 是它们的共同出口（`record_input_stats`、
    /// `note_commit_action`、`expire_scope_override` 都因同一理由收口在那里），
    /// 在此改判，天然覆盖全部分支——不必去盘回车的五条独立路径。
    ///
    /// # 三种命运，穷尽 match 强制新变体表态
    ///
    /// 判据是**这个动作到了宿主那边，会不会碰组合**：
    ///
    /// - 会结束或替换 ⇒ 孤儿被顺带收掉，撤标记、动作原样发出。
    /// - 把键交还宿主却不碰组合（`PassThrough` / `NotHandled`）⇒ **正是要拦的那一格**，
    ///   改判成 [`KeyAction::ClearCompositionThenPassThrough`]：收组合 + 把这一键还给宿主。
    /// - 完全不碰组合（状态更新、纯吃键、移光标）⇒ 孤儿还在，标记必须**留到下一次按键**。
    ///   曾想过「任何按键都撤标记」，那会让超时后先按一个热键的用户永远收不掉组合。
    pub(crate) fn adopt_orphaned_placeholder(&self, action: KeyAction) -> KeyAction {
        use std::sync::atomic::Ordering;
        if !self.assoc_placeholder_orphaned.load(Ordering::Relaxed) {
            return action;
        }
        /// 一个动作对宿主组合的作用。只在本函数内用，命名从宿主视角出发。
        enum Fate {
            /// 会结束或替换宿主组合。
            Absorbs,
            /// 交还按键但不碰组合——孤儿会继续悬着。
            LeavesOrphan,
            /// 完全不碰宿主组合。
            Untouched,
        }
        let fate = match &action {
            KeyAction::InsertText { .. }
            | KeyAction::InsertTextWithCursor { .. }
            | KeyAction::UpdateComposition { .. }
            | KeyAction::ClearComposition
            | KeyAction::ClearCompositionThenPassThrough
            | KeyAction::ReplaceBackward { .. }
            | KeyAction::HoldComposition { .. }
            | KeyAction::CommitReplacingHeld { .. }
            | KeyAction::CommitAndHoldComposition { .. }
            | KeyAction::CommitThenDeferComposition { .. } => Fate::Absorbs,
            KeyAction::PassThrough | KeyAction::NotHandled => Fate::LeavesOrphan,
            // ⚠️ `StatusUpdate` 明确**不结束组合**（那正是「切了方案编码还挂在应用里」的
            // 根因，见 `schema_switch_key_action`）；`MoveCursorRight` / `DeletePair` 在
            // C++ 侧也不碰组合、不清 `_hasCandidates`（`KeyEventSink.cpp` 对应分支）。
            KeyAction::StatusUpdate(_)
            | KeyAction::Consumed
            | KeyAction::MoveCursorRight { .. }
            | KeyAction::DeletePair => Fate::Untouched,
        };
        match fate {
            Fate::Absorbs => {
                self.assoc_placeholder_orphaned
                    .store(false, Ordering::Relaxed);
                action
            }
            Fate::LeavesOrphan => {
                self.assoc_placeholder_orphaned
                    .store(false, Ordering::Relaxed);
                tracing::debug!("联想占位组合成孤儿：本次透传改判为「收组合 + 交还按键」");
                KeyAction::ClearCompositionThenPassThrough
            }
            Fate::Untouched => action,
        }
    }

    /// **收掉联想**：清候选、结束占位组合、吞掉这一键。
    ///
    /// # 哪些键该走这里
    ///
    /// 判据是「这个键落回既有分支之后，会不会有人结束或替换掉占位组合」：
    ///
    /// - **会** ⇒ 不走这里。字母走 `update_candidates` 出 `UpdateComposition`（替换）、
    ///   标点/选词走 `InsertText`（结束）、Esc 走 `cancel_session`（结束）。
    /// - **不会** ⇒ 必须走这里。目前是**退格**与**回车**：它们的既有分支门槛都是
    ///   「缓冲或已转换前缀非空」，而联想两者皆空，会一路落到 `PassThrough`——
    ///   那会把占位组合悬在宿主里（编码栏留着「联想输入」，再按什么都不对）。
    ///
    /// # 为什么不顺手上屏高亮那条
    ///
    /// 退格与回车都是**终结性**动作：一个是「删掉」、一个是「换行/发送」。联想态的
    /// 高亮是输入法猜的，不是用户选的，替他选一个是越权。
    ///
    /// # 这一键是吃掉还是交还宿主，由配置定
    ///
    /// `cancels_only = true` 时吃掉（要按第二次才生效），`false` 时连同收窗一起把键交还
    /// 宿主（[`KeyAction::ClearCompositionThenPassThrough`]）。**判据不在本函数里**——
    /// 回车与退格的默认值相反，各自的取值见 [`Self::assoc_enter`] / [`Self::assoc_backspace`]。
    fn assoc_dismiss_with(&self, state: &mut State, cancels_only: bool) -> KeyAction {
        self.exit_assoc(state, AssocExit::Dismiss);
        self.notify_ui_hide();
        if cancels_only {
            KeyAction::ClearComposition
        } else {
            KeyAction::ClearCompositionThenPassThrough
        }
    }

    /// 联想态的**回车**处置（`input.association.enter_cancels_only`，默认 `false` = 透传）。
    ///
    /// 默认让回车穿过去：它是终结性动作，用户按它是要发送/换行，而联想窗是输入法自己弹的、
    /// 用户并没在选词。让一个「建议」吞掉一次回车，正事就被挡住了。
    pub(crate) fn assoc_enter(&self, state: &mut State) -> KeyAction {
        let cancels_only = self.rt().config.input.association.enter_cancels_only;
        self.assoc_dismiss_with(state, cancels_only)
    }

    /// 联想态的**退格**处置（`input.association.backspace_cancels_only`，默认 `true` = 吃键）。
    ///
    /// 与回车默认相反，是刻意的（2026-08-20 用户拍板）：回车透传是「把正事办了」，退格透传
    /// 却是**删掉刚上屏的字**——不可逆。联想窗弹出时用户的手正停在刚打完的字上，误触退格
    /// 若直接删字，代价远大于多按一次键。要这个行为的人可以开。
    pub(crate) fn assoc_backspace(&self, state: &mut State) -> KeyAction {
        let cancels_only = self.rt().config.input.association.backspace_cancels_only;
        self.assoc_dismiss_with(state, cancels_only)
    }

    /// 上屏一条**联想候选**之后，还要不要再联想一轮。
    ///
    /// 一次性档：不续（出完这一轮就结束）。持续档：续——「中」→「中国」→ 再找以
    /// 「中国」开头的词。判据是「刚上屏的那条是不是联想来的」，故由调用方把候选来源传进来。
    pub(crate) fn assoc_may_chain(&self, from_assoc: bool) -> bool {
        !from_assoc || self.assoc_config().mode == AssocMode::Continuous
    }
}

#[cfg(test)]
mod tests {
    //! 只测**端到端测不到的那几件**：配置默认、编码栏标识、自动隐藏计时、UI 下发。
    //!
    //! 按键行为一律不在这里测——联想候选住在 `state.candidates` 里，翻页/高亮/
    //! 二三候选/数字选词走的都是主输入路的既有分支，验它们必须有真实词库，
    //! 那组在 `tests/assoc_end_to_end.rs`。
    use super::*;
    use std::sync::Arc;
    use wind_config::Config;

    /// 智能联想档 + 标点源（标点源不依赖词库，headless 下也出得来）。
    fn coord_smart() -> Arc<Coordinator> {
        coord_with(|_| {})
    }

    /// 同上，但可以再改几项配置。
    ///
    /// ⚠️ **显式开 `punct`**：桌面基线里它是关的（实体键盘上标点一键可达）。本模块的
    /// 单测大多需要「联想能出候选」这个前提，而 headless 下唯一不依赖词库的源就是标点
    /// ——不开的话它们测的就成了一片空候选，全部静默失去意义（2026-08-16 实测红 10 条）。
    /// 桌面出厂不出标点这件事本身，由 wind-assoc 的
    /// `desktop_smart_yields_no_punct_by_default` 钉。
    fn coord_with(tweak: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.input.symbol.smart_mode = false;
        cfg.input.association.kind = "smart".to_string();
        cfg.input.association.punct = true;
        tweak(&mut cfg);
        Coordinator::new_headless(cfg, None)
    }

    fn enter(c: &Coordinator, text: &str) -> bool {
        let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
        c.maybe_enter_assoc(&mut st, text)
    }

    fn texts(c: &Coordinator) -> Vec<String> {
        c.debug_assoc_texts()
    }

    /// ★ 桌面默认关，而且**是靠基线段本身就是 `"off"`**——不再有哨兵值参与。
    ///
    /// 断言前提值本身：`[input.association]` 一旦又被写成某个平台哨兵，这里立刻报出来。
    /// 平台差异的唯一落点是 `[mobile.association]`，桌面构建根本不读它
    /// （由 `use_mobile_overrides` 守着，见 wind-assoc 的 `mobile_section_switches_*`）。
    #[test]
    fn desktop_defaults_to_off() {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        assert_eq!(cfg.input.association.kind, "off", "前提：桌面基线就是 off");
        assert_eq!(
            cfg.mobile.association.kind, "smart",
            "反向对照：移动端段确实是另一个值，否则本测试测不出任何东西"
        );
        let c = Coordinator::new_headless(cfg, None);
        assert!(!enter(&c, "你好"), "桌面不该进联想");
    }

    /// **反向对照**：显式开启后确实进得去。少了它，上面那条可能只是因为联想压根不工作。
    #[test]
    fn smart_kind_enters() {
        let c = coord_smart();
        assert!(enter(&c, "你好"));
        assert_eq!(texts(&c)[0], "，");
    }

    /// 联想候选必须带上 `Assoc` 来源——那是它唯一的身份标记，词频/造词/调序全靠它跳过。
    #[test]
    fn candidates_carry_assoc_source() {
        let c = coord_smart();
        assert!(enter(&c, "你好"));
        let st = c.state.lock().unwrap();
        assert!(
            st.candidates
                .iter()
                .all(|x| x.source == CandidateSource::Assoc),
            "整批候选都该标成 Assoc，否则「是不是联想」的判据会时真时假"
        );
        assert!(st.assoc_active());
    }

    /// 普通候选**不该**被认成联想（`assoc_active` 的反向对照）。
    #[test]
    fn normal_candidates_are_not_assoc() {
        let c = coord_smart();
        {
            let mut st = c.state.lock().unwrap();
            st.candidates = vec![Candidate {
                text: "普通".into(),
                source: CandidateSource::Pinyin,
                ..Default::default()
            }];
        }
        assert!(!c.state.lock().unwrap().assoc_active());
        assert!(texts(&c).is_empty());
    }

    /// **非嵌入模式**：编码栏标识进联想时填上、退出时随候选一并清掉。
    ///
    /// ⚠️ 它借用 `state.preedit`——那个字段平时装真实编码，清理漏了就会留到下一次打字。
    #[test]
    fn hint_fills_and_clears_when_not_inline() {
        let c = coord_with(|cfg| {
            cfg.ui.candidate.preedit_display = "candidate_top".to_string();
        });
        assert!(enter(&c, "你好"));
        assert_eq!(c.state.lock().unwrap().preedit, "联想输入");
        {
            let mut st = c.state.lock().unwrap();
            c.exit_assoc(&mut st, AssocExit::Dismiss);
        }
        assert!(
            c.state.lock().unwrap().preedit.is_empty(),
            "退出必须把标识清掉"
        );
    }

    /// ★★★ **嵌入模式下不给标识**——候选窗本来就没有编码栏，凭空多一栏会让窗口高度
    /// 一跳。而联想是上屏后自动弹的，用户没做任何操作就看见窗口变高又变矮，
    /// 比没有标识更烦（2026-08-16 用户反馈）。
    ///
    /// `app_inline` 是**出厂默认**，所以这条覆盖的是绝大多数用户的实际路径。
    #[test]
    fn no_hint_when_inline() {
        let c = coord_smart(); // Config::default() ⇒ preedit_display = app_inline
        assert!(enter(&c, "你好"), "前提：进了联想态");
        assert!(
            c.state.lock().unwrap().preedit.is_empty(),
            "嵌入模式不该给编码栏标识——那会让候选窗高度一跳"
        );
    }

    /// 空标识 = 不显示（用户可以关掉它）。
    #[test]
    fn empty_hint_shows_nothing() {
        let c = coord_with(|cfg| {
            cfg.ui.candidate.preedit_display = "candidate_top".to_string();
            cfg.input.association.hint = String::new();
        });
        assert!(enter(&c, "你好"));
        assert!(c.state.lock().unwrap().preedit.is_empty());
    }

    /// 自动隐藏计时到期即收窗。
    #[test]
    fn hide_timer_closes_the_window() {
        let c = coord_smart();
        assert!(enter(&c, "你好"));
        let t = *c.assoc_hide_token.lock().unwrap();
        c.fire_assoc_hide(t);
        assert!(texts(&c).is_empty(), "计时到期该收窗");
        assert!(c.state.lock().unwrap().preedit.is_empty(), "标识一并清掉");
    }

    /// ★ 退出联想态必须作废未触发的计时。
    ///
    /// 不作废的话它会在**下一轮**联想里提前把窗收掉——现象是「有时联想刚出来就没了」，
    /// 且只在两次上屏间隔短于 `hide_after_ms` 时复现，是最难对着日志复盘的那类。
    #[test]
    fn exiting_invalidates_pending_hide_timer() {
        let c = coord_smart();
        assert!(enter(&c, "你好"));
        let t1 = *c.assoc_hide_token.lock().unwrap();
        {
            let mut st = c.state.lock().unwrap();
            c.exit_assoc(&mut st, AssocExit::Dismiss);
        }
        assert_ne!(
            t1,
            *c.assoc_hide_token.lock().unwrap(),
            "退出时须 bump 令牌"
        );
        assert!(enter(&c, "你好"), "重新进入");
        c.fire_assoc_hide(t1); // 陈旧令牌
        assert!(!texts(&c).is_empty(), "陈旧计时不得收掉新一轮的联想窗");
    }

    /// `hide_after_ms = 0` = 不自动隐藏。
    #[test]
    fn zero_hide_after_ms_keeps_it_open() {
        let c = coord_with(|cfg| cfg.input.association.hide_after_ms = 0);
        assert!(enter(&c, "你好"));
        assert!(!texts(&c).is_empty());
    }

    /// ★★★ 超时收窗必须留下「宿主组合成了孤儿」的标记。
    ///
    /// 真机实证（2026-09-03，记事本 pid 70300）：超时 5.8 秒后按回车，TSF 侧仍是
    /// `composing=1 candidates=1 inputSession=1`——键被判「有会话」吃下并转发，服务端
    /// 此刻已不在联想态、回落到 `PassThrough`，于是 `eaten=0` 的「吃了再吐」翻转，
    /// 不补发 `WM_KEYDOWN` 的宿主直接丢键。
    #[test]
    fn timeout_leaves_placeholder_orphaned() {
        use std::sync::atomic::Ordering;
        let c = coord_smart();
        assert!(enter(&c, "你好"));
        assert!(
            !c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
            "刚进联想态时组合有主，不该有孤儿标记"
        );
        let t = *c.assoc_hide_token.lock().unwrap();
        c.fire_assoc_hide(t);
        assert!(
            c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
            "超时收窗没有按键应答可搭载收口动作，必须留标记等下一次按键"
        );
    }

    /// **反向对照**：按键路径退出联想不留标记——它自己就带着收口动作回宿主。
    ///
    /// 少了这条，上一条测试在「一进联想态就置位」这种接错线下照样绿。
    #[test]
    fn key_path_dismiss_leaves_no_orphan() {
        use std::sync::atomic::Ordering;
        let c = coord_smart();
        assert!(enter(&c, "你好"));
        let action = {
            let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
            c.assoc_enter(&mut st)
        };
        assert!(
            matches!(
                action,
                KeyAction::ClearComposition | KeyAction::ClearCompositionThenPassThrough
            ),
            "按键路径的收口动作搭着这次应答就送到宿主了"
        );
        assert!(!c.assoc_placeholder_orphaned.load(Ordering::Relaxed));
    }

    /// 孤儿在场时透传改判为「收组合 + 交还按键」——本修复的核心那一格。
    #[test]
    fn orphan_rewrites_passthrough_into_clear_then_pass() {
        use std::sync::atomic::Ordering;
        let c = coord_smart();
        c.assoc_placeholder_orphaned.store(true, Ordering::Relaxed);
        assert!(matches!(
            c.adopt_orphaned_placeholder(KeyAction::PassThrough),
            KeyAction::ClearCompositionThenPassThrough
        ));
        assert!(
            !c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
            "收口一次即撤标记"
        );
        assert!(
            matches!(
                c.adopt_orphaned_placeholder(KeyAction::PassThrough),
                KeyAction::PassThrough
            ),
            "标记已撤，后续透传不得再被改判——否则每个透传键都白挨一次收口 + 重放"
        );
    }

    /// 没有孤儿时不得改判。
    #[test]
    fn passthrough_untouched_without_orphan() {
        let c = coord_smart();
        assert!(matches!(
            c.adopt_orphaned_placeholder(KeyAction::PassThrough),
            KeyAction::PassThrough
        ));
    }

    /// ★★ 不碰宿主组合的动作**必须把标记留着**。
    ///
    /// 「任何按键都撤标记」会让超时后先按一个热键的用户永远收不掉那个组合：
    /// `StatusUpdate` 明确不结束组合——那正是「切了方案编码还挂在应用里」的根因
    /// （见 `schema_switch_key_action`）；`MoveCursorRight` / `DeletePair` 在 C++ 侧
    /// 同样不碰组合、连 `_hasCandidates` 都不清。
    #[test]
    fn non_composition_actions_keep_the_orphan() {
        use std::sync::atomic::Ordering;
        let c = coord_smart();
        for action in [
            KeyAction::StatusUpdate(c.build_status()),
            KeyAction::Consumed,
            KeyAction::MoveCursorRight { count: 1 },
            KeyAction::DeletePair,
        ] {
            c.assoc_placeholder_orphaned.store(true, Ordering::Relaxed);
            let out = c.adopt_orphaned_placeholder(action);
            assert!(
                c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
                "{out:?} 不碰宿主组合，孤儿还在，标记必须留到下一次按键"
            );
        }
    }

    /// 会结束或替换组合的动作顺带收掉孤儿：动作原样发出，标记撤掉。
    #[test]
    fn composition_absorbing_actions_clear_the_orphan() {
        use std::sync::atomic::Ordering;
        let c = coord_smart();
        for action in [
            KeyAction::ClearComposition,
            KeyAction::UpdateComposition {
                text: "a".to_string(),
                caret_pos: 1,
            },
            KeyAction::InsertText {
                text: "你".to_string(),
                new_composition: None,
                mode_changed: false,
                chinese_mode: true,
                has_new_composition: false,
            },
        ] {
            c.assoc_placeholder_orphaned.store(true, Ordering::Relaxed);
            let out = c.adopt_orphaned_placeholder(action);
            assert!(
                !c.assoc_placeholder_orphaned.load(Ordering::Relaxed),
                "{out:?} 到了宿主那边会结束或替换组合，孤儿已被顺带收掉"
            );
        }
    }

    /// ★ 联想候选真的被推到了候选窗上——本功能可见的那一半。
    ///
    /// 预置 caret 绕开首显闸门：headless 无宿主坐标，首帧会被拦下不下发。
    #[test]
    fn candidates_reach_the_candidate_window() {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.input.symbol.smart_mode = false;
        cfg.input.association.kind = "smart".to_string();
        // 桌面基线里 punct 是关的；headless 下它是唯一不依赖词库的源，不开就没候选。
        cfg.input.association.punct = true;
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        c.debug_mark_coords_ready();
        {
            let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
            assert!(c.maybe_enter_assoc(&mut st, "你好"));
            c.notify_ui_update(&st);
        }
        let mut last: Option<Vec<String>> = None;
        while let Ok(cmd) = rx.try_recv() {
            if let wind_ui_types::UiCommand::UpdateCandidates { candidates, .. } = cmd {
                last = Some(candidates.into_iter().map(|i| i.text).collect());
            }
        }
        let items = last.expect("联想态该下发 UpdateCandidates");
        assert_eq!(items.first().map(String::as_str), Some("，"));
    }

    /// 联想态**不画模式标记**：候选旁边挂个「拼」会让用户以为自己还在打字。
    ///
    /// 这一条不需要为联想加任何判据——「有候选即隐藏标记」本就适用。曾为它加过
    /// `|| assoc_active()`，结果让「空则隐藏」守卫在联想态成立，把整个候选窗收掉了。
    #[test]
    fn no_mode_label_in_assoc() {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = true;
        cfg.input.symbol.smart_mode = false;
        cfg.input.association.kind = "smart".to_string();
        // 桌面基线里 punct 是关的；headless 下它是唯一不依赖词库的源，不开就没候选。
        cfg.input.association.punct = true;
        let (c, rx) = Coordinator::new_headless_with_ui(cfg, None);
        c.debug_mark_coords_ready();
        {
            let mut st = c.state.lock().unwrap_or_else(|e| e.into_inner());
            assert!(c.maybe_enter_assoc(&mut st, "你好"));
            c.notify_ui_update(&st);
        }
        let mut label = None;
        while let Ok(cmd) = rx.try_recv() {
            if let wind_ui_types::UiCommand::UpdateCandidates { mode_label, .. } = cmd {
                label = Some(mode_label);
            }
        }
        assert_eq!(label.as_deref(), Some(""), "联想态不该显示模式标记");
    }
}
