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
//! 于是**只剩这么几处**需要主动接：退格与回车（见 [`Coordinator::assoc_enter`] 与
//! [`Coordinator::assoc_backspace`]）、**其余落到透传的键**（Del / Home / End / 左右 /
//! Insert…，见 [`Coordinator::assoc_release_on_passthrough`]——它们与退格回车是同一个
//! 病灶，只是名单数不清，故按动作而非按键名收口）、空缓冲模式激活的让位与临英
//! Shift+字母那道门（都在 `try_activate_mode`）、以及标点不顶屏（见
//! `commit_highlight_then_char` 与标点臂里的同款守卫）。
//!
//! ⚠️ **这份清单不等于「占位组合已被全部收口」**（2026-09-07 审查发现的既有缺口，
//! 本次未修）：语言栏中/英按钮（`CMD_TOGGLE_MODE`）与系统 Ctrl+Space
//! （`CMD_SYSTEM_MODE_SWITCH`）在联想态下 `take_input_on_mode_switch` 拿不到文本 ⇒
//! bridge 回 `StatusUpdate`，而 C++ 的 StatusUpdate 分支**明确不结束组合** ⇒ 占位空格
//! 悬着，可能被宿主 finalize 进文档。对照组是 Shift 单击那条 keyup 路径：它的
//! `had_pending` 把 `!candidates.is_empty()` 算进去了，于是回空文本 `InsertText`
//! 顺带 `EndComposition`。修法与超时孤儿同源（`push_end_composition`，零协议改动）。
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
//! 3. **挂占位组合**（见 [`Self::assoc_composition`]）—— `HasActiveComposition()` 是 TSF
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
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::{Candidate, CandidateSource};
use wind_keys::keymap;

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
    /// 这一键在既有分支里落到了透传（Del / Home / End / 左右 / Insert…）。
    /// 收口见 [`Coordinator::assoc_release_on_passthrough`]。
    PassThroughKey,
    /// 模式切换（中英 / CapsLock / 系统切换）。
    ModeSwitch,
    /// 按键激活了独占模式（目前只有临时英文的 Shift+字母走到这里）。
    ModeActivate,
    /// 自动隐藏计时到期（`hide_after_ms`）。
    Timeout,
    /// 无占位联想（`assoc_placeholder = "none"`）下用户把光标挪到了别处（点单元格、点击定位）。
    /// 有占位组合时这件事由宿主终止组合来表达，没有组合就只能靠选区变化回报。
    CaretMoved,
    /// 顶屏类按键（标点 / 小键盘 / 非码元字符 / 进模式、开软键盘的引导键）。联想态不顶屏，
    /// 只收掉联想再出该键——与空格选联想、Esc 取消后的状态一致（候选、标识、计时一并清）。
    TopCommitKey,
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

impl Coordinator {
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
    /// # 为什么是那个占位字符
    ///
    /// 默认复用非嵌入模式的既有约定（[`Coordinator::composition_placeholder`]，默认空格、
    /// 浏览器按 compat 规则为 ZWSP，GH#175），compat 的 `assoc_placeholder` 可只给联想态另指定
    /// （[`Coordinator::assoc_placeholder`]）——占位 **且光标落在它前面**。两半缺一不可：
    /// 只放空格不移光标，用户看到插入点凭空右移一格，很突兀（2026-08-16 用户反馈）。
    /// 光标那一半由 C++ 侧按「组合内容恰为占位符」判定（空格与 ZWSP 都认）。
    ///
    /// 要给用户看的「联想输入」标识在 `state.preedit` 里，走候选窗自己的编码栏，
    /// **不流进宿主**——而且只在非嵌入模式给：嵌入模式下候选窗本就没有编码栏，
    /// 凭空多一栏会让窗口高度一跳。
    pub(crate) fn assoc_composition(&self) -> &'static str {
        self.assoc_placeholder()
    }
}

/// 这个 VK 是不是**修饰键本身**（按下它不构成「用户做了别的事」）。
///
/// # ⚠️ 为什么不复用 `keymap::is_key_up_only_vk` / `is_pure_modifier_vk`
///
/// 那两个回答的是**另一根轴**上的问题：
///
/// - `is_key_up_only_vk`（`0xA0..=0xA3 | VK_CAPITAL`）问的是「这个键**只有 keyup
///   到得了服务端**」——它是为「修饰键绑功能只能挂 keyup」定的，值域刚好等于桌面
///   TSF 会转发的那几个。
/// - `is_pure_modifier_vk` 更窄，连 CapsLock 都不含（其单元测试还专门钉了
///   `is_pure_modifier_vk(0xA4) == false`）。
///
/// 而这里要问的是「按下的是不是修饰键本身」。两根轴今天在桌面上答案恰好重合
/// （C++ `isModifierKeyItself` 不转发、`ClassifyInputKey` 对 Ctrl/Alt 按下回 `None`，
/// Alt 与笼统的 `VK_SHIFT/VK_CONTROL/VK_MENU` 根本到不了服务端），但
/// [`Coordinator::assoc_release_on_passthrough`] 这道守卫的自我定位是**给薄宿主的
/// 保险**——而薄宿主正是最可能上报 Alt keydown 或未拆左右的笼统 VK 的那一类。
/// 借来的判据恰好会在它要保的场景下失效。同
/// `project_tsf_docmgr_judgement_layers`：判据分层，跨层复用即漂移。
///
/// ⇒ 就地写全。左右 Alt（`0xA4`/`0xA5`）与笼统的 `VK_SHIFT`/`VK_CONTROL`/`VK_MENU`
/// （`0x10`/`0x11`/`0x12`）在 `keymap` 里没有常量——它们在既有通路上确实用不到，
/// 为本函数单独加一组公开常量反而会诱使别处误用。
fn is_modifier_like_vk(vk: u32) -> bool {
    // 0x10/0x11/0x12 = VK_SHIFT / VK_CONTROL / VK_MENU（笼统，未拆左右）
    // 0xA4/0xA5      = VK_LMENU / VK_RMENU
    // is_key_up_only_vk = 0xA0..=0xA3 四个纯修饰键 + VK_CAPITAL
    matches!(vk, 0x10 | 0x11 | 0x12 | 0xA4 | 0xA5) || keymap::is_key_up_only_vk(vk)
}

/// 数字键此刻是否进编码缓冲（而不是选候选）。
///
/// 只有**移动端、英文方案、已在组码**三者同时成立才进：英文词里常带数字（mp3、h2o、win10），
/// 软键盘上数字要上滑或切层才打得出，没人会拿它去选候选（点候选更顺手）。
/// 中文方案不进：打完字接数字是常态，数字仍是「首选上屏 + 数字」（宿主侧处理）。
/// 桌面不进：物理键盘上数字选词是长期习惯。
pub(crate) const fn digit_enters_english_buffer(
    mobile: bool,
    composing: bool,
    english: bool,
) -> bool {
    mobile && composing && english
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
        let words = self.mgr.assoc_prefix_words(&self.schema, ctx.text, limit);
        let n = words.len() as i64;
        words
            .into_iter()
            .enumerate()
            .map(|(i, (word, _weight))| {
                // 上文必是它的前缀（`assoc_prefix_words` 的后置条件），故 strip 恒成功；
                // 兜底成整词只是不让一个不该发生的情况变成 panic。
                let commit = word.strip_prefix(ctx.text).unwrap_or(&word).to_string();
                AssocHit {
                    text: word,
                    commit: Some(commit),
                    source: AssocSource::Prefix,
                    // ★ 按**名次**给分，不按权重：取数口已把用户词 / 系统词 / 临时词三层
                    // 分档排好（见 `assoc_prefix_words`），三层权重量纲不同，按权重重排
                    // 会把分档打乱。
                    score: n - i as i64,
                }
            })
            .collect()
    }
}

/// 智能联想的「下一词」源：离线蒸馏的上文 → 后继表（[`wind_assoc::bigram`]）。
///
/// 表里的句末记号 `$` 换成「，」：模型说这里常断句，就把逗号给到候选条上。自训的表直接
/// 存了统计出的句读（「，」「？」），同样处理。标点几乎是每个上文的头名——原样放第一位，
/// 每次上屏后首位都是逗号，最可能的那个词反被挤到第二；故头名总留给词，标点最早排第二，
/// 表里没有的标点仍由标点源按规则补。
struct BigramWords<'a> {
    table: &'a wind_assoc::bigram::AssocBigram,
}

/// 光标前文留几个字。「下一词」表只看 4 个字，多留一些给以后整句的上文用（语法模型）。
pub(crate) const PRECEDING_KEEP_CHARS: usize = 32;

fn pop_chars(s: &mut String, count: usize) {
    for _ in 0..count {
        if s.pop().is_none() {
            break;
        }
    }
}

/// 后继是标点：旧表的句末记号 `$`，或自训表里保留下来的句读（「，」「？」）。
fn is_punct_successor(w: &str) -> bool {
    w == wind_assoc::bigram::SENTENCE_END || !w.chars().any(char::is_alphanumeric)
}

impl AssocProvider for BigramWords<'_> {
    fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
        let Some(succ) = self
            .table
            .lookup_after(ctx.preceding, ctx.text, ctx.char_wise)
        else {
            return Vec::new();
        };
        let mut words: Vec<&str> = succ.into_iter().take(limit).collect();
        for w in &mut words {
            if *w == wind_assoc::bigram::SENTENCE_END {
                *w = "，";
            }
        }
        // 头名是标点：把排在最前的那个词提到它前面
        if let Some(i) = words.iter().position(|w| !is_punct_successor(w))
            && i > 0
        {
            let w = words.remove(i);
            words.insert(0, w);
        }
        let n = words.len() as i64;
        words
            .into_iter()
            .enumerate()
            .map(|(i, w)| AssocHit {
                text: w.to_string(),
                commit: None,
                source: AssocSource::Bigram,
                // 按名次给分：表里已按模型分值排好
                score: n - i as i64,
            })
            .collect()
    }
}

/// 联想历史的取数源：用户**在这个上文之后**从联想里选过的词（论坛 t185 方案 B）。
///
/// 读 store 的 `assoc_history` 表（一次范围扫描）；写在 [`Coordinator::record_assoc_pick`]。
/// 分值 = 选中次数的衰减分（与 FREQ 同一套 `pinyin_score`），源内按它降序。
///
/// ★ 读端过滤存在性：历史表不随删词级联清理，选过的词被删掉之后，这里按
/// [`wind_engine::EngineManager::assoc_word_known`] 丢掉它。
struct HistoryWords<'a> {
    store: &'a wind_store::Store,
    mgr: &'a wind_engine::EngineManager,
    /// 联想词源方案（查存在性用，见 `assoc_word_schema`）。
    word_schema: String,
    schema: String,
    profile: wind_store::freq::FreqProfile,
    /// 是否出「下一词」行（与上文无前缀关系的词）。只有智能联想出：词语联想档的契约是
    /// 「只出上文的延长」（见 `AssocKind::allows`），下一词混进去就成了两种东西的拼盘。
    next_words: bool,
}

impl AssocProvider for HistoryWords<'_> {
    fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
        if limit == 0 || self.schema.is_empty() {
            return Vec::new();
        }
        // 取该上文下的**全部**行（limit=0），过滤后再截名额：先截后滤会让已删的高分词
        // 永久占住名额、挤掉排在后面的有效历史。store 本就整段扫描再排序，全取不多花 IO。
        let rows = match self.store.assoc_history(&self.schema, ctx.text, 0) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("读联想历史失败（本次不出历史）: {e}");
                return Vec::new();
            }
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        rows.into_iter()
            // 存在性：历史表不随删词级联清理，删掉的词在这里丢掉（见类型文档）。
            .filter(|(word, _)| self.mgr.assoc_word_known(&self.word_schema, word))
            .filter_map(|(word, rec)| {
                // 两类行共用这张表：
                // - **前缀延伸**（「荷」→「荷枪实弹」）：词以上文开头，上屏只补剩余部分；
                // - **下一词**（「我们」→「一起」）：词与上文无前缀关系，整词上屏
                //   （写入端 `learn_next_word`）。
                // 与上文完全相同的行没有可补的，丢掉。
                let commit = match word.strip_prefix(ctx.text) {
                    Some("") => return None,
                    Some(rest) => Some(rest.to_string()),
                    None if self.next_words => None,
                    None => return None,
                };
                Some(AssocHit {
                    score: (self.profile.pinyin_score(&rec, now) * 1000.0) as i64,
                    text: word,
                    commit,
                    source: AssocSource::History,
                })
            })
            .take(limit)
            .collect()
    }
}

/// 滤掉被屏蔽文字的联想源包装（拼音「屏蔽这个词」）。
struct SkipBlocked<'a> {
    inner: &'a dyn AssocProvider,
    blocked: &'a std::collections::HashSet<String>,
}

impl AssocProvider for SkipBlocked<'_> {
    fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
        let mut hits = self.inner.suggest(ctx, limit);
        hits.retain(|h| !self.blocked.contains(&h.text));
        hits
    }
}

impl Coordinator {
    /// 词语联想的数据归属 id（store 键里的 schema）：联想词源方案折叠后的数据域。
    fn assoc_data_schema(&self) -> String {
        self.engine_mgr
            .data_schema_id(&self.engine_mgr.assoc_word_schema())
    }

    /// 选中联想候选后记一笔联想历史（方案 B 的写入端）。
    ///
    /// 只记**前缀延伸**类的选择（有 `commit_override`：显示整词、上屏补剩余）——上文就是
    /// 整词去掉补出的那截。标点联想等没有「上文 → 词」的前缀关系，不记。
    pub(crate) fn record_assoc_pick(&self, cand: &Candidate) {
        if self.learning_suspended() {
            return;
        }
        let (Some(store), Some(rest)) = (&self.store, cand.commit_override.as_deref()) else {
            return;
        };
        let Some(context) = cand.text.strip_suffix(rest).filter(|c| !c.is_empty()) else {
            return;
        };
        if let Err(e) = store.record_assoc_pick(&self.assoc_data_schema(), context, &cand.text) {
            tracing::debug!("记联想历史失败: {e}");
        }
    }

    /// 下一词学习的写入端：`commit_selected` 整体上屏 `word` 时调用。`prev` 是这次选词
    /// 记账前取出的上文（见 [`Self::take_next_word_prev`]）：上一次整体上屏的词，其间有任何
    /// 断点就是 `None`（见 [`Coordinator::next_word_prev`]）。有上文就记一笔「上一个词 →
    /// 这个词」，下次上屏那个词时联想里就有它（读端 [`HistoryWords`]，与联想历史同表）。
    ///
    /// `already_recorded`：选的是前缀延伸类联想候选，「上文 → 整词」已由
    /// [`Self::record_assoc_pick`] 记过，这里只更新上文、不重复记。
    ///
    /// `word` 里没有一个字母或汉字（标点联想、符号候选）就是断点：不记、也不当下一次的上文。
    ///
    /// 不记的情形：
    /// - 不在智能联想档或关了历史源（读端只在智能联想出下一词，见
    ///   [`wind_assoc::AssocConfig::learns_next_words`]；不让用不上的用户默默攒数据）；
    /// - 任一侧不含非 ASCII 字符（英文方案的词另有补全，进不了中文联想，记了只是垃圾）；
    /// - 后一个词以前一个词开头（「中」→「中国」）：读端会把它当前缀延伸，只补「国」，
    ///   而用户打的是两个词。
    pub(crate) fn learn_next_word(&self, prev: Option<String>, word: &str, already_recorded: bool) {
        // 暂停学习：不记，也不把这个词留作下一次的上文——否则恢复后的第一个词会和
        // 无痕框里的最后一个词连成一对，等于把它带了出去。
        if self.learning_suspended() || !word.chars().any(char::is_alphabetic) {
            self.note_word_boundary();
            return;
        }
        self.restore_next_word_prev(Some(word.to_string()));
        let (Some(prev), Some(store)) = (prev, &self.store) else {
            return;
        };
        if already_recorded
            || !self.assoc_config().learns_next_words()
            || prev.is_ascii()
            || word.is_ascii()
            || word.starts_with(prev.as_str())
        {
            return;
        }
        if let Err(e) = store.record_assoc_pick(&self.assoc_data_schema(), &prev, word) {
            tracing::debug!("记下一词失败: {e}");
        }
    }

    /// 取出下一词学习的上文（随之清空）。`commit_selected` 在 `record_commit`（会清上文）
    /// 之前调用，见那里。
    pub(crate) fn take_next_word_prev(&self) -> Option<String> {
        self.next_word_prev
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// 放回 / 设定下一词学习的上文。
    pub(crate) fn restore_next_word_prev(&self, prev: Option<String>) {
        *self
            .next_word_prev
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = prev;
    }

    /// 下一词学习的断点：清掉上文（见 [`Coordinator::next_word_prev`]）。廉价、幂等。
    pub fn note_word_boundary(&self) {
        self.restore_next_word_prev(None);
    }

    /// 宿主报来光标前的正文（不含组合区）。只留末尾 [`PRECEDING_KEEP_CHARS`] 个字。
    ///
    /// 时机由宿主定：光标被挪动、宿主自己改了正文之后（Android `onUpdateSelection`）。
    /// 核心自己的上屏不必等它报——[`Self::note_host_edits`] 已跟上。
    /// 不知道光标前是什么（换了输入框、读不到）就报空串。
    pub fn set_preceding_text(&self, text: &str) {
        let n = text.chars().count();
        let at = text
            .char_indices()
            .nth(n.saturating_sub(PRECEDING_KEEP_CHARS))
            .map_or(text.len(), |(i, _)| i);
        let mut cur = self
            .preceding_text
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cur.clear();
        cur.push_str(&text[at..]);
    }

    /// 一次按键 / 选词交给宿主的编辑，照样施加到光标前文上：宿主的报告是异步的，
    /// 紧接着的下一次上屏多半等不到它。挪光标的情形算不出新位置的前文，清空等宿主报。
    pub fn note_host_edits(&self, ops: &[wind_host::EditOp]) {
        use wind_host::EditOp;
        let mut cur = self
            .preceding_text
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for op in ops {
            match op {
                EditOp::Commit(text) => cur.push_str(text),
                EditOp::SetComposition { .. } => {}
                EditOp::DeleteBackward { count } => pop_chars(&mut cur, *count),
                EditOp::ReplaceBackward { count, text } => {
                    pop_chars(&mut cur, *count);
                    cur.push_str(text);
                }
                EditOp::MoveCursor { .. } => cur.clear(),
            }
        }
        let n = cur.chars().count();
        if n > PRECEDING_KEEP_CHARS {
            let at = cur
                .char_indices()
                .nth(n - PRECEDING_KEEP_CHARS)
                .map_or(0, |(i, _)| i);
            cur.drain(..at);
        }
    }

    /// 当前记着的光标前文（诊断 / 测试用）。
    pub fn preceding_text(&self) -> String {
        self.preceding_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 退出联想态。已不在联想态时是空操作（返回 false）。
    ///
    /// 调用点遍布各条按键分派臂，故**必须幂等且极廉价**——绝大多数按键并不在联想态下发生。
    pub(crate) fn exit_assoc(&self, state: &mut State, why: AssocExit) -> bool {
        if !state.assoc_active() {
            return false;
        }
        tracing::debug!(reason = ?why, n = state.candidates.len(), "退出联想态");
        state.clear_comment_flips();
        state.candidates.clear();
        state.preedit.clear();
        self.reset_candidate_view(state);
        // 作废未触发的自动隐藏计时。不作废的话，它会在**下一轮**联想里提前把窗收掉——
        // 现象是「有时联想刚出来就没了」，且只在两次上屏间隔短于 hide_after_ms 时复现。
        self.arm_assoc_hide(0);
        true
    }

    /// 「下一词」表：已加载就返回；还没加载就派后台加载、本次按暂缺处理（返回 None）。
    /// 按键路径上调，不读盘。没有数据文件或读失败也是 None。
    pub(crate) fn assoc_bigram(&self) -> Option<std::sync::Arc<wind_assoc::bigram::AssocBigram>> {
        if let Some(table) = self.assoc_bigram.get() {
            return table.clone();
        }
        self.ensure_assoc_bigram_async();
        None
    }

    /// 后台加载下一词表（只派一次）。智能联想档生效时由 `apply_data_needs` / `prewarm_indexes`
    /// 提前调，按键路径上的 [`Self::assoc_bigram`] 只是兜底。
    pub(crate) fn ensure_assoc_bigram_async(&self) {
        if self.assoc_bigram.get().is_some()
            || self
                .assoc_bigram_loading
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let Some(path) = self.assoc_bigram_path.clone() else {
            let _ = self.assoc_bigram.set(None);
            return;
        };
        let slot = std::sync::Arc::clone(&self.assoc_bigram);
        let spawned = std::thread::Builder::new()
            .name("assoc-bigram-load".into())
            .spawn(move || {
                slot.get_or_init(|| {
                    std::panic::catch_unwind(|| load_assoc_bigram(&path)).unwrap_or_else(|_| {
                        tracing::warn!("联想下一词表解析崩溃，本次运行不出");
                        None
                    })
                });
            });
        if let Err(e) = spawned {
            tracing::warn!("联想下一词表后台加载线程起不来，本次运行不出: {e}");
            let _ = self.assoc_bigram.set(None);
        }
    }

    /// `[input.association]`（移动端再叠 `[mobile.association]`）的运行时视图（热重载快照）。
    pub(crate) fn assoc_config(&self) -> AssocConfig {
        let rt = self.rt();
        let mobile = (self.profile.config_overlay == wind_config::ConfigOverlay::Mobile)
            .then(|| &rt.config.mobile.association);
        AssocConfig::from_config(&rt.config.input.association, mobile)
    }

    /// 读一个联想布尔项：移动端取 `[mobile.association]`，桌面取 `[input.association]`。
    fn assoc_flag(
        &self,
        base: impl Fn(&wind_config::AssociationConfig) -> bool,
        mobile: impl Fn(&wind_config::MobileAssociationConfig) -> bool,
    ) -> bool {
        let rt = self.rt();
        if self.profile.config_overlay == wind_config::ConfigOverlay::Mobile {
            mobile(&rt.config.mobile.association)
        } else {
            base(&rt.config.input.association)
        }
    }

    fn assoc_select_keys_commit(&self) -> bool {
        self.assoc_flag(|a| a.select_keys_commit, |m| m.select_keys_commit)
    }

    /// 按上文算联想候选（不改状态）。`maybe_enter_assoc` 与评测 / 诊断共用这一份。
    pub(crate) fn assoc_hits(
        &self,
        cfg: &AssocConfig,
        preceding: &str,
        text: &str,
        char_wise: bool,
    ) -> Vec<AssocHit> {
        if cfg.kind == AssocKind::Off {
            return Vec::new();
        }
        let ctx = AssocContext {
            text,
            preceding,
            char_wise,
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
        // 四个源全部接上：History 见 `HistoryWords`（2026-09；2026-10 起也出用户自己的
        // 「下一词」，见 `learn_next_word`），Bigram 见 `BigramWords`（2026-10，离线蒸馏表；
        // 数据文件缺失时不登记 ⇒ `associate` 对它 `continue`，配额顺延）。
        //
        // 档位过滤在 `AssocConfig::source_enabled` 里，**不在这里**：这里一律登记全部
        // 已实现的源，由 kind 决定谁能出场。两处都做过滤会让「词语联想为什么没标点」
        // 这类问题有两个答案。
        let history = self.store.as_deref().map(|store| HistoryWords {
            store,
            mgr: &self.engine_mgr,
            word_schema: self.engine_mgr.assoc_word_schema(),
            schema: self.assoc_data_schema(),
            profile: self.engine_mgr.pinyin_freq_profile(),
            next_words: cfg.kind == AssocKind::Smart,
        });
        // 这里只管**读不读表**，出不出场仍由 `associate` 判：表整份常驻约 2 MB，词语联想档
        // （PC 用户常开的那档）用不上它，就不该因为调到这里而读进内存。
        let bigram_table = cfg
            .source_enabled(AssocSource::Bigram)
            .then(|| self.assoc_bigram())
            .flatten();
        let bigram = bigram_table.as_deref().map(|table| BigramWords { table });
        let mut providers: Vec<(AssocSource, &dyn AssocProvider)> =
            vec![(AssocSource::Prefix, &prefix), (AssocSource::Punct, &punct)];
        if let Some(h) = &history {
            providers.push((AssocSource::History, h));
        }
        if let Some(b) = &bigram {
            providers.push((AssocSource::Bigram, b));
        }
        // 拼音「屏蔽这个词」：在各源**之内**滤掉，`associate` 按源配额截取时空出的格由同源
        // 后面的补上；滤在汇聚之后会少条数。全被滤光 = 没有联想，调用方照常不进联想态。
        let blocked = self.blocked_words_in(&self.assoc_data_schema());
        let filtered: Vec<SkipBlocked<'_>>;
        if let Some(blocked) = blocked.as_deref() {
            filtered = providers
                .iter()
                .map(|(_, p)| SkipBlocked { inner: *p, blocked })
                .collect();
            providers = providers
                .iter()
                .zip(&filtered)
                .map(|((src, _), f)| (*src, f as &dyn AssocProvider))
                .collect();
        }
        wind_assoc::associate(&ctx, cfg, &providers)
    }

    /// 用户是不是逐字上屏的：拼音以外的方案（五笔等形码、混输）算是。单字上屏后「下一词」
    /// 据此决定要不要给词内接续，见 [`wind_assoc::bigram::AssocBigram::lookup_after`]。
    pub(crate) fn assoc_char_wise(&self) -> bool {
        !self.engine_mgr.is_pinyin()
    }

    /// 按刚上屏的文本生成联想候选并进入联想态。返回是否真的进入了。
    ///
    /// # 调用契约
    ///
    /// 调用前编码缓冲与常规候选**必须已清空**（互斥不变式，见
    /// [`State::debug_assert_assoc_invariant`]）。上屏路径上这件事由
    /// `reset_pinyin_composition` 完成，故本函数只该在它之后调用。
    pub(crate) fn maybe_enter_assoc(&self, state: &mut State, text: &str, on_screen: &str) -> bool {
        let cfg = self.assoc_config();
        // 前文是这次按键之前的：本次上屏要等按键返回后才经 `note_host_edits` 记进去。
        // `on_screen`：`text` 里本来就在屏上的那截（选前缀延伸联想「中国」时的「中」），
        // 前文末尾就是它，去掉才接得上整词。
        let preceding = self.preceding_text();
        let preceding = preceding.strip_suffix(on_screen).unwrap_or(&preceding);
        let hits = self.assoc_hits(&cfg, preceding, text, self.assoc_char_wise());
        if hits.is_empty() {
            return false;
        }
        tracing::debug!(n = hits.len(), kind = ?cfg.kind, mode = ?cfg.mode, "进入联想态");
        // 上屏即关了上一个候选窗，其注释翻转不带进联想窗。
        state.clear_comment_flips();
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
        // [`Self::assoc_composition`]（占位空格）。真写进去就是把「联想输入」四个字塞进用户的文档。
        // 用**有效**归属：候选窗被宿主压住时（UI-less 游戏）这个标识同样没有落点，与嵌入模式同理。
        //
        // ★ 更要紧的是**方向**。上面那条「绝不流进宿主 composition」今天成立，靠的是
        // 「压制态下这个串恰好没人读到宿主那边去」——而 `state.preedit` 的消费者其实**很多**
        // （Home/End 移组合区光标、前插补码、各 overlay 模式十余处都会把它克隆进
        // `UpdateComposition`），只是每一条都要求非空缓冲或活跃 overlay，而联想是上屏**之后**
        // 弹的、缓冲为空，才统统够不着。这个「恰好」太薄，不该拿来当铁律的支点。
        // 取有效值之后压制态下这里装的是空串：哪天真给 `UiElementPage` 补上编码串字段
        // （见 `docs/design/game-compat-tsf-uielement.md` §4.4），交出去的也是空串，
        // 而不是把「联想输入」四个字送进游戏的聊天框。
        let inline = self.preedit_in_app_effective();
        state.preedit = if inline {
            String::new()
        } else {
            self.rt().config.input.association.hint.clone()
        };
        // 每次进联想都是宿主里一个新建的占位组合，起点已右移；不解锁的话联想连环时候选窗
        // 会钉在第一次联想的位置（见 `unpin_composition_anchor_if_shown`）。
        self.unpin_composition_anchor_if_shown();
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
        // 复核：上面的校验在 state 锁外，等锁期间按键线程可能已进了新一轮联想并重置 token，
        // 不复核就会把新一轮联想误退掉。锁序 state → assoc_hide_token 与 `exit_assoc` 一致。
        if *self
            .assoc_hide_token
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            != token
        {
            return;
        }
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
        // 无占位（`none`）时宿主里没有组合，也就没有孤儿；要告知 DLL 的只剩「会话没了」。
        if self.assoc_placeholder_is_none() {
            self.push_hotkey_session_if_changed();
            self.notify_ui_hide();
            return;
        }
        self.assoc_placeholder_orphaned
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // 同时**主动**推一条收口命令过去（走既有的 `CMD_CLEAR_COMPOSITION` push 通道，
        // 见 [`Self::push_end_composition`]）。两条路各管一半，不是二选一：
        //
        // - push（本行）：唯一能覆盖「用户一个键都不按、直接点鼠标或切窗」的那一半——
        //   按键侧永远够不着，而那正是占位空格被宿主 finalize 进文档的场景。
        //   但它不保证成功（宿主可能拒绝非按键上下文的 `TF_ES_ASYNCDONTCARE` edit session，
        //   push 管道也可能没连上），故**不能**据此撤上面的标记。
        // - 标记（上一行）：宿主拒绝时的兜底。
        //
        // ★ push 成功时也不会留下多余的收口：组合没了 ⇒ `_HasInputSession()` 为假 ⇒
        //   后续透传键根本不会被 TSF 吃下发过来，标记只会被用户重新打字时的
        //   `UpdateComposition`（`Fate::Absorbs`）顺手撤掉。
        self.push_end_composition();
        self.notify_ui_hide();
    }

    /// 宿主告知「用户正在操作联想候选」，期间暂停自动隐藏。
    ///
    /// 计时只认按键：触屏上手指滑候选条、长按看提示、打开展开面板挑词都不是按键，
    /// 于是操作到一半窗就被计时收掉（真机反馈）。桌面的候选窗靠鼠标点、没有这类停留，
    /// 不调用本函数，行为不变。
    ///
    /// - `hold = true`：作废当前计时，操作期间不收；
    /// - `hold = false`：操作结束，从此刻起重新计满 `hide_after_ms`。
    ///
    /// 不在联想态时什么也不做（不能凭空起计时，也不能误伤下一轮联想的计时）。
    /// 判联想态与改计时在同一把 state 锁里完成：先判后改之间若插进一次退出 + 新一轮联想，
    /// 这里的 `arm(0)` 会把新一轮的计时作废，那一轮联想就永远不收了。
    pub fn assoc_hold(&self, hold: bool) {
        // 配置在取 state 锁之前读，不在 state 锁里再去拿配置锁
        let ms = if hold {
            0
        } else {
            self.assoc_config().hide_after_ms
        };
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.assoc_active() {
            return;
        }
        self.arm_assoc_hide(ms);
        drop(state);
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

    /// 联想态下**落到透传的那一键**：收掉占位组合，并把这一键原样交还宿主。
    ///
    /// # 病灶
    ///
    /// 联想态在宿主里挂着占位组合（见 [`Self::assoc_composition`]），TSF 的 `_HasInputSession()`
    /// 因此为真 ⇒ `OnTestKeyDown` 把 Del / Home / End / 左右（C++ 归 `HotkeyType::CursorKey`）
    /// 一律吃下转发。而协调器这边，这些键的既有分支门槛都是「缓冲或已转换段非空」，
    /// 联想两者皆空 ⇒ 一路落到 `PassThrough`。
    ///
    /// 「吃了再吐」的后果与超时孤儿那条完全同形（见 [`Self::adopt_orphaned_placeholder`]）：
    /// 不补发 `WM_KEYDOWN` 的宿主直接丢键——用户报的正是「Del / Home / End 按了没反应，
    /// 只有退格能用」（退格早有专门分支，见 [`Self::assoc_backspace`]）；而补发的宿主
    /// 则把占位空格 finalize 进文档。分界线全在宿主侧，我们这边是同一条路径。
    ///
    /// # 为什么收口在按键处理的唯一出口，而不是逐个键加分支
    ///
    /// 「哪些键会落到透传」不是一份数得清的名单：左右/Home/End 一条臂、Del 一条臂、
    /// Insert 落兜底臂、Ctrl 组合另有一条。本仓已多次栽在「N 条通路只接了 N-1 条」上。
    /// ★ 反证也有：Tab 在 C++ 侧同样「有 session 就吃」，按键名列表会把它一起收进来，
    /// 而它在协调器里其实归**辅助码触发键**（`enter_aux_code`）、给的是 `Consumed`
    /// 而非透传——按动作判就自动认得出这个差别。
    /// 判据其实只有一句——**这个动作是不是把键交还宿主却不碰组合**，那正是
    /// [`KeyAction::PassThrough`] / [`KeyAction::NotHandled`] 这一格。在
    /// `handle_key_event_policed` 上问一次即可，与 [`Self::adopt_orphaned_placeholder`]
    /// 同址同理（那边治的是超时孤儿，这边治的是联想仍在活着时的同一格）。
    ///
    /// ⚠️ 「唯一出口」只对**桌面与 macOS** 成立（`wind-bridge/src/server.rs` → 协调器这份
    /// 重写）。移动端 `wind-mobile/src/lib.rs` 的 `key_down` 直调**内层**
    /// `handle_key_event`，本函数与 [`Self::adopt_orphaned_placeholder`] 两道收口在那里
    /// 都不生效；即便走到，`edit_ops.rs` 也会把 `ClearCompositionThenPassThrough` 降级
    /// 成吃键（`KeyOutcome` 表达不了「清理后不消费」，见该文件）。移动端没有 TSF 的
    /// 「吃了再吐」问题，故这不是活缺陷，但别把这句话当成全平台承诺。
    ///
    /// # 顺带把窗收掉，不是附带效果而是正解
    ///
    /// 这批键要么移动了宿主光标（Home/End/左右）、要么改了光标后的文档（Del）——
    /// 联想的上文**已经断了**，那批候选此刻就是错的。留着它比收掉更糟。
    ///
    /// # ⚠️ 邻居状态同病，本函数刻意不管
    ///
    /// `_HasInputSession()` 只要 `_hasCandidates` 为真即成立，于是**空码补全**那类
    /// 「缓冲空 + 候选非空 + 非联想」的状态按 Del/Home/End 会走到**逐字节同形**的
    /// 「吃了再吐」。本函数一进门就按 `assoc_active()` 把它挡在外面——这是取舍不是遗漏：
    /// 空码补全时用户确实在看那批候选，「顺带收窗」在那里未必是对的语义。
    /// ⇒ 日后若收到「空码时 Home 没反应」的报障，根因与本函数治的是同一个，但该单独定
    /// 它的收窗语义，**别顺手把判据放宽成「候选非空」**。
    ///
    /// # ⚠️ 这条路的曝光面变大了
    ///
    /// C++ `_ReplayKeyToHost` 会往 skip 表压一条，而 `_TryConsumeSkipKey` **只比对队首、
    /// 且没有超时**。本改动之前走这条路的只有联想回车/退格与 hold 的 500ms 窗口；现在
    /// 联想窗生命周期（`hide_after_ms` 默认 5 秒）内每个 Del / Home / End / ←→ / Insert /
    /// Tab 与每个 Ctrl/Alt 组合都走它。注入若没能回到我们的 sink（前台窗口已换、宿主此刻
    /// 不走 TSF、SendInput 被 UIPI 拦），队首那条会残留并挡住后续全部 skip（含自动配对
    /// 合成的右符号），直到用户碰巧再按一次同一个键。
    /// **既有隐患，本改动只是提高了触发频次**；根治该让 `_TryConsumeSkipKey` 按值扫描整张
    /// 表（表很小）或加 TTL——那是 C++ 侧的独立改动，要真机验证，不在本次范围。
    ///
    /// # 三道守卫
    ///
    /// - **只在 keydown**（守在调用点）：`ClearCompositionThenPassThrough` 的「交还按键」
    ///   靠 C++ 在按键入口重放，只在 `OnKeyDown` / `DispatchHotkey` 消费。理由同
    ///   [`Self::adopt_orphaned_placeholder`]。
    /// - **纯修饰键与 CapsLock 除外**：按住 Shift 准备打 Shift+字母时，光按下修饰键不该
    ///   把联想窗收掉。Windows 侧这类 keydown 压根不转发（`OnTestKeyDown` 的
    ///   `isModifierKeyItself` 分支），此处是给薄宿主的保险。
    /// - **不在联想态时原样返回**。⚠️ 代价**不是一次布尔判断**：判据 `assoc_active()` 读的是
    ///   `state.candidates`，得先取 state 锁。透传是英文模式每个字母的常态返回值，于是那条
    ///   路上每次按键多一次 state 锁往返（`matches!` 与修饰键两道廉价筛已排在锁之前，
    ///   非透传动作根本走不到取锁那步）。
    ///   ⛔ **不加 `AtomicBool` 镜像**来省这把锁（`adopt_orphaned_placeholder` 有一个，那是
    ///   因为它的真相源本来就是个原子标志）。联想的真相源是候选自己——本模块开头那段
    ///   「没有第二份状态」讲的正是这件事，加镜像就是把它请回来，而镜像与候选失步的
    ///   表现会是「联想窗关不掉」这类最难查的缺陷。同一把锁在本函数之前的
    ///   `expire_scope_override` / `touch_pair_state` 已经取过，边际成本远小于那个风险。
    ///
    /// ⛔ **不给它配开关**。回车/退格各有一个 `*_cancels_only`，是因为那两个键「只收窗、
    /// 吃掉这一键」也说得通（退格透传会删掉刚上屏的字，不可逆）；而「按 Home 只收窗、
    /// 光标不动」对不上任何一种用户期待——那不是取舍，是坏掉。
    ///
    /// # ★ 刻意**不排除** Ctrl/Alt 组合
    ///
    /// 联想态下按 Ctrl+S / Ctrl+C 走的是 `MOD_SHORTCUT` 那条臂，缓冲为空 ⇒ 同样落
    /// `PassThrough` ⇒ 同一个病灶（TSF 的 `ctrl_alt_cleanup` 分支已把它吃下转发了）。
    /// 改判后 C++ 走 `_ReplayKeyToHost`，重放时**物理修饰键仍按着**，宿主 `GetKeyState`
    /// 能还原 Ctrl+S 语义——这正是 `KeyEventSink.h` 里记着「修法同构、暂未实施」的那条。
    ///
    /// ⚠️ 那条臂的注释叮嘱「改动本分支的返回值前先读 macOS 侧」，已读。**对照基线是
    /// `passThrough` 那一臂，不是 `clearComposition` 那一臂**——联想态下缓冲与已转换段
    /// 皆空，那条臂给的本来就是 `PassThrough`。`BridgeResponseRouter` 两臂都
    /// `return false`（消费与否不变），区别只在收尾：`passThrough` 走
    /// `flushPendingPrefix`、`clearThenPassThrough` 走 `applyClearComposition`，两者都
    /// 「取出待定前缀 → 清 marked text → 把前缀真上屏」，只是后者在前缀为空时也清
    /// marked text——而联想态的 marked text 正是那个占位符，清掉恰是所求。
    /// ⇒ macOS 无文本丢失、无消费差异，无需同步改动。
    pub(crate) fn assoc_release_on_passthrough(
        &self,
        data: &KeyEventData,
        action: KeyAction,
    ) -> KeyAction {
        if !matches!(action, KeyAction::PassThrough | KeyAction::NotHandled)
            || is_modifier_like_vk(data.key_code)
        {
            return action;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.exit_assoc(&mut state, AssocExit::PassThroughKey) {
            return action;
        }
        drop(state);
        self.notify_ui_hide();
        KeyAction::ClearCompositionThenPassThrough
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

    /// 联想态下这一键若是二三候选键，是否**让出**（不选联想、按「联想窗不在」处理）。
    ///
    /// `input.association.select_keys_commit = false` 时成立（论坛 t251，对标 QQ 五笔）。
    /// 只给 keydown 消费的可打印选词键用（主路符号臂与字母臂）：让出后那一键照常往下走，
    /// 由既有分支收联想并出字 / 进模式——与联想态的其它标点同一条路，这里不另写出口。
    pub(crate) fn assoc_yields_select_key(&self, state: &State) -> bool {
        state.assoc_active() && !self.assoc_select_keys_commit()
    }

    /// 联想态的**回车**处置（`input.association.enter_cancels_only`，默认 `false` = 透传）。
    ///
    /// 默认让回车穿过去：它是终结性动作，用户按它是要发送/换行，而联想窗是输入法自己弹的、
    /// 用户并没在选词。让一个「建议」吞掉一次回车，正事就被挡住了。
    pub(crate) fn assoc_enter(&self, state: &mut State) -> KeyAction {
        let cancels_only = self.assoc_flag(|a| a.enter_cancels_only, |m| m.enter_cancels_only);
        self.assoc_dismiss_with(state, cancels_only)
    }

    /// 联想态的**退格**处置（`input.association.backspace_cancels_only`，默认 `true` = 吃键）。
    ///
    /// 与回车默认相反，是刻意的（2026-08-20 用户拍板）：回车透传是「把正事办了」，退格透传
    /// 却是**删掉刚上屏的字**——不可逆。联想窗弹出时用户的手正停在刚打完的字上，误触退格
    /// 若直接删字，代价远大于多按一次键。要这个行为的人可以开。
    pub(crate) fn assoc_backspace(&self, state: &mut State) -> KeyAction {
        let cancels_only =
            self.assoc_flag(|a| a.backspace_cancels_only, |m| m.backspace_cancels_only);
        self.assoc_dismiss_with(state, cancels_only)
    }

    /// **引擎替用户做主上屏**（满码唯一自动上屏 `InputOutcome::AutoCommit`）的出口：
    /// 上屏 `out`，并接上与 `commit_selected` 整串分支**同一段**联想接线（论坛 t185）。
    ///
    /// 此前两个 `AutoCommit` 消费点只 `commit_action` 了事——开着自动上屏的五笔用户，
    /// 单字全码几乎都唯一，于是「打完一个字给以它开头的词」整个失效且完全静默。
    ///
    /// `assoc_ctx` 取**简体域**的上屏文本（未经简繁转换、未补空格），理由同
    /// `commit_selected`：词库前缀检索在简体域。
    ///
    /// 调用前 `commit_candidate` 已清空缓冲与候选。门槛再验一遍「主输入态、无已转换段、
    /// 缓冲空」——联想态的互斥不变式要的正是这个形状，任何一条不满足就只上屏不联想。
    ///
    /// ⚠️ **顶码上屏不走这里**：顶码之后余码留在缓冲里继续组码，那一刻不是「上屏完毕」，
    /// 与联想态「缓冲为空」的前提互斥。
    pub(crate) fn auto_commit_then_assoc(
        &self,
        state: &mut State,
        out: String,
        assoc_ctx: &str,
    ) -> KeyAction {
        if state.active.is_none()
            && state.committed_text.is_empty()
            && state.input_buffer.is_empty()
            && self.maybe_enter_assoc(state, assoc_ctx, "")
        {
            self.notify_ui_update(state);
            // 与 `commit_selected` 同：上屏后重开占位组合，联想态才收得到后续按键。
            return self.commit_then_new_composition(out, self.assoc_composition().to_string());
        }
        self.notify_ui_hide();
        Self::commit_action(out, true)
    }

    /// 上屏一条**联想候选**之后，还要不要再联想一轮。
    ///
    /// 一次性档：不续（出完这一轮就结束）。持续档：续——「中」→「中国」→ 再找以
    /// 「中国」开头的词。判据是「刚上屏的那条是不是联想来的」，故由调用方把候选来源传进来。
    pub(crate) fn assoc_may_chain(&self, from_assoc: bool) -> bool {
        !from_assoc || self.assoc_config().mode == AssocMode::Continuous
    }
}

/// 读入并解析下一词表（后台线程上调，见 `Coordinator::ensure_assoc_bigram_async`）。
fn load_assoc_bigram(
    path: &std::path::Path,
) -> Option<std::sync::Arc<wind_assoc::bigram::AssocBigram>> {
    let loaded = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(wind_assoc::bigram::AssocBigram::parse);
    match loaded {
        Ok(t) => {
            tracing::info!(n = t.len(), path = %path.display(), "联想下一词表已加载");
            Some(std::sync::Arc::new(t))
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "联想下一词表读不了，本次运行不出: {e}");
            None
        }
    }
}

#[cfg(test)]
#[path = "handle_assoc_tests.rs"]
mod tests;
