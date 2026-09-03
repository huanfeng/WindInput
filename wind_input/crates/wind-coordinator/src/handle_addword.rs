//! 造词 / 加词：选中后自动造词、命令栏 dict.add 加用户词。
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。

use crate::coordinator::{Coordinator, LEARN_ADD_WEIGHT, State};
use tracing::{debug, warn};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::CandidateSource;
use wind_ipc::protocol::MOD_CTRL;
use wind_keys::keymap;
use wind_ui_types::CandidateItem;
use wind_ui_types::UiCommand;
use wind_ui_types::{ToastKind, ToastPosition};

/// 最小加词长度
const ADD_WORD_MIN_LEN: usize = 1;
/// 默认加词长度
const ADD_WORD_DEFAULT_LEN: usize = 2;
/// 最大加词长度。
///
/// ★ 判据是**方案 encoder 规则的上界**，不是一个凑整的数：码表方案的词组码由
/// `[[encoder.rules]]` 从单字全码组装，出厂 wubi86 的最后一条规则是
/// `length_in_range = [4, 10]` ⇒ **超过 10 字没有任何规则匹配，`calc_word_code` 直接
/// 出不了码**。2026-09-03 真机实测：面板里选到 11 字就恒显示「无法计算编码」。
///
/// 原值 20（对齐 Go 默认上限）于是有一半是空转：11–20 字选得出来、却注定加不进去，
/// 用户要按着 ↓ 一路减到 10 才看见编码出现。把上限收到真实能力边界上，「选得出来的
/// 都加得进去」才成立。
///
/// ⚠️ 两处不精确，都是**有意**接受的：
/// - 拼音方案的码来自读音、不走 encoder 规则，本可以更长——但 11 字以上的「词」实际上
///   是句子，为它把两族方案的上限拆成两个值不划算。
/// - 第三方码表方案若把规则写到 `[4, 20]`，这里仍拦在 10。真要跟随得让上限随方案动态
///   算（面板 ↑↓ 上限、剪贴板准入、`check_derivable_word` 三处都得跟着变），等真出现
///   那样的方案再说。
const ADD_WORD_MAX_LEN: usize = 10;
/// 手动加词默认权重（略高于系统词库归一化中位 1000，对齐 Go addWordMaxWeight）
const ADD_WORD_WEIGHT: i32 = 1200;
/// 临时词库淘汰的检查间隔（每 N 次造词写入检查一次）。全表扫描代价高，而上限是软约束，
/// 略微超出无害，故不必每次都查。
const EVICT_CHECK_INTERVAL: usize = 64;

/// 自动造词超长裁剪：从尾部保留整段（最近输入优先）使合并字数 ≤ max_chars。
/// 返回保留区间的起始段索引；max_chars=0 不限（返回 0）。
fn trim_segs_start(
    segs: &[(String, String, String, wind_candidate::CandidateSource, u64)],
    max_chars: usize,
) -> usize {
    if max_chars == 0 {
        return 0;
    }
    let mut total = 0;
    let mut start = segs.len();
    for (i, (_, _, t, _, _)) in segs.iter().enumerate().rev() {
        let n = t.chars().count();
        if total + n > max_chars {
            break;
        }
        total += n;
        start = i;
    }
    start
}

/// 汉字判定：只认表意文字区。**刻意排除全角标点（U+FF00–FFEF）与中文标点（U+3000–303F）**
/// ——那些是造词的**终止符**而非素材。若用 `c >= 0x3400` 这种粗判据，全角逗号 U+FF0C 会被
/// 当成汉字混进词里。
/// ⚠️ 与 `wind_candidate::is_han`（常用性判定域）**刻意不同源**：那边把部首、笔画一并纳入
/// （它们在码表里占着汉字编码出现），这边不能——部首不是造词素材。补充平面的处理则一致。
fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF      // 基本区
        | 0x3400..=0x4DBF    // 扩展 A
        | 0xF900..=0xFAFF    // 兼容表意文字
        // 平面 2（SIP）/ 平面 3（TIP）整体：两个平面专用于表意文字，扩展 B–J 与兼容汉字
        // 补充全在其中，将来的扩展 K/L 亦然。原先逐块列举到 `0x323AF`（扩展 H 末尾），
        // 漏掉扩展 I（2EBF0–2EE5F）与 Unicode 17 新增的扩展 J（323B0 起）——那批字**加不了词**。
        | 0x20000..=0x3FFFF)
}

/// `dict.add` 入库文本规整：只去首尾空白，**不做一行化 / 截断**。
///
/// 来源可能是剪贴板整段文本（`coad`）。把一大段悄悄截成前 N 字入库，比直接拒绝更糟——
/// 用户不知道自己加了什么，词库还多一条打不出来的垃圾。故规整只做无损清理，剩下的
/// 不合格情形一律拒绝并提示（见 `check_derivable_word`）。
fn sanitize_dict_add_text(raw: &str) -> anyhow::Result<&str> {
    let s = raw.trim();
    if s.is_empty() {
        anyhow::bail!("内容为空");
    }
    if s.contains(['\r', '\n']) {
        anyhow::bail!("内容含换行，请只复制一个词");
    }
    Ok(s)
}

/// 推导编码路径**专属**的额外校验。
///
/// 编码由方案规则从单字全码组装（见 `calc_add_word_code`），只有汉字词推得出来；长度上限
/// 对齐加词界面的 [`ADD_WORD_MAX_LEN`]。**显式给了 code 的调用方不走这里**——那是用户
/// 明确意图（可能加颜文字、外文等无法自动取码的词条），套上这些守卫是回归。
fn check_derivable_word(word: &str) -> anyhow::Result<()> {
    let n = word.chars().count();
    if n > ADD_WORD_MAX_LEN {
        anyhow::bail!("内容过长（{} 字，上限 {}）", n, ADD_WORD_MAX_LEN);
    }
    if !word.chars().all(is_han) {
        anyhow::bail!("含非汉字，无法自动取码");
    }
    Ok(())
}

/// 加词编码的**展示/交换形态**：扁平 key + boundary → 带音节空格的音节码（`ni hao`）。
///
/// 编码有两个域：存储域是「扁平 key + boundary bitmask」（前缀查询的命脉——`nihao` 必须
/// 被 `niha` 匹配到，带空格的串一旦成了 key 就再也查不出来），展示/交换域是带空格串。
/// 设置页的词库列表（`webdata::word_item`）与「出码」按钮（`dict.encode`）早已统一到后者，
/// 唯独加词这三处（候选窗预览 / Ctrl+Enter 转设置页 / Ctrl+Shift+= 直开）此前只传扁平串，
/// 于是同一个加词窗里预填显示 `nihao`、用户点一下「出码」却变成 `ni hao`，自相矛盾。
///
/// 回写方向由设置端 `webdata::normalize_add_code` 兜住：带空格的码提交回来会被拆回扁平
/// key，且串里的空格**被当作显式声明的切分直接采信**，比「手输码 == 推导码才借用边界」的
/// 兜底更准。命令行参数侧亦已就绪——`build_settings_args` 对含空白的值加引号。
///
/// 码表码 boundary 恒 0 → 原样返回，本函数对非拼音方案是恒等变换。
fn display_code(code: &str, boundary: u64) -> String {
    wind_store::wdict::join_code_by_boundary(code, boundary)
}

/// 加词界面的默认上下文：设置端在**没有预填参数**时据此把窗口填成可用状态。
///
/// 存在的理由是 `wind-setting --add-word` 这条裸入口：它不经输入法热键，拿不到方案，也
/// 拿不到最近输入，此前开出来是一个方案为空的窗——`dict.encode` / `dict.add` 都会失败，
/// 界面看着正常却什么也做不了。
pub struct AddWordContext {
    /// 加词目标方案 id（混输已解析到主码表方案，见 `add_word_target_schema`）。
    pub schema_id: String,
    /// 最近上屏文本，**取字符池上限那么多**（[`ADD_WORD_MAX_LEN`]），不是默认词长。
    ///
    /// ★ 判据来自「加词是为了打不出来的词」：一个词若能整段一次上屏，说明词库里已经有
    /// 它，根本不需要加。真正要加的词恰恰是**逐字/分段上屏**的，散落在多条上屏记录里，
    /// 取末尾两字往往只截到半个词。设置端的词条框是多行的，多给的部分删起来很便宜，
    /// 少给却要用户回到输入法重打一遍。
    pub recent_text: String,
    /// 加词字数上限（[`ADD_WORD_MAX_LEN`]）。
    ///
    /// 一并交代出去，是因为设置端也会自己读剪贴板填词条，而**上限是它够不着的**——
    /// wind-setting 不能依赖 core 的 crate（那边是 dev-dependency）。不给的话它只能猜一个
    /// 数或者干脆不截断，于是填进去 15 字、点「生成编码」出不来、点「确定」被
    /// `check_derivable_word` 拒掉，又是一个「填得进去、加不进来」。
    pub max_len: usize,
}

/// toast 回显用的词截断（按字符，超出加省略号）。词本身最长受 [`ADD_WORD_MAX_LEN`] 约束，
/// 但显式 code 路径不限长，故回显仍需兜底，避免 toast 被撑爆。
fn toast_clamp(word: &str) -> String {
    const MAX: usize = 16;
    if word.chars().count() <= MAX {
        return word.to_string();
    }
    word.chars().take(MAX).collect::<String>() + "…"
}

impl Coordinator {
    // ──────────────────────────────────────────────────────────────────────
    // 码表自动造词：连续单字 + 终止信号 = 自动组词
    // 状态机在 `crate::auto_phrase`（纯逻辑）；此处只做接线与 IO。
    // ──────────────────────────────────────────────────────────────────────

    /// 上屏后处理：**自提交打点** + 喂码表造词缓冲。
    ///
    /// 收口在 `handle_key_event_policed`（而非 `commit_action`）——后者不是唯一出口，
    /// 另有约 10 处直接构造 `InsertText` 的路径（顶码/智能符号/临拼等），散点打点必漏。
    /// 与 `record_input_stats` 同一收口思路。
    pub(crate) fn note_commit_action(&self, action: &KeyAction) {
        // 撤销上屏窗口（`ime.undo_commit`）：记录本次按键「同步落到光标前」的字符数（UTF-16
        // 单元，对齐 ReplaceBackward 删除量纲）。覆盖一切确定落屏的返回变体——每次上屏都顶掉
        // 上一次计数，故 undo 只精准删「刚输入完那次」；英文/标点逐键上屏自然把计数刷回 1，
        // 不会残留更早的中文整词计数致误删。组合态（PassThrough/UpdateComposition/
        // HoldComposition）尚未落屏，不动计数。本收口点覆盖 40+ 返回点，避免散点接线漏更新。
        let committed = match action {
            KeyAction::InsertText { text, .. }
            | KeyAction::InsertTextWithCursor { text, .. }
            | KeyAction::ReplaceBackward { text, .. } => text.as_str(),
            KeyAction::CommitAndHoldComposition { commit_text, .. }
            | KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.as_str(),
            _ => "",
        };
        let n = committed.encode_utf16().count();
        if n > 0 {
            self.last_commit_len
                .store(n, std::sync::atomic::Ordering::Relaxed);
        }

        // 打点无条件进行：它服务的是 SelectionChanged 的回声判别，与造词是否开启无关。
        //
        // ★ 判据必须与上面的 `committed` 同源——**凡是真落屏的文字，都会让宿主移动光标、
        // 回送一条 SelectionChanged**，与它由哪个 KeyAction 变体送出去无关。此前这里只认
        // `InsertText`/`InsertTextWithCursor` 两种，把 `ReplaceBackward` 和 `Commit*` 系
        // 全漏了（两个 match 就挨着写，清单却不一致）。
        //
        // 漏掉的后果（2026-09-02 五笔长按 d 实测，记事本）：满码自动上屏走
        // `CommitThenDeferComposition`（TSF 日志 `Processing CommitThenDefer: commit=大
        // defer=d`）⇒ 不打点 ⇒ 紧随其后的 SelectionChanged 被判成「用户移动光标」
        // （日志里 `since_self_commit=Some(162.9s)`）⇒ 清 `caret_cache_verified`
        // ⇒ 下一键信任门命中、arm 600ms 长兜底 ⇒ 而五笔 4 码一组、typematic 32ms 一键，
        // 组合寿命只有 ~128ms，600ms 的 timer **永远等不到到期**就被下次上屏
        // `reset_first_show` 作废 ⇒ **候选窗一次都不显示**。
        // 正是 [`Coordinator::arm_pending_first_show`] 记的那个「兜底超时长于组合寿命」
        // 死结，这次由一个漏打的点触发。
        if !committed.is_empty() {
            *self
                .last_self_commit
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        }

        // 造词只喂「整段新插入的文本」，刻意仍限这两种变体：`ReplaceBackward` 是回改已上屏
        // 的内容、`Commit*` 系的 commit_text 是上一段的收尾，都不是新起的一段输入。
        // 与上面的打点判据不同源是**有意**的，别顺手合并。
        let text = match action {
            KeyAction::InsertText { text, .. } | KeyAction::InsertTextWithCursor { text, .. } => {
                text.as_str()
            }
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        self.feed_auto_phrase(text);
    }

    /// 造词是否启用（码表/混输方案 + 开关开启）。拼音方案走 `[pinyin.auto_learn]` 的
    /// 选词即学路线，不进本缓冲。
    pub(crate) fn auto_phrase_enabled(&self) -> bool {
        !self.engine_mgr.is_pinyin() && self.engine_mgr.codetable_settings().auto_phrase.enabled
    }

    /// 把上屏文本喂给造词缓冲。
    ///
    /// - 全汉字单字 → 追加（混输下拼音打出的单字**同样计入**：编码在 flush 时由
    ///   `[[encoder.rules]]` 从字重算，段自身带什么码不影响结果，故来源无意义）
    /// - 全汉字多字词 → 终止（选了词组说明这不是散字序列）
    /// - 含非汉字（标点/英文/数字/空格）→ 终止
    fn feed_auto_phrase(&self, text: &str) {
        if !self.auto_phrase_enabled() {
            return;
        }
        let all_han = text.chars().all(is_han);
        let now = std::time::Instant::now();
        let idle = self.auto_phrase_idle_timeout();
        let flushed = {
            let mut buf = self.auto_phrase.lock().unwrap_or_else(|e| e.into_inner());
            if all_han {
                buf.on_commit(text, now, idle)
            } else {
                // 非汉字上屏 = 终止符，且该文本自身不入缓冲。
                buf.terminate()
            }
        }; // 锁在此释放：flush 要做词库 IO，不可持缓冲锁。
        if let Some(seq) = flushed {
            // 与 `terminate_auto_phrase` 的「终止信号」日志对齐：本路径同样会造词，
            // 缺了它排查时会看到「凭空出现的已造词」，误以为触发源丢了。
            //
            // ⚠️ **三条来源必须分开写**。`flushed` 为 `Some` 有两个出处：多字词终止
            // （`AutoPhraseBuf::on_commit` 的 `!is_single`）与**单字 idle 超时**（`stale`），
            // 两者 `all_han` 同为 true。原先只按 `all_han` 二分，于是超时被一律打成
            // 「多字词上屏」——真机日志里一条单字上屏的记录长得和词组上屏一模一样。
            // 这不是措辞瑕疵而是**会把排查带向相反结论**：`aqgy` 那次事故里，按 Y 上屏的
            // 是单字「葡」，日志却说「多字词上屏」，照字面读会得出「第 4 码上屏了词组」。
            //
            // 判据无需回改 `on_commit` 签名：`all_han` 且 `text` 为单字时，能走到这里就
            // 只可能是 stale 分支（单字未超时恒返回 `None`，压根进不来）。
            if !all_han {
                debug!("auto-phrase: 终止信号 非汉字上屏 → flush {} 字", seq.len());
            } else if text.chars().count() > 1 {
                debug!("auto-phrase: 终止信号 多字词上屏 → flush {} 字", seq.len());
            } else {
                // 语义与上面两条**不同**：序列不是被终止，而是超时截断后本字另起一段。
                debug!(
                    "auto-phrase: 空闲超时（间隔 > {:?}）→ flush {} 字，本字另起新序列",
                    idle,
                    seq.len()
                );
            }
            self.flush_auto_phrase(&seq);
        }
    }

    /// 终止信号统一入口（标点/回车/空格/焦点丢失/IME 停用/模式切换/光标移动）。
    /// `reason` 只进 DEBUG 日志，便于排查「词为什么没造出来 / 为什么被切断」。
    pub(crate) fn terminate_auto_phrase(&self, reason: &str) {
        if !self.auto_phrase_enabled() {
            return;
        }
        let flushed = {
            let mut buf = self.auto_phrase.lock().unwrap_or_else(|e| e.into_inner());
            buf.terminate()
        };
        if let Some(seq) = flushed {
            debug!("auto-phrase: 终止信号 {} → flush {} 字", reason, seq.len());
            self.flush_auto_phrase(&seq);
        }
    }

    /// idle 超时（连续单字最大间隔）。0 = 用默认 5s。
    fn auto_phrase_idle_timeout(&self) -> std::time::Duration {
        let ms = self
            .rt()
            .config
            .schema
            .codetable
            .auto_phrase
            .idle_timeout_ms;
        if ms == 0 {
            crate::auto_phrase::DEFAULT_IDLE_TIMEOUT
        } else {
            std::time::Duration::from_millis(ms as u64)
        }
    }

    /// 对吐出的字序列造词：长度策略 → 取码 → 查重 → 写临时层 → 晋升判定 → 淘汰。
    fn flush_auto_phrase(&self, seq: &[char]) {
        let ap = self.engine_mgr.codetable_settings().auto_phrase;
        let Some(word) =
            crate::auto_phrase::word_from_seq(seq, ap.min_phrase_len, ap.max_phrase_len)
        else {
            return; // 太短或超长（超长整体放弃，不切末尾 N 字——中间切一刀多半是杂词）
        };
        let active = self.engine_mgr.active_schema_id();
        // 出码方案与入库方案是**两个不同的 id**（对齐 `add_word_target_schema` 的既有区分）：
        // 出码要真实方案（读它的 [[encoder.rules]] 与码表词库），入库要数据方案（混输折叠到主码表）。
        let encode_schema =
            if self.engine_mgr.schema_engine_type(&active).as_deref() == Some("mixed") {
                match self.engine_mgr.mixed_primary_schema(&active) {
                    Some(s) => s,
                    None => {
                        debug!("auto-phrase: 混输方案主码表缺失，跳过造词");
                        return;
                    }
                }
            } else {
                active.clone()
            };
        // ★★★ 索引未就绪时**跳过本次造词**并后台预热，两条理由缺一不可：
        //
        // ① 不能在此现建：本函数跑在上屏（按键）线程上，而单字全码表与反查索引都是
        //    惰性全量构建，大词库上是秒级——TSF→服务同步 IPC，那一等就是整机卡顿。
        // ② **更不能把「没就绪」当成「查不到」继续往下走**：下面的查重①靠
        //    `word_codes_in` 判断系统词库是否已有这个「码+词」。拿空结果去判，
        //    `"".split('/')` 产出 `[""]`，永远不等于非空的 code ⇒ 去重判据**静默失效**
        //    ⇒ 往临时层写入一条系统词库本就有的重复条目。那不是「这一屏少显示点东西」，
        //    而是**写进 redb 的持久错误**：候选出现重复项，且该条目计入提升计数、
        //    可能被 `maybe_promote_temp` 永久固化进用户词库。
        //
        // 自动造词是机会性功能，丢掉这一次完全无感；下次上屏时通常已就绪。
        if self
            .engine_mgr
            .reverse_index_if_ready(&encode_schema)
            .is_none()
            || !self.engine_mgr.single_char_codes_ready(&encode_schema)
        {
            debug!("auto-phrase: 词库索引未就绪，跳过本次造词并后台预热");
            self.ensure_word_encoding_async(&encode_schema);
            return;
        }
        let code = match self.engine_mgr.encode_word(&encode_schema, &word) {
            Ok(c) => c,
            Err(e) => {
                // DEBUG 级可带具体字符（CLAUDE.md 隐私规则：INFO 及以下不得带）。
                // 这条是排查「自动造词不生效」最关键的线索——通常是某个字在码表里没有全码。
                debug!("auto-phrase: 取码失败，整词作废（{}）: {}", word, e);
                return;
            }
        };
        // 查重①系统词库：反查索引给的是该词在词库里的**实际**编码列表（`a/ab/abc`），
        // 命中同码即说明系统库已收录这个「码+词」，不必再造。
        // 上面的就绪闸保证了这里的 `None` 不可能是「索引没建好」，故 `unwrap_or_default`
        // 是安全的——它只会在「方案 id 为空」时兜底，而那种情况下本来也无从查重。
        let existing = self
            .engine_mgr
            .word_codes_in(&encode_schema, &word)
            .unwrap_or_default();
        if existing.split('/').any(|c| c == code) {
            debug!("auto-phrase: 系统词库已有 {} -> {}，跳过", code, word);
            return;
        }
        let Some(store) = &self.store else { return };
        let Some(schema) = self
            .engine_mgr
            .write_data_schema_id(&active, CandidateSource::CodeTable)
        else {
            debug!("auto-phrase: 无法归属入库方案，跳过造词");
            return;
        };
        // 查重②用户词库：同「码+词」已存在则不再写临时层（否则候选会出现重复项）。
        if let Ok(recs) = store.get_user_words(&schema, &code)
            && recs.iter().any(|r| r.text == word)
        {
            debug!("auto-phrase: 用户词库已有 {} -> {}，跳过", code, word);
            return;
        }
        // 码表词组码无音节边界语义 → boundary=0（消费方降级回 DAG）。
        match store.learn_temp_word(&schema, &code, &word, LEARN_ADD_WEIGHT, 0) {
            Ok(count) => {
                debug!("auto-phrase: 已造词 {} -> {} (count={})", code, word, count);
                self.maybe_promote_temp(store, &schema, &code, &word, count, ap.promote_count);
                self.maybe_evict_temp(store, &schema);
            }
            Err(e) => warn!("auto-phrase: 写临时词库失败: {}", e),
        }
    }

    /// 临时词库上限淘汰。按写入次数节流——每次造词都全表扫描代价过高，而上限本身
    /// 是软约束（略微超出无害）。`max_entries = 0` 视为不限。
    fn maybe_evict_temp(&self, store: &wind_store::Store, schema: &str) {
        let max = self
            .rt()
            .config
            .schema
            .codetable
            .auto_phrase
            .temp_max_entries;
        if max == 0 {
            return;
        }
        let n = self
            .auto_phrase_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if !n.is_multiple_of(EVICT_CHECK_INTERVAL) {
            return;
        }
        match store.evict_temp_words(schema, max) {
            Ok(k) if k > 0 => debug!("auto-phrase: 临时词库淘汰 {} 条（上限 {}）", k, max),
            Ok(_) => {}
            Err(e) => warn!("auto-phrase: 临时词库淘汰失败: {}", e),
        }
    }

    /// 加词到用户层。`code` 为空时**按当前方案规则推导**——兑现 `dict.add` 注册签名里
    /// 「code 可选, 不传时按当前方案规则推导」的承诺（见 `wind-cmdbar` funcs/dict_ime.rs）。
    /// 系统短语 `coad`（`$CC("剪贴板加词:{clip()}", dict.add(clip()))`）正是照该契约写的，
    /// 而推导那一半此前从未实现，于是 `coad` 恒在下面第一道守卫处失败、且只有一句 warn
    /// 日志、用户侧完全无感——「选了没反应」即由此而来。
    ///
    /// 成功弹一次 toast 回显入库的词——这条具体反馈只有本函数给得出（通用失败路径
    /// 拿不到词本身）。
    ///
    /// **失败不在此弹**：动作链的失败反馈统一由 [`Coordinator::run_command_candidate`]
    /// 收口，本函数只把错误原样上抛。职责这样切分是为了不重复弹窗——本函数唯一的
    /// 非测试调用点就是 cmdbar 的 `DictService::add`，两处都弹会得到两个 toast。
    pub(crate) fn cmd_dict_add(&self, text: &str, code: &str) -> anyhow::Result<()> {
        let word = self.try_dict_add(text, code)?;
        self.show_toast(
            &format!("已加词：{}", toast_clamp(&word)),
            ToastPosition::BottomCenter,
            ToastKind::Success,
        );
        Ok(())
    }

    /// `cmd_dict_add` 的实际逻辑，成功返回入库的词（供 toast 回显）。
    ///
    /// 隐私红线（docs/logging-convention.md）：返回的错误会被 cmdbar 侧包成 `dict.add: …`
    /// 打进 **warn** 日志，故消息内**不得含词本身或剪贴板内容**；词只出现在 toast（UI）
    /// 与 debug 日志里。消息也不再自带 `dict.add:` 前缀——`runtime_err` 已负责加，
    /// 重复会得到 `dict.add: dict.add: …`。
    fn try_dict_add(&self, text: &str, code: &str) -> anyhow::Result<String> {
        let Some(store) = &self.store else {
            anyhow::bail!("无词库存储");
        };
        let word = sanitize_dict_add_text(text)?;
        let (code, boundary) = if code.is_empty() {
            check_derivable_word(word)?;
            // 与快捷加词（Ctrl+=）同一套推导：拼音方案走引擎词级消歧、无果回退逐字反查；
            // 码表方案按方案 `[[encoder.rules]]` 从单字全码组装。boundary 一并取回——
            // 只有拼音那条路给得出音节边界，透传后消费方才不必降级回 DAG。
            let (c, b) = self.calc_add_word_code(word);
            if c.is_empty() {
                // 具体是哪个字没码由 calc_add_word_code 内部 debug 日志给出。
                anyhow::bail!("当前方案取不出编码");
            }
            (c, b)
        } else {
            // 调用方显式给码：扁平串，无音节边界表达 → boundary=0，消费方降级回 DAG。
            (code.to_string(), 0)
        };
        let active = self.engine_mgr.active_schema_id();
        // 加词按码表语义归属：混输落主码表方案，primary 缺失则报错不静默写孤儿。
        // **这与推导侧必须同步**——`calc_add_word_code` 经 `add_word_target_schema()` 对混输
        // 同样解析到 primary 码表方案，故推导出的恒是码表码，与此处 `CodeTable` 一致；
        // 非混输方案 `write_data_schema_id` 直接返回 data id、忽略 source，拼音方案照样正确。
        // 改动任一侧的方案解析都要回头核对另一侧，否则会把拼音码写进码表词库。
        let Some(schema) = self
            .engine_mgr
            .write_data_schema_id(&active, CandidateSource::CodeTable)
        else {
            anyhow::bail!("混输方案主码表缺失，无法归属加词");
        };
        store.add_user_word(&schema, &code, word, ADD_WORD_WEIGHT, boundary)?;
        debug!("dict.add: 已加词 {} -> {}", code, word);
        Ok(word.to_string())
    }

    /// 拼音自动造词（L）：用户**分步**组成（committed_segs ≥2 段），
    /// **或**一次选中引擎合成的**整句**（单段，`single_is_synthesized`）时学。
    /// 完整拼音码 = 各段码拼接；词 = 各段汉字拼接。写入临时层（达阈值由 store 晋升路线处理）。
    ///
    /// # 为什么这里只剩拼音
    ///
    /// `committed_segs` 是拼音专属的「组合区逐步转换」态，**码表永不进入**（见
    /// `crate::auto_phrase` 模块头）。原实现在此兼管码表，但守卫 `committed_segs.len() < 2`
    /// 对码表恒真 → 一行都执行不到，是码表自动造词「完全不工作」的根因之一。码表已迁至
    /// 独立的连续单字缓冲（`feed_auto_phrase` / `flush_auto_phrase`），此处不再兼管，
    /// 否则两套路径会对同一次输入重复造词。
    ///
    /// # 为什么单段整句要单独放行
    ///
    /// `committed_segs.len() >= 2` 问的是「用户**是不是分步组的**」——那是**过程**特征。
    /// 造词真正关心的是**产出**：这次上屏是不是一个词库里没有的多字词。整句消费整串
    /// （`consumed == total` ⇒ `partial == false`）故只 push 一段，用段数当判据它永远够不着
    /// 门槛，于是「智能组句出来的长词打不出简拼」——词根本没进临时库，简拼索引自然是空的。
    ///
    /// 判据取 `Candidate::is_synthesized` 而非「单段且够长」：后者会把用户**直接选中的
    /// 词典整词**（「你好」）也写进临时层，造出一堆本就存在于系统词库的冗余条目。
    /// 同款教训见 `crate::auto_phrase` 模块头——用过程指纹代替结果判据，码表侧已栽过一次。
    ///
    /// ⚠️ **也不能取 `is_sentence`**：整句与词典候选同文合并时，引擎会给那条**已有的词典
    /// 候选**补标 `existing.is_sentence = true`（`pinyin/mod.rs` 三处），于是打 `nihao` 选
    /// 系统词「你好」时它同样为真。`is_synthesized` 只由「引擎新建整句」置位，语义恰好是
    /// 造词要问的那句话：**这个词词库里有没有**。详见该字段文档。
    ///
    /// 判据必须由调用方传入：它是**候选级**信息，而 `committed_segs` 的元组
    /// （`raw_code, code, text, source, boundary`）里没有它。三个调用点
    /// （`handle_candidate` / `handle_temp` / `handle_mode`）都要传，漏一个就是该模式下静默失效。
    ///
    /// # 返回值：写入临时层的那个 code（未造词则 `None`）
    ///
    /// 调用方据此**跳过重复计数**。`handle_candidate` 在本函数之后还有一段「6b 临时词使用
    /// 累积」：它点查 `(schema, 本次候选码, 文本)`，命中就再 `learn_temp_word` 一次。
    /// 单段整句时两者的 key 完全相同（拼出的 code 就是那唯一一段的 code），于是刚写进去的
    /// 记录必然被点查命中 —— **count 每次 +2，`promote_count` 被腰斩**。
    ///
    /// 多段时两者 key 不同（造词写整串 `nihaoshijie`，6b 查末段 `shijie`），都该执行，
    /// 故返回的是 code 而非 bool：调用方比对 code 是否相同，只在相同时跳过。
    pub(crate) fn learn_phrase_on_commit(
        &self,
        state: &State,
        single_is_synthesized: bool,
    ) -> Option<String> {
        // 单段仅当它是整句解时放行；其余仍要求 ≥2 段。
        if state.committed_segs.len() < 2
            && !(state.committed_segs.len() == 1 && single_is_synthesized)
        {
            return None;
        }
        // **纯码表**方案不经此路：该态是拼音专属（码表选词消费整串），对码表恒为死代码；
        // 且码表已迁至 auto_phrase 连续单字缓冲，留在这里会对同一次输入重复造词。
        //
        // 混输**不**在此排除：其拼音子引擎的分步转换会正常产生 committed_segs，那是合法的
        // 拼音造词路径（学成拼音码的词）。混输的**单字序列**另由 auto_phrase 缓冲学成码表词，
        // 两者是不同维度、可并存。
        if self.engine_mgr.is_codetable() {
            return None;
        }
        // 闸门统一读 `[schema.pinyin.auto_learn]`：**走到这里的产出恒是拼音词**
        // （纯码表已在上方 `is_codetable()` 处返回），故闸门、字数上下限、晋升阈值都归拼音。
        //
        // ⚠️ 此处**曾按 `is_pinyin()` 分流**，混输读 `[codetable.auto_phrase]` —— 那是个
        // 会静默吞掉整个功能的错配：出厂 `available` 里只有纯码表 `wubi86` 与混输
        // `wubi86_pinyin`，**没有纯拼音方案**，于是实际用户几乎恒走 else 分支，而
        // `auto_phrase.enabled` 出厂为 `false` ⇒ 拼音造词从不发生。用户把
        // `[schema.pinyin.auto_learn].min_word_length` 调了也毫无反应，因为混输根本不读它。
        //
        // 「学什么词就读什么词的配置」是这里唯一自洽的判据：`auto_phrase` 的参数
        // （max 5）是为**码表连续单字序列**定的，用它约束拼音分步/整句产出的词没有依据。
        // 码表侧造词另有 `feed_auto_phrase` / `flush_auto_phrase` 一路，读它自己的段，不受影响。
        let al = self.engine_mgr.auto_learn_settings();
        let (enabled, min_len, max_len, promote_count) = (
            al.enabled,
            al.min_word_length,
            al.max_word_length,
            al.promote_count,
        );
        if !enabled {
            return None;
        }
        // 超长裁剪：从尾部保留整段使合并字数 ≤ max_len（最近输入优先）。
        let start = trim_segs_start(&state.committed_segs, max_len);
        let segs = &state.committed_segs[start..];
        // 裁剪只在段边界上切，故可能一段都留不下（首段自身就超上限）。
        //
        // 只剩一段时的放行判据是**原始段数为 1**，不是 `single_is_synthesized`：后者表达的是
        // 「本次上屏的候选是整句」，多段被裁到只剩末段时它同样为真，而那时学到的只是末段
        // 那个候选自己的词——多半词库里本就有。入口守卫已保证 `len()==1` 时必是整句。
        if segs.is_empty() || (segs.len() < 2 && state.committed_segs.len() != 1) {
            return None;
        }
        // 拼接各段码，并把**段内**音节边界平移到全局位置。段自身可能是多音节整词
        // （选「你好」→ 段码 nihao、段内边界 ni|hao），故不能按「一段一音节」记。
        // 任一段无边界（boundary=0，如码表段/手输码）则整词作废为 0——半截边界比没有更糟。
        let mut code = String::new();
        let mut boundary = 0u64;
        let mut boundary_ok = true;
        // 取**全拼** code（第 2 元）而非 raw_code：写入词库的编码与 boundary 位移都须全拼语义。
        for (_, c, _, _, b) in segs {
            if *b == 0 || code.len() + c.len() > 64 {
                boundary_ok = false;
            } else {
                boundary |= b << code.len();
            }
            code.push_str(c);
        }
        let boundary = if boundary_ok { boundary } else { 0 };
        let text: String = segs.iter().map(|(_, _, t, _, _)| t.as_str()).collect();
        let min_len = if min_len == 0 { 2 } else { min_len };
        let n_chars = text.chars().count();
        if n_chars < min_len || code.is_empty() {
            return None;
        }
        // 裁剪后**仍**超上限 → 整体放弃，不在词中间切一刀（对齐 `auto_phrase::word_from_seq`
        // 的「宁可放过，不可错造」）。整句只有一段可裁，裁剪对它是 no-op，这条判断才是
        // 上限对整句真正生效的地方。
        if max_len > 0 && n_chars > max_len {
            debug!("auto-learn: 超长（{} 字 > {}）整体放弃", n_chars, max_len);
            return None;
        }
        let Some(store) = &self.store else {
            return None;
        };
        let active = self.engine_mgr.active_schema_id();
        // 归属方案：非混输维持折叠自身/拼音（不看段来源，现行为）；
        // 混输仅当全段同源时用该源归属 id（混源/无法归因跳过，混合码写给谁都无意义）。
        // 注：混源判定使用截后 segs，截掉的段不参与归属判断。
        let schema = if self.engine_mgr.schema_engine_type(&active).as_deref() == Some("mixed") {
            let first = segs[0].3; // 上面的 is_empty 守卫已保证非空
            if segs.iter().any(|(_, _, _, s, _)| *s != first) {
                return None; // 混源：跳过自动造词
            }
            // 全段码表：混输超码长回捞的前缀候选现在带 `consumed_length`（见 `mixed/engine.rs`
            // 的 `convert_overflow`），码表首次进得了分段态——但本路径是**拼音专属**的，码表侧
            // 造词由 `auto_phrase` 连续单字缓冲负责，在此放行会对同一次输入重复造词。何况这里
            // 拼出来的码是「前 N 码 + 尾码」的机械拼接（`yijg` + `a`），本就不是一个有意义的词条。
            if first == CandidateSource::CodeTable {
                return None;
            }
            self.engine_mgr.write_data_schema_id(&active, first)? // None = 无法归因来源
        } else {
            self.engine_mgr.data_schema_id(&active) // 拼音族折叠到 "pinyin"，与 record_freq 写读一致
        };
        // 查重用户词库：同「码+词」已在用户层则不再写临时层（对齐码表侧 `flush_auto_phrase`
        // 的查重②）。已经是用户词的，再进临时层既无晋升意义、又让候选面多一条同文项。
        //
        // **刻意不查临时词库**：`learn_temp_word` 对已存在的记录是 `count++`（推进晋升进度），
        // 那正是复选该学的东西。在这里过滤掉就等于冻结晋升计数。
        if let Ok(recs) = store.get_user_words(&schema, &code)
            && recs.iter().any(|r| r.text == text)
        {
            debug!("auto-learn: 用户词库已有 {} -> {}，跳过", code, text);
            return None;
        }
        // add_weight 取保守默认；达 promote_count 阈值时晋升入用户词库（权重统一为 PROMOTED_WEIGHT）。
        match store.learn_temp_word(&schema, &code, &text, LEARN_ADD_WEIGHT, boundary) {
            Ok(count) => {
                debug!(
                    "auto-learned phrase: {} -> {} (count={})",
                    code, text, count
                );
                self.maybe_promote_temp(store, &schema, &code, &text, count, promote_count);
                // 容量上限：本路径此前从不淘汰，因为它只在「分步组词」这种低频场景写库。
                // 放行单段整句后它变成**几乎每次上屏都可能写一条**，不接淘汰会让
                // TEMP_WORDS / TEMP_ABBREV 越过上限无限增长（`flush_auto_phrase` 一直是接的）。
                self.maybe_evict_temp(store, &schema);
                Some(code)
            }
            Err(e) => {
                warn!("learn_temp_word failed: {}", e);
                None
            }
        }
    }

    /// 临时词晋升判定：promote_count>0 且累积 count 达阈 → 移入用户词库。0=禁用（对齐 Go 语义）。
    pub(crate) fn maybe_promote_temp(
        &self,
        store: &wind_store::Store,
        schema: &str,
        code: &str,
        text: &str,
        count: u32,
        promote_count: usize,
    ) {
        if promote_count == 0 || (count as usize) < promote_count {
            return;
        }
        match store.promote_temp_word(schema, code, text) {
            Ok(true) => debug!("temp word promoted: {} -> {}", code, text),
            Ok(false) => {}
            Err(e) => warn!("promote_temp_word failed: {}", e),
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // 快捷加词模式（对齐 Go internal/coordinator/handle_addword.go）
    // 从最近上屏字符中选取末尾 N 字组词，自动计算编码后加入用户词库。
    // ──────────────────────────────────────────────────────────────────────

    /// 还原最近上屏字符池：`recent_commits` 最新在前，反转为时间序（旧→新）后展开为字符，
    /// 取末尾 `max_len` 个（最近输入的字符）。
    pub(crate) fn add_word_recent_chars(&self, max_len: usize) -> Vec<char> {
        let snap = self.recent_commits_snapshot(); // 最新在前
        let mut chars: Vec<char> = Vec::new();
        for s in snap.iter().rev() {
            chars.extend(s.chars());
        }
        let n = chars.len();
        if n > max_len {
            chars.split_off(n - max_len)
        } else {
            chars
        }
    }

    /// 剪贴板字符池：读一次系统剪贴板并清洗，无可用内容时返回空 Vec。
    ///
    /// 两条守卫与命令栏 `dict.add` 的 [`sanitize_dict_add_text`] 同源：trim 后为空、
    /// 含换行（那边的话是「请只复制一个词」）。差别只在**出口形态**——那边是用户显式提交、
    /// 报错弹提示；这边是顺手一按，静默降级成「剪贴板无可用内容」。剪贴板里本来就常驻着
    /// 与加词无关的东西（整段文章、一个 URL），为它弹错是噪音。
    ///
    /// **超长不再算不合规，直接取前 [`ADD_WORD_MAX_LEN`] 字**（2026-09-03 用户实测提出）。
    /// 原先整段作废，于是复制一句话按 Tab 切过去只看到「剪贴板无可用内容」——可用户想要的
    /// 那个词往往就在开头。截断与剪贴板来源「取开头 N 字、↓ 砍尾」的裁剪方向也一致：
    /// 给出的就是他按 ↓ 之前会看到的东西。
    ///
    /// ⚠️ 必须用**阻塞版** `clipboard_get_text`：cached 版在剪贴板被别的进程占用时返回
    /// **上一次**的内容（契约见 `wind_ui::popup_menu::get_clipboard_text_cached`），而加词
    /// 是执行路径——拿陈旧内容等于往词库里写一条用户没复制过的词，且界面毫无异常。
    ///
    /// 代价是最坏 sleep 重试 40ms，**在按键线程上**。故调用点一律走惰性入口
    /// [`Self::add_word_clip_pool`]，别直接调本函数。
    fn clipboard_add_word_chars(&self) -> Vec<char> {
        let raw = self
            .host_services()
            .clipboard_get_text()
            .unwrap_or_default();
        let s = raw.trim();
        if s.is_empty() || s.contains(['\r', '\n']) {
            return Vec::new();
        }
        let mut chars: Vec<char> = s.chars().collect();
        if chars.len() > ADD_WORD_MAX_LEN {
            // 隐私红线（docs/logging-convention.md）：不打内容明文，只打字数。
            debug!(
                "addword: 剪贴板 {} 字，截取前 {}",
                chars.len(),
                ADD_WORD_MAX_LEN
            );
            chars.truncate(ADD_WORD_MAX_LEN);
        }
        chars
    }

    /// 剪贴板池的**惰性**取用：本轮没读过才真去读系统剪贴板，读完记进 `State`。
    ///
    /// ★ 进入加词模式时**不预读**，是 2026-09-03 那次「Ctrl+= 有时明显卡顿」的修法：
    /// 读一次最坏 sleep 重试 40ms，而它发生在**按键线程**上、恰好又在 C++ 建立占位
    /// composition 的同一拍里。多数人进来是要加刚打的字、根本不按 Tab，那 40ms 纯属白付。
    ///
    /// 现在只有两个地方会真读：用户按 Tab 切过来（他主动要剪贴板，等一下是应该的），
    /// 以及最近上屏为空时的自动落点判断（那时本来也没别的可干）。
    fn add_word_clip_pool<'a>(&self, state: &'a mut State) -> &'a [char] {
        if state.add_word_clip.is_none() {
            state.add_word_clip = Some(self.clipboard_add_word_chars());
        }
        state.add_word_clip.as_deref().unwrap_or(&[])
    }

    /// 当前生效的字符池（按 `add_word_from_clip` 选源）。
    ///
    /// 「池子有多少字」这个判据的**所有**读点都必须走这里——`add_word_chars` 只是两个池子
    /// 之一，直接读它会让剪贴板来源下的长度上限、空池判据整体错位（表现为剪贴板里明明有
    /// 词，面板却说「无最近输入」）。
    fn add_word_pool<'a>(&self, state: &'a State) -> &'a [char] {
        if state.add_word_from_clip {
            // 只读路径不触发惰性读取：走到这里时来源已是剪贴板，池子必已读过
            // （`toggle_add_word_source` / `enter_add_word_mode` 两个切过来的入口都读了）。
            state.add_word_clip.as_deref().unwrap_or(&[])
        } else {
            &state.add_word_chars
        }
    }

    /// 某来源刚生效时的默认词长。
    ///
    /// 两个来源的默认值**刻意不同**：最近上屏是一条没有边界的字流，「最后两个字」是唯一
    /// 有意义的起点；剪贴板则是用户主动划出来复制的一段，那段本身就是他要加的词，故默认
    /// 全选，↑↓ 只是微调掉尾巴上多复制的标点。
    fn default_add_word_len(from_clip: bool, pool_len: usize) -> usize {
        if from_clip {
            pool_len.min(ADD_WORD_MAX_LEN)
        } else {
            ADD_WORD_DEFAULT_LEN.min(pool_len)
        }
    }

    /// 当前加词编码的**展示形态**（带音节空格），候选窗预览与设置端预填共用。
    ///
    /// `add_word_code` 是扁平 key、`add_word_boundary` 是它的音节切分，二者必须**成对**读出
    /// 才还原得出 `ni hao`。此前两个调用点都只读了前者，boundary 字段一直在 State 里躺着
    /// 没人用——收口到这里，避免再出现「只读一半」。见 [`display_code`]。
    fn add_word_display_code(&self, state: &State) -> String {
        display_code(&state.add_word_code, state.add_word_boundary)
    }

    /// 取当前选取的词。**裁剪方向随来源反向**：
    /// - 最近上屏 → 取池子**末尾** N 字（新字在尾，缩短即丢掉更早的输入）；
    /// - 剪贴板 → 取**开头** N 字（整段就是词，缩短是砍掉尾部多复制的标点）。
    ///
    /// 方向搞反不会报任何错，表现是「复制『量子纠缠态』按一下 ↓ 变成『子纠缠态』」。
    fn add_word_current_word(&self, state: &State) -> String {
        let pool = self.add_word_pool(state);
        let len = state.add_word_len.min(pool.len());
        if state.add_word_from_clip {
            pool[..len].iter().collect()
        } else {
            pool[pool.len() - len..].iter().collect()
        }
    }

    /// 进入加词模式：取最近字符、默认词长 2、强制竖排、占位 composition。
    pub(crate) fn enter_add_word_mode(&self, state: &mut State) -> KeyAction {
        // 清理任何未上屏的输入/候选/独占模式残留
        self.reset_exclusive_modes(state);
        self.reset_pinyin_composition(state);
        self.notify_ui_hide();

        state.add_word_chars = self.add_word_recent_chars(ADD_WORD_MAX_LEN);
        // 剪贴板池**不在这里预读**——那 40ms 会摊在每一次 Ctrl+= 上（见 add_word_clip_pool）。
        state.add_word_clip = None;
        // 默认来源是最近输入：Ctrl+= 是带面板的连续加词，接着刚打的字最顺手（与
        // Ctrl+Shift+= 刻意相反，见 open_add_word_from_history）。
        //
        // **例外：最近输入为空而剪贴板有内容时直接落在剪贴板一侧**——否则开面板第一眼是
        // 「无最近输入」，而唯一有用的下一步就是 Tab，不如替用户按了。反向不做：最近输入
        // 有内容就按它来，剪贴板可不可用都不改变默认——**这也是惰性读取的前提**：短路求值
        // 使得有最近输入时右边整个不求值，即不读剪贴板。
        state.add_word_from_clip = state.add_word_chars.len() < ADD_WORD_MIN_LEN
            && self.add_word_clip_pool(state).len() >= ADD_WORD_MIN_LEN;
        state.add_word_active = true;

        // 候选布局（input.add_word.candidate_layout，出厂竖排）由 show_add_word_preview
        // 末尾的 notify_ui_update 统一重算，这里不再自己切布局（见 layout.rs）。

        self.reset_add_word_len_for_source(state);

        self.show_add_word_preview(state);

        // 占位 composition：激活 C++ 侧 composition，转发后续 ↑↓/Enter/Esc 给我们处理。
        KeyAction::UpdateComposition {
            text: " ".to_string(),
            caret_pos: 0,
        }
    }

    /// 退出加词模式：清状态、隐藏候选窗。
    /// 布局无需在此恢复：`add_word_active` 已清，下一次 notify_ui_update 自动算回基线。
    pub(crate) fn exit_add_word_mode(&self, state: &mut State) {
        state.add_word_active = false;
        state.add_word_chars.clear();
        state.add_word_clip = None;
        state.add_word_from_clip = false;
        state.add_word_len = 0;
        state.add_word_code.clear();
        state.add_word_boundary = 0;
        self.notify_ui_hide();
    }

    /// 调整加词长度（↑ +1 / ↓ -1），夹在 [1, min(字符数, 上限)]。
    pub(crate) fn adjust_add_word_length(&self, state: &mut State, delta: i32) -> KeyAction {
        let pool_len = self.add_word_pool(state).len();
        if pool_len < ADD_WORD_MIN_LEN {
            return KeyAction::Consumed;
        }
        let max_len = ADD_WORD_MAX_LEN.min(pool_len);
        let mut new_len = state.add_word_len as i32 + delta;
        new_len = new_len.clamp(ADD_WORD_MIN_LEN as i32, max_len as i32);
        let new_len = new_len as usize;
        if new_len != state.add_word_len {
            state.add_word_len = new_len;
            self.update_add_word_code(state);
            self.show_add_word_preview(state);
        }
        KeyAction::Consumed
    }

    /// 按当前来源把词长重置为默认，并同步编码（空池则清零）。
    /// 进入模式与 Tab 切换共用——两处对「池空怎么办」必须是同一套处置，分头写必然走偏。
    fn reset_add_word_len_for_source(&self, state: &mut State) {
        let pool_len = self.add_word_pool(state).len();
        if pool_len < ADD_WORD_MIN_LEN {
            state.add_word_len = 0;
            state.add_word_code.clear();
            state.add_word_boundary = 0;
        } else {
            state.add_word_len = Self::default_add_word_len(state.add_word_from_clip, pool_len);
            self.update_add_word_code(state);
        }
    }

    /// Tab：在「最近上屏」与「剪贴板」两个字符池之间切换。
    ///
    /// **两个来源恒对称**：哪一侧没内容都照样切得过去，切过去看到的是那一侧的空态
    /// （「无最近输入」/「剪贴板无可用内容」）。
    ///
    /// ⛔ 曾给剪贴板侧加过「空则不许切、面板也不提示 Tab」的守卫，已推翻：最近输入为空时
    /// 面板照常显示、照常能停在那儿，剪贴板凭什么不能——用户看到的是「Tab 有时在有时不在」，
    /// 比一个诚实的空态更费解。两侧同构之后，「切不切得动」不再是一个需要判断的问题。
    ///
    /// 切过去后词长按目标来源的默认值**重置**而非沿用：两个池子的裁剪方向相反、长度上限
    /// 也不同，沿用旧值只会得到一个用户没选过的词。
    pub(crate) fn toggle_add_word_source(&self, state: &mut State) -> KeyAction {
        state.add_word_from_clip = !state.add_word_from_clip;
        // 切到剪贴板这一侧才真去读系统剪贴板（见 add_word_clip_pool）。用户主动按了 Tab，
        // 这一次的等待是他要的；切回最近上屏则什么也不读。
        if state.add_word_from_clip {
            let _ = self.add_word_clip_pool(state);
        }
        self.reset_add_word_len_for_source(state);
        self.show_add_word_preview(state);
        KeyAction::Consumed
    }

    /// 确认加词：写入用户词库（权重 1200）并广播 dict.changed；编码为空则中止。
    pub(crate) fn confirm_add_word(&self, state: &mut State) -> KeyAction {
        if state.add_word_len < ADD_WORD_MIN_LEN
            || self.add_word_pool(state).len() < ADD_WORD_MIN_LEN
        {
            self.exit_add_word_mode(state);
            return KeyAction::ClearComposition;
        }
        let word = self.add_word_current_word(state);
        let code = state.add_word_code.clone();
        let boundary = state.add_word_boundary;
        if code.is_empty() {
            // 隐私红线（docs/logging-convention.md）：warn 不得含用户输入明文，词本身降到 debug。
            warn!(
                "addword: 无法计算编码，放弃加词 chars={}",
                word.chars().count()
            );
            debug!("addword: 无法计算编码，放弃加词 word={}", word);
            self.exit_add_word_mode(state);
            return KeyAction::ClearComposition;
        }
        if let Some(store) = &self.store {
            let active = self.engine_mgr.active_schema_id();
            // 手动造词是码表语义（编码来自码表反查）；混输落主码表方案，primary 缺失则 warn 跳过。
            let Some(schema) = self
                .engine_mgr
                .write_data_schema_id(&active, CandidateSource::CodeTable)
            else {
                warn!(
                    "addword: 混输方案主码表缺失，跳过加词 schema={} chars={}",
                    active,
                    word.chars().count()
                );
                debug!("addword: 混输方案主码表缺失，跳过加词 word={}", word);
                self.exit_add_word_mode(state);
                return KeyAction::ClearComposition;
            };
            match store.add_user_word(&schema, &code, &word, ADD_WORD_WEIGHT, boundary) {
                Ok(_) => {
                    // 注：dict.changed 广播在 RPC dispatch 层（EventSink），协调器不持有该 sink，
                    // 故此处不发事件——与现有 web_dict_add 一致；设置端用户词库视图重开时刷新。
                    debug!("addword: 已加词 {} -> {}", code, word);
                }
                Err(e) => warn!("addword: 写库失败 {}", e),
            }
        }
        self.exit_add_word_mode(state);
        KeyAction::ClearComposition
    }

    /// 加词目标方案 id（设置端出码/入库用）。混输方案自身无用户词库，解析到其主码表
    /// 方案的**真实 id**（供设置端 `dict.encode` 正确判引擎类型、`dict.add` 落到码表用户词库）；
    /// 非混输保持真实 active id（拼音族的存储折叠由 `web_dict_add` 内部 `data_schema_id` 处理，
    /// 此处不能提前折叠成 "pinyin"，否则出码会误判为码表）。
    pub(crate) fn add_word_target_schema(&self) -> String {
        let active = self.engine_mgr.active_schema_id();
        if self.engine_mgr.schema_engine_type(&active).as_deref() == Some("mixed") {
            self.engine_mgr
                .mixed_primary_schema(&active)
                .unwrap_or(active)
        } else {
            active
        }
    }

    /// 加词界面的默认上下文（见 [`AddWordContext`]）。设置端经 `dict.addWordContext`
    /// 取用，**只在它自己缺参数时**——带 `--schema` / `--text` 进来的深链不会走到这里。
    pub(crate) fn add_word_context(&self) -> AddWordContext {
        AddWordContext {
            schema_id: self.add_word_target_schema(),
            recent_text: self
                .add_word_recent_chars(ADD_WORD_MAX_LEN)
                .into_iter()
                .collect(),
            max_len: ADD_WORD_MAX_LEN,
        }
    }

    /// 拉起设置端加词界面（预填 word/code/schema）。两条入口共用。
    pub(crate) fn open_add_word_dialog_with(
        &self,
        word: &str,
        code: &str,
        schema: &str,
    ) -> KeyAction {
        let args = crate::handle_menu::build_settings_args(&[
            ("text", word),
            ("code", code),
            ("schema", schema),
        ]);
        self.open_settings_with(Some("add-word"), &args);
        KeyAction::ClearComposition
    }

    /// Ctrl+Enter：从加词模式转到设置端加词界面，预填当前 词/编码/方案。
    pub(crate) fn open_add_word_dialog(&self, state: &mut State) -> KeyAction {
        let (word, code) = if state.add_word_len >= ADD_WORD_MIN_LEN
            && self.add_word_pool(state).len() >= ADD_WORD_MIN_LEN
        {
            (
                self.add_word_current_word(state),
                // 同 `open_add_word_from_history`：转交设置端用展示形态（带音节空格）。
                self.add_word_display_code(state),
            )
        } else {
            (String::new(), String::new())
        };
        let schema = self.add_word_target_schema();
        self.exit_add_word_mode(state);
        self.open_add_word_dialog_with(&word, &code, &schema)
    }

    /// Ctrl+Shift+=：不进加词模式，直接预填并拉起加词界面
    /// （对齐 Go openAddWordDialogFromHistory）。
    ///
    /// 来源优先级与 Ctrl+= 的默认**刻意相反**：剪贴板优先，不可用才回退最近输入。这条路
    /// 一按就把词交给设置端、不给调长度的机会，用户按它多半是「我刚复制了一个词，收进
    /// 词库」；而 Ctrl+= 是带面板的连续加词，接着刚打的字更顺。
    pub(crate) fn open_add_word_from_history(&self, state: &mut State) -> KeyAction {
        // 清理未上屏输入/候选/独占残留，避免残留 composition
        self.reset_exclusive_modes(state);
        self.reset_pinyin_composition(state);
        self.notify_ui_hide();

        let (word, code) = self.add_word_prefill_from_history();
        let schema = self.add_word_target_schema();
        self.open_add_word_dialog_with(&word, &code, &schema)
    }

    /// [`Self::open_add_word_from_history`] 的选词/取码部分，单独成函数只为**可测**：
    /// 原函数末尾要拉起设置端进程，测试里跑不了，于是「按哪个来源预填」这条判据在
    /// 改动前根本没有守门测试。返回 `(word, 展示形态的 code)`，两者皆空即无可预填内容。
    fn add_word_prefill_from_history(&self) -> (String, String) {
        // 剪贴板整段就是词（已受 ADD_WORD_MAX_LEN 约束，不合规的一律当空，见
        // clipboard_add_word_chars）；回退到最近上屏时才按默认长度取末尾几字。
        let clip = self.clipboard_add_word_chars();
        let word: String = if clip.len() >= ADD_WORD_MIN_LEN {
            clip.iter().collect()
        } else {
            let chars = self.add_word_recent_chars(ADD_WORD_MAX_LEN);
            if chars.len() >= ADD_WORD_MIN_LEN {
                let len = ADD_WORD_DEFAULT_LEN.min(chars.len());
                chars[chars.len() - len..].iter().collect()
            } else {
                String::new()
            }
        };
        if word.is_empty() {
            return (String::new(), String::new());
        }
        let (code, boundary) = self.calc_add_word_code(&word);
        // 预填给设置端的是**展示形态**（带音节空格）：与该窗「出码」按钮及词库列表同形，
        // 回写时由 normalize_add_code 拆回扁平 key。见 [`display_code`]。
        (word, display_code(&code, boundary))
    }

    /// 更新当前加词的编码与音节边界（按方案：拼音生成 / 码表反查）。
    fn update_add_word_code(&self, state: &mut State) {
        if state.add_word_len < ADD_WORD_MIN_LEN
            || self.add_word_pool(state).len() < state.add_word_len
        {
            state.add_word_code.clear();
            state.add_word_boundary = 0;
            return;
        }
        let word = self.add_word_current_word(state);
        let (code, boundary) = self.calc_add_word_code(&word);
        state.add_word_code = code;
        state.add_word_boundary = boundary;
    }

    /// 为词计算编码（对齐设置端 `dict.encode` / web_dict_encode）：
    /// 拼音方案走引擎词级消歧，无果回退逐字反查表；码表方案走 [`EngineManager::encode_word`]
    /// （按方案 `[[encoder.rules]]` 从码表词库的单字全码组装，支持词库中尚不存在的新词）。
    /// 返回 `(code, boundary)`：boundary 见 `wind_dict::binformat::DictEntry::boundary`。
    /// 只有引擎词级消歧这条路能给出边界（造词本就逐音节拼接）；逐字反查表回退与码表取码
    /// 无音节语义，为 0（消费方降级回 DAG）。
    ///
    /// # 码源变更（与自动造词统一）
    ///
    /// 原实现走 `wind_reverse::wubi_word_code`：码源是**拆字表**、规则是**硬编码的五笔 86
    /// 三分支**。两个问题——① 拆字表是可选资源（全仓 5 个方案只有 wubi86 配了），未配拆字的
    /// 第三方码表方案取码恒空、手动加词直接失败；② 硬编码规则对非五笔码表方案静默出错。
    /// 且拆字表与词库解耦，用户换词库/加扩展库后可能造出**打不出来**的码。
    /// 现统一走码表词库自身的单字全码 + 方案声明的公式，与「造出的词必须能打出来」对齐。
    fn calc_add_word_code(&self, word: &str) -> (String, u64) {
        let schema = self.add_word_target_schema();
        let engine_type = self.engine_mgr.schema_engine_type(&schema);
        // 英文方案：码即单词本身。走不到下面任何一条——`encode_word` 按方案的
        // `[[encoder.rules]]` 从单字全码组装，英文方案没有那一段，取码必失败，
        // 结果是英文方案下加词恒提示「当前方案取不出编码」。
        if engine_type.as_deref() == Some("english") {
            return Self::english_add_word_code(word);
        }
        let is_pinyin = engine_type.map(|t| t == "pinyin").unwrap_or(false);
        if is_pinyin {
            // 含非汉字 → 不取码，让加词中止（界面显示「无法计算编码」）。
            //
            // 拼音码来自读音，而非汉字没有读音：`gen_pinyin` 的 `filter_map` 会**静默跳过**
            // 它们，「你好a」产出 `ni hao`——一个覆盖不全的码，打 `nihao` 出「你好a」。
            // 悄悄丢字入库比直接拒绝更糟：用户不知道自己加了什么，词库还多一条对不上的记录
            // （同 `sanitize_dict_add_text` 的取向）。这也补上了一处不一致——纯英文「abc」
            // 取码本就为空、早已中止，只有**混合**的情况从这里漏了过去。
            //
            // 守卫**只作用于拼音**：码表方案的码不来自读音，词库里可能真收录了符号条目
            // （标点/特殊符号有合法的码），一刀切会把它们的加词能力砍掉。非汉字在码表下
            // 取不出码时，`encode_word` 自己会失败返回空码，行为不变。
            if !word.chars().all(is_han) {
                debug!("addword: 含非汉字，拼音方案不取码（word={}）", word);
                return (String::new(), 0);
            }
            let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
            // **两条路产出的都是带空格的音节码**，故统一 split 成扁平 code + 边界。
            //
            // 逐字反查表（`gen_pinyin`）同样以 `.join(" ")` 收尾——此前这里把它的结果当扁平码
            // 直接返回，于是 `add_user_word` 拿带空格的串当 key（`pinyin\0ni hao\0…`），
            // 前缀查询再也命中不到，**加进去的词一个都打不出来**，且界面毫无异常。
            // 触发条件不罕见：`generate_word_pinyin` 对含非汉字的词返回 None（快捷加词的
            // 字符池直接取最近上屏字符，中英混输下「你好a」这种一抓一个准），正好落进回退。
            //
            // 顺带把回退路径的 boundary 从恒 0 修成真值：逐字反查每字一音节，切分完全确定，
            // 与 code 自洽。原先记 0 意味着双拼相容校验一律放行（`boundary_compatible` 任一侧
            // 为 0 即不设防），填真值后这些词才和词典词受同样的校验。
            //
            // ⚠️ 上面的非汉字守卫落地后，这条回退**在实践中已几乎不可达**：含非汉字的词被守卫
            // 挡在前面，全汉字的词则 `generate_word_pinyin` 几乎总有值（实测拼音词典连 CJK
            // 扩展 B 区都覆盖）。剩下的可达情形只有引擎加载失败，而那时 reverse 表通常也是空的。
            // 保留它是为了契约自洽（两条路同形、同样落扁平 key），**不是**在防一个活跃的缺陷；
            // 故不为它写端到端测试——那种测试只会永远绿着却从不执行被测分支。
            self.engine_mgr
                .generate_word_pinyin(&schema, word)
                .or_else(|| Some(reverse.gen_pinyin(word)))
                .map(|spaced| wind_store::wdict::split_spaced_code(&spaced))
                .unwrap_or_default()
        } else {
            match self.engine_mgr.encode_word(&schema, word) {
                Ok(code) => (code, 0),
                Err(e) => {
                    // 空码由调用方处理（加词界面中止并提示）。带原因便于排查是哪个字没码。
                    debug!("addword: 取码失败（{}）: {}", word, e);
                    (String::new(), 0)
                }
            }
        }
    }

    /// 英文方案的加词取码：**码就是单词本身的小写**，无音节边界（boundary=0）。
    ///
    /// 英文词库以 `type = "english"` 声明，加载期把 code 列小写化、做大小写不敏感的前缀匹配
    /// （打 `hel` 出 `hello`）。所以「这个词的码」这个问题对英文只有一个答案：它自己。
    ///
    /// 三道守卫，取空码即让加词中止（界面提示「无法计算编码」）：
    /// - **含空白** —— 带空格的串当 key 会让前缀查询永远命中不到，加进去的词一个也打不出来
    ///   且界面毫无异常。拼音侧正因为这个坑栽过一次（见 `calc_add_word_code` 里的长注释）。
    /// - **非 ASCII** —— 英文词库的码空间就是 ASCII；中文字符落进来只会写出一条查不到的记录。
    /// - **一个字母都没有** —— 纯数字/纯符号不是英文词，拦下来免得污染词库。
    fn english_add_word_code(word: &str) -> (String, u64) {
        if word.is_empty()
            || !word.is_ascii()
            || word.chars().any(|c| c.is_whitespace())
            || !word.chars().any(|c| c.is_ascii_alphabetic())
        {
            debug!("addword: 英文方案取不出码（word={}）", word);
            return (String::new(), 0);
        }
        (word.to_ascii_lowercase(), 0)
    }

    /// 加词面板的三行内容：标题（含来源）/ 词与编码 / 操作提示。
    ///
    /// **与发送分离只为可测**：headless 构造把 UI 接收端当场丢弃（`construct.rs` 里
    /// `drop(_rx)`），测试收不到 `UpdateCandidates`，面板内容此前无从断言。
    ///
    /// # 为什么是三行
    ///
    /// 原本是两行、操作提示挂在标题右侧。`Tab` 那一项加进来后，「标题 + 五个动作」挤在
    /// 同一行把面板撑到了半个屏幕宽（真机实测）。拆行后最宽的只剩提示本身，标题那 10 个
    /// 全角字不再叠加上去。
    ///
    /// # ⚠️ 提示必须放 comment，不能放 text
    ///
    /// comment 走注释色（`candidate_window.rs` 默认 150 灰）与更小字号，提示天然退到次要
    /// 层级；放 text 会用候选正文色，比标题还抢眼——面板上最不重要的一行反而最显眼。
    /// 空 text 行在 `no_index` 下不留序号位，渲染出来就是一整行浅灰小字。
    ///
    /// 这条判据**只体现在颜色上，编译器与运行时都不会报错**，故留了守门测试盯着
    /// （`panel_hint_is_a_dim_third_row`）。
    fn add_word_panel_rows(&self, state: &State) -> Vec<CandidateItem> {
        // no_index=true 完全不显示序号（三行都是提示行，避免默认主题的空圆圈）。
        let row = |text: String, comment: String| CandidateItem {
            text,
            code: String::new(),
            label: String::new(),
            tooltip: String::new(),
            comment,
            no_index: true,
        };
        // 来源后缀与 Tab 提示**恒显示**，不随哪一侧有没有内容变化：面板在两个来源下必须
        // 长得一样，否则用户看到的是「Tab 有时在有时不在」（见 toggle_add_word_source 的
        // 已推翻守卫）。空的那一侧照常切得过去，只是正文换成该侧的空态。
        let title = if state.add_word_from_clip {
            "快捷加词 · 剪贴板"
        } else {
            "快捷加词 · 最近输入"
        };
        let empty = self.add_word_pool(state).len() < ADD_WORD_MIN_LEN
            || state.add_word_len < ADD_WORD_MIN_LEN;
        let body = if empty {
            // 空态文案分来源给：两侧「为什么空」和「怎么办」完全不同，共用一句就等于
            // 两边都说不清。剪贴板侧顺带交代准入条件——不合规（多行/超长）与真的没内容
            // 在这里是同一个可见状态，不说清用户会以为复制没生效。
            if state.add_word_from_clip {
                // 只剩「单行」这一个准入条件——超长已改为取前 ADD_WORD_MAX_LEN 字，
                // 不再算不合规（见 clipboard_add_word_chars）。
                row("剪贴板无可用内容".into(), "需要单行文本".into())
            } else {
                row("无最近输入".into(), "请先输入文字后再使用".into())
            }
        } else {
            let word = self.add_word_current_word(state);
            let code_comment = if state.add_word_code.is_empty() {
                "无法计算编码".to_string()
            } else {
                // 候选窗里也显示音节形态：用户在这里看到的码要与 Ctrl+Enter 转过去的
                // 设置页、以及词库列表一致，否则同一个码在三个界面有两种样子。
                self.add_word_display_code(state)
            };
            row(word, code_comment)
        };
        // 空态下不提「↑↓调整长度 / Enter添加」——没有词可调、可加，列出来只是噪音。
        let hint = if empty {
            "Tab切换来源  Esc关闭"
        } else {
            "↑↓调整长度  Tab切换来源  Enter添加  Ctrl+Enter编辑  Esc取消"
        };
        vec![
            row(title.into(), String::new()),
            body,
            row(String::new(), hint.into()),
        ]
    }

    /// 显示加词预览候选窗（三行，内容见 [`Self::add_word_panel_rows`]）。
    fn show_add_word_preview(&self, state: &State) {
        // 加词面板**不走** notify_ui_update（下方直接发 UpdateCandidates），所以布局重算要在
        // 这里单独接一次——`UpdateCandidates` 的发送点共两处，两处都得接，否则加词的
        // candidate_layout 完全失效（见 layout.rs / docs/design/mode-candidate-layout.md）。
        self.sync_candidate_layout(state);
        // 字体同理：同一批发送点，漏一处的表现是加词面板里字体变回默认。
        self.sync_candidate_font(state);
        let candidates = self.add_word_panel_rows(state);
        // 加词面板复用候选窗实例，定位方式须与候选窗一致（见 candidate_fixed_pos）。
        let (fixed, fixed_x, fixed_y) = self.candidate_fixed_pos();
        let _ = self.ui_tx.send(UiCommand::UpdateCandidates {
            preedit: String::new(),
            preedit_caret: 0, // 加词面板无编码区
            preedit_host_owned: false,
            mode_label: String::new(),
            candidates,
            selected: usize::MAX, // 两行均为提示、非可选候选，不高亮任何行
            hover: -1,
            page: 1,
            total_pages: 1,
            caret_x: state.caret_x,
            caret_y: state.caret_y,
            caret_height: state.caret_height,
            caret_valid: true,
            fixed,
            fixed_x,
            fixed_y,
        });
    }

    /// 加词模式下的按键分派（对齐 Go handleAddWordKey）。
    pub(crate) fn handle_add_word_key(&self, state: &mut State, data: &KeyEventData) -> KeyAction {
        let has_ctrl = data.modifiers & MOD_CTRL != 0;
        match data.key_code {
            keymap::VK_ESCAPE | keymap::VK_BACK => {
                self.exit_add_word_mode(state);
                KeyAction::ClearComposition
            }
            keymap::VK_UP => self.adjust_add_word_length(state, 1),
            keymap::VK_DOWN => self.adjust_add_word_length(state, -1),
            keymap::VK_TAB => self.toggle_add_word_source(state),
            keymap::VK_RETURN if has_ctrl => self.open_add_word_dialog(state),
            keymap::VK_RETURN => self.confirm_add_word(state),
            // 加词模式下消费所有按键，避免误操作退出。
            _ => KeyAction::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    //! 快捷加词状态机单元测试：无头 Coordinator + 临时 store，覆盖纯逻辑
    //! （字符还原/词长调整/确认写库）。编码计算依赖引擎，headless 下为空，
    //! 故写库测试手动注入 add_word_code。
    use super::{ADD_WORD_MAX_LEN, trim_segs_start};
    use crate::coordinator::Coordinator;
    use std::sync::Arc;
    use wind_config::Config;
    use wind_keys::keymap;
    use wind_store::Store;

    /// ★★ 自提交打点必须覆盖**一切真落屏**的返回变体，不只 `InsertText` 那两种。
    ///
    /// 真机现场（2026-09-02，记事本 + 五笔，长按 d）：满码自动上屏走
    /// `CommitThenDeferComposition`（TSF 日志 `Processing CommitThenDefer: commit=大
    /// defer=d`）。此前打点只认 `InsertText`/`InsertTextWithCursor` ⇒ 这条路不打点
    /// ⇒ 紧随其后的 `SelectionChanged` 被回声过滤判成「用户移动光标」（日志
    /// `since_self_commit=Some(162.9s)`）⇒ 清 `caret_cache_verified` ⇒ 下一键信任门
    /// 命中、arm 600ms 长兜底 ⇒ 而五笔 4 码一组、typematic 32ms 一键，组合寿命仅
    /// ~128ms，600ms timer **永远等不到到期**就被下次上屏作废 ⇒ **候选窗一次都不显示**。
    ///
    /// 一个漏打的点，末端表现是「打字时候选窗根本不出来」，中间隔着四层，没有任何报错。
    #[test]
    fn self_commit_is_marked_for_every_landed_variant() {
        use wind_bridge::handler::KeyAction;
        let landed: Vec<KeyAction> = vec![
            KeyAction::InsertText {
                text: "大".into(),
                new_composition: None,
                mode_changed: false,
                chinese_mode: true,
                has_new_composition: false,
            },
            KeyAction::InsertTextWithCursor {
                text: "大".into(),
                cursor_offset: 0,
            },
            KeyAction::ReplaceBackward {
                count: 1,
                text: "大".into(),
            },
            KeyAction::CommitAndHoldComposition {
                commit_text: "大".into(),
                hold_text: "d".into(),
                timeout_ms: 150,
            },
            KeyAction::CommitThenDeferComposition {
                commit_text: "大".into(),
                deferred_composition: "d".into(),
                timeout_ms: 150,
            },
        ];
        for action in landed {
            let c = Coordinator::new_headless(Config::default(), None);
            assert!(c.last_self_commit.lock().unwrap().is_none(), "初始应为空");
            c.note_commit_action(&action);
            assert!(
                c.last_self_commit.lock().unwrap().is_some(),
                "落屏变体必须打点，否则它引发的 SelectionChanged 会被误判成用户移动光标：{action:?}"
            );
        }
    }

    /// 反向对照：**尚未落屏**的组合更新不得打点——否则回声宽限期会被无谓刷新，
    /// 用户真正移动光标时反而识别不出来（那正是 `caret_cache_verified` 要清位的时机）。
    #[test]
    fn self_commit_is_not_marked_before_text_lands() {
        use wind_bridge::handler::KeyAction;
        let c = Coordinator::new_headless(Config::default(), None);
        c.note_commit_action(&KeyAction::UpdateComposition {
            text: "d".into(),
            caret_pos: 1,
        });
        assert!(
            c.last_self_commit.lock().unwrap().is_none(),
            "组合区更新还没落屏，不该算作自提交"
        );
    }

    /// 英文取码 = 单词本身的小写；三类非法输入取空码（让加词中止）。
    ///
    /// 「含空白」那条是重点：带空格的串当 key 会让前缀查询永远命中不到——加进去的词一个
    /// 都打不出来，而界面毫无异常。拼音侧正因为这个坑栽过一次。
    #[test]
    fn english_add_word_code_is_lowercased_word() {
        let f = Coordinator::english_add_word_code;
        assert_eq!(f("Hello"), ("hello".to_string(), 0));
        assert_eq!(f("WindInput"), ("windinput".to_string(), 0));
        // 连字符/下划线/数字都是英文标识符里的常见成分，只要还有字母就放行。
        assert_eq!(f("well-known"), ("well-known".to_string(), 0));
        assert_eq!(f("utf8"), ("utf8".to_string(), 0));

        // 空白 → 空码（前缀查询命中不到，加了等于没加）
        assert_eq!(f("thank you").0, "", "带空格的码会让前缀查询永远查不到");
        // 非 ASCII → 空码（英文词库的码空间是 ASCII）
        assert_eq!(f("你好").0, "");
        assert_eq!(f("café").0, "");
        // 一个字母都没有 → 空码（纯数字/纯符号不是英文词）
        assert_eq!(f("123").0, "");
        assert_eq!(f("---").0, "");
        assert_eq!(f("").0, "");
    }

    fn coord(tag: &str) -> Arc<Coordinator> {
        coord_with_clip(tag, "")
    }

    /// 假剪贴板：`clipboard_get_text` 恒返回构造时给的串，并**记下被读了几次**。
    ///
    /// ⚠️ 每个测试协调器都必须注入它（`coord` 默认注入空串），否则桌面默认实现
    /// （`desktop-ui` 是默认 feature）会去读**开发机真实剪贴板**——本机跑测试的结果随
    /// 剪贴板内容漂移，而 CI 在 Linux 上恒读到空，于是这类失败只在本机出现、且看着像
    /// 随机失败。
    ///
    /// 计数是惰性读取的唯一守门：读一次最坏 40ms 且发生在按键线程上，多读一次不会有任何
    /// 报错，只会让用户按 Ctrl+= 时卡一下（2026-09-03 的真实反馈）。
    struct MockClip(String, Arc<std::sync::atomic::AtomicUsize>);
    impl crate::host_services::HostServices for MockClip {
        fn clipboard_get_text(&self) -> anyhow::Result<String> {
            self.1.fetch_add(1, Ordering::Relaxed);
            Ok(self.0.clone())
        }
    }

    fn coord_with_clip(tag: &str, clip: &str) -> Arc<Coordinator> {
        coord_counting_clip(tag, clip).0
    }

    /// 同 [`coord_with_clip`]，另外交出剪贴板读取次数的计数器。
    fn coord_counting_clip(
        tag: &str,
        clip: &str,
    ) -> (Arc<Coordinator>, Arc<std::sync::atomic::AtomicUsize>) {
        let path = std::env::temp_dir().join(format!("wind_addword_{tag}.redb"));
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let c = Coordinator::new_headless_with_store(Config::default(), None, store);
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        c.set_host_services(Arc::new(MockClip(clip.to_string(), reads.clone())));
        (c, reads)
    }

    /// 构造一个 key_down 事件（加词模式的按键分派只看 key_code 与修饰位）。
    fn key(vk: u32, mods: u32) -> wind_bridge::handler::KeyEventData {
        wind_bridge::handler::KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: mods,
            event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }
    }

    /// 按时间序模拟上屏：最早先入，最新最后（push_front 保证最新在前，对齐运行时）。
    fn push_commits(c: &Coordinator, items: &[&str]) {
        let mut h = c.recent_commits.lock().unwrap();
        for it in items {
            h.push_front(it.to_string());
        }
    }

    use std::sync::atomic::Ordering;
    use wind_bridge::handler::{KeyAction, MessageHandler};

    /// 撤销上屏计数（`last_commit_len`）随「同步落屏」按键更新：中文整词记字符数、
    /// 英文/标点逐键覆盖回 1（防残留旧中文计数误删）、emoji 按 UTF-16 单元、组合态不动。
    #[test]
    fn undo_commit_len_tracks_last_commit() {
        let c = coord("undo_len");
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 1, "默认删 1");

        c.note_commit_action(&Coordinator::commit_action("你好".into(), true));
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2, "中文整词记 2");

        // 英文逐键上屏刷回 1：这一步顶掉上面的中文计数，正是「敲 abc 后 undo 不误删你好」的关键。
        c.note_commit_action(&Coordinator::commit_action("a".into(), false));
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 1, "英文覆盖为 1");

        // emoji：UTF-16 surrogate pair 计 2 单元，与 TSF/macOS 删除量纲一致。
        c.note_commit_action(&Coordinator::commit_action("😀".into(), true));
        assert_eq!(
            c.last_commit_len.load(Ordering::Relaxed),
            2,
            "emoji 记 2 单元"
        );

        // 组合态（尚未落屏）不动计数。
        c.note_commit_action(&KeyAction::PassThrough);
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2, "组合态不覆盖");

        // 智能标点替换：按替换后光标前文本长度计。
        c.note_commit_action(&KeyAction::ReplaceBackward {
            count: 2,
            text: "—".into(),
        });
        assert_eq!(
            c.last_commit_len.load(Ordering::Relaxed),
            1,
            "替换后记新符号长"
        );
    }

    /// 焦点变化复位撤销计数：换窗/换文本框后光标前已非「刚上屏那段」，退化删 1。
    #[test]
    fn focus_lost_resets_undo_commit_len() {
        let c = coord("undo_focus");
        c.note_commit_action(&Coordinator::commit_action("世界".into(), true));
        assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2);
        c.handle_focus_lost(0, wind_bridge::handler::FocusLostReason::Thread);
        assert_eq!(
            c.last_commit_len.load(Ordering::Relaxed),
            1,
            "失焦后退化删 1"
        );
    }

    /// 打字中（输入缓冲非空）触发 undo 应提前返回、不消耗计数（不 swap）。
    #[test]
    fn undo_commit_ignored_while_composing() {
        let c = coord("undo_composing");
        c.last_commit_len.store(5, Ordering::Relaxed);
        c.state.lock().unwrap().input_buffer = "abc".to_string();
        c.cmd_undo_commit();
        assert_eq!(
            c.last_commit_len.load(Ordering::Relaxed),
            5,
            "缓冲非空应忽略，计数不变"
        );
    }

    /// learn_phrase_on_commit 6a 晋升路径：promote_count=2，造词两次达阈值 → 自动晋升。
    #[test]
    fn learn_phrase_promotes_at_threshold_via_6a() {
        use wind_candidate::CandidateSource as CS;
        use wind_store::Store;
        let path = std::env::temp_dir().join("wind_addword_promote6a.redb");
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());

        // 开启自动造词 + 晋升阈值=2。闸门归 [schema.pinyin.auto_learn]——
        // `learn_phrase_on_commit` 的产出恒是拼音词（纯码表在 is_codetable() 处已返回）。
        let mut cfg = Config::default();
        cfg.schema.pinyin.auto_learn.enabled = true;
        cfg.schema.pinyin.auto_learn.promote_count = 2;

        let c = Coordinator::new_headless_with_store(cfg, None, store.clone());

        // 辅助：构造含 2 段的 State 并调用 learn_phrase_on_commit
        let make_state_with_segs = || {
            let mut st = c.state.lock().unwrap();
            // 码表段：raw_code 与 code 同为击键码（无双拼转换）。
            st.committed_segs = vec![
                (
                    "aa".to_string(),
                    "aa".to_string(),
                    "工".to_string(),
                    CS::CodeTable,
                    0,
                ),
                (
                    "bb".to_string(),
                    "bb".to_string(),
                    "人".to_string(),
                    CS::CodeTable,
                    0,
                ),
            ];
            drop(st);
        };

        // 第 1 次造词 → temp count=1，未达阈值
        make_state_with_segs();
        {
            let st = c.state.lock().unwrap();
            c.learn_phrase_on_commit(&st, false); // 分步造词路径，非整句
        }

        let active = c.engine_mgr.active_schema_id();
        let schema = c.engine_mgr.data_schema_id(&active);
        let count1 = store.get_temp_word(&schema, "aabb", "工人").unwrap();
        assert_eq!(count1, Some(1), "第 1 次造词后临时层 count 应为 1");

        // 第 2 次造词 → temp count=2，达阈值 → 自动晋升
        make_state_with_segs();
        {
            let st = c.state.lock().unwrap();
            c.learn_phrase_on_commit(&st, false); // 分步造词路径，非整句
        }

        // 临时层应已删除
        let count2 = store.get_temp_word(&schema, "aabb", "工人").unwrap();
        assert_eq!(count2, None, "晋升后临时层应删除");
        // 用户词层应含晋升的词
        let user = store.get_user_words(&schema, "aabb").unwrap();
        assert!(
            user.iter().any(|r| r.text == "工人"),
            "用户词层应含晋升的词"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recent_chars_order_and_truncate() {
        let c = coord("recent");
        push_commits(&c, &["你", "好", "世界"]);
        assert_eq!(
            c.add_word_recent_chars(20).iter().collect::<String>(),
            "你好世界"
        );
        assert_eq!(
            c.add_word_recent_chars(2).iter().collect::<String>(),
            "世界"
        );
    }

    #[test]
    fn enter_sets_default_len_and_word() {
        let c = coord("enter");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert!(st.add_word_active);
        assert_eq!(st.add_word_len, 2);
        assert_eq!(c.add_word_current_word(&st), "你好");
    }

    #[test]
    fn enter_single_char_caps_len_to_one() {
        let c = coord("single");
        push_commits(&c, &["好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert_eq!(st.add_word_len, 1);
        assert_eq!(c.add_word_current_word(&st), "好");
    }

    #[test]
    fn enter_no_history_zero_len() {
        let c = coord("empty");
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert!(st.add_word_active);
        assert_eq!(st.add_word_len, 0);
        assert!(st.add_word_code.is_empty());
    }

    #[test]
    fn adjust_length_clamps() {
        let c = coord("adjust");
        push_commits(&c, &["一", "二", "三"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert_eq!(st.add_word_len, 2);
        c.adjust_add_word_length(&mut st, 1);
        assert_eq!(st.add_word_len, 3);
        c.adjust_add_word_length(&mut st, 1); // 上限 = 字符数 3
        assert_eq!(st.add_word_len, 3);
        assert_eq!(c.add_word_current_word(&st), "一二三");
        c.adjust_add_word_length(&mut st, -5); // 下限 1
        assert_eq!(st.add_word_len, 1);
        assert_eq!(c.add_word_current_word(&st), "三");
    }

    #[test]
    fn confirm_empty_code_aborts_without_write() {
        let c = coord("abort");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert!(st.add_word_code.is_empty(), "headless 无引擎，编码应为空");
        c.confirm_add_word(&mut st);
        assert!(!st.add_word_active, "确认后应退出加词模式");
        drop(st);
        let schema = c.engine_mgr.active_schema_id();
        let schema = c.engine_mgr.data_schema_id(&schema); // 与写入路径一致
        let store = c.store.as_ref().unwrap();
        // 编码为空时不应写任何用户词；遍历常见空码均无记录。
        assert!(store.get_user_words(&schema, "").unwrap().is_empty());
    }

    #[test]
    fn confirm_with_code_writes_user_word() {
        let c = coord("write");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        st.add_word_code = "nihao".to_string(); // headless 无引擎，手动注入编码
        c.confirm_add_word(&mut st);
        assert!(!st.add_word_active);
        drop(st);
        let schema = c.engine_mgr.active_schema_id();
        let schema = c.engine_mgr.data_schema_id(&schema); // 与写入路径一致（拼音族→"pinyin"；headless 下 ""→""）
        let store = c.store.as_ref().unwrap();
        let recs = store.get_user_words(&schema, "nihao").unwrap();
        assert_eq!(recs.len(), 1, "应写入 1 条用户词");
        assert_eq!(recs[0].text, "你好");
        assert_eq!(recs[0].weight, 1200);
    }

    #[test]
    fn exit_resets_state() {
        let c = coord("exit");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert!(st.add_word_active);
        c.exit_add_word_mode(&mut st);
        assert!(!st.add_word_active);
        assert!(st.add_word_chars.is_empty());
        assert_eq!(st.add_word_len, 0);
        assert!(st.add_word_code.is_empty());
    }

    #[test]
    fn trim_segs_keeps_tail_within_max() {
        use wind_candidate::CandidateSource as S;
        let seg = |c: &str, t: &str| {
            (
                c.to_string(),
                c.to_string(),
                t.to_string(),
                S::CodeTable,
                0u64,
            )
        };
        let segs = vec![seg("aa", "工人"), seg("bb", "们"), seg("cc", "好的")];
        // 总 5 字，max=3 → 从尾部保留 "们"(1)+"好的"(2)=3 字，起始索引 1
        assert_eq!(trim_segs_start(&segs, 3), 1);
        // max=0 不限
        assert_eq!(trim_segs_start(&segs, 0), 0);
        // max=1 装不下末段(2字) → 起始=len（全部裁掉，调用方跳过）
        assert_eq!(trim_segs_start(&segs, 1), 3);
    }

    /// 加词参数串：空字段跳过（设置端把"空串"与"没传"当同一回事）。
    /// 页名由 `open_settings_with` 单独带，故这里只断言参数部分。
    #[test]
    fn build_page_omits_empty_fields() {
        use crate::handle_menu::build_settings_args;
        let args = |w, c, s| build_settings_args(&[("text", w), ("code", c), ("schema", s)]);
        assert_eq!(
            args("你好", "nihao", "pinyin"),
            "--text=你好 --code=nihao --schema=pinyin"
        );
        assert_eq!(args("", "", ""), "");
        assert_eq!(args("你好", "", "wubi"), "--text=你好 --schema=wubi");
    }

    /// 含空白的值必须加引号：参数串经 ShellExecuteW 交给设置端后由
    /// CommandLineToArgvW 重新切分，不加引号会把 `--text=你 好` 拆成两个 argv，
    /// 设置端只收得到 `--text=你`（剪贴板加词的整段文本正是这种值）。
    #[test]
    fn build_args_quotes_values_with_whitespace() {
        use crate::handle_menu::build_settings_args;
        assert_eq!(
            build_settings_args(&[("text", "hello world"), ("schema", "wubi86")]),
            "--text=\"hello world\" --schema=wubi86"
        );
    }

    /// 拼音方案下含非汉字的词**不取码**，加词中止，不留坏数据。
    ///
    /// 病灶：`gen_pinyin` 的 `filter_map` 静默跳过无读音字符，「你好a」产出 `ni hao`——
    /// 码覆盖不全，且（在 split 修复前）还带着空格落成 key，词彻底打不出来。快捷加词的
    /// 字符池直接取最近上屏字符，中英混输下「你好a」这种一抓一个准。
    ///
    /// 同时补上一处不一致：**纯**英文取码本就为空、早已中止，只有混合的情况漏了过去。
    ///
    /// ⚠️ 本测试**必须用真实词库**：headless 无引擎时 `is_pinyin` 判false、reverse 表也空，
    /// 取码恒为空串 —— 那样「断言空码」会因为完全错误的原因通过，成为典型假绿。故对照组
    /// （纯汉字词必须取得出码）是这条测试的命脉，不能省。
    #[test]
    fn pinyin_rejects_non_han_word_and_adds_han_word() {
        let data =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !data.join("schemas/pinyin.schema.toml").exists() {
            return;
        }
        let mut cfg = Config::default();
        cfg.schema.available = vec!["pinyin".into()];
        cfg.schema.active = "pinyin".into();
        let path = std::env::temp_dir().join("wind_addword_fallback_flat.redb");
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).unwrap());
        let c = Coordinator::new_headless_with_store(cfg, Some(&data), store);

        let sid = c.engine_mgr.active_schema_id();
        let sid = c.engine_mgr.data_schema_id(&sid);

        // ① 含非汉字「你好a」→ 不取码、不写库
        push_commits(&c, &["你", "好", "a"]);
        {
            let mut st = c.state.lock().unwrap();
            c.enter_add_word_mode(&mut st);
            c.adjust_add_word_length(&mut st, 1); // 默认 2（"好a"）→ 3（"你好a"）
            assert_eq!(c.add_word_current_word(&st), "你好a");
            assert!(
                st.add_word_code.is_empty(),
                "含非汉字应取不出码，实际 {:?}",
                st.add_word_code
            );
            c.confirm_add_word(&mut st);
        }
        let store = c.store.as_ref().unwrap();
        assert!(
            store.get_user_words(&sid, "nihao").unwrap().is_empty(),
            "不得把丢了字母的「ni hao → 你好a」写进库"
        );
        assert!(
            store.get_user_words(&sid, "ni hao").unwrap().is_empty(),
            "更不得写出带空格的坏 key"
        );

        // ② 对照组：纯汉字「你好」照常取码入库。
        //    没有这一半，上面的「空码」断言在引擎根本没加载时也会通过——那是假绿。
        push_commits(&c, &["你", "好"]);
        {
            let mut st = c.state.lock().unwrap();
            c.enter_add_word_mode(&mut st);
            assert_eq!(c.add_word_current_word(&st), "你好");
            assert_eq!(st.add_word_code, "nihao", "落库 code 必须扁平、无空格");
            assert_eq!(st.add_word_boundary, 0b101, "音节边界 ni|hao");
            // 展示形态还原得出空格（与设置页「出码」按钮、词库列表同形）
            assert_eq!(c.add_word_display_code(&st), "ni hao");
            c.confirm_add_word(&mut st);
        }
        assert_eq!(
            store.get_user_words(&sid, "nihao").unwrap().len(),
            1,
            "扁平 key 必须查得到——这正是候选查询用的形态"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ──────────────────────────────────────────────────────────────────────
    // 编码的展示形态（带音节空格）——三个加词界面的共用口径
    // ──────────────────────────────────────────────────────────────────────

    /// 存储域（扁平 key + boundary）→ 展示域（带空格音节码）的转换语义。
    /// 拼音方案下这一步此前完全没做，Ctrl+Shift+= 的对话框才会预填出 `nihao`。
    #[test]
    fn display_code_restores_syllable_spaces() {
        use super::display_code;
        // 拼音多音节：boundary 的 bit 位是各音节起始**字节**偏移（ni|hao → {0,2}）
        assert_eq!(display_code("nihao", 0b101), "ni hao");
        assert_eq!(display_code("chongqing", 0b100001), "chong qing");
        // 码表码 boundary 恒 0 → 恒等变换，五笔等方案不受影响
        assert_eq!(display_code("aabb", 0), "aabb");
        // 单音节（0b1）无内部边界可插，与 0 同样原样返回
        assert_eq!(display_code("ni", 0b1), "ni");
        assert_eq!(display_code("", 0), "");
    }

    /// `add_word_code` 与 `add_word_boundary` 必须**成对**读出。
    ///
    /// 本次 bug 的形态正是「字段在 State 里、但消费端只读了一半」——boundary 一直被算出来
    /// 并存着，三个显示/传参点却都只取扁平 code。这条测试钉的就是这一步不再退化。
    #[test]
    fn add_word_display_code_reads_boundary_from_state() {
        let c = coord("display_code_state");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        // headless 无引擎，calc_add_word_code 取不出码 → 手动注入引擎会给出的那一对
        st.add_word_code = "nihao".to_string();
        st.add_word_boundary = 0b101;
        assert_eq!(
            c.add_word_display_code(&st),
            "ni hao",
            "展示形态必须带音节空格，与设置页「出码」按钮及词库列表同形"
        );
        // 码表方案（boundary=0）不受影响
        st.add_word_code = "aabb".to_string();
        st.add_word_boundary = 0;
        assert_eq!(c.add_word_display_code(&st), "aabb");
    }

    /// 带空格的码进参数串必须被引号包住。
    ///
    /// 这是本次改动**新走通**的组合：在此之前 code 恒无空格，`build_settings_args` 的加引号
    /// 分支从来只有 `--text`（剪贴板整段）会碰到。不加引号则 CommandLineToArgvW 会把
    /// `--code=ni hao` 切成两个 argv，设置端只收得到 `--code=ni`——比不带空格更糟。
    #[test]
    fn prefill_args_quote_spaced_code() {
        use super::display_code;
        use crate::handle_menu::build_settings_args;
        let code = display_code("nihao", 0b101);
        assert_eq!(
            build_settings_args(&[("text", "你好"), ("code", &code), ("schema", "pinyin")]),
            "--text=你好 --code=\"ni hao\" --schema=pinyin"
        );
    }

    // ── 剪贴板加词来源（Ctrl+= 的 Tab 切换 / Ctrl+Shift+= 的默认来源） ──────────

    /// Ctrl+= 默认仍是最近输入；Tab 切到剪贴板后**整段全选**（而非沿用默认 2 字）。
    #[test]
    fn tab_switches_to_clipboard_and_selects_whole() {
        let c = coord_with_clip("tabclip", "量子纠缠态");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert!(!st.add_word_from_clip, "有最近输入时默认来源必须是最近输入");
        assert_eq!(c.add_word_current_word(&st), "你好");

        c.toggle_add_word_source(&mut st);
        assert!(st.add_word_from_clip);
        assert_eq!(st.add_word_len, 5, "剪贴板来源默认全选");
        assert_eq!(c.add_word_current_word(&st), "量子纠缠态");

        // 再按一次切回，长度按最近输入的默认值重置
        c.toggle_add_word_source(&mut st);
        assert!(!st.add_word_from_clip);
        assert_eq!(st.add_word_len, 2);
        assert_eq!(c.add_word_current_word(&st), "你好");
    }

    /// 剪贴板来源下 ↓ 砍的是**尾部**——方向搞反会得到「子纠缠态」，且不报任何错。
    #[test]
    fn clipboard_source_trims_from_tail() {
        let c = coord_with_clip("cliptrim", "量子纠缠态");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        c.toggle_add_word_source(&mut st);
        c.adjust_add_word_length(&mut st, -1);
        assert_eq!(c.add_word_current_word(&st), "量子纠缠");
        c.adjust_add_word_length(&mut st, -1);
        assert_eq!(c.add_word_current_word(&st), "量子纠");
    }

    /// 最近输入为空、剪贴板有内容 ⇒ 进入时**自动落在剪贴板一侧**，但仍切得回来。
    #[test]
    fn enter_auto_lands_on_clipboard_when_no_recent() {
        let c = coord_with_clip("autoclip", "量子纠缠");
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert!(st.add_word_from_clip, "没有最近输入时应直接落在剪贴板");
        assert_eq!(c.add_word_current_word(&st), "量子纠缠");

        // 自动落点不是锁死：Tab 仍能切回（那一侧是空的，面板给空态）
        c.toggle_add_word_source(&mut st);
        assert!(!st.add_word_from_clip);
        assert_eq!(st.add_word_len, 0);
    }

    /// 反向不自动：最近输入有内容时，剪贴板可不可用都不改变默认来源。
    #[test]
    fn enter_keeps_recent_when_available() {
        for (tag, clip) in [("keepempty", ""), ("keepfull", "剪贴内容")] {
            let c = coord_with_clip(tag, clip);
            push_commits(&c, &["你", "好"]);
            let mut st = c.state.lock().unwrap();
            c.enter_add_word_mode(&mut st);
            assert!(!st.add_word_from_clip, "{tag}: 有最近输入就该停在最近输入");
        }
    }

    /// 剪贴板不可用时 Tab **照样切得过去**，切过去是剪贴板侧的空态（不是原地不动）。
    ///
    /// ⛔ 此前的「空则不许切」守卫已推翻：两个来源必须对称，否则 Tab 时灵时不灵。
    #[test]
    fn tab_switches_even_when_clipboard_unusable() {
        // 空 / 全空白 / 多行：三种都归到「剪贴板无可用内容」这一个可见状态。
        // ⚠️ 超长**不在此列**——它已改为截取前 ADD_WORD_MAX_LEN 字，见
        // `overlong_clipboard_is_truncated_not_rejected`。
        for (tag, clip) in [
            ("clipempty", "".to_string()),
            ("clipblank", "   \t ".to_string()),
            ("clipmulti", "第一行\n第二行".to_string()),
        ] {
            let c = coord_with_clip(tag, &clip);
            push_commits(&c, &["你", "好"]);
            let mut st = c.state.lock().unwrap();
            c.enter_add_word_mode(&mut st);
            assert!(
                c.add_word_pool(&st).is_empty() || !st.add_word_from_clip,
                "{tag}: 不合规的剪贴板必须当作空池"
            );

            c.toggle_add_word_source(&mut st);
            assert!(st.add_word_from_clip, "{tag}: 空的一侧也必须切得过去");
            assert_eq!(st.add_word_len, 0, "{tag}: 空池无可选词");
            let rows = c.add_word_panel_rows(&st);
            assert_eq!(rows[0].text, "快捷加词 · 剪贴板");
            assert_eq!(rows[1].text, "剪贴板无可用内容");

            c.toggle_add_word_source(&mut st);
            assert_eq!(c.add_word_current_word(&st), "你好", "{tag}: 切得回来");
        }
    }

    /// 恰好等于上限的剪贴板仍可用（守卫是 `>` 不是 `>=`，差一即整条功能对长词失效）。
    #[test]
    fn clipboard_at_max_len_is_usable() {
        let word = "字".repeat(ADD_WORD_MAX_LEN);
        let c = coord_with_clip("clipmax", &word);
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        c.toggle_add_word_source(&mut st);
        assert!(st.add_word_from_clip);
        assert_eq!(c.add_word_current_word(&st), word);
    }

    /// 剪贴板池在**进入模式时定格**：Tab 用的是 State 里的池，不再现读剪贴板
    /// （读一次最坏 40ms，见 clipboard_add_word_chars）。
    #[test]
    fn tab_uses_frozen_pool_not_a_fresh_read() {
        let c = coord_with_clip("clipfrozen", "定格");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        st.add_word_clip = Some("改过".chars().collect());
        c.toggle_add_word_source(&mut st);
        assert_eq!(
            c.add_word_current_word(&st),
            "改过",
            "Tab 必须用 State 里定格的池，而不是现读剪贴板"
        );
    }

    /// 退出加词模式必须连剪贴板池与来源一起清——留着会让下一次进入时首屏直接显示
    /// **上一次**的剪贴板内容（`enter` 虽会重读池，但 `from_clip` 残留为真即错位）。
    #[test]
    fn exit_clears_clipboard_source() {
        let c = coord_with_clip("clipexit", "残留");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        c.toggle_add_word_source(&mut st);
        assert!(st.add_word_from_clip);
        c.exit_add_word_mode(&mut st);
        assert!(!st.add_word_from_clip, "退出必须复位来源");
        assert!(
            st.add_word_clip.is_none(),
            "退出必须把剪贴板池复位成「没读过」"
        );
    }

    /// 超长剪贴板**截断取前 N 字**，不再整段作废。
    ///
    /// 原先超过上限就当作「无可用内容」，于是复制一句话按 Tab 切过去只看到空态——可用户
    /// 要的那个词往往就在开头。截断方向与剪贴板来源「取开头 N 字、↓ 砍尾」一致。
    #[test]
    fn overlong_clipboard_is_truncated_not_rejected() {
        let long: String = "量子纠缠态的测量与坍缩过程很复杂".to_string();
        assert!(long.chars().count() > ADD_WORD_MAX_LEN, "夹具本身要够长");
        let head: String = long.chars().take(ADD_WORD_MAX_LEN).collect();

        // Ctrl+= 面板：Tab 切过去拿到的是前 N 字
        let c = coord_with_clip("cliptrunc", &long);
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        c.toggle_add_word_source(&mut st);
        assert_eq!(c.add_word_current_word(&st), head);
        assert_eq!(st.add_word_len, ADD_WORD_MAX_LEN, "截断后仍是全选");
        drop(st);

        // Ctrl+Shift+= 直开：同样截断，而不是回退最近上屏
        let (word, _) = c.add_word_prefill_from_history();
        assert_eq!(word, head, "超长剪贴板须截断取用，不得回退最近输入");
    }

    /// ★★★ 有最近上屏时进入加词模式**一次剪贴板都不读**。
    ///
    /// 这是「Ctrl+= 有时明显卡顿」的守门（2026-09-03 用户反馈）：读一次最坏 sleep 重试
    /// 40ms，且发生在**按键线程**上、恰好与 C++ 建立占位 composition 同一拍。多数人进来
    /// 是要加刚打的字、根本不按 Tab，那 40ms 纯属白付。
    ///
    /// 多读一次不会有任何报错，只会让用户觉得卡——只有这条计数断言拦得住回归。
    #[test]
    fn entering_with_recent_input_never_reads_clipboard() {
        let (c, reads) = coord_counting_clip("lazyenter", "剪贴内容");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "有最近上屏时进入加词模式不得读剪贴板"
        );
        // ↑↓ 调长度、确认前的重算同样不该读
        c.adjust_add_word_length(&mut st, 1);
        c.adjust_add_word_length(&mut st, -1);
        assert_eq!(reads.load(Ordering::Relaxed), 0, "调词长不得读剪贴板");
    }

    /// 没有最近上屏时**才**读一次（那时要靠它决定自动落点），且此后不再重复读。
    #[test]
    fn clipboard_is_read_once_and_reused() {
        let (c, reads) = coord_counting_clip("lazyonce", "量子纠缠");
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert_eq!(reads.load(Ordering::Relaxed), 1, "自动落点判断读一次");
        assert!(st.add_word_from_clip);

        // Tab 来回切：池子已在 State 里，不得再读
        c.toggle_add_word_source(&mut st);
        c.toggle_add_word_source(&mut st);
        assert_eq!(reads.load(Ordering::Relaxed), 1, "Tab 复用定格池，不得重读");
        assert_eq!(c.add_word_current_word(&st), "量子纠缠");
    }

    /// 用户按 Tab 主动切过去时才读——那一次的等待是他要的。
    #[test]
    fn tab_reads_clipboard_on_demand() {
        let (c, reads) = coord_counting_clip("lazytab", "量子纠缠");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert_eq!(reads.load(Ordering::Relaxed), 0);

        c.toggle_add_word_source(&mut st);
        assert_eq!(reads.load(Ordering::Relaxed), 1, "切到剪贴板时读一次");
        assert_eq!(c.add_word_current_word(&st), "量子纠缠");

        // 切回最近上屏不读，再切回来也不读（池子已在 State 里）
        c.toggle_add_word_source(&mut st);
        c.toggle_add_word_source(&mut st);
        assert_eq!(reads.load(Ordering::Relaxed), 1, "只读那一次");
    }

    /// ★★ ESC 在**任何一种状态**下都必须退出加词模式。
    ///
    /// 2026-09-03 用户反馈「进入这个模式后按 ESC 不退出了」。协调器这一侧的分派本身没变，
    /// 但它此前从没有测试——ESC 走的是 `handle_add_word_key` 的第一条臂，而那条臂只要被
    /// 前面任何一个新增分支抢走（或 `add_word_active` 被别处清掉），表现就是「面板还在、
    /// 按键没反应」，没有任何报错。
    #[test]
    fn escape_exits_from_every_state() {
        // ① 最近上屏来源 ② 剪贴板来源 ③ 两侧都空的空态
        let cases: [(&str, &str, &[&str], bool); 3] = [
            ("esc_recent", "剪贴内容", &["你", "好"], false),
            ("esc_clip", "量子纠缠", &["你", "好"], true),
            ("esc_empty", "", &[], false),
        ];
        for (tag, clip, commits, switch) in cases {
            let c = coord_with_clip(tag, clip);
            push_commits(&c, commits);
            let mut st = c.state.lock().unwrap();
            c.enter_add_word_mode(&mut st);
            if switch {
                c.toggle_add_word_source(&mut st);
            }
            assert!(st.add_word_active, "{tag}: 前提——已进入加词模式");

            let act = c.handle_add_word_key(&mut st, &key(keymap::VK_ESCAPE, 0));
            assert!(
                matches!(act, KeyAction::ClearComposition),
                "{tag}: ESC 必须清掉占位组合，否则宿主那边的组合区留着不动"
            );
            assert!(!st.add_word_active, "{tag}: ESC 必须退出加词模式");
            assert!(st.add_word_clip.is_none(), "{tag}: 退出须复位剪贴板池");
            assert!(!st.add_word_from_clip, "{tag}: 退出须复位来源");
        }
    }

    /// 退格与 ESC 同一条臂（历史约定：加词面板里退格也是「取消」）。
    #[test]
    fn backspace_exits_like_escape() {
        let c = coord_with_clip("escback", "");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        c.handle_add_word_key(&mut st, &key(keymap::VK_BACK, 0));
        assert!(!st.add_word_active, "退格应与 ESC 同样退出");
    }

    /// 面板是**三行**，且操作提示落在 `comment` 而非 `text`。
    ///
    /// 后者是配色判据：放 text 会用候选正文色，面板上最不重要的一行反而最显眼。颜色不对
    /// 不会报任何错，只能靠这条断言盯着。
    #[test]
    fn panel_hint_is_a_dim_third_row() {
        let c = coord_with_clip("panelrows", "剪贴内容");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        let rows = c.add_word_panel_rows(&st);
        assert_eq!(rows.len(), 3, "标题 / 词与编码 / 提示");
        assert_eq!(rows[0].text, "快捷加词 · 最近输入");
        assert!(rows[0].comment.is_empty(), "标题行不再挂提示，否则又被撑宽");
        assert_eq!(rows[1].text, "你好");
        assert!(
            rows[2].text.is_empty(),
            "提示必须留在 comment 走注释色，放 text 会比标题还抢眼"
        );
        assert!(rows[2].comment.contains("Tab切换来源"));
        assert!(rows.iter().all(|r| r.no_index), "三行都不显序号");
    }

    /// 面板在两个来源下**长得一样**：来源后缀与 Tab 提示恒显示，与哪一侧有没有内容无关。
    #[test]
    fn panel_looks_the_same_on_both_sources() {
        // 两侧都空：仍带来源后缀、仍提示 Tab
        let c = coord("panelbothempty");
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        let rows = c.add_word_panel_rows(&st);
        assert_eq!(rows[0].text, "快捷加词 · 最近输入", "空也要标来源");
        assert_eq!(rows[1].text, "无最近输入");
        assert_eq!(rows[2].comment, "Tab切换来源  Esc关闭");

        c.toggle_add_word_source(&mut st);
        let rows = c.add_word_panel_rows(&st);
        assert_eq!(rows[0].text, "快捷加词 · 剪贴板");
        assert_eq!(rows[1].text, "剪贴板无可用内容");
        assert_eq!(
            rows[1].comment, "需要单行文本",
            "空态须交代准入条件，否则用户以为复制没生效"
        );
        assert_eq!(rows[2].comment, "Tab切换来源  Esc关闭", "两侧提示同形");
    }

    /// Ctrl+Shift+= 的来源优先级与 Ctrl+= **相反**：剪贴板优先、整段作词。
    #[test]
    fn from_history_prefers_clipboard() {
        let c = coord_with_clip("histclip", "量子纠缠");
        push_commits(&c, &["你", "好"]);
        let (word, _) = c.add_word_prefill_from_history();
        assert_eq!(word, "量子纠缠", "剪贴板可用时必须优先于最近输入");
    }

    /// 剪贴板不可用则回退最近输入（默认 2 字）——「如果剪贴板为空，则以最新输入的数据」。
    #[test]
    fn from_history_falls_back_to_recent() {
        // ⚠️ 超长**不在此列**：它已改为截断取用，不再回退（见
        // `overlong_clipboard_is_truncated_not_rejected`）。
        for (tag, clip) in [
            ("histempty", "".to_string()),
            ("histmulti", "一行\n二行".to_string()),
        ] {
            let c = coord_with_clip(tag, &clip);
            push_commits(&c, &["世", "界", "和", "平"]);
            let (word, _) = c.add_word_prefill_from_history();
            assert_eq!(word, "和平", "{tag}: 剪贴板不可用须回退最近输入末尾 2 字");
        }
    }

    /// 两个来源都没内容时预填为空（设置端加词界面开着但不填）。
    #[test]
    fn from_history_empty_when_no_source() {
        let c = coord_with_clip("histnone", "");
        let (word, code) = c.add_word_prefill_from_history();
        assert!(word.is_empty() && code.is_empty());
    }

    #[test]
    fn from_history_does_not_enter_add_word_mode() {
        let c = coord("fromhist");
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.open_add_word_from_history(&mut st);
        // 直开路径不得进入加词模式、不得改候选窗布局占位
        assert!(!st.add_word_active, "直开加词界面不应进入加词模式");
        assert!(st.add_word_chars.is_empty(), "不应填充加词字符池");
    }

    // ──────────────────────────────────────────────────────────────────────
    // dict.add（`coad` 剪贴板加词的落点）
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn sanitize_trims_but_never_truncates() {
        use super::sanitize_dict_add_text;
        assert_eq!(sanitize_dict_add_text("  你好 \r\n").unwrap(), "你好");
        // 首尾换行属无损清理；**内部**换行则拒绝，不做一行化（宁可不加也不加错）。
        assert!(
            sanitize_dict_add_text("你好\n世界").is_err(),
            "内含换行应拒绝"
        );
        assert!(sanitize_dict_add_text("   \t \n ").is_err(), "空白串应拒绝");
        assert!(sanitize_dict_add_text("").is_err());
    }

    #[test]
    fn derivable_word_rejects_non_han_and_overlong() {
        use super::{ADD_WORD_MAX_LEN, check_derivable_word};
        assert!(check_derivable_word("你好").is_ok());
        assert!(check_derivable_word("hello").is_err(), "纯英文取不出码");
        assert!(check_derivable_word("你好abc").is_err(), "混入非汉字应拒绝");
        // 全角/中文标点是造词终止符、不是素材（见 is_han 的刻意排除）。
        assert!(check_derivable_word("你好，").is_err(), "中文标点应拒绝");
        let long: String = std::iter::repeat_n('好', ADD_WORD_MAX_LEN + 1).collect();
        assert!(check_derivable_word(&long).is_err(), "超上限应拒绝");
        let ok: String = std::iter::repeat_n('好', ADD_WORD_MAX_LEN).collect();
        assert!(check_derivable_word(&ok).is_ok(), "恰好等于上限应放行");
    }

    #[test]
    fn toast_clamp_limits_length() {
        use super::toast_clamp;
        assert_eq!(toast_clamp("你好"), "你好");
        let long: String = std::iter::repeat_n('好', 40).collect();
        let out = toast_clamp(&long);
        assert_eq!(out.chars().count(), 17, "16 字 + 省略号");
        assert!(out.ends_with('…'));
    }

    /// 显式 code 路径：保持原行为（不受新增的汉字/长度守卫影响），照常写库。
    #[test]
    fn dict_add_with_explicit_code_writes_user_word() {
        let c = coord("dictadd_code");
        // 颜文字等无法自动取码的词条，显式给码时必须仍能加——新守卫只作用于推导路径。
        c.cmd_dict_add("(╯°□°)╯", "kaomoji").unwrap();
        let schema = c.engine_mgr.active_schema_id();
        let schema = c.engine_mgr.data_schema_id(&schema); // 与写入路径一致
        let store = c.store.as_ref().unwrap();
        let recs = store.get_user_words(&schema, "kaomoji").unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].text, "(╯°□°)╯");
    }

    /// 推导路径：headless 无引擎 → 取码为空 → 报错且**不写库**（不留空码孤儿记录）。
    /// 这是 `coad` 的实际形态（`dict.add(clip())`，只有一个参数）。
    #[test]
    fn dict_add_without_code_takes_derive_path_and_reports() {
        let c = coord("dictadd_derive");
        let err = c.cmd_dict_add("你好", "").unwrap_err().to_string();
        // 关键：不再是「暂未支持自动推导编码」——已进入推导，只是 headless 取不出码。
        assert!(err.contains("取不出编码"), "应走推导路径并如实报因: {err}");
        // 隐私红线：错误消息会进 warn 日志，不得回显用户输入。
        assert!(!err.contains("你好"), "错误消息不得含词本身: {err}");
        let schema = c.engine_mgr.active_schema_id();
        let schema = c.engine_mgr.data_schema_id(&schema);
        let store = c.store.as_ref().unwrap();
        assert!(
            store.get_user_words(&schema, "").unwrap().is_empty(),
            "取码失败不得写入空码记录"
        );
    }

    /// 剪贴板整段文本（多行）走推导路径：拒绝并提示，不截断、不入库。
    #[test]
    fn dict_add_rejects_multiline_clipboard() {
        let c = coord("dictadd_multiline");
        let err = c
            .cmd_dict_add("第一行\n第二行", "")
            .unwrap_err()
            .to_string();
        assert!(err.contains("换行"), "应因换行被拒: {err}");
        assert!(!err.contains("第一行"), "错误消息不得含剪贴板内容: {err}");
    }
}
