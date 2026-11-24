//! 联想：上屏之后按刚上屏的内容给出下一批候选。设计见 `docs/design/association.md`。
//!
//! 联想 = 正常输入态，只是没有输入缓冲：候选住在 `state.candidates`、`state.active` 仍是 `None`，
//! 翻页、高亮、二三候选键、数字选词、Esc、鼠标点选天然全部适用。凡以输入码为 key 的加工
//! （词频、造词、调序）靠 [`CandidateSource::Assoc`] 跳过。不要另立一份联想候选字段（§2）。
//!
//! 需要主动接的只有几处：退格与回车（[`Coordinator::assoc_enter`] / [`Coordinator::assoc_backspace`]）、
//! 其余落到透传的键（[`Coordinator::assoc_release_on_passthrough`]，按动作而非键名收口）、
//! `try_activate_mode` 的让位与临英门、标点不顶屏。已知缺口：语言栏中/英按钮与系统 Ctrl+Space
//! 在联想态下回 `StatusUpdate`，不结束占位组合（设计文档 §3）。
//!
//! 联想态必须在宿主里挂一个占位组合（[`Coordinator::assoc_composition`]）：TSF 的同步判据
//! `HasActiveComposition()` 才能保证后续按键送达，异步回填的候选镜像赢不了下一次
//! `OnTestKeyDown` 的竞速（§3.2）。
//!
//! 候选生成在 `wind-assoc`（纯函数），何时展示与退出在这里，因为依赖宿主事件。

use crate::coordinator::{Coordinator, State};
use wind_assoc::{
    AssocConfig, AssocContext, AssocHit, AssocKind, AssocMode, AssocProvider, AssocSource,
};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::{Candidate, CandidateSource};
use wind_keys::keymap;

/// 联想态显式退出的原因，只用于日志。
///
/// 只列主动调 [`Coordinator::exit_assoc`] 的路径；绝大多数退出是隐式的：谁清空 / 重填了候选，
/// 联想就自然没了。
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
/// 必须独立于首显 timer：联想态与首显等待会同时存在，共用单槽等于互相取消。
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
    /// 真相源就是候选自己，没有第二份状态，因此天然不会失步（见模块文档）。
    pub(crate) fn assoc_active(&self) -> bool {
        self.candidates
            .first()
            .is_some_and(|c| c.source == CandidateSource::Assoc)
    }
}

impl Coordinator {
    /// 联想态挂在宿主里的占位组合内容（为什么必须有组合见模块文档）。
    ///
    /// 默认复用 [`Coordinator::composition_placeholder`]，compat 的 `assoc_placeholder` 可另指定
    /// （[`Coordinator::assoc_placeholder`]）。光标须落在占位符前面，否则插入点凭空右移一格；
    /// 这一半由 C++ 按「组合内容恰为占位符」判定。「联想输入」标识走候选窗编码栏，不流进宿主。
    pub(crate) fn assoc_composition(&self) -> &'static str {
        self.assoc_placeholder()
    }
}

/// 这个 VK 是不是修饰键本身（按下它不构成「用户做了别的事」）。
///
/// 不复用 `keymap::is_key_up_only_vk` / `is_pure_modifier_vk`：它们答的是「只有 keyup 到得了
/// 服务端」，是另一根轴，今天在桌面上恰好重合。本判据是给薄宿主的保险，而薄宿主正是最可能
/// 上报 Alt keydown 或未拆左右的笼统 VK 的那类，故就地写全。
fn is_modifier_like_vk(vk: u32) -> bool {
    // 0x10/0x11/0x12 = VK_SHIFT / VK_CONTROL / VK_MENU（笼统，未拆左右）
    // 0xA4/0xA5      = VK_LMENU / VK_RMENU
    // is_key_up_only_vk = 0xA0..=0xA3 四个纯修饰键 + VK_CAPITAL
    matches!(vk, 0x10 | 0x11 | 0x12 | 0xA4 | 0xA5) || keymap::is_key_up_only_vk(vk)
}

/// 数字键此刻是否进编码缓冲（而不是选候选）。
///
/// 只有移动端、英文方案、已在组码三者同时成立才进：英文词里常带数字（mp3、h2o、win10），
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
/// 显示整词、上屏只补剩余部分：`AssocHit::commit` 在这里填好，下游不再现算。
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
                    // 按名次给分：取数口已把三层词分档排好，三层权重量纲不同。
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

/// 联想历史的取数源：用户在这个上文之后从联想里选过的词。
///
/// 读 store 的 `assoc_history` 表（一次范围扫描）；写在 [`Coordinator::record_assoc_pick`]。
/// 分值 = 选中次数的衰减分（与 FREQ 同一套 `pinyin_score`），源内按它降序。
///
/// 读端过滤存在性：历史表不随删词级联清理，选过的词被删掉之后，这里按
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
        // 取全部行（limit=0）再过滤截名额：先截后滤会让已删的高分词永久占住名额。
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
                // 两类行共用这张表：前缀延伸（「荷」→「荷枪实弹」，只补剩余部分）与下一词
                // （「我们」→「一起」，整词上屏，写入端 `learn_next_word`）。与上文相同的行丢掉。
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

    /// 选中联想候选后记一笔联想历史。
    ///
    /// 只记前缀延伸类的选择（有 `commit_override`：显示整词、上屏补剩余）——上文就是
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
    /// 调用点遍布各条按键分派臂，故必须幂等且极廉价。
    pub(crate) fn exit_assoc(&self, state: &mut State, why: AssocExit) -> bool {
        if !state.assoc_active() {
            return false;
        }
        tracing::debug!(reason = ?why, n = state.candidates.len(), "退出联想态");
        state.clear_comment_flips();
        state.candidates.clear();
        state.preedit.clear();
        self.reset_candidate_view(state);
        // 作废未触发的自动隐藏计时，否则它会在下一轮联想里提前把窗收掉。
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
            // 上屏这一瞬间上文必然连续；断链发生在失焦 / 切窗等别的入口，那些路径直接 `exit_assoc`。
            boundary_broken: false,
        };
        let punct = wind_assoc::punct::PunctRules;
        let prefix = PrefixWords {
            mgr: &self.engine_mgr,
            // 不能直接用 `active_schema_id()`：混输方案自己没有词库，词语联想会静默为空。
            schema: self.engine_mgr.assoc_word_schema(),
        };
        // 一律登记全部已实现的源，档位过滤只在 `AssocConfig::source_enabled` 一处做。
        // Bigram 数据文件缺失时不登记，配额顺延。
        let history = self.store.as_deref().map(|store| HistoryWords {
            store,
            mgr: &self.engine_mgr,
            word_schema: self.engine_mgr.assoc_word_schema(),
            schema: self.assoc_data_schema(),
            profile: self.engine_mgr.pinyin_freq_profile(),
            next_words: cfg.kind == AssocKind::Smart,
        });
        // 这里只管读不读表（出不出场仍由 `associate` 判）：表常驻约 2 MB，词语联想档用不上它。
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
        // 拼音「屏蔽这个词」在各源之内滤掉，空出的配额由同源后面的补上；滤在汇聚之后会少条数。
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
    /// 调用前编码缓冲与常规候选必须已清空（互斥不变式，见
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
        // 填进 `candidates` 而不是另建一份列表（见模块文档）。
        state.candidates = hits
            .into_iter()
            .map(|h| Candidate {
                // 显示整词，上屏可能只补剩余部分（词语联想）。
                commit_override: h.commit.filter(|c| *c != h.text),
                text: h.text,
                // 唯一的身份标记：以输入码为 key 的加工据此跳过。
                source: CandidateSource::Assoc,
                ..Default::default()
            })
            .collect();
        self.reset_candidate_view(state);
        // 编码栏标识「联想输入」，只在非嵌入模式给：嵌入模式下候选窗没有编码栏，凭空多一栏会让
        // 窗口高度一跳，是有意放弃。取有效归属，候选窗被宿主压住时（UI-less 游戏）同样给空串。
        //
        // 这个串绝不能流进宿主 composition（宿主拿到的恒是 [`Self::assoc_composition`]）。
        // `state.preedit` 的消费者很多，今天够不着只因它们都要求非空缓冲，这不足为凭；压制态下
        // 装空串，日后给 `UiElementPage` 补编码串字段（见 `docs/design/game-compat-tsf-uielement.md`
        // §4.4）也不会把它送进游戏聊天框。
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
    /// 用独立 timer 而非首显那个，理由见 `AssocHideTimer`。
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
        // 本函数在定时器线程上，没有按键应答可搭载收口，宿主里的占位组合成了孤儿。不处理的话
        // 下一键会被 TSF 判「有会话」吃下、而服务端回 `PassThrough`，「吃了再吐」丢键且占位空格
        // 被 finalize 进文档。无占位（`none`）时没有孤儿，只需告知 DLL 会话没了。
        if self.assoc_placeholder_is_none() {
            self.push_hotkey_session_if_changed();
            self.notify_ui_hide();
            return;
        }
        self.assoc_placeholder_orphaned
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // 标记与主动 push（[`Self::push_end_composition`]）各管一半：push 覆盖用户不按键直接
        // 点鼠标或切窗的情形，但不保证成功，故不据此撤标记；标记由下一次按键在
        // [`Self::adopt_orphaned_placeholder`] 收口。push 成功时后续透传键不会被 TSF 吃下，
        // 标记只会被下一次 `UpdateComposition` 顺手撤掉。
        self.push_end_composition();
        self.notify_ui_hide();
    }

    /// 宿主告知「用户正在操作联想候选」，期间暂停自动隐藏。
    ///
    /// 计时只认按键，触屏上滑候选条、长按看提示、开展开面板都不是按键，会被计时中途收掉。
    /// 桌面不调用本函数。
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

    /// 在按键处理的唯一出口（`handle_key_event_policed`）上，把孤儿占位组合搭上这一次按键收掉，
    /// 天然覆盖全部返回点。
    ///
    /// 按「这个动作到了宿主那边会不会碰组合」分三种命运（穷尽 match 强制新变体表态）：
    /// - 结束或替换组合：孤儿被顺带收掉，撤标记、动作原样发出；
    /// - 交还按键却不碰组合（`PassThrough` / `NotHandled`）：改判成
    ///   [`KeyAction::ClearCompositionThenPassThrough`]；
    /// - 完全不碰组合：孤儿还在，标记留到下一次按键（任何按键都撤会让先按热键的用户收不掉组合）。
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
            // `StatusUpdate` 不结束组合（见 `schema_switch_key_action`）；`MoveCursorRight` /
            // `DeletePair` 在 C++ 侧也不碰组合（`KeyEventSink.cpp`）。
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

    /// 联想态下落到透传的那一键：收掉占位组合，并把这一键原样交还宿主。
    ///
    /// 占位组合让 TSF 的 `_HasInputSession()` 为真，Del / Home / End / 左右等被吃下转发，而协调器
    /// 这边联想态缓冲为空、一路落到 `PassThrough`，即「吃了再吐」。按动作而非键名收口（键名数不清，
    /// Tab 又归辅助码触发键），与 [`Self::adopt_orphaned_placeholder`] 同址。这批键移动了光标或改了
    /// 文档，上文已断，顺带收窗是正解；不配开关。
    ///
    /// 守卫：只在 keydown（守在调用点）；修饰键本身除外（否则 Shift+字母的第一下就收窗）；不在
    /// 联想态时原样返回。判联想态要取 state 锁，但不加原子镜像：真相源是候选自身。Ctrl/Alt 组合
    /// 刻意不排除。「唯一出口」不覆盖移动端；空码补全同病但刻意不管；skip 表曝光面等见
    /// `docs/design/association.md` §3.3.1。
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

    /// 收掉联想：清候选、结束占位组合。用于退格与回车：它们的既有分支在联想态会落到
    /// `PassThrough`，把占位组合悬在宿主里。不顺手上屏高亮那条：两者是终结性动作，联想的高亮
    /// 是输入法猜的。
    ///
    /// `cancels_only = true` 时吃掉这一键，`false` 时连同收窗把键交还宿主
    /// （[`KeyAction::ClearCompositionThenPassThrough`]）；取值见 [`Self::assoc_enter`] /
    /// [`Self::assoc_backspace`]。
    fn assoc_dismiss_with(&self, state: &mut State, cancels_only: bool) -> KeyAction {
        self.exit_assoc(state, AssocExit::Dismiss);
        self.notify_ui_hide();
        if cancels_only {
            KeyAction::ClearComposition
        } else {
            KeyAction::ClearCompositionThenPassThrough
        }
    }

    /// 联想态下这一键若是二三候选键，是否让出（不选联想、按「联想窗不在」处理）。
    ///
    /// `input.association.select_keys_commit = false` 时成立（对标 QQ 五笔）。
    /// 只给 keydown 消费的可打印选词键用（主路符号臂与字母臂）：让出后那一键照常往下走，
    /// 由既有分支收联想并出字 / 进模式——与联想态的其它标点同一条路，这里不另写出口。
    pub(crate) fn assoc_yields_select_key(&self, state: &State) -> bool {
        state.assoc_active() && !self.assoc_select_keys_commit()
    }

    /// 联想态的回车处置（`input.association.enter_cancels_only`，默认 `false` = 透传）。
    ///
    /// 默认让回车穿过去：它是终结性动作，用户按它是要发送/换行，而联想窗是输入法自己弹的、
    /// 用户并没在选词。让一个「建议」吞掉一次回车，正事就被挡住了。
    pub(crate) fn assoc_enter(&self, state: &mut State) -> KeyAction {
        let cancels_only = self.assoc_flag(|a| a.enter_cancels_only, |m| m.enter_cancels_only);
        self.assoc_dismiss_with(state, cancels_only)
    }

    /// 联想态的退格处置（`input.association.backspace_cancels_only`，默认 `true` = 吃键）。
    ///
    /// 与回车默认相反是刻意的：退格透传会删掉刚上屏的字，不可逆，误触代价远大于多按一次键。
    pub(crate) fn assoc_backspace(&self, state: &mut State) -> KeyAction {
        let cancels_only =
            self.assoc_flag(|a| a.backspace_cancels_only, |m| m.backspace_cancels_only);
        self.assoc_dismiss_with(state, cancels_only)
    }

    /// 引擎替用户做主上屏（满码唯一自动上屏 `InputOutcome::AutoCommit`）的出口：上屏 `out`，
    /// 并接上与 `commit_selected` 整串分支同一段联想接线，否则自动上屏的五笔用户没有联想。
    ///
    /// `assoc_ctx` 取简体域的上屏文本（未经简繁转换、未补空格），理由同
    /// `commit_selected`：词库前缀检索在简体域。
    ///
    /// 调用前 `commit_candidate` 已清空缓冲与候选。门槛再验一遍「主输入态、无已转换段、
    /// 缓冲空」——联想态的互斥不变式要的正是这个形状，任何一条不满足就只上屏不联想。
    ///
    /// 顶码上屏不走这里：顶码之后余码留在缓冲里继续组码，那一刻不是「上屏完毕」，
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

    /// 上屏一条联想候选之后，还要不要再联想一轮。
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
