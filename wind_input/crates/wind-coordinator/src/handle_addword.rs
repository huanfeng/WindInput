//! 造词 / 加词：选中后自动造词、命令栏 dict.add 加用户词、快捷加词面板。
//! 快捷加词的设计与取舍见 `docs/architecture/quick-add-word-dialog-design.md`。

use crate::coordinator::{CommittedSeg, Coordinator, LEARN_ADD_WEIGHT, State};
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
/// 最大加词长度：出厂 wubi86 `[[encoder.rules]]` 的上界（`length_in_range = [4, 10]`），
/// 超过就取不出码，「选得出来的都加得进去」才成立。拼音本可更长、第三方码表规则可能更宽，
/// 都是有意接受的近似。见 `docs/architecture/quick-add-word-dialog-design.md`「上限为什么是 10 字」。
const ADD_WORD_MAX_LEN: usize = 10;
/// 手动加词默认权重（略高于系统词库归一化中位 1000，对齐 Go addWordMaxWeight）
const ADD_WORD_WEIGHT: i32 = 1200;
// 手动加的英文词要压过按词频名次赋权的系统词（见 `wind_dict::freq_rank`）。
const _: () = assert!(wind_dict::freq_rank::RANK_WEIGHT_MAX < ADD_WORD_WEIGHT);
/// 临时词库淘汰的检查间隔（每 N 次造词写入检查一次）。全表扫描代价高，而上限是软约束，
/// 略微超出无害，故不必每次都查。
const EVICT_CHECK_INTERVAL: usize = 64;

/// 自动造词超长裁剪：从尾部保留整段（最近输入优先）使合并字数 ≤ max_chars。
/// 返回保留区间的起始段索引；max_chars=0 不限（返回 0）。
fn trim_segs_start(segs: &[CommittedSeg], max_chars: usize) -> usize {
    if max_chars == 0 {
        return 0;
    }
    let mut total = 0;
    let mut start = segs.len();
    for (i, seg) in segs.iter().enumerate().rev() {
        let n = seg.text.chars().count();
        if total + n > max_chars {
            break;
        }
        total += n;
        start = i;
    }
    start
}

/// 汉字判定：只认表意文字区，刻意排除全角标点与中文标点（它们是造词的终止符而非素材）。
/// 与 `wind_candidate::is_han`（常用性判定域，含部首笔画）刻意不同源。
///
/// 区段定义在 [`wind_engine::encoder::is_han`]：加词准入与取码时的跳过判据必须同源，否则会
/// 「放进来了却取不出码」。
pub(crate) fn is_han(c: char) -> bool {
    wind_engine::encoder::is_han(c)
}

/// `dict.add` 入库文本规整：只去首尾空白，不做一行化 / 截断。来源可能是剪贴板整段文本
/// （`coad`），悄悄截断入库比直接拒绝更糟，不合格情形一律拒绝并提示（见 `check_derivable_word`）。
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

/// 加词推导取码按方案分的三条路。[`Coordinator::add_word_code_family`] 是唯一判据，准入校验
/// （`try_dict_add`）与取码（`calc_add_word_code`）各判一次就会漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddWordFamily {
    /// 码即单词本身。
    English,
    /// 码来自读音：非汉字一律不收。
    Pinyin,
    /// 按方案 `[[encoder.rules]]` 从单字全码组装（含混输主码表与类型未知的兜底）。
    CodeTable,
}

/// 码表方案推导取码时，整段文本（含被跳过的非汉字）的总长上限。
///
/// 码表取码只用其中的汉字，汉字数仍受 [`ADD_WORD_MAX_LEN`] 约束；这一道防剪贴板里一整句外文
/// 夹着一两个汉字。32 足以放下「姓名 + 手机号 / 座机 + 分机」这类组合。
const ADD_WORD_MAX_TEXT_LEN: usize = 32;

/// 推导编码路径专属的额外校验。
///
/// `han_only`：拼音 / 英文类方案传 `true`（非汉字没有读音，会产出覆盖不全的码）。码表方案传
/// `false`：取码跳过码表里没码的非汉字（如姓名+手机号），有没有可取码的字交给
/// `wind_engine::encoder::encode_text` 判（`〇` 这类有码的非汉字这里判不出来）。
///
/// 码表方案按汉字数比 [`ADD_WORD_MAX_LEN`]，另有总长上限 [`ADD_WORD_MAX_TEXT_LEN`]。显式给了
/// code 的调用方不走这里：那是用户明确意图（颜文字、外文等）。
fn check_derivable_word(word: &str, han_only: bool) -> anyhow::Result<()> {
    if han_only {
        let n = word.chars().count();
        if n > ADD_WORD_MAX_LEN {
            anyhow::bail!("内容过长（{} 字，上限 {}）", n, ADD_WORD_MAX_LEN);
        }
        if !word.chars().all(is_han) {
            anyhow::bail!("含非汉字，无法自动取码");
        }
        return Ok(());
    }
    let total = word.chars().count();
    if total > ADD_WORD_MAX_TEXT_LEN {
        anyhow::bail!("内容过长（{} 字，上限 {}）", total, ADD_WORD_MAX_TEXT_LEN);
    }
    let han = word.chars().filter(|c| is_han(*c)).count();
    if han > ADD_WORD_MAX_LEN {
        anyhow::bail!("汉字过多（{} 字，上限 {}）", han, ADD_WORD_MAX_LEN);
    }
    Ok(())
}

/// 加词编码的展示 / 交换形态：扁平 key + boundary → 带音节空格的音节码（`ni hao`）。
///
/// 存储域是扁平 key + boundary（带空格的串成了 key 就前缀查不中），展示 / 交换域是带空格串，
/// 与设置页词库列表、「出码」按钮一致。回写由 [`crate::user_words_api::normalize_add_code`]
/// 拆回扁平 key，空格当作显式切分采信。码表码 boundary 恒 0，本函数对非拼音方案是恒等变换。
fn display_code(code: &str, boundary: u64) -> String {
    wind_store::wdict::join_code_by_boundary(code, boundary)
}

/// 加词界面的默认上下文：设置端在**没有预填参数**时据此把窗口填成可用状态。
///
/// 服务于 `wind-setting --add-word` 这条裸入口：它不经输入法热键，拿不到方案与最近输入。
pub struct AddWordContext {
    /// 加词目标方案 id（混输已解析到主码表方案，见 `add_word_target_schema`）。
    pub schema_id: String,
    /// 最近上屏文本，取字符池上限那么多（[`ADD_WORD_MAX_LEN`]）而非默认词长：要加的词多是逐字 /
    /// 分段上屏的，取末尾两字往往只截到半个词，多给的部分删起来便宜。
    pub recent_text: String,
    /// 加词字数上限（[`ADD_WORD_MAX_LEN`]）。
    ///
    /// 设置端也会自己读剪贴板填词条，而它不能依赖 core 的 crate，拿不到这个上限。
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

/// 造词的方案对：出码方案与入库方案是两个不同的 id。
///
/// 出码要真实方案（读它的 `[[encoder.rules]]` 与码表词库），入库要数据方案
/// （混输折叠到主码表）。对齐 `add_word_target_schema` 的既有区分。
pub(crate) struct PhraseSchemas {
    pub(crate) encode: String,
    pub(crate) write: String,
}

impl Coordinator {
    // ──────────────────────────────────────────────────────────────────────
    // 码表自动造词的接线与 IO；滑窗决策在 `crate::draft_window`（纯逻辑）。
    // ──────────────────────────────────────────────────────────────────────

    /// 按键来源的上屏后处理（自提交打点 + 喂草稿滑窗）的测试合成口。生产路径经
    /// `finish_session_action` 按来源分别调两步，见 `session_exit::exit_steps`。
    #[cfg(test)]
    pub(crate) fn note_commit_action(&self, action: &KeyAction) {
        self.note_self_commit(action);
        self.note_draft_commit(action);
    }

    /// 自提交打点：撤销上屏计数 + SelectionChanged 回声判别的时间戳。
    pub(crate) fn note_self_commit(&self, action: &KeyAction) {
        // 撤销上屏（`ime.undo_commit`）：记录本次落到光标前的字符数（UTF-16 单元，对齐
        // ReplaceBackward 的量纲）。每次上屏顶掉上一次计数，故 undo 只删「刚输入完那次」；
        // 组合态尚未落屏，不动计数。
        let committed = match action {
            KeyAction::InsertText { text, .. }
            | KeyAction::InsertTextWithCursor { text, .. }
            | KeyAction::ReplaceBackward { text, .. } => text.as_str(),
            KeyAction::CommitAndHoldComposition { commit_text, .. }
            | KeyAction::CommitThenDeferComposition { commit_text, .. } => commit_text.as_str(),
            _ => "",
        };
        // 撤销删的是光标前的字：带光标定位的插入（配对「（|）」）只计光标前那段。
        // `cursor_offset` 是从末尾往回退的格数（DLL 合成同样次数的 VK_LEFT），按字符取右段。
        let n = match action {
            KeyAction::InsertTextWithCursor { cursor_offset, .. } => {
                let right: usize = committed
                    .chars()
                    .rev()
                    .take(*cursor_offset as usize)
                    .map(char::len_utf16)
                    .sum();
                committed.encode_utf16().count() - right
            }
            _ => committed.encode_utf16().count(),
        };
        if n > 0 {
            self.last_commit_len
                .store(n, std::sync::atomic::Ordering::Relaxed);
        }

        // 自提交打点服务于 SelectionChanged 的回声判别，与造词开关无关。判据必须与上面的
        // `committed` 同源：凡真落屏的文字都会让宿主回送 SelectionChanged。漏打一个变体（如满码
        // 自动上屏的 `CommitThenDeferComposition`）会让回声被判成用户移动光标、清掉
        // `caret_cache_verified`，长按时候选窗一次都不显示（见 [`Coordinator::arm_pending_first_show`]）。
        if !committed.is_empty() {
            *self
                .last_self_commit
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        }
    }

    /// 造词是否启用（码表/混输方案 + 开关开启）。拼音方案走 `[pinyin.auto_learn]` 的
    /// 选词即学路线，不进本缓冲。
    pub(crate) fn auto_phrase_enabled(&self) -> bool {
        self.engine_mgr.auto_phrase_enabled()
    }

    /// 终止信号统一入口（回车/焦点丢失/IME 停用/模式切换/切换方案/光标移动）。
    ///
    /// 标点与空格不走这里，由 `note_draft_commit` 按「文本含非汉字」内联判定。
    /// 它断的是滑窗草稿流（函数名沿用旧称）。`reason` 只进 DEBUG 日志。
    pub(crate) fn terminate_auto_phrase(&self, reason: &str) {
        self.terminate_draft_window(reason);
    }

    /// idle 超时（连续单字最大间隔）。0 = 用默认 5s。
    pub(crate) fn auto_phrase_idle_timeout(&self) -> std::time::Duration {
        let ms = self
            .rt()
            .config
            .schema
            .codetable
            .auto_phrase
            .idle_timeout_ms;
        if ms == 0 {
            crate::draft_window::DEFAULT_IDLE_TIMEOUT
        } else {
            std::time::Duration::from_millis(ms as u64)
        }
    }

    pub(crate) fn resolve_phrase_schemas(&self) -> Option<PhraseSchemas> {
        let active = self.engine_mgr.active_schema_id();
        let encode = if self.engine_mgr.schema_engine_type(&active).as_deref() == Some("mixed") {
            match self.engine_mgr.mixed_primary_schema(&active) {
                Some(s) => s,
                None => {
                    debug!("auto-phrase: 混输方案主码表缺失，跳过造词");
                    return None;
                }
            }
        } else {
            active.clone()
        };
        // 反查索引就绪、只缺单字全码表（被闲置清扫释放，内存设计 §7）：就地建回来，否则停手
        // 后回来的头几个词会被整批丢掉。本函数只在后台草稿落库线程上调，不碰按键线程。
        if self.engine_mgr.reverse_index_if_ready(&encode).is_some()
            && !self.engine_mgr.single_char_codes_ready(&encode)
        {
            self.engine_mgr.prewarm_single_char_codes(&encode);
        }
        if self.engine_mgr.reverse_index_if_ready(&encode).is_none()
            || !self.engine_mgr.single_char_codes_ready(&encode)
        {
            debug!("auto-phrase: 词库索引未就绪，跳过本次造词并后台预热");
            self.ensure_word_encoding_async(&encode);
            return None;
        }
        let write = self
            .engine_mgr
            .write_data_schema_id(&active, CandidateSource::CodeTable)
            .or_else(|| {
                debug!("auto-phrase: 无法归属入库方案，跳过造词");
                None
            })?;
        Some(PhraseSchemas { encode, write })
    }

    /// 给一个词取码并过两道查重闸；通过则返回它的码表词组码。
    ///
    /// 返回：`Err` = 取码失败（调用方据此汇总 info 日志）；`Ok(None)` = 查重命中、不必造；
    /// `Ok(Some(code))` = 可造。`code_hints` 是各字上屏时用户实际选中的码，
    /// 取码优先按它取位，见 `EngineManager::encode_word_with_hints`。
    ///
    /// 两道查重（系统词库 / 用户词库）：造一个本来就打得出的词只会多一条重复项。
    /// `also_skip_temp` 供草稿层用：临时词库已有即用户已用过，再记草稿只占容量。
    pub(crate) fn encode_and_dedup(
        &self,
        sc: &PhraseSchemas,
        word: &str,
        code_hints: &[(char, String)],
        also_skip_temp: bool,
    ) -> Result<Option<String>, wind_engine::encoder::EncodeError> {
        let code = match self
            .engine_mgr
            .encode_word_with_hints(&sc.encode, word, code_hints)
        {
            Ok(c) => c,
            Err(e) => {
                // 排查「自动造词不生效」的关键线索（常是某字没有全码）。DEBUG 级才可带字符，
                // INFO 级汇总由调用方按批打。
                debug!("auto-phrase: 取码失败，整词作废（{}）: {}", word, e);
                return Err(e);
            }
        };
        // 查重①系统词库：反查索引给的是该词的实际编码列表（`a/ab/abc`）。就绪闸已保证
        // 这里的 `None` 不是「索引没建好」。
        let existing = self
            .engine_mgr
            .word_codes_in(&sc.encode, word)
            .unwrap_or_default();
        if existing.split('/').any(|c| c == code) {
            debug!("auto-phrase: 系统词库已有 {} -> {}，跳过", code, word);
            return Ok(None);
        }
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        // 查重②用户词库：同「码+词」已存在则不再写（否则候选会出现重复项）。
        if let Ok(recs) = store.get_user_words(&sc.write, &code)
            && recs.iter().any(|r| r.text == word)
        {
            debug!("auto-phrase: 用户词库已有 {} -> {}，跳过", code, word);
            return Ok(None);
        }
        if also_skip_temp
            && let Ok(recs) = store.get_temp_words(&sc.write, &code)
            && recs.iter().any(|r| r.text == word)
        {
            debug!("draft: 临时词库已有 {} -> {}，不再记草稿", code, word);
            return Ok(None);
        }
        Ok(Some(code))
    }

    /// 临时词库上限淘汰。按写入次数节流——每次造词都全表扫描代价过高，而上限本身
    /// 是软约束（略微超出无害）。`max_entries = 0` 视为不限。
    ///
    /// 上限按数据域取：英文类方案读 `schema.english.auto_learn.temp_max_entries`，
    /// 其余沿用码表那份——两者各在自己的 schema 分区里淘汰，互不相干。
    pub(crate) fn maybe_evict_temp(&self, store: &wind_store::Store, schema: &str) {
        let english = self.is_english_data_schema(schema);
        let max = {
            let rt = self.rt();
            if english {
                rt.config.schema.english.auto_learn.temp_max_entries
            } else {
                rt.config.schema.codetable.auto_phrase.temp_max_entries
            }
        };
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

    /// 加词到用户层。`code` 为空时按当前方案规则推导（`dict.add` 注册签名的承诺，见
    /// `wind-cmdbar` funcs/dict_ime.rs；系统短语 `coad` 依赖它）。
    ///
    /// 成功弹一次 toast 回显入库的词。失败不在此弹：由 [`Coordinator::run_command_candidate`]
    /// 统一收口，否则会弹两个 toast。
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
    /// 隐私红线（docs/logging-convention.md）：返回的错误会进 warn 日志，消息内不得含词本身或
    /// 剪贴板内容。也不自带 `dict.add:` 前缀，`runtime_err` 已负责加。
    fn try_dict_add(&self, text: &str, code: &str) -> anyhow::Result<String> {
        let Some(store) = &self.store else {
            anyhow::bail!("无词库存储");
        };
        let word = sanitize_dict_add_text(text)?;
        let (code, boundary) = if code.is_empty() {
            // 码表方案跳过没码的非汉字取码；拼音 / 英文仍要求整段是汉字。
            let han_only = self.add_word_code_family().1 != AddWordFamily::CodeTable;
            check_derivable_word(word, han_only)?;
            // 与快捷加词同一套推导；boundary 一并取回（只有拼音那条路给得出音节边界）。
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
        // 加词按码表语义归属：混输落主码表方案，primary 缺失则报错不静默写孤儿。必须与推导侧
        // （`add_word_target_schema` 对混输同样解析到 primary）同步，否则会把拼音码写进码表词库；
        // 非混输时 `write_data_schema_id` 忽略 source。
        let Some(schema) = self
            .engine_mgr
            .write_data_schema_id(&active, CandidateSource::CodeTable)
        else {
            anyhow::bail!("混输方案主码表缺失，无法归属加词");
        };
        store.add_user_word(&schema, &code, word, ADD_WORD_WEIGHT, boundary)?;
        crate::user_words_api::clear_deleted_for_added(self, &schema, &[(&code, word)]);
        debug!("dict.add: 已加词 {} -> {}", code, word);
        Ok(word.to_string())
    }

    /// 拼音自动造词：用户分步组成（committed_segs ≥2 段），或一次选中引擎合成的整句（单段，
    /// `single_is_synthesized`）时学。码 = 各段码拼接，词 = 各段汉字拼接，写入临时层。
    ///
    /// 只管拼音：`committed_segs` 是拼音专属的逐步转换态，码表造词走滑窗草稿
    /// （`crate::draft_window`），在此兼管会对同一次输入重复造词。
    ///
    /// 单段整句单独放行：段数是过程特征，造词关心的是产出是不是词库里没有的多字词。判据取
    /// `Candidate::is_synthesized`：「单段且够长」会把直接选中的词典整词写进临时层；`is_sentence`
    /// 在整句与词典候选同文合并时也会给已有词典候选补标。判据是候选级信息，三个调用点
    /// （`handle_candidate` / `handle_temp` / `handle_mode`）都要传。
    ///
    /// 返回写入临时层的 code（未造词则 `None`），调用方据此跳过「6b 临时词使用累积」的重复计数：
    /// 单段整句时两者 key 相同，不跳会让 count 每次 +2；多段时 key 不同，两者都该执行。
    ///
    /// 主输入路专用（闸门与归属按活跃方案）；overlay 走 [`Self::learn_phrase_on_commit_in`]。
    pub(crate) fn learn_phrase_on_commit(
        &self,
        state: &State,
        single_is_synthesized: bool,
    ) -> Option<String> {
        self.learn_phrase_on_commit_in(state, single_is_synthesized, None)
    }

    /// [`Self::learn_phrase_on_commit`] 的内核。`overlay` = overlay 模式下产出这些段的方案
    /// （临拼：`temp_pinyin_schema`；快捷输入：本次候选的成员方案 `mix_candidate_owner`）。
    ///
    /// overlay 不能按活跃方案判：主方案是五笔时 `is_codetable()` 恒真，overlay 分步组词永远不造。
    /// 故 `Some(schema)` 时闸门与归属一律按该方案：只接拼音型与混输型；各段来源必须一致
    /// （快捷输入可先选拼音「你好」再选英文 `hel`，拼成的词谁也打不出来）。
    pub(crate) fn learn_phrase_on_commit_in(
        &self,
        state: &State,
        single_is_synthesized: bool,
        overlay: Option<&str>,
    ) -> Option<String> {
        // 暂停学习：拼音分步 / 整句造词不进临时词库（返回 None，调用方的 6b 照常判，
        // 它自己也挡）。
        if self.learning_suspended() {
            return None;
        }
        // 单段仅当它是整句解时放行；其余仍要求 ≥2 段。
        if state.committed_segs.len() < 2
            && !(state.committed_segs.len() == 1 && single_is_synthesized)
        {
            return None;
        }
        // 纯码表方案不经此路。混输不排除：其拼音子引擎的分步转换是合法的拼音造词路径，与码表侧
        // 草稿造词是不同维度。overlay 按它自己的方案判。
        match overlay {
            None if self.engine_mgr.is_codetable() => return None,
            Some(o)
                if !matches!(
                    self.engine_mgr.schema_engine_type(o).as_deref(),
                    Some("pinyin" | "mixed")
                ) =>
            {
                return None;
            }
            _ => {}
        }
        // 走到这里的产出恒是拼音词，故闸门、字数上下限、晋升阈值统一读 `[schema.pinyin.auto_learn]`，
        // 混输也是：学什么词就读什么词的配置，不能按方案类型改读码表 `auto_phrase`。
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
        // 只剩一段时的放行判据是原始段数为 1，不是 `single_is_synthesized`：多段被裁到只剩末段
        // 时它同样为真，那时学到的只是末段自己的词。
        if segs.is_empty() || (segs.len() < 2 && state.committed_segs.len() != 1) {
            return None;
        }
        // 拼接各段码，并把段内音节边界平移到全局位置（段自身可能是多音节整词）。任一段无边界
        // 则整词作废为 0，半截边界比没有更糟。取全拼 code 而非 raw_code。
        let mut code = String::new();
        let mut boundary = 0u64;
        let mut boundary_ok = true;
        for seg in segs {
            // 优先取规范词条码（`learn`）：模糊音命中时候选 `code` 是所打原码（`senri`），拼出的
            // 混域码谁也打不出。边界跟着走：模糊命中的候选 `boundary` 恒 0，`learn` 里的才是真值。
            let (c, b) = match &seg.learn {
                Some((dict_code, dict_boundary)) => (dict_code.as_str(), *dict_boundary),
                None => (seg.code.as_str(), seg.boundary),
            };
            if b == 0 || code.len() + c.len() > 64 {
                boundary_ok = false;
            } else {
                boundary |= b << code.len();
            }
            code.push_str(c);
        }
        let boundary = if boundary_ok { boundary } else { 0 };
        let text: String = segs.iter().map(|s| s.text.as_str()).collect();
        let min_len = if min_len == 0 { 2 } else { min_len };
        let n_chars = text.chars().count();
        if n_chars < min_len || code.is_empty() {
            return None;
        }
        // 裁剪后仍超上限则整体放弃，不在词中间切（宁可放过，不可错造）。整句只有一段，
        // 这条才是上限对它真正生效的地方。
        if max_len > 0 && n_chars > max_len {
            debug!("auto-learn: 超长（{} 字 > {}）整体放弃", n_chars, max_len);
            return None;
        }
        let Some(store) = &self.store else {
            return None;
        };
        let active = self.engine_mgr.active_schema_id();
        // 段的产出方案：overlay 用它自己的（临拼目标 / 快捷输入成员），主路用活跃方案。
        let owner = overlay.unwrap_or(&active);
        // 归属方案：非混输维持折叠自身/拼音（主路不看段来源，现行为）；
        // 混输仅当全段同源时用该源归属 id（混源/无法归因跳过，混合码写给谁都无意义）。
        // overlay 的拼音方案同样要求全段同源（快捷输入可跨成员分步，见本函数文档）。
        // 注：混源判定使用截后 segs，截掉的段不参与归属判断。
        let first = segs[0].source; // 上面的 is_empty 守卫已保证非空
        let mixed_source = segs.iter().any(|seg| seg.source != first);
        let schema = if self.engine_mgr.schema_engine_type(owner).as_deref() == Some("mixed") {
            if mixed_source {
                return None; // 混源：跳过自动造词
            }
            // 全段码表（混输超码长回捞会让码表进分段态）：本路径拼音专属，且拼出的「前 N 码 + 尾码」
            // 不是有意义的词条。
            if first == CandidateSource::CodeTable {
                return None;
            }
            self.engine_mgr.write_data_schema_id(owner, first)? // None = 无法归因来源
        } else {
            if overlay.is_some() && mixed_source {
                return None; // overlay 混源（拼音段 + 英文 / 码表段）：跳过自动造词
            }
            self.engine_mgr.data_schema_id(owner) // 拼音族折叠到 "pinyin"，与 record_freq 写读一致
        };
        // 拼音「屏蔽这个词」优先于自动学词：拼出被屏蔽的词照常上屏、不学。落 `pinyin` 桶的都管。
        if self
            .blocked_words_in(&schema)
            .is_some_and(|b| b.contains(&text))
        {
            debug!("auto-learn: {} 已被屏蔽，跳过", text);
            return None;
        }
        // 查重用户词库：已是用户词就不再写临时层。刻意不查临时词库：`learn_temp_word` 对已存在
        // 的记录是 `count++`，过滤掉就冻结了晋升计数。
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
                // 单段整句放行后几乎每次上屏都可能写一条，必须接容量淘汰。
                self.maybe_evict_temp(store, &schema);
                Some(code)
            }
            Err(e) => {
                warn!("learn_temp_word failed: {}", e);
                None
            }
        }
    }

    /// 6b：选中已有临时词时累加计数、推进晋升。主输入路、临拼整体上屏、快捷输入共用。
    ///
    /// - `owner`：产出该候选的方案（主路传活跃方案，overlay 传它实际用的方案）。
    /// - `code`：候选码；`learned_code`：本次上屏造词写入的码，命中同码则跳过，否则同一次上屏
    ///   count +2（比的是命中的那个码）。
    ///
    /// 点查依次试 `meta.store_code`、`code`：模糊召回的候选 `code` 是所打码，记录在规范码下。
    /// 边界跟着码走：命中记录码时只能用同码的 `learn_code` 边界，否则给 0。
    ///
    /// 晋升阈值按落桶选（拼音数据域读 `[schema.pinyin.auto_learn]`，英文读英文那份，其余读码表
    /// `auto_phrase`），不按 `owner` 的引擎类型：混输选中拼音侧临时词时落在拼音桶。
    pub(crate) fn bump_selected_temp_word(
        &self,
        owner: &str,
        cand: &wind_candidate::Candidate,
        code: &str,
        learned_code: Option<&str>,
    ) {
        // 暂停学习：选中临时词也不累加计数、不推晋升。
        if self.learning_suspended() {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        let hit = self
            .engine_mgr
            .write_data_schema_id(owner, cand.source)
            // 英文只认真从临时词库出来的候选：原文格选中时英文自动造词刚写过一条，再点查会记两次。
            .filter(|schema| cand.meta.is_temp_dict || !self.is_english_data_schema(schema))
            .and_then(|schema| {
                [cand.meta.store_code.as_deref(), Some(code)]
                    .into_iter()
                    .flatten()
                    .find(|c| {
                        matches!(
                            store.get_temp_word(&schema, c, cand.freq_text()),
                            Ok(Some(_))
                        )
                    })
                    .map(|c| (schema, c.to_string()))
            });
        let Some((schema, temp_code)) = hit else {
            return;
        };
        if learned_code == Some(temp_code.as_str()) {
            return;
        }
        let temp_boundary = if temp_code == code {
            cand.boundary
        } else {
            cand.meta
                .learn_code
                .as_ref()
                .filter(|(c, _)| *c == temp_code)
                .map_or(0, |(_, b)| *b)
        };
        let promote_count = if schema == wind_engine::manager::PINYIN_DATA_SCHEMA {
            self.engine_mgr.auto_learn_settings().promote_count
        } else if self.is_english_data_schema(&schema) {
            // 英文临时词（自动造词）的晋升阈值在英文那份配置里；落到下面码表那份就是读错桶。
            self.rt().config.schema.english.auto_learn.promote_count
        } else {
            self.engine_mgr
                .codetable_settings()
                .auto_phrase
                .promote_count
        };
        // learn_temp_word 沿用旧 boundary，仅当旧值为 0 时用上面算出的补上。文本取投影前的原形
        // （`freq_text`）：英文候选会按输入大小写投影。
        let text = cand.freq_text();
        if let Ok(count) =
            store.learn_temp_word(&schema, &temp_code, text, LEARN_ADD_WEIGHT, temp_boundary)
        {
            self.maybe_promote_temp(store, &schema, &temp_code, text, count, promote_count);
        }
    }

    /// 临时词晋升判定：promote_count>0 且累积 count 达阈 → 移入用户词库。0=禁用（对齐 Go 语义）。
    ///
    /// 拼音族数据域（`PINYIN_DATA_SCHEMA`）晋升前校验：码须能按记录自己的边界切回合法全拼音节
    /// （`is_full_pinyin_code`），切不回的留在临时层自然淘汰，挡的是按击键混合码造的打不出的词
    /// （`bzdnihao`）。码表方案不校验。只管自动晋升，设置页的手动晋升不经此处。
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
        if schema == wind_engine::manager::PINYIN_DATA_SCHEMA {
            let boundary = store
                .get_temp_words(schema, code)
                .ok()
                .and_then(|recs| recs.into_iter().find(|r| r.text == text))
                .map_or(0, |r| r.boundary);
            if !wind_engine::pinyin::syllable::is_full_pinyin_code(code, boundary) {
                // 日志规范：不带词文本；码是拼音串，debug 级可带。
                debug!("temp word not promoted: code {code} is not full pinyin");
                return;
            }
        }
        match store.promote_temp_word(schema, code, text) {
            Ok(true) => debug!("temp word promoted: code={}", code),
            Ok(false) => {}
            Err(e) => warn!("promote_temp_word failed: {}", e),
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // 快捷加词模式：从最近上屏字符（或剪贴板）选取 N 字组词，自动计算编码后加入用户词库。
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
    /// 守卫与 [`sanitize_dict_add_text`] 同源（空、含换行），但静默降级成空池而不报错：剪贴板
    /// 里常驻与加词无关的东西。超长取前 [`ADD_WORD_MAX_LEN`] 字。
    ///
    /// 必须用阻塞版 `clipboard_get_text`：cached 版在剪贴板被占用时返回上一次的内容，会往词库写
    /// 一条用户没复制过的词。代价是按键线程上最坏 40ms，故面板一侧一律走惰性入口
    /// [`Self::add_word_clip_pool`]；唯一直接调用点 [`Self::add_word_prefill_from_history`] 不进
    /// 加词模式，且只调一次。
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

    /// 剪贴板池的惰性取用：本轮没读过才真去读系统剪贴板，读完记进 `State`。进入加词模式时
    /// 不预读（按键线程上最坏 40ms），只在按 Tab 切过来或最近上屏为空时真读。
    fn add_word_clip_pool<'a>(&self, state: &'a mut State) -> &'a [char] {
        if state.add_word_clip.is_none() {
            state.add_word_clip = Some(self.clipboard_add_word_chars());
        }
        state.add_word_clip.as_deref().unwrap_or(&[])
    }

    /// 当前生效的字符池（按 `add_word_from_clip` 选源）。
    ///
    /// 「池子有多少字」的所有读点都必须走这里，直接读 `add_word_chars` 会让剪贴板来源下的判据错位。
    fn add_word_pool<'a>(&self, state: &'a State) -> &'a [char] {
        if state.add_word_from_clip {
            // 只读路径不触发惰性读取：切到剪贴板的两个入口都已读过。
            state.add_word_clip.as_deref().unwrap_or(&[])
        } else {
            &state.add_word_chars
        }
    }

    /// 某来源刚生效时的默认词长。
    ///
    /// 两个来源刻意不同：最近上屏是没有边界的字流，取最后两字；剪贴板那段本身就是要加的词，默认全选。
    fn default_add_word_len(from_clip: bool, pool_len: usize) -> usize {
        if from_clip {
            pool_len.min(ADD_WORD_MAX_LEN)
        } else {
            ADD_WORD_DEFAULT_LEN.min(pool_len)
        }
    }

    /// 当前加词编码的展示形态（带音节空格），候选窗预览与设置端预填共用。
    /// `add_word_code` 与 `add_word_boundary` 必须成对读出，见 [`display_code`]。
    fn add_word_display_code(&self, state: &State) -> String {
        display_code(&state.add_word_code, state.add_word_boundary)
    }

    /// 取当前选取的词。裁剪方向随来源反向：最近上屏取末尾 N 字，剪贴板取开头 N 字。
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
        // 剪贴板池不在这里预读（见 add_word_clip_pool）。
        state.add_word_clip = None;
        // 默认来源是最近输入（与 Ctrl+Shift+= 刻意相反）；最近输入为空而剪贴板有内容时直接落在
        // 剪贴板一侧。短路求值保证有最近输入时不读剪贴板。
        state.add_word_from_clip = state.add_word_chars.len() < ADD_WORD_MIN_LEN
            && self.add_word_clip_pool(state).len() >= ADD_WORD_MIN_LEN;
        state.add_word_active = true;

        // 候选布局（input.add_word.candidate_layout）由 show_add_word_preview 统一重算（见 layout.rs）。

        self.reset_add_word_len_for_source(state);

        self.show_add_word_preview(state);

        // 组合区由 `input.caret.add_word_via_composition` 选：true 时在这里就把占位发出去（不能指望
        // 出口补），换取 `GetTextExt` 的准确坐标，代价是选中文字会被替换；false 时完全不碰
        // composition、返回 `Consumed`，后续按键转发靠 C++ 的 `_hotkeyModeSession`。见
        // docs/architecture/caret-position-host-compat-notes.md 的「热键直达模式的占位 composition」。
        if self.rt().config.input.caret.add_word_via_composition {
            KeyAction::UpdateComposition {
                text: self.composition_placeholder().to_string(),
                caret_pos: 0,
            }
        } else {
            KeyAction::Consumed
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
    /// 两个来源恒对称：哪一侧没内容都照样切得过去，看到那一侧的空态（不要加「空则不许切」的
    /// 守卫）。切过去后词长按目标来源的默认值重置：两个池子的裁剪方向与上限都不同。
    pub(crate) fn toggle_add_word_source(&self, state: &mut State) -> KeyAction {
        state.add_word_from_clip = !state.add_word_from_clip;
        // 切到剪贴板这一侧才真去读系统剪贴板（见 add_word_clip_pool）。
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
                    crate::user_words_api::clear_deleted_for_added(
                        self,
                        &schema,
                        &[(&code, &word)],
                    );
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
    /// 来源优先级与 Ctrl+= 刻意相反：剪贴板优先，不可用才回退最近输入。这条路不给调长度的
    /// 机会，按它多半是「刚复制了一个词，收进词库」。
    pub(crate) fn open_add_word_from_history(&self, state: &mut State) -> KeyAction {
        // 清理未上屏输入/候选/独占残留，避免残留 composition
        self.reset_exclusive_modes(state);
        self.reset_pinyin_composition(state);
        self.notify_ui_hide();

        let (word, code) = self.add_word_prefill_from_history();
        let schema = self.add_word_target_schema();
        self.open_add_word_dialog_with(&word, &code, &schema)
    }

    /// [`Self::open_add_word_from_history`] 的选词 / 取码部分，单独成函数为可测（原函数要拉起
    /// 设置端进程）。返回 `(word, 展示形态的 code)`，两者皆空即无可预填内容。
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
        // 预填给设置端的是展示形态（带音节空格），见 [`display_code`]。
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

    /// 加词目标方案及其取码路线（见 [`AddWordFamily`]）。类型未知（方案读不到）归码表，
    /// 那条路取不出码时自会失败。
    fn add_word_code_family(&self) -> (String, AddWordFamily) {
        let schema = self.add_word_target_schema();
        let family = match self.engine_mgr.schema_engine_type(&schema).as_deref() {
            Some("english") => AddWordFamily::English,
            Some("pinyin") => AddWordFamily::Pinyin,
            _ => AddWordFamily::CodeTable,
        };
        (schema, family)
    }

    /// 为词计算编码（对齐设置端 `dict.encode`）：英文方案码即单词本身；拼音方案走引擎词级消歧，
    /// 无果回退逐字反查表；码表方案走 [`EngineManager::encode_word`]（按方案 `[[encoder.rules]]`
    /// 从码表词库的单字全码组装，不用拆字表，保证造出的词打得出来）。
    ///
    /// 返回 `(code, boundary)`：只有拼音那条路给得出音节边界，码表取码为 0（消费方降级回 DAG）。
    fn calc_add_word_code(&self, word: &str) -> (String, u64) {
        let (schema, family) = self.add_word_code_family();
        // 英文方案没有 `[[encoder.rules]]`，不能走 `encode_word`。
        if family == AddWordFamily::English {
            return Self::english_add_word_code(word);
        }
        if family == AddWordFamily::Pinyin {
            // 含非汉字则不取码、让加词中止：非汉字没有读音，`gen_pinyin` 会静默跳过它们，产出
            // 覆盖不全的码（「你好a」→ `ni hao`）。守卫只作用于拼音，码表下有码的非汉字（`〇`）
            // 合法参与取码。
            if !word.chars().all(is_han) {
                debug!("addword: 含非汉字，拼音方案不取码（word={}）", word);
                return (String::new(), 0);
            }
            let reverse = self.reverse.read().unwrap_or_else(|e| e.into_inner());
            // 两条路产出的都是带空格的音节码，统一 split 成扁平 code + 边界：带空格的串当 key 会让
            // 前缀查询永远命中不到。逐字反查回退在实践中几乎不可达（只剩引擎加载失败），保留是为了
            // 两条路同形，不为它写端到端测试。
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

    /// 英文方案的加词取码：码就是单词本身的小写，boundary=0（英文词库加载期把 code 列小写化）。
    ///
    /// 取空码即让加词中止的三道守卫：含空白（带空格的 key 前缀查不中）、非 ASCII、一个字母都没有。
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
    /// 与发送分离为可测（headless 构造丢弃了 UI 接收端）。操作提示单独成第三行，否则与标题挤在
    /// 一行会把面板撑得很宽。
    ///
    /// 提示必须放 comment 而非 text：comment 走注释色与更小字号，放 text 会比标题还抢眼。
    /// 见 `panel_hint_is_a_dim_third_row`。
    fn add_word_panel_rows(&self, state: &State) -> Vec<CandidateItem> {
        // no_index=true 完全不显示序号（三行都是提示行，避免默认主题的空圆圈）。
        let row = |text: String, comment: String| CandidateItem {
            text_styled: Default::default(),
            text,
            code: String::new(),
            label: String::new(),
            tooltip: Default::default(),
            comment: comment.into(),
            comment_above: Default::default(),
            no_index: true,
        };
        // 来源后缀与 Tab 提示恒显示：面板在两个来源下必须长得一样（见 toggle_add_word_source）。
        let title = if state.add_word_from_clip {
            "快捷加词 · 剪贴板"
        } else {
            "快捷加词 · 最近输入"
        };
        let empty = self.add_word_pool(state).len() < ADD_WORD_MIN_LEN
            || state.add_word_len < ADD_WORD_MIN_LEN;
        let body = if empty {
            // 空态文案分来源给；剪贴板侧顺带交代准入条件（只剩「单行」，超长会截取）。
            if state.add_word_from_clip {
                row("剪贴板无可用内容".into(), "需要单行文本".into())
            } else {
                row("无最近输入".into(), "请先输入文字后再使用".into())
            }
        } else {
            let word = self.add_word_current_word(state);
            let code_comment = if state.add_word_code.is_empty() {
                "无法计算编码".to_string()
            } else {
                // 显示音节形态，与设置页、词库列表一致。
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
        // 中立通道（移动端）：没有候选窗布局/字体可重算，只发一帧。值与下面桌面的
        // `UpdateCandidates` 一致：无编码区、不高亮任何行、单页。
        if self.session_tx.is_some() {
            self.emit_view_frame(crate::CompositionView {
                preedit: String::new(),
                preedit_caret: 0,
                composing: false,
                mode_label: String::new(),
                has_candidates: true,
                selected: usize::MAX,
                page: 1,
                total_pages: 1,
            });
            return;
        }
        // 加词面板不走 notify_ui_update（直接发 UpdateCandidates），布局与字体要在这里单独同步，
        // 否则加词的 candidate_layout 失效（见 docs/design/mode-candidate-layout.md）。
        self.sync_candidate_layout(state);
        self.sync_candidate_font(state);
        let candidates = self.add_word_panel_rows(state);
        // 加词面板复用候选窗实例，定位方式须与候选窗一致（见 candidate_fixed_pos）。
        let (fixed, fixed_x, fixed_y) = self.candidate_fixed_pos();
        let _ = self.ui_tx.send(UiCommand::UpdateCandidates {
            preedit: String::new(),
            preedit_caret: 0, // 加词面板无编码区
            preedit_host_owned: false,
            composing: false, // 加词面板不是在打编码
            mode_label: String::new(),
            candidates,
            selected: usize::MAX, // 各行均为提示、非可选候选，不高亮任何行
            hover: -1,
            page: 1,
            total_pages: 1,
            grid: None,
            caret_x: state.caret_x,
            caret_y: state.caret_y,
            caret_height: state.caret_height,
            caret_valid: true,
            fixed,
            fixed_x,
            fixed_y,
        });
    }

    /// 加词模式下的按键分派。
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
#[path = "handle_addword_tests.rs"]
mod tests;
