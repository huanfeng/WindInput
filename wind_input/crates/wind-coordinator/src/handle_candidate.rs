//! 候选处理：生成 / 过滤 / shadow / 词频重排 / 翻页导航 / 选词上屏 / 右键操作。
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。

use crate::coordinator::{
    CommittedSeg, Coordinator, DEFERRED_COMPOSITION_FALLBACK_MS, InputOutcome, State,
    now_unix_secs, punct_char,
};
use crate::pipeline::ModeKind;
use crate::preedit_cursor;
use crate::short_code_yield;
use tracing::{debug, warn};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::{Candidate, CandidateMeta, CandidateSource};
use wind_engine::manager::ENGLISH_SCHEMA;
use wind_ipc::protocol::{MOD_SHIFT, MOD_SHORTCUT};
use wind_keys::keymap;
use wind_store::freq::FreqRecord;
use wind_ui_types::CandidateOp;

/// 候选刷新各段超过它才记日志（偶发慢键排查用，平时不进日志）。
const SLOW_STAGE: std::time::Duration = std::time::Duration::from_millis(15);

/// 英文词频召回（[`Coordinator::english_freq_recall`]）每次最多补进几个词。
///
/// 召回的词从池尾起步、按 `base_pos / 2^有效次数` 前移；按有效次数取前 16 个（已验证是
/// 词库词的）已经覆盖「能进前几页」的全部候选者，再多的只是在池尾附近换位置。
const ENGLISH_RECALL_MAX: usize = 16;

/// 候选词条操作（置顶 / 前移 / 后移 / 删除 / 恢复默认）的作用域快照。
/// 由 [`Coordinator::candidate_op_scope`] 解析，菜单构建、写端、macOS 禁用位三处共用。
pub(crate) struct CandidateOpScope {
    /// 词库 / shadow 归属方案 id（原始 id，未经 `data_schema_id` 折叠：Delete 分支
    /// 还要靠它走 `write_data_schema_id` 的按来源分流）。
    pub schema: String,
    /// 候选调整（shadow）专用码：主输入路取归一形态（双拼 → 全拼码，见 `shadow_code_of`），
    /// 特殊模式 = `special_buffer`。只喂 `pin_shadow` / `shadow_has_rule` / `clear_shadow`。
    pub code: String,
    /// 击键原码：恒等于该模式的编码缓冲，不做归一。
    ///
    /// 短语按击键召回，主键就是击键串；双拼下它与 `code` 取值不同，拿归一码去
    /// `set_phrase_enabled` 会写到永远读不到的键上，短语删除静默失效。
    pub raw_code: String,
    /// 出这批候选的引擎类型（调位判据用）；引擎未加载时为 None。
    pub engine_type: Option<wind_engine::EngineType>,
    /// 特殊模式标记：重建候选须走 `update_special_candidates`——主路径的 `update_candidates`
    /// 读 `input_buffer`，在此模式下恒空，会把候选列表整个清掉。
    pub special: bool,
}

/// 候选统一层级排序，已下沉到 [`wind_candidate::candidate_display_order`]（文档在那边），
/// 以便引擎侧评测（`pinyin_eval` 的界面序指标）与界面共用同一个比较器。
pub(crate) use wind_candidate::candidate_display_order;

/// 把英文候选统一定位到「中文常用精确解之后、其余候选之前」。
///
/// 用后置定位而不是往 [`candidate_display_order`] 加比较键：加键会改变非英文候选之间的
/// 既有次序，而只对「英文 vs 中文」表态的比较器不是全序。这里只搬动英文那几条，
/// 其余候选按原序收进 `rest`。
///
/// 插入点是「开头那段连续的常用精确解」（`source_tier <= 1`，另加吃满整串的②b 混合简拼
/// 整句）之后；取连续段而不是 `rposition`，否则词频重排后英文会被推到列表深处。
/// `pinyin_words_lead`（只在拼音方案为 `Some`）：开头段为空、首选是吃满整串的高频拼音
/// 词库词时让过它这一条，判据见 [`pinyin_head_word_outranks_english`]。
/// `positions` 是完整单词 / 前缀补全英文的位次下限（1 起，只会让英文更靠后），
/// `(1, 1)` = 不限。规则全貌见 docs/architecture/engine-candidate-pipeline.md 的
/// 「6.1.2 英文候选的位次由协调器决定」。
pub(crate) fn place_english_after_common_exact(
    candidates: &mut Vec<Candidate>,
    input: &str,
    positions: (usize, usize),
    pinyin_words_lead: Option<&EnglishUsedFn>,
) {
    if input.is_empty()
        || !candidates
            .iter()
            .any(|c| c.source == CandidateSource::English)
    {
        return;
    }
    let mut english: Vec<Candidate> = Vec::new();
    let mut rest: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for c in std::mem::take(candidates) {
        if c.source == CandidateSource::English {
            english.push(c);
        } else {
            rest.push(c);
        }
    }
    // 整串恰好是一个英文词时，引擎新合成的整句（`is_synthesized`，`hello` 的「和理论哦」）
    // 不得排在它前面。判据不用权重：拼凑句的 Viterbi 权重跨三个数量级，没有区分力。
    // 有词库背书的候选（`hen` 的「很」、`data` 的「打他」）不受影响。
    let lower = input.to_lowercase();
    let english_exact = english.iter().any(|c| c.code == lower);
    // ②b 混合简拼整句带 `is_abbrev`，`source_tier` 不认它为精确档，但它仍是吃满整串、全为
    // 常用字的完整解读，在这里要算进开头段（否则 `hell` 的补全 `hello` 会插到「哈额乐乐」前）。
    // 混输的跨来源档位刻意不跟着改。
    let whole_pinyin = |c: &Candidate| {
        c.source == CandidateSource::Pinyin
            && c.is_common
            && wind_candidate::effective_consumed(c, input.len()) >= input.len()
    };
    let leads_as_exact = |c: &Candidate| {
        wind_candidate::source_tier(c, input) <= 1
            || (c.is_sentence && c.is_abbrev && whole_pinyin(c))
    };
    let mut lead = rest
        .iter()
        .take_while(|c| leads_as_exact(c) && !(english_exact && c.is_synthesized))
        .count();
    // 拼音方案：首选是吃满整串的高频拼音词库词（`bus` 的「不是」）时，英文让过它这一条。
    // 只让首选一条而不是整段：简拼下吃满整串的真词成片出现，整段让会把英文推出首页。
    // 英文单词选过就免让；词频查询放最后，只有首选够格时才查。
    if let Some(english_used) = pinyin_words_lead
        && lead == 0
        && rest
            .first()
            .is_some_and(|c| whole_pinyin(c) && pinyin_head_word_outranks_english(c))
        && !english_used(&english)
    {
        lead = 1;
    }
    let floor = |p: usize| p.saturating_sub(1).min(rest.len());
    let pos = lead.max(floor(positions.0));
    // 前缀补全的英文（`meta.from_prefix_complete`）还要让过紧随其后、吃满整串的拼音真词
    // （`xiaom` 的「小米」）。两头都要收窄：只挪带标记的英文（不拆英文方案自己的前缀候选），
    // 只让 `source == Pinyin`（短语 `consumed` 为 0 会被当成吃满）。
    let has_completion = english.iter().any(|c| c.meta.from_prefix_complete);
    let pos_prefix = if has_completion {
        // 位次按最终列表计：补全前面还会插进完整单词那几条，下限要扣掉它们。
        let n_exact = english
            .iter()
            .filter(|c| !c.meta.from_prefix_complete)
            .count();
        // 起点：有完整单词时从它们落位处（`pos`）起数，补全恒在其后；没有时完整单词的下限
        // 与补全无关，只从开头完整匹配段之后（`lead`）起数。
        let start = if n_exact > 0 { pos } else { lead };
        let yielded = start
            + rest[start..]
                .iter()
                .take_while(|c| {
                    c.source == CandidateSource::Pinyin
                        && !c.is_synthesized
                        && wind_candidate::effective_consumed(c, input.len()) >= input.len()
                })
                .count();
        yielded.max(floor(positions.1.saturating_sub(n_exact)))
    } else {
        pos
    };
    if !has_completion {
        // 没有补全：英文整块原序插入（英文块内的次序可能是调频排出来的，不能动）。
        rest.splice(pos..pos, english);
    } else {
        // 完整单词恒在补全之前：补全插在 `pos_prefix`（≥ `pos`），完整单词再插到 `pos`。
        let (completion, others): (Vec<Candidate>, Vec<Candidate>) = english
            .into_iter()
            .partition(|c| c.meta.from_prefix_complete);
        rest.splice(pos_prefix..pos_prefix, completion);
        rest.splice(pos..pos, others);
    }
    *candidates = rest;
}

/// 「这批英文候选里有没有选过的完整单词」的查询（拼音首选让英文的「选过即免」）。
pub(crate) type EnglishUsedFn<'a> = dyn Fn(&[Candidate]) -> bool + 'a;

/// 拼音首选让英文的词频门槛（拼音系统词库权重）。
///
/// 只在拼音词库内比，英文与拼音权重是两套量纲。取值依据见
/// docs/architecture/engine-candidate-pipeline.md 的「6.1.2」。
pub(crate) const PINYIN_HEAD_WORD_MIN_WEIGHT: i32 = 3000;

/// 拼音首选这条够不够格让英文让位：非引擎合成，且是用户词或系统词库权重够高的词。
///
/// - 用户词（非临时词）不看权重：量纲不同，且用户自己加的词本身就是表态。
///   代价是整批导入的大词库里的冷僻词也会压过英文（选过一次英文即免）。
/// - 细胞词库等系统层的原始权重未必与主词库归一，门槛对它们只是近似。
/// - 临时词不算：误打一次就可能进去。
/// - 其余按系统词库权重过 [`PINYIN_HEAD_WORD_MIN_WEIGHT`]。
pub(crate) fn pinyin_head_word_outranks_english(c: &Candidate) -> bool {
    if c.is_synthesized {
        return false;
    }
    if c.meta.is_user_dict {
        return true;
    }
    !c.meta.is_temp_dict && c.weight >= PINYIN_HEAD_WORD_MIN_WEIGHT
}

/// 整句 N-best 块：`show > 1` 时把 `sentence_rank > 0` 的候选按名次连续摆到最前。
///
/// 整句本身不锚定、靠 weight 挣位置；但 N-best 的用途是并排比较几种解读，散开就失去
/// 意义，所以只在用户开了多条时整块前置，出厂 `show == 1` 直接返回。
/// 只搬整句那几条，非整句候选相对次序不变；2b / 2c 整句的 `sentence_rank` 为 0，不在块内。
///
/// 调用位置在词频重排与英文定位之后、shadow 之前（前两者会搅散块，shadow 保留最终话语权）。
/// 已降位的整句（`is_sentence_demoted`，让位于精确整词）不进块，否则等于撤销那条降位。
/// 块里不足两条时不摆：单条整句（如混输把拼音子引擎钉成 1/1）按普通规矩挣位置，
/// 否则会被拉到码表精确候选前面。
pub(crate) fn place_sentence_block(candidates: &mut Vec<Candidate>, show: usize) {
    let in_block = |c: &Candidate| c.sentence_rank > 0 && !c.is_sentence_demoted;
    if show <= 1 || candidates.iter().filter(|c| in_block(c)).count() < 2 {
        return;
    }
    let (mut block, rest): (Vec<Candidate>, Vec<Candidate>) = std::mem::take(candidates)
        .into_iter()
        .partition(|c| in_block(c));
    block.sort_by_key(|c| c.sentence_rank);
    block.extend(rest);
    *candidates = block;
}

/// 整句切换键：把显示窗口在整句池里向后滚一格并回卷，换掉候选里露出的整句。
///
/// 返回换完后窗口第一条的下标（供调用方落高亮）；池子不足两条、或候选里没有露出的整句时
/// 返回 `None`，调用方据此不吃键，让键回到原语义（Tab 出厂是高亮下移）。
///
/// 池 `K` 条、露 `m` 条，窗口起点 `w` 每按一次 +1 对 `K` 取模，露出 `pool[(w+i) % K]`。
/// 换进来的整句按窗口内位置重编名次（1..=m），使 `place_sentence_block` 的块内顺序跟着窗口走。
/// 池里没露过的整句可能与某条普通候选同文，换进来后删掉那条普通候选，免得一屏两条同字。
pub(crate) fn cycle_sentence_window(
    candidates: &mut Vec<Candidate>,
    base: &mut Vec<Candidate>,
    pool: &[Candidate],
    window: &mut usize,
) -> Option<usize> {
    let k = pool.len();
    if k < 2 {
        return None;
    }
    // 基底 = 窗口为 0 时的列表，第一次切换时拍下（此时后处理都已做完）。每次从基底重建而不是
    // 在当前列表上累积改，否则同文去重删掉的候选回不来，转满一圈也回不到原列表。
    // 池子一重填基底就作废（调用方清空）。
    let src: &[Candidate] = if base.is_empty() { candidates } else { base };
    let mut slots: Vec<usize> = src
        .iter()
        .enumerate()
        .filter(|(_, c)| c.sentence_rank > 0)
        .map(|(i, _)| i)
        .collect();
    if slots.is_empty() {
        return None;
    }
    if base.is_empty() {
        *base = candidates.clone();
    }
    slots.sort_by_key(|&i| base[i].sentence_rank);
    slots.truncate(k);
    *window = (*window + 1) % k;
    let mut v = base.clone();
    if *window != 0 {
        for (i, &slot) in slots.iter().enumerate() {
            let mut incoming = pool[(*window + i) % k].clone();
            incoming.sentence_rank = (i + 1) as u8;
            v[slot] = incoming;
        }
        // 同文去重：只删「不在窗口里」的那条。先记下窗口里的文本，再按下标过滤。
        let window_texts: Vec<String> = slots.iter().map(|&i| v[i].text.clone()).collect();
        let mut idx = 0usize;
        v.retain(|c| {
            let keep = slots.contains(&idx) || !window_texts.contains(&c.text);
            idx += 1;
            keep
        });
    }
    *candidates = v;
    // 去重可能删掉了窗口之前的条目，窗口首条的下标要重新找。
    candidates.iter().position(|c| c.sentence_rank == 1)
}

/// 满码空码清空的最终复核：候选列表里是否存在「拦得住清空」的候选。
///
/// 清空要穿过三道门：码表 `clear_on_empty_max`（满码 + 无更长后继）、混输 `should_clear`
/// （拼音守护）、本复核。「空不空」只在这里判，因为引擎看不见协调器随后并入的短语与过滤，
/// 须以用户所见的最终列表裁决。三处语义无编译期同步，改任一道都要回头核另外两道。
///
/// 拼音的部分匹配不算匹配：`nunl` 的「嫩」只解释了 3 码，不能替整串挡下清空；消费整串的
/// 拼音候选照常拦住。`consumed_length == 0` = 未标注（码表恒为 0），视为整串匹配，
/// 与 `apply_freq_rerank` 的 `consumes_all` 同一约定。
fn clear_blocked_by_candidates(candidates: &[Candidate], input_len: usize) -> bool {
    candidates.iter().any(|c| {
        c.source != CandidateSource::Pinyin
            || c.consumed_length == 0
            || c.consumed_length >= input_len
    })
}

/// 自动上屏最短码长的归一（纯函数）：配置 0 = 跟随全码长。
///
/// 复刻 `CodeTableEngine::new` 里的同名归一（那份协调器取不到），两处语义必须一致。
/// `max_code_length` 为 0（拼音等）时结果为 0、不设闸；这类引擎的短语自动上屏在调用前已整体拦下。
fn resolve_auto_commit_min_len(configured: usize, max_code_length: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        max_code_length
    }
}

/// 「固定到第 N 位」给不给：英文引擎（英文方案、临英）不给——头部候选（原文、大小写变形、
/// 被挪进头部的词库词）在 shadow 之后才插入，名次会偏头部条数。菜单与写端同用，引擎类型取自
/// `CandidateOpScope`（按引擎类型判，自建的英文类方案 id 不叫 `english` 也认得）。
pub(crate) fn pin_at_offered(engine_type: Option<wind_engine::EngineType>) -> bool {
    engine_type != Some(wind_engine::EngineType::English)
}

/// 单字母重复上屏项（`refill_candidates` 插在首位）的 `natural_order`，调位口径据它认出这一项
/// （见 `candidate_grid::inserted_after_shadow`）。
pub(crate) const LEADING_REPEAT_ORDER: i32 = -1;

/// 候选列表里唯一的非 emoji 扩展候选；不足一条或多于一条都返回 `None`。
///
/// 抽成纯函数是为了单测：这条唯一性判据最容易被 emoji 扩展悄悄破坏（插入一条 emoji 即恒假）。
/// 只有唯一性类判据需要排除 emoji；显隐、翻页与选中索引照常把 emoji 计入。
fn sole_non_emoji(candidates: &[Candidate]) -> Option<&Candidate> {
    // 低置信候选（换位纠错、误读召回）同样不计：满码唯一的短语不该被它们拦下。
    let mut it = candidates
        .iter()
        .filter(|c| !c.is_emoji_suggestion && !c.is_low_confidence());
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

/// 规划 emoji 插入：返回 `(宿主下标, 该宿主要插入的 emoji)`，不修改候选列表。
///
/// emoji 去重键：剥掉 U+FE0F（变体选择符 16，要求 emoji 呈现）。
///
/// 上游表里过半符号带尾随 FE0F，同一符号两种写法会挤占 `max_per_word` 配额。只用来比、
/// 不改写输出：FE0F 决定彩色还是文本字形，谁先到用谁（见 `docs/design/text-mapping-tables.md` §2.3）。
pub(crate) fn emoji_dedup_key(s: &str) -> String {
    s.replace('\u{FE0F}', "")
}

/// 规划 emoji 插入的纯计算部分（选宿主、查表、去重、截断），与读配置、落位分开以便单测。
///
/// 判据（顺序即优先级）：
/// - 只看列表前 `max_hosts` 条，而不是「找够 max_hosts 个合格宿主」，emoji 位置才可预测；
/// - 跳过已有的 emoji 扩展候选与短于 `min_chars` 的词；
/// - `exact_only` 时跳过前缀补全与子短语（本身就是猜的，再挂 emoji 只放大噪音）；
/// - 去重集合起手装入全部既有候选文本，按 [`emoji_dedup_key`] 比，保留先到者的原形。
fn plan_emoji_insertions(
    candidates: &[Candidate],
    exact_only: bool,
    max_hosts: usize,
    max_per_word: usize,
    min_chars: usize,
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(usize, Vec<String>)> {
    let mut seen: std::collections::HashSet<String> = candidates
        .iter()
        .map(|c| emoji_dedup_key(&c.text))
        .collect();
    let mut picks: Vec<(usize, Vec<String>)> = Vec::new();
    for (i, c) in candidates.iter().enumerate().take(max_hosts) {
        if c.is_emoji_suggestion || c.text.chars().count() < min_chars {
            continue;
        }
        if exact_only && (c.is_prefix || c.is_partial) {
            continue;
        }
        let Some(list) = lookup(&c.text) else {
            continue;
        };
        let mut picked = Vec::new();
        for e in list.split_whitespace() {
            if picked.len() >= max_per_word {
                break;
            }
            if seen.insert(emoji_dedup_key(e)) {
                picked.push(e.to_string());
            }
        }
        if !picked.is_empty() {
            picks.push((i, picked));
        }
    }
    picks
}

/// `show_as = "tail"` 的插入点：最后一条常用候选之后，且不越过任何放宽补充的候选。
///
/// 不能直接放物理末尾：智能档下孤儿码位的生僻字留在列表尾部，emoji 会被埋到好几页后。
/// 判据与过滤器共用 [`wind_candidate::is_common_like`]，两边对「什么算常用」不可分叉。
/// 边界：全是生僻字时落到放宽补充段之前、不抢首位；放宽补充的候选（`is_scope_filtered`）
/// 恒在 emoji 之后；全部字符档下可能落在列表中段，这是同一判据的自然结果。
fn emoji_tail_insert_at(candidates: &[Candidate]) -> usize {
    let before_relaxed = candidates
        .iter()
        .position(|c| c.is_scope_filtered)
        .unwrap_or(candidates.len());
    candidates[..before_relaxed]
        .iter()
        .rposition(wind_candidate::is_common_like)
        .map_or(before_relaxed, |i| i + 1)
}

/// 造一条 emoji 扩展候选。
///
/// `code` 恒空、`source` 恒 `None`：引擎 `decide_auto_commit` 的唯一性判断、词频读写的
/// 非空码要求、`AutoCommit` 只认 `CodeTable` 首选，三处都依赖这一点，给它编码会同时破坏。
fn make_emoji_candidate(text: String) -> Candidate {
    Candidate {
        text,
        is_emoji_suggestion: true,
        ..Default::default()
    }
}

/// 移动端拼音单字母的首批候选上限，见 [`Coordinator::main_path_candidate_limit`]。
/// 取值使宿主侧耗时在一帧以内（100 条明显变慢）。
pub(crate) const MOBILE_PINYIN_SINGLE_LIMIT: usize = 60;

impl Coordinator {
    /// 后台把当前拼音类方案的各个声母各查一遍（移动端在核心就绪、切方案后调）。
    ///
    /// 单声母在进程里第一次查询明显更慢（词库映射页首次缺页），这里提前替用户付掉。
    /// 直接查引擎、不走按键路径：喂假按键会改输入状态，与用户打字交错时会插进用户的编码。
    /// 引擎本就跨线程共享；上限与移动端单字母首批一致。
    pub fn prewarm_single_initials(self: &std::sync::Arc<Self>) {
        if self.engine_mgr.current_engine_type() != Some(wind_engine::engine::EngineType::Pinyin) {
            return;
        }
        let this = std::sync::Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("wind-prewarm-initials".into())
            .spawn(move || {
                let id = this.engine_mgr.active_schema_id();
                let t0 = std::time::Instant::now();
                for b in b"bpmfdtnlgkhjqxzcsrywaeo" {
                    let key = (*b as char).to_string();
                    let _ = this
                        .engine_mgr
                        .convert_with(&id, &key, MOBILE_PINYIN_SINGLE_LIMIT);
                }
                tracing::info!("单字母预热 {id} 用时 {:?}", t0.elapsed());
            });
    }
}

impl Coordinator {
    /// 记一条上屏历史（最近置前，限 16 条）。供命令栏 `last(n)`、加词推荐、
    /// z 键重复上屏与快捷输入的 `quick_input.repeat` 共用同一事实源。
    ///
    /// 与 [`Self::record_selection`] 分开：快捷输入数字透镜的上屏（计算结果、日期）没有编码、
    /// 不记词频，却同样是一次上屏，历史若只挂在选词上就取不到刚算的结果。
    pub(crate) fn push_commit_history(&self, text: &str) {
        // 暂停学习也不记：经重复上屏、加词推荐会把无痕框里打的字带到别处，隐私上与落库无异。
        if text.is_empty() || self.learning_suspended() {
            return;
        }
        let mut h = self
            .recent_commits
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        h.push_front(text.to_string());
        if h.len() > 16 {
            h.truncate(16);
        }
    }

    /// 记一次选词到 FREQ（count+1、last_used=now，按 schema+code+text），上屏历史恒记。
    /// 未开自动调频时不记词频；例外是用户开了「只记次数」（`record_always`），那时只写不读。
    ///
    /// 短语（`CandidateSource::Phrase`）恒不记词频：其次序由短语定义决定，且上屏文本可能是
    /// 模板求值结果（`date`），每次都是新键，只会堆积垃圾行。读端 `apply_freq_rerank`
    /// 对应地跳过短语候选，两端必须同时成立。
    pub(crate) fn record_selection(&self, code: &str, text: &str, source: CandidateSource) {
        self.record_selection_in(None, code, text, source);
    }

    /// 同 [`Self::record_selection`]，但从候选本身判来源：emoji 扩展候选恒不记词频。
    ///
    /// emoji 在 `apply_freq_rerank` 之后才插入，词频读端看不到它，记了只堆垃圾行，所以不做开关。
    /// 不能在调用点整条跳过记账：这里还有 `push_commit_history`（重复上屏的数据源）。
    ///
    /// 凡是从 `state.candidates` 取候选上屏的路径都该走本函数而非 `record_selection`
    /// （`commit_selected`、`take_committed_with_highlight`、`commit_highlight_then_char`、
    /// 标点顶屏两处）；`every_record_selection_call_goes_through_freq_code` 同时扫描两个名字。
    ///
    /// 英文方案的头部候选（原文 / 大小写变形）按对应词库词记（同 `english_head_dict_word`），
    /// 否则 `(translation, Translation)` 是读端永远查不中的键；找不到词库词的只记上屏历史。
    /// 调用方传入的 `code` 对头部候选就是小写输入缓冲。输入统计由各出口自己记，不经本函数。
    pub(crate) fn record_selection_cand(&self, code: &str, cand: &Candidate) {
        if self.engine_mgr.active_is_english()
            && crate::english_candidates::is_english_head_candidate(cand, code)
        {
            let active = self.engine_mgr.active_schema_id();
            if let Some(word) = self.english_head_dict_word(&active, code, cand) {
                let freq_code = self.freq_code(code, &word);
                return self.record_selection_cand_in(None, &freq_code, &word);
            }
            self.push_commit_history(&cand.text);
            // 头部候选找不到词库词：英文自动造词的信号。`english_head_dict_word` 在不记词频时
            // 恒 None，所以「是不是词库词」由 `learn_english_raw` 自己再问一遍引擎。
            self.learn_english_raw(&active, &cand.text);
            return;
        }
        // 码表自动造词：登记这个字是用哪条码打出来的。主输入路的选词 / 顶屏出口都经过这里。
        self.note_draft_code_hint(cand, code);
        self.record_selection_cand_in(None, code, cand);
        // 混入别的方案的英文候选，词频另记一份进英文桶（与英文方案、临英同一记频口），
        // 在哪儿选过三处都受益。上面按主方案的记账照旧，主方案读端靠它排序。
        if cand.source == CandidateSource::English && !self.engine_mgr.active_is_english() {
            self.record_freq_only(Some(ENGLISH_SCHEMA), code, cand.freq_text(), cand.source);
        }
    }

    /// 列表里与输入同码的英文完整单词有没有被选用过（拼音首选让英文的「选过即免」）。
    /// 前缀补全来的英文（选过 business）不算：用户表态的是那个长词，不是这串输入。
    ///
    /// 两个桶都看：选中混入的英文时，主方案的桶（拼音调频，出厂开）与英文桶（英文方案调频，
    /// 出厂关）各记一份（见 [`Self::record_selection_cand`]）。只看英文桶的话，出厂配置下一条
    /// 记录都没有，这条豁免等于不存在。
    fn english_used_before(&self, english: &[Candidate], code: &str) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        let lower = code.to_lowercase();
        let mut english = english
            .iter()
            .filter(|c| {
                c.source == CandidateSource::English
                    && !c.meta.from_prefix_complete
                    && c.code == lower
            })
            .peekable();
        if english.peek().is_none() {
            return false;
        }
        let active = self.engine_mgr.active_schema_id();
        let buckets = [
            self.engine_mgr
                .write_data_schema_id(&active, CandidateSource::English),
            Some(self.engine_mgr.data_schema_id(ENGLISH_SCHEMA)),
        ];
        english.any(|c| {
            buckets
                .iter()
                .flatten()
                // 与调频读端同口径：次数为 0 的记录不算用过。
                .any(|b| matches!(store.get_freq(b, &c.code, c.freq_text()), Ok(Some(r)) if r.count > 0))
        })
    }

    /// 同 [`Self::record_selection_cand`]，但可指定归属方案（overlay 用）。
    ///
    /// 临拼与 mix 的候选列表里同样会插入 emoji，它们的上屏出口也要过这道守卫：否则 emoji
    /// 会写出 `("pinyin", "ni", "🙂")` 这种读端永远命中不了的行。
    pub(crate) fn record_selection_cand_in(
        &self,
        schema_override: Option<&str>,
        code: &str,
        cand: &Candidate,
    ) {
        if cand.is_emoji_suggestion {
            // 只跳词频，历史照记。
            self.push_commit_history(&cand.text);
            return;
        }
        // 词频记投影前的词库原文、历史记实际上屏的形态，理由见 `record_selection_cased_in`。
        self.record_selection_cased_in(
            schema_override,
            code,
            cand.freq_text(),
            &cand.text,
            cand.source,
        );
    }

    /// 同 [`Self::record_selection`]，但可指定生效方案（特殊模式用，见
    /// [`Self::effective_data_schema`]）。`None` = 按 active 归属。
    ///
    /// 开关也跟着生效方案走（`freq_settings_for`），否则会出现「主方案开着 → 往特殊方案记账」。
    pub(crate) fn record_selection_in(
        &self,
        schema_override: Option<&str>,
        code: &str,
        text: &str,
        source: CandidateSource,
    ) {
        self.record_selection_cased_in(schema_override, code, text, text, source)
    }

    /// 同 [`Self::record_selection_in`]，但词频文本与历史文本分开给。
    ///
    /// 两者只在英文候选被大小写投影改写过时才不同（见 `english_candidates::project_case`）：
    /// 词频要词库原文（读端排在投影之前，存 `Hill` 读 `hill` 会两端永不相交）；
    /// 历史要实际上屏的形态（重复上屏要所见即所得）。
    pub(crate) fn record_selection_cased_in(
        &self,
        schema_override: Option<&str>,
        code: &str,
        freq_text: &str,
        history_text: &str,
        source: CandidateSource,
    ) {
        // 历史先记、与词频文本空不空无关：两条通路独立。空显示标签的 `$CC` 纯文本命令就是
        // `freq_text` 空而上屏文本非空的例子。
        self.push_commit_history(history_text);
        self.record_freq_only(schema_override, code, freq_text, source);
    }

    /// [`Self::record_selection_cased_in`] 的词频半边（不记上屏历史）：一次上屏要记进两个
    /// 桶时（混入的英文候选，见 [`Self::record_selection_cand`]），历史只该记一次。
    fn record_freq_only(
        &self,
        schema_override: Option<&str>,
        code: &str,
        text: &str,
        source: CandidateSource,
    ) {
        // 暂停学习：一切选词记账（主路、临拼、快捷输入、特殊模式、英文头部候选）都汇到这里。
        if text.is_empty() || self.learning_suspended() {
            return;
        }
        if source == CandidateSource::Phrase {
            return;
        }
        if let Some(store) = &self.store {
            let owner = schema_override
                .map(str::to_string)
                .unwrap_or_else(|| self.engine_mgr.active_schema_id());
            // 未开自动调频则不记，除非开了「只记次数」`record_always`。读端仍只认 `enabled`，
            // 见 `FreqSettings::records`。
            let settings = self.engine_mgr.freq_settings_for(&owner);
            if !settings.records() {
                return;
            }
            // 排除区块（`schema.frequency.exclude_blocks`）：这类候选不学词频。必须在
            // `push_commit_history` 之后（重复上屏的数据源），统计 `record_commit` 也不受影响。
            // 读端 `apply_freq_rerank_in` 调同一个 `excluded_from_freq`，两端必须同时跳过。
            if settings.excluded_from_freq(text) {
                return;
            }
            // 归属 id：非混输折叠自身/拼音；混输按候选来源分流，无法归因则跳过本次记频。
            let Some(schema) = self.engine_mgr.write_data_schema_id(&owner, source) else {
                return;
            };
            if let Err(e) = store.record_freq(&schema, code, text) {
                warn!("record_freq failed: {}", e);
            }
        }
    }

    /// 按候选文本查 emoji 表，把命中的 emoji 作为候选插入（`[input.emoji]`）。
    ///
    /// 钉在候选管线的最末端：前面每一步都会误伤它（去重、`apply_filter` 整批滤掉、
    /// 词频永久顶前、shadow 打乱顺序），见 `docs/design/emoji-suggestion.md` §3.2。
    ///
    /// 本函数只增不改：不动既有候选的顺序与内容，`after` 档恒插在宿主之后，首选永远不被顶替。
    /// 非首选候选配 emoji 靠放大 `max_hosts`；刻意不做「跟随高亮」：列表长度随导航变化会与
    /// `selected_index` 的语义冲突。
    pub(crate) fn apply_emoji_suggestions(&self, candidates: &mut Vec<Candidate>) {
        let (scope, show_as, max_per_word, max_hosts, min_chars) = {
            let rt = self.rt();
            let e = &rt.config.input.emoji;
            if !e.enabled {
                return;
            }
            (
                e.scope.clone(),
                e.show_as.clone(),
                e.max_per_word,
                e.max_hosts,
                e.min_word_chars,
            )
        };
        // `comment` 档不进候选——它由注释模板的 `${emoji}` 变量呈现，是另一条通路。
        if scope == "off" || show_as == "comment" || max_per_word == 0 {
            return;
        }
        let guard = self.emoji_dict.read().unwrap_or_else(|e| e.into_inner());
        let Some(dict) = guard.as_ref() else {
            return;
        };
        // 未知值按最保守的一档处理（值域告警在 `sync_emoji_dict` 里发，热路径不重复告警）。
        let picks = plan_emoji_insertions(
            candidates,
            scope != "all",
            max_hosts,
            max_per_word,
            min_chars,
            |t| dict.lookup(t),
        );
        if picks.is_empty() {
            return;
        }

        if show_as == "tail" {
            // 「列表末尾」＝常用候选之后（不是物理末尾），落点判据见 `emoji_tail_insert_at`。
            let at = emoji_tail_insert_at(candidates);
            let mut k = 0;
            for (_, es) in picks {
                for e in es {
                    candidates.insert(at + k, make_emoji_candidate(e));
                    k += 1;
                }
            }
        } else {
            // `after`：紧随宿主。从后往前插入，否则前面的插入会把后面记下的下标顶偏。
            for (i, es) in picks.into_iter().rev() {
                for (k, e) in es.into_iter().enumerate() {
                    candidates.insert(i + 1 + k, make_emoji_candidate(e));
                }
            }
        }
    }

    /// 词频重排（独立维度，不改 weight）：按 redb 词频记录做档位感知的稳定重排，
    /// 用过的候选按策略上浮、未用候选保持权重序。
    ///
    /// 码表 / 混输按 `freq_strategy` 走 used-first（`step` 累积次数、`top` 最近使用）或位置提升，
    /// 拼音走位置提升（docs/design/freq-rerank-model.md）。主开关关闭则完全不重排。
    ///
    /// `schema_override` 指定生效方案（特殊模式用；`None` = 活跃方案）。读端方案必须与写端
    /// [`Self::record_selection_in`] 取自同一处（[`Self::effective_data_schema`]），否则记账成功
    /// 而候选顺序永远不动。生产路径经 `postprocess::run_passes` 调用。
    pub(crate) fn apply_freq_rerank_in(
        &self,
        schema_override: Option<&str>,
        candidates: &mut [Candidate],
        code: &str,
    ) {
        self.apply_freq_rerank_with_extra(
            schema_override,
            candidates,
            code,
            std::collections::HashMap::new(),
        );
    }

    /// 同 [`Self::apply_freq_rerank_in`]，另并入调用方给的外部热度记录（按候选文本）。
    ///
    /// 给不在词频表里、却有自己使用次数的候选用（见 `quick_history::quick_history_english_pool`）。
    /// 与词频表记录同键时以外部为准。
    pub(crate) fn apply_freq_rerank_with_extra(
        &self,
        schema_override: Option<&str>,
        candidates: &mut [Candidate],
        code: &str,
        extra: std::collections::HashMap<String, FreqRecord>,
    ) {
        let Some(store) = &self.store else {
            return;
        };
        if code.is_empty() || candidates.len() < 2 {
            return;
        }
        let active = schema_override
            .map(str::to_string)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        let settings = self.engine_mgr.freq_settings_for(&active);
        if !settings.enabled {
            return;
        }
        // 非混输单次折叠归属；混输预解析两个子方案归属 id，循环内按候选来源选用。
        let is_mixed = self.engine_mgr.schema_engine_type(&active).as_deref() == Some("mixed");
        let schema = self.engine_mgr.data_schema_id(&active); // 非混输：拼音族折叠到 "pinyin"
        let (ct_id, py_id) = if is_mixed {
            (
                self.engine_mgr
                    .write_data_schema_id(&active, CandidateSource::CodeTable),
                self.engine_mgr
                    .write_data_schema_id(&active, CandidateSource::Pinyin),
            )
        } else {
            (None, None)
        };
        let input_len = code.len();
        // 只取消费整串的候选（`consumed_length == 0` 视为整串）：分段子候选（`nihao` 里的「你」）
        // 和混输超码长回捞的前缀候选只解释了前 N 码，不能借整串码的历史上浮。
        //
        // 查询码按来源分流，见 [`Self::freq_code`]：拼音 / 英文用候选存储码（全拼扁平域，双拼缓冲、
        // 分隔符、前缀补全下都与击键不同）；码表用输入码（候选 code 是词条全码，拿它当键会让
        // `d`/`de`/`def` 码位互相串扰）。写端各调用点同样经 `freq_code`，两侧必须同口径。
        // 收齐键后一次事务批量查（见 `get_freq_batch`），避免每候选一次读事务。
        let mut probe: Vec<(String, String, String)> = Vec::new();
        // 词频按 `freq_text()` 查（与写端同键），重排纯函数按候选 `text` 认记录，故另收一份 `text`。
        // 两者只在模板展开 / 大小写投影过的候选上不同。
        let mut rec_keys: Vec<&str> = Vec::new();
        for c in candidates.iter() {
            // 短语不参与词频（写端对称跳过）：其次序由短语定义决定，求值型短语的文本逐日变化。
            if c.is_phrase {
                continue;
            }
            // 排除区块（与写端同一判据）：跳过即不进 probe，热路径上是净省。
            if settings.excluded_from_freq(c.freq_text()) {
                continue;
            }
            let consumes_all = c.consumed_length == 0 || c.consumed_length >= input_len;
            if !consumes_all {
                continue;
            }
            // 混输按候选来源读子方案键空间（无法归因跳过）；非混输用统一 schema。
            let sid: &str = if is_mixed {
                match c.source {
                    CandidateSource::CodeTable => match ct_id.as_deref() {
                        Some(v) => v,
                        None => continue,
                    },
                    CandidateSource::Pinyin => match py_id.as_deref() {
                        Some(v) => v,
                        None => continue,
                    },
                    _ => continue,
                }
            } else {
                &schema
            };
            probe.push((
                sid.to_string(),
                Self::freq_code_with(code, c, settings.english_code_by_input),
                c.freq_text().to_string(),
            ));
            rec_keys.push(&c.text);
        }
        let found = store.get_freq_batch(&probe).unwrap_or_default();
        // 只为命中的候选 clone 文本：多数候选没有词频记录。
        let mut recs: std::collections::HashMap<String, FreqRecord> = rec_keys
            .into_iter()
            .zip(found)
            .filter_map(|(t, r)| match r {
                Some(r) if r.count > 0 => Some((t.to_string(), r)),
                _ => None,
            })
            .collect();
        recs.extend(extra);
        if recs.is_empty() {
            return;
        }
        // 词频重排的模型在引擎侧纯函数里，这里只取记录、按方案分流。
        //
        // 判据必须问归属方案（`is_pinyin_of(&active)`），不是活跃引擎：`freq_settings_for` 的拼音
        // 分支返回的 `strategy` / `protect` 只是占位值。临拼 / mix 的拼音段若在五笔主方案下走进
        // 码表分支，会拿到不衰减的布尔 used-first，`promote_prefix` 的防线也随之失效。
        if self.engine_mgr.is_pinyin_of(&active) {
            let profile = self.engine_mgr.pinyin_freq_profile();
            // 位置提升模型：候选按位次前移、不比权重，故无需引擎的量纲基准。
            // 依赖入参已按 `candidate_display_order` 排好（base_pos 取的是入参下标）；
            // 本调用位于 filter 之后、shadow 之前，是最后一道整体排序。
            wind_engine::freq_rerank::rerank_pinyin_positional(
                candidates,
                &recs,
                now_unix_secs(),
                profile,
                settings.promote_prefix,
                input_len,
            );
        } else {
            // `strategy = position` 时走与拼音同一套位置提升（档位仍是硬约束，只在档内提升）；
            // `top`/`step` 是布尔 used-first，不读 profile / promote_prefix。
            // profile 取生效方案那份（与 `freq_settings_for` 同一个 id），不用拼音那份：
            // 否则特殊方案会用自己的策略配上别的方案的衰减速度。
            wind_engine::freq_rerank::rerank_codetable_usedfirst(
                candidates,
                &recs,
                code,
                settings.strategy,
                settings.protect,
                now_unix_secs(),
                self.engine_mgr.freq_profile_for(&active),
                settings.promote_prefix,
            );
        }
    }

    /// 英文词频召回：词频表里以 `code` 开头、有记录、仍是词库词、却不在 `pool` 里的英文词，
    /// 造成候选返回，由调用方并进池子后再跑 [`Self::apply_freq_rerank_in`]。
    ///
    /// 词频重排只作用于已取回的候选，而单字母前缀下引擎只取 300 条，原序在其外的词用多少次
    /// 都排不上来。口径：
    /// - 只在调频开着、归属方案是英文引擎、`code_scope = candidate`（出厂）时做；`input` 口径下
    ///   本码位的记录都是从本码位列表里选出来的，本就在池里。前缀区间扫描（`for_each_freq`）取齐。
    /// - 只收仍是词库词的记录（按记录码向引擎精确查、文本须一致），词频不是词条来源。
    /// - 召回的词放在池尾（`natural_order` 接在池内最大值之后），与池内词同一把尺前移。
    /// - 按衰减后的有效次数从高到低验证，攒够 [`ENGLISH_RECALL_MAX`] 个即停。
    pub(crate) fn english_freq_recall(
        &self,
        schema_override: Option<&str>,
        pool: &[Candidate],
        code: &str,
    ) -> Vec<Candidate> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        if code.is_empty() {
            return Vec::new();
        }
        let owner = schema_override
            .map(str::to_string)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        if self.engine_mgr.loaded_engine_type(&owner) != Some(wind_engine::EngineType::English) {
            return Vec::new();
        }
        let settings = self.engine_mgr.freq_settings_for(&owner);
        if !settings.enabled || settings.english_code_by_input {
            return Vec::new();
        }
        let data_schema = self.engine_mgr.data_schema_id(&owner);
        // 有效次数与重排同一套算法（`count × 半衰期衰减`），不足 `MIN_PROMOTION_POWER` 的在重排里
        // 提升强度为 0，召回回来也只钉在池尾，不如不召。
        let now = now_unix_secs();
        let profile = self.engine_mgr.freq_profile_for(&owner);
        let mut rows: Vec<(String, String, f64)> = Vec::new();
        let scanned = store.for_each_freq(&data_schema, code, &mut |rc, text, rec| {
            let eff = rec.count as f64 * profile.decay_factor(&rec, now);
            if eff >= wind_engine::freq_rerank::MIN_PROMOTION_POWER
                && !settings.excluded_from_freq(text)
            {
                rows.push((rc.to_string(), text.to_string(), eff));
            }
            true
        });
        if let Err(e) = scanned {
            warn!("english freq recall scan failed: {}", e);
            return Vec::new();
        }
        if rows.is_empty() {
            return Vec::new();
        }
        let in_pool: std::collections::HashSet<&str> =
            pool.iter().map(|c| c.text.as_str()).collect();
        rows.retain(|(_, text, _)| !in_pool.contains(text.as_str()));
        rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let mut next_order = pool
            .iter()
            .map(|c| c.natural_order)
            .max()
            .map_or(0, |m| m + 1);
        let mut out: Vec<Candidate> = Vec::new();
        // 先验证、后截断：先截前 N 条再验证的话，非词库残留会占满名额，把真在用的词挤出去。
        for (rc, text, _) in rows {
            if out.len() >= ENGLISH_RECALL_MAX {
                break;
            }
            // 同一个词可能在词频表里挂在两个码下（`translation` / `t9n` 两个词库条目），
            // 只召回一次。
            if out.iter().any(|c| c.text == text) {
                continue;
            }
            let Some(mut c) = self
                .engine_mgr
                .convert_with(
                    &owner,
                    &rc,
                    crate::english_candidates::ENGLISH_EXACT_LOOKUP_LIMIT,
                )
                .candidates
                .into_iter()
                .find(|c| c.source == CandidateSource::English && c.code == rc && c.text == text)
            else {
                continue;
            };
            // 引擎按记录码查，标的是「对记录码精确」；对当前输入它是前缀补全（同
            // `CodeTableEngine::convert` 的 `is_exact_code = code == input`）。
            c.is_exact_code = c.code == code;
            c.natural_order = next_order;
            next_order += 1;
            out.push(c);
        }
        if !out.is_empty() {
            debug!(recalled = out.len(), "英文词频召回");
        }
        out
    }

    /// 满码自动上屏复核：引擎意向 `t` 是否指向这条候选，可以放行。
    ///
    /// 意向有两种来路：引擎首轮按词条原文给（`$` 模板要比 `template_source`），显示态复评
    /// （`recheck_auto_commit`）按展开后的 `text` 给。`$CC` 命令另由调用方按 `phrase_template` 认。
    /// 含 `{..}` 插值的一律不放行：剪贴板 / 反查取自外部状态，自动上屏等于把用户没看过的内容
    /// 送出去，须手动选。纯 `$` 模板可预期，放行。
    pub(crate) fn auto_commit_target_matches(c: &Candidate, t: &str) -> bool {
        if c.template_source
            .as_deref()
            .is_some_and(|s| s.contains('{'))
        {
            return false;
        }
        c.text == t || c.template_source.as_deref() == Some(t)
    }

    /// 短语候选的稳定 id（`Candidate::id`）：`phrase:{code}:{原始记录文本}`，对齐 Go
    /// `dict.phraseCandID`。短语显示文本可能是模板求值结果，以文本为键的 shadow 规则次日必失配。
    ///
    /// `code` 用短语自身的完整码（与 shadow 规则的存储键同源），`raw` 为未展开的 `PhraseEntry.text`。
    /// `raw` 为空时返回空 id，表示无稳定身份，shadow 落回文本匹配。
    pub(crate) fn phrase_cand_id(code: &str, raw: &str) -> String {
        if raw.is_empty() {
            return String::new();
        }
        format!("phrase:{code}:{raw}")
    }

    /// 候选总数（测试/诊断用）
    pub fn debug_candidate_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .candidates
            .len()
    }

    /// 全部候选文本列表（不分页；测试/诊断用）
    pub fn debug_all_candidate_texts(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .candidates
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }

    /// 候选词条操作（测试/诊断用）
    pub fn debug_candidate_op(&self, op: CandidateOp, page_local: usize) {
        // 走与右键菜单同一条分发（格式候选 → 格式调整，其余 → 词库 shadow）。
        // 直接调 `candidate_op` 会绕过格式分流，测试就测不到用户实际走的那条路。
        self.candidate_or_quick_format_op(op, page_local);
    }

    /// 当前状态下词条操作的作用域 `(归属方案, 编码)`；`None` = 右键菜单只给复制（测试/诊断用）。
    /// 菜单可用性与写端准入共用同一判据，故断言此函数等于同时锁住两条通路。
    pub fn debug_candidate_op_scope(&self) -> Option<(String, String)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.candidate_op_scope(&state).map(|s| (s.schema, s.code))
    }

    /// 首次加载候选上限（对齐 Go：短前缀小批量分级加载，长前缀近全量）。
    pub(crate) fn initial_candidate_limit(&self, input: &str) -> usize {
        Self::main_path_candidate_limit(
            self.engine_mgr.current_engine_type(),
            input,
            self.profile.pinyin_single_letter_limit,
        )
    }

    /// 主路径首批上限：[`Self::initial_candidate_limit_of`]，移动端再对拼音单字母收紧到
    /// [`MOBILE_PINYIN_SINGLE_LIMIT`]。
    ///
    /// 单个声母是拼音里展开最宽的输入，引擎耗时随 k 超线性增长，手机上每个词的首字母都要在
    /// 主线程同步付这笔钱。代价是词频重排只能排池内候选，由移动端「滑到尾部扩容」补上
    /// （[`crate::candidate_pull`]）。桌面不收紧：首页只露 9 个，池外常用词会直接从首页消失。
    /// 只管主路径：临拼 / 混输成员 / 临英各自调 `initial_candidate_limit_of`。
    pub(crate) fn main_path_candidate_limit(
        engine_type: Option<wind_engine::engine::EngineType>,
        input: &str,
        single_letter_limit: Option<usize>,
    ) -> usize {
        let base = Self::initial_candidate_limit_of(engine_type, input);
        match single_letter_limit {
            Some(limit)
                if engine_type == Some(wind_engine::engine::EngineType::Pinyin)
                    && input.chars().count() == 1 =>
            {
                base.min(limit)
            }
            _ => base,
        }
    }

    /// 同 [`Self::initial_candidate_limit`]，但按指定引擎类型分级。
    ///
    /// overlay 类模式的候选来自它自己引用的方案，而 `current_engine_type()` 报的是主方案；
    /// 拿它分级会给英文词库套上码表的小上限，前缀下排名靠后的词（含用过的词）取不进池子，
    /// 词频重排也就无从谈起。
    pub(crate) fn initial_candidate_limit_of(
        engine_type: Option<wind_engine::engine::EngineType>,
        input: &str,
    ) -> usize {
        let len = input.chars().count();
        match engine_type {
            Some(wind_engine::engine::EngineType::CodeTable) => match len {
                0 | 1 => 100,
                2 => 300,
                _ => 1000,
            },
            // 拼音 / 混输 / 英文
            _ => 300,
        }
    }

    /// 词库候选 value 内嵌特殊语法（`$CC` 命令 / `$Y` 模板 / `$AA`·`$SS` 组 / `{..}` 插值）的
    /// 统一展开汇聚点。所有候选生成路径在写入 `state.candidates` 前均须过此点（对齐 Go
    /// `dict.ValueExpander`；见 docs/redesign/unified-candidate-value-expansion.md）。
    ///
    /// - `$CC` → 标 `is_command`（选中由 `select_candidate`、顶屏由 `top_commit_command_guard` 执行）；
    /// - `$AA`/`$SS` 组 → 精确码时逐成员炸开；前缀（候选码更长）时折叠为单个组名候选
    ///   （`is_group`，`group_code` = 完整码），选中经 `complete_to_group_code` 补全后再展开；
    /// - 模板 / 花括号插值 → 直接以展开文本上屏；
    /// - 普通候选经廉价预检零开销原样返回。
    ///
    /// `input` 为该路径当前编码缓冲（供 cmdbar 语法内 `input()` 求值）。已是 `is_phrase`/`is_command`
    /// 的候选跳过二次展开。
    pub(crate) fn finalize_candidates(&self, raw: Vec<Candidate>, input: &str) -> Vec<Candidate> {
        // 快路径：无任一候选含特殊语法（普通词库/拼音结果）→ 零拷贝原样返回。
        if !raw.iter().any(|c| {
            !c.is_phrase && !c.is_command && (c.text.contains('$') || c.text.contains('{'))
        }) {
            return raw;
        }
        let now = chrono::Local::now();
        let recent = self.recent_commits_snapshot();
        // 走 _cached：本闭包在每次按键的候选构建期求值，不能卡按键线程（阻塞版打不开剪贴板时会
        // sleep 重试）。真正执行动作时另用非缓存版取值。
        let clip = |_n: i64| -> String { self.host_services().clipboard_get_text_cached() };
        let reverse = |text: &str, fmt: &str, min_len: usize| -> String {
            self.reverse_render_min(text, fmt, min_len)
        };
        let host = wind_phrase::PhraseHost {
            clip: &clip,
            reverse: &reverse,
        };
        let mut expanded: Vec<Candidate> = Vec::with_capacity(raw.len());
        for cand in raw.into_iter() {
            if cand.is_phrase || cand.is_command {
                expanded.push(cand);
                continue;
            }
            match wind_phrase::expand_dict_value(&cand.text, input, now, &recent, &host) {
                wind_phrase::DictExpansion::None => expanded.push(cand),
                // 是特殊语法但求值为空（剪贴板空 / 反查查不到）→ 整条不出。不能退回原候选，
                // 那会把 `{dict.rev(clip())}` 这串源码当文本上屏。
                wind_phrase::DictExpansion::Drop => {}
                wind_phrase::DictExpansion::Single {
                    display,
                    command_src,
                } => {
                    let mut c = cand;
                    let source_text = std::mem::replace(&mut c.text, display);
                    if let Some(src) = command_src {
                        c.phrase_template = src;
                        c.is_command = true;
                    } else {
                        // 模板 / 插值：展开文本逐日变化，以文本为键的记账改认源文本——词频走
                        // `template_source`，候选调整走稳定 id（同短语的 `phrase:{code}:{原文}`）。
                        // 命令候选另有 phrase_template。
                        if c.id.is_empty() {
                            c.id = format!("tmpl:{source_text}");
                        }
                        c.template_source = Some(source_text);
                    }
                    expanded.push(c);
                }
                wind_phrase::DictExpansion::Group { name, items } => {
                    // 精确码（候选码 == 输入，或引擎未给码信息）→ 逐成员炸开；
                    // 前缀（候选码更长）→ 折叠为组名候选，选中补全到完整码再展开。
                    if cand.code.is_empty() || cand.code == input {
                        for (display, command_src) in items {
                            let mut c = cand.clone();
                            c.text = display;
                            // 组成员标记：主路汇总全部来源后据此判重码、必要时折回组名候选。
                            c.group_name = name.clone();
                            c.group_template = cand.text.clone();
                            if let Some(src) = command_src {
                                c.phrase_template = src;
                                c.is_command = true;
                            }
                            expanded.push(c);
                        }
                    } else {
                        let mut g = cand;
                        g.group_code = g.code.clone();
                        g.group_name = name.clone();
                        g.group_template = g.text.clone(); // 源 $AA/$SS(..) 备查
                        g.text = name;
                        g.is_group = true;
                        expanded.push(g);
                    }
                }
            }
        }
        expanded
    }

    /// `$SS`/`$AA` 在精确码下的展开态判定（前缀匹配恒折叠，不在此处理）。
    ///
    /// - 无重码（同码无别的候选、也只有这一组）→ 自动展开成员；
    /// - 有重码 → 每组折回一条组名候选（`is_group`），选中后靠 `expanded_group` 只列该组成员。
    ///   成员动辄十几条，整组涌进会把同码的常规候选挤出首页。
    ///
    /// 「重码」的判据见 `competes_with_group`。
    pub(crate) fn resolve_group_expansion(
        expanded: Option<&(String, String)>,
        input: &str,
        codetable_mode: bool,
        candidates: Vec<Candidate>,
    ) -> Vec<Candidate> {
        let is_member = |c: &Candidate| !c.is_group && !c.group_template.is_empty();
        if !candidates.iter().any(is_member) {
            return candidates;
        }
        // 用户已选中某组：只列该组成员。组源对不上（词库被改、组被禁用）则走常规判定。
        if let Some((code, key)) = expanded
            && code == input
            && candidates
                .iter()
                .any(|c| is_member(c) && c.group_template == *key)
        {
            return candidates
                .into_iter()
                .filter(|c| is_member(c) && c.group_template == *key)
                .collect();
        }
        let mut keys: Vec<&str> = Vec::new();
        for c in candidates.iter().filter(|c| is_member(c)) {
            if !keys.contains(&c.group_template.as_str()) {
                keys.push(&c.group_template);
            }
        }
        let competitors = candidates
            .iter()
            .filter(|c| !is_member(c) && Self::competes_with_group(c, input, codetable_mode))
            .count();
        if keys.len() == 1 && competitors == 0 {
            return candidates;
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(candidates.len());
        for c in candidates {
            if !is_member(&c) {
                out.push(c);
                continue;
            }
            if !seen.insert(c.group_template.clone()) {
                continue;
            }
            // 以首个成员为底折回组名候选：保留来源/权重/精确档标志，抹掉成员自带的命令与文本。
            let mut g = c;
            g.text = g.group_name.clone();
            g.is_group = true;
            g.is_group_member = false;
            g.is_command = false;
            g.group_code = input.to_string();
            g.phrase_template = if g.is_phrase {
                g.group_template.clone()
            } else {
                String::new()
            };
            out.push(g);
        }
        out
    }

    /// 候选是否与「当前输入码的组」构成重码：只认同码精确候选（码表精确词与精确码短语都标
    /// `is_exact_code`）；前缀导航、前缀补全不算。
    ///
    /// 拼音域（`codetable_mode=false`）的普通拼音候选一律不算竞争：简拼形态的组码恒有同码简拼词
    /// 与回退补条，算进来折叠就成了常态，短语全码自动展开失效。
    fn competes_with_group(c: &Candidate, input: &str, codetable_mode: bool) -> bool {
        if c.is_group && c.group_code != input {
            return false;
        }
        if c.is_command && !c.group_code.is_empty() && c.group_code != input {
            return false;
        }
        if c.is_exact_code {
            return true;
        }
        codetable_mode || c.is_prefix || c.is_phrase
    }

    /// 用给定上限转换并构建候选（引擎 + 词频 + 短语 + 排序去重），不复位翻页 / 高亮。
    ///
    /// 返回 (引擎候选数（不含短语，供判断 has_more）, 输入结局)。结局含全码自动上屏 / 满码空码
    /// 清空；自动上屏文本经 shadow 复核后才放行。调用方仅在「正向输入字母」时消费结局。
    pub(crate) fn build_candidates(
        &self,
        state: &mut State,
        limit: usize,
    ) -> (usize, InputOutcome) {
        // 出简让全的沿途记录：淘汰与当前码无关的那些。放在最前面，因为下面每条返回路径都可能
        // 走不到记录点，而陈旧记录比没有更危险。这是唯一的失效点，缓冲清空、中间编辑、方案切换
        // 都由这条按前缀关系的淘汰覆盖。
        short_code_yield::evict_stale(&mut state.shortcode_tops, &state.input_buffer);
        if state
            .expanded_group
            .as_ref()
            .is_some_and(|(code, _)| *code != state.input_buffer)
        {
            state.expanded_group = None;
        }
        // 分段上屏进行中且最后一段来自拼音选词：剩余编码强制按混输方案的拼音子方案
        // （`[engine.mixed].secondary_schema`）转换，避免五笔抢首选；不用全局 primary_pinyin。
        // 看最后一段的来源而不是「committed 非空」：混输超码长回捞的码表前缀候选也会进入分段态。
        let last_seg_is_pinyin = crate::wildcard::last_seg_is_pinyin(state);
        let pinyin_schema = if last_seg_is_pinyin {
            let active = self.engine_mgr.active_schema_id();
            self.engine_mgr.mixed_secondary_schema(&active)
        } else {
            None
        };
        // 这条路走独立的拼音方案引擎，拿到的是用户配的 N-best 条数；混输下整句块置顶、
        // 切换键夺键都不该出现，故在这里一并关掉。
        let via_mixed_pinyin = pinyin_schema.is_some();
        // 通配组码（spec §5.3 / §10）：纯码表只查码表；五笔拼音混输码长内是「字面混输 ⊕ 主码表通配」。
        // 都不走短语 / 整句 / 逆切分。pattern 由协调器重算，同时是下面各处短路
        // （短语、自动上屏复评、清空复核、短语自动上屏、显示层去重、出简让全）的唯一判据。
        let wildcard_pattern = self.wildcard_pattern_of(state);
        // 分段续转排在通配之前（spec §10），`wildcard_pattern_of` 在续转态本就给 `None`，这里是第二道保险。
        // 连续辅助码（双拼 + 直接辅助码）只接主转换；未就绪 / 不适用 = `None`，走旧路径。
        let continuous = (pinyin_schema.is_none() && wildcard_pattern.is_none())
            .then(|| self.continuous_aux_lookup(state))
            .flatten();
        let t_convert = std::time::Instant::now();
        let faults0 = crate::process_memory::page_faults();
        let result = match pinyin_schema {
            Some(ps) if self.engine_mgr.ensure_schema(&ps) => {
                self.engine_mgr
                    .convert_with(&ps, &state.input_buffer, limit)
            }
            _ => match (&wildcard_pattern, &continuous) {
                (Some(p), _) => self
                    .engine_mgr
                    .convert_wildcard(&state.input_buffer, p, limit)
                    .unwrap_or_default(),
                (None, Some(ca)) => {
                    self.engine_mgr
                        .convert_active_with_opts(&state.input_buffer, limit, ca.opts())
                }
                (None, None) => self.engine_mgr.convert(&state.input_buffer, limit),
            },
        };
        let t_convert = t_convert.elapsed();
        let faults = faults0
            .zip(crate::process_memory::page_faults())
            .map(|(a, b)| b.saturating_sub(a));
        // 缺页多但不慢的也记：用来区分软缺页（内存里换个映射）和真去读盘的硬缺页。
        if t_convert >= SLOW_STAGE || faults.is_some_and(|n| n >= 100) {
            debug!(
                "convert 慢或缺页多: {:?} faults={:?} buf='{}'",
                t_convert, faults, state.input_buffer
            );
        }
        // 拼音音节拆分形态（供「混输高亮跟随」按高亮候选类型选择显示原始码 / 拆分串）。
        // 码表 / 无拼音 → 空串（恒原始码）。state.preedit 本身由 sync_preedit_to_highlight
        // 按高亮重算（见 update_candidates 末尾 / apply_session_action）。
        state.preedit_split_body = result.preedit_pinyin.clone();
        // 全拼降级形态（双拼下按全拼的切法），供 effective_preedit_body 按高亮候选切换。
        state.preedit_fp_body = result.preedit_fullpinyin.clone();
        // 简拼分段形态（双拼下按简拼的切法，`wbwn` → `w'b'w'n`），同上按高亮候选切换。
        state.preedit_abbrev_body = result.preedit_abbrev.clone();
        // 码表整句的编码单元切分（`aawt'aawt`）。同上按高亮候选切换，见
        // `effective_preedit_body`。非码表 / 未开整句 / 本次无整句解 → 空串。
        state.preedit_codetable_body = result.preedit_codetable.clone();
        // 候选调整（shadow）的归一编码。双拼下 = 全拼码（`hc`→`hao`），使双拼与全拼共享
        // 同一条规则；全拼/码表/混输恒空串 = 落回击键，行为不变。见 `State::shadow_code`。
        state.shadow_code = result.shadow_code.clone();
        // 整句 N-best 池子（出厂恒空），供整句切换键滚动窗口。见 `State::sentence_pool`。
        // 池子一重填，窗口与基底一并作废：本函数也被翻页扩容调用，列表按引擎原序重建，窗口不清零
        // 就与列表错位。代价是扩容会丢掉用户切到的位置，一致性优先。
        state.sentence_pool = if via_mixed_pinyin {
            Vec::new()
        } else {
            result.sentence_pool.clone()
        };
        state.sentence_window = 0;
        state.sentence_base.clear();
        let engine_count = result.candidates.len();
        // 引擎给出的全码自动上屏意向（基于引擎候选；下方 shadow 后复核存活性）。
        let auto_commit = if result.should_commit && !result.commit_text.is_empty() {
            Some(result.commit_text.clone())
        } else {
            None
        };
        let should_clear = result.should_clear;

        // 特殊语法统一展开（见 `finalize_candidates`）。精确匹配空码补全有两个候选源，在下方
        // 「空码补全收口」处统一判空后择一采纳：`engine_completion`（码表引擎备下的更长编码首选）
        // 与 `completion_pool`（短语侧前缀命中）。`completion_hints` 同样要过汇聚点，否则补出来的
        // 直通命令会显示成 `$CC(...)` 源码。
        let engine_completion =
            self.finalize_candidates(result.completion_hints, &state.input_buffer);
        let mut completion_pool: Vec<Candidate> = Vec::new();
        let mut candidates = self.finalize_candidates(result.candidates, &state.input_buffer);
        // 方案级短语作用域：`[phrases] enabled/categories/exclude_categories`。
        // 归属方案取 `effective_data_schema`（临英归 english），见 phrase_spec_of。
        let phrase_spec = self.phrase_spec_of(state);
        let phrase_scope = crate::schema_scope::phrase_scope(&phrase_spec);
        let phrases = self.phrases.read().unwrap_or_else(|e| e.into_inner());
        // 本轮是否真出了精确码短语候选（下方合成候选让位的判据）。不能用 `phrase_has_exact_code`：
        // 求值为空或出错的条目被 `lookup` 整条丢弃，照那个判据会把切分候选也删掉，候选窗全空。
        let mut exact_phrase_shown = false;
        if wildcard_pattern.is_none() && !phrases.is_empty() && !phrase_scope.is_closed() {
            let recent = self.recent_commits_snapshot();
            // 精确码命令 display 含 {clip()} 时按需读取；非 Windows 返回空。走 _cached：这里每次按键
            // 都经过、只为拼 display 标签，不能用会 sleep 重试的阻塞版。
            let clip = |_n: i64| -> String {
                // cfg 外壳保持：非 Windows 返回空串是既定行为，勿「顺手统一」成读 Pasteboard。
                #[cfg(windows)]
                {
                    self.host_services().clipboard_get_text_cached()
                }
                #[cfg(not(windows))]
                {
                    String::new()
                }
            };
            // 反查渲染（`dict.rev`）：无平台调用，三平台一致。注意 `{dict.rev(clip())}` 在非 Windows
            // 上因 `clip` 恒空而整条候选不出现。
            let reverse = |text: &str, fmt: &str, min_len: usize| -> String {
                self.reverse_render_min(text, fmt, min_len)
            };
            let host = wind_phrase::PhraseHost {
                clip: &clip,
                reverse: &reverse,
            };
            for hit in phrases.lookup(&state.input_buffer, &recent, &host, &phrase_scope) {
                exact_phrase_shown = true;
                let is_command = hit.command_src.is_some();
                let is_system = hit.is_system;
                // 稳定 id 取模板原文：text 可能是求值结果（date/time/clip），逐日变化。
                let cand_id = Self::phrase_cand_id(&state.input_buffer, &hit.source_text);
                // `$SS`/`$AA` 精确码成员：带上组标记（见 `resolve_group_expansion`）。
                let (group_name, group_template, is_group_member) = match &hit.group {
                    Some(name) => (name.clone(), hit.source_text.clone(), true),
                    None => Default::default(),
                };
                candidates.push(Candidate {
                    group_name,
                    group_template,
                    is_group_member,
                    // text 存完整原文、含真换行，它就是上屏文本，不得在此改写。一行化是显示层关注点，
                    // 由渲染层 `wind_ui::candidate_window::visible_whitespace` 负责，放在数据层必然殃及上屏。
                    text: hit.text,
                    // 短语自身权重，不加类别硬顶：全仓自产权重在同一条轴上（短语默认 1000、五笔主库
                    // 约定上界 10000），与码表精确候选可直接比。前提是码表权重守约：未归一的导入方案会让短语
                    // 全线沉底且无报错，护栏在 `wind_dict::SystemDictLayer::effective_weight`。各模式的实际
                    // 裁决者见 docs/architecture/candidate-sorting-rules.md 的「5.1」。
                    weight: hit.weight,
                    is_phrase: true,
                    // $CC 命令短语：phrase_template 暂存命令源，选中时由 commit_selected 执行动作。
                    // 非命令短语存原始记录文本（模板未展开），供右键「禁用短语」按 (code, 原文) 定位记录。
                    is_command,
                    // `lookup` 查的是精确码短语，须标精确、与码表精确候选同层按权重竞争；漏标会被
                    // `cmp_exact_first` 压到码表精确候选之下。下面 `lookup_prefix` 的前缀枚举不标。
                    is_exact_code: true,
                    phrase_template: hit.command_src.unwrap_or(hit.source_text),
                    id: cand_id,
                    // 来源标为短语：`record_selection` 据此跳过词频记账；排序层走 `is_phrase`，不受影响。
                    source: CandidateSource::Phrase,
                    meta: CandidateMeta {
                        is_system_phrase: is_system,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
            // 前缀导航：敲前缀（长度 ≥ min_prefix_length）列出该前缀的 marker 短语。$CC 命令 →
            // is_command（选中直接执行，group_code 作执行上下文）；$SS/$AA 组 → is_group（二级选择）。
            let min_prefix = self.rt().config.input.phrase.min_prefix;
            // 精确匹配模式（`single_code_input`，仅纯码表）：抑制短语前缀枚举，与码表引擎跳过
            // `search_prefix` 对齐；混输不适用（拼音半边恒前缀匹配）。例外是 `single_code_complete`：
            // 当前无任何候选时放行一次前缀枚举作补全候选源。
            let ct = self.engine_mgr.codetable_settings();
            // 全局短语的前缀命中按来源统一处理，不按语法类型区分（见下方 `phrase_prefix_is_prefix`）。
            // 这里只承载引擎差异：码表用 is_exact_code 分档、拼音用 is_prefix 分层。
            let codetable_mode = self.engine_mgr.is_codetable();
            let exact_only = codetable_mode && ct.single_code_input;
            // 空码补全的短语侧取数闸门：此处 `candidates.is_empty()` 已是「码表候选 + 精确短语」之和
            // （码表引擎不再抢先塞补全候选），判得准。不设码长闸门，与引擎侧 `completion_hints` 同口径。
            let complete_fallback = exact_only && ct.single_code_complete && candidates.is_empty();
            // `input.phrase.prefix_match` 关闭前缀列举，只留精确码短语。空码补全兜底不受它管。
            let prefix_match = self.rt().config.input.phrase.prefix_match;
            let prefix_hits = if (prefix_match && !exact_only) || complete_fallback {
                phrases.lookup_prefix(&state.input_buffer, &recent, min_prefix, &phrase_scope)
            } else {
                Vec::new()
            };
            // 前缀命中 → 候选的构造，正常枚举与补全池共用：补全要取「若开启前缀匹配本会显示在最前
            // 的那一条」，必须构造得一模一样才能用同一个显示排序器比出首条。
            //
            // 三个前缀分支（`$CC`/`$SS`·`$AA`/静态）的排序标志一律相同，语法只决定选中行为：
            // - `is_exact_code=false`：前缀非完全匹配，不进精确档；
            // - `is_prefix=!codetable_mode`：码表下与更长编码补全同档，拼音/混输下降到拼音精确候选之下；
            // - `weight=hit.weight`：与 `lookup` 分支同口径。
            let phrase_prefix_is_prefix = !codetable_mode;
            let mut built: Vec<Candidate> = Vec::new();
            for hit in prefix_hits {
                // 完整原文，含真换行（理由同 `lookup` 分支），此处不得改写。
                let text = hit.text;
                let is_system = hit.is_system;
                let phrase_meta = || CandidateMeta {
                    is_system_phrase: is_system,
                    ..Default::default()
                };
                if let Some(src) = hit.command_src {
                    // $CC 命令短语：选中直接执行，不二级展开。
                    let code = hit.nav_code.unwrap_or_default();
                    // 稳定 id 的 code 取短语自身完整码（nav_code）：敲 `co` 与 `coad` 时应是同一个身份。
                    let cand_id = Self::phrase_cand_id(&code, &src);
                    built.push(Candidate {
                        text,
                        // 排序标志三分支统一，见上方 `phrase_prefix_is_prefix` 处说明。
                        weight: hit.weight,
                        is_phrase: true,
                        is_command: true,
                        is_prefix: phrase_prefix_is_prefix,
                        phrase_template: src,
                        group_code: code,
                        comment: hit.comment,
                        id: cand_id,
                        source: CandidateSource::Phrase,
                        meta: phrase_meta(),
                        ..Default::default()
                    });
                } else if let Some(code) = hit.nav_code {
                    // $SS/$AA 组短语：选中补全到完整码再二级展开。
                    // phrase_template 存原始记录文本：右键「禁用短语」按 (group_code, 原文) 定位。
                    let cand_id = Self::phrase_cand_id(&code, &hit.source_text);
                    built.push(Candidate {
                        text: text.clone(),
                        // 排序标志三分支统一，见上方 `phrase_prefix_is_prefix` 处说明。
                        weight: hit.weight,
                        is_phrase: true,
                        is_group: true,
                        is_prefix: phrase_prefix_is_prefix,
                        group_code: code,
                        group_name: text,
                        comment: hit.comment,
                        phrase_template: hit.source_text,
                        id: cand_id,
                        source: CandidateSource::Phrase,
                        meta: phrase_meta(),
                        ..Default::default()
                    });
                } else {
                    // 静态短语前缀命中（Literal/Template，command_src=None, nav_code=None）。
                    // 排序标志三分支统一，见上方 `phrase_prefix_is_prefix` 处说明。
                    // 无 nav_code → id 的 code 位退回当前输入缓冲（该分支的短语码即输入前缀）。
                    let cand_id = Self::phrase_cand_id(&state.input_buffer, &hit.source_text);
                    built.push(Candidate {
                        text,
                        weight: hit.weight,
                        is_phrase: true,
                        is_prefix: phrase_prefix_is_prefix,
                        comment: hit.comment,
                        phrase_template: hit.source_text,
                        id: cand_id,
                        source: CandidateSource::Phrase,
                        meta: phrase_meta(),
                        ..Default::default()
                    });
                }
            }
            if complete_fallback {
                completion_pool.extend(built);
            } else {
                candidates.extend(built);
            }
        }
        drop(phrases);
        // 码表合成候选（逆切分 / 整句）让位给精确码短语：引擎判「空码」只看得见码表，短语在
        // convert 之后才追加，精确码短语就是这串码的精确解。限定码表来源：拼音整句与短语本就并列。
        let codetable_composed = |c: &Candidate| {
            (c.is_split_composed || c.is_sentence) && c.source == CandidateSource::CodeTable
        };
        if exact_phrase_shown && candidates.iter().any(codetable_composed) {
            candidates.retain(|c| !codetable_composed(c));
            state.preedit_codetable_body.clear();
        }
        // `$SS`/`$AA` 精确码展开态：所有来源（词库组、短语库组、同码码表词条）汇齐之后才能判重码。
        if wildcard_pattern.is_none() {
            candidates = Self::resolve_group_expansion(
                state.expanded_group.as_ref(),
                &state.input_buffer,
                self.engine_mgr.is_codetable(),
                candidates,
            );
        }
        let mixed =
            self.engine_mgr.current_engine_type() == Some(wind_engine::engine::EngineType::Mixed);
        // 供跨来源档位判「消费整串」与「码 == 输入」。缓冲恒 ASCII，字节长度与 `consumed_length` 同域。
        let input_str = state.input_buffer.clone();
        // 空码补全的择一推迟到全部过滤之后（见下方「空码补全收口」）。
        // 候选层级排序见 `candidate_display_order`；base_sort=natural 时忽略权重，对齐引擎 by_natural。
        let ignore_weight = self.engine_mgr.active_base_sort_ignores_weight();
        // 英文方案：把用过、但原序在取数上限之外的词补进来（非英文方案与调频关时空操作）。
        // 放在排序 / 去重 / 过滤之前，让召回词走同一条加工链，过滤掉的东西不能事后塞回来。
        // 召回词 `natural_order` 接在池尾，排序后落在池尾，正是重排要的 base_pos。
        let recalled = self.english_freq_recall(None, &candidates, &state.input_buffer);
        candidates.extend(recalled);
        // 常用字判定必须先于排序：混输的拼音精确档拿 `is_common` 作提档准入条件。
        self.mark_common(&mut candidates);
        // 同文的纠错候选让给非纠错那条（下面的去重留排序靠前者，纠错那条可能更靠前）。
        wind_candidate::drop_corrected_duplicates(&mut candidates);
        candidates.sort_by(|a, b| candidate_display_order(a, b, ignore_weight, mixed, &input_str));
        // 按 text 去重，不能用 `retain` + `HashSet`：被丢弃那条的码位要并进幸存者，否则按
        // (source, code) 分组的过滤会丢掉「该码位有常用字」的事实（见 `Candidate::merged_codes`）。
        // 通配组码按 `(text, code)` 去重（spec §3.1）：同字不同码要各留一条。`\u{1}` 是槽位字符。
        let dedup_by_code = wildcard_pattern.is_some();
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut deduped: Vec<Candidate> = Vec::with_capacity(candidates.len());
        for c in std::mem::take(&mut candidates) {
            let key: std::borrow::Cow<str> = if c.is_command {
                // 命令候选的身份是「显示名 + 动作」：仅按文本去重会让命令被同名候选吞掉。
                format!("{}\u{2}{}", c.text, c.phrase_template).into()
            } else if dedup_by_code {
                format!("{}\u{1}{}", c.text, c.code).into()
            } else {
                c.text.as_str().into()
            };
            if let Some(&idx) = seen.get(key.as_ref()) {
                deduped[idx].absorb_codes_from(&c);
                continue;
            }
            seen.insert(key.into_owned(), deduped.len());
            deduped.push(c);
        }
        candidates = deduped;
        // 检索范围过滤 → 单字输入 → 用户词频重排（`MAIN_PRE` 链）。
        // - 单字位置钉在 shadow 之前：shadow 的位次记的是用户所见的列表。
        // - 自动补充的候选（`is_scope_filtered`，恒在末尾）排除在重排之外：码表侧 used-first
        //   不衰减，误选一次就会永久顶到常用字前面。
        self.run_passes(
            state,
            &mut candidates,
            crate::postprocess::profile::MAIN_PRE,
            &crate::postprocess::PassCtx {
                owner: None,
                freq_code: Some(&state.input_buffer),
                shadow_code: None,
            },
        );
        // 英文候选定位，必须在词频重排之后（那是最后一道整体排序），在 shadow 之前（用户置顶保留
        // 最终话语权）。位次下限按方案取；混输分段续转走的是拼音方案，不受约束。
        let english_positions = if via_mixed_pinyin {
            (1, 1)
        } else {
            self.engine_mgr.active_english_positions()
        };
        // 首选拼音词库词让英文只对拼音方案成立；选过的英文词免让（惰性：真要让位时才查词频）。
        let pinyin_words_lead = !via_mixed_pinyin
            && self.engine_mgr.current_engine_type()
                == Some(wind_engine::engine::EngineType::Pinyin);
        let input_code = state.input_buffer.clone();
        let english_used = |english: &[Candidate]| self.english_used_before(english, &input_code);
        place_english_after_common_exact(
            &mut candidates,
            &state.input_buffer,
            english_positions,
            pinyin_words_lead.then_some(&english_used as &EnglishUsedFn),
        );
        // 整句 N-best 块（仅 `sentence_count > 1`）：放在上面两道重排之后、shadow 之前，
        // 理由见 `place_sentence_block`。
        place_sentence_block(
            &mut candidates,
            if via_mixed_pinyin {
                1
            } else {
                self.active_sentence_count()
            },
        );
        // 纠错候选不占首选（D6）：放在全部重排之后、shadow 之前——用户置顶仍有最终话语权。
        wind_candidate::protect_top_from_corrected(&mut candidates, input_str.len());
        // 直接辅助码（双拼，末 1～2 位自动当辅码）：命中项并入主候选。钉在全部重排之后、shadow 之前
        // （设计 §6）：放重排之前会被重新排走；放进 `update_candidates` 会在翻页扩容时丢失命中项。
        self.apply_direct_aux(
            state,
            &mut candidates,
            limit,
            engine_count >= limit,
            continuous.as_ref().map(|c| &c.lookup),
        );
        let shadow_code = Self::shadow_code_of(state).to_string();
        // Shadow 规则：删除过滤 + 置顶/移动重排。取码口与写端 `candidate_op_scope` 同源
        // （见 `shadow_code_of`），与词频记账的 `freq_code` 刻意不同域，别互相照抄。
        // 「优先级最高」由返回值 `user_pinned` 兑现：出简让全排在它之后，靠这条判据让路。
        let mut user_pinned = self
            .run_passes(
                state,
                &mut candidates,
                crate::postprocess::profile::MAIN_SHADOW,
                &crate::postprocess::PassCtx {
                    owner: None,
                    freq_code: None,
                    shadow_code: Some(&shadow_code),
                },
            )
            .user_pinned;
        let pool_ctx = crate::postprocess::PassCtx {
            owner: None,
            freq_code: None,
            shadow_code: Some(&shadow_code),
        };
        // 整句池走同一条过滤链：否则切换键能把用户隐藏的、被检索范围滤掉的整句换回来。
        // shadow 会挪动次序，过完按名次排回。
        if !state.sentence_pool.is_empty() {
            let mut pool = std::mem::take(&mut state.sentence_pool);
            self.run_passes(
                state,
                &mut pool,
                crate::postprocess::profile::SENTENCE_POOL,
                &pool_ctx,
            );
            pool.sort_by_key(|c| c.sentence_rank);
            state.sentence_pool = pool;
        }
        // ── 空码补全收口 ──────────────────────────────────────────────────────
        // 精确匹配模式下「一条候选都没有」时补一条兜底。码表引擎与短语层各自只看得见一半，
        // 故两边只交候选源，在此统一判空、统一取一条。必须在过滤链末尾：候选被检索范围滤光或被
        // 用户全部隐藏时，早判会看到「还有候选」而空屏。补全池自己也走同一条过滤链（尤其 shadow，
        // 否则隐藏完当场又被补回来）。
        // 取的是「若开启前缀匹配本会显示在最前的那一条」：用同一个 `candidate_display_order`，
        // 末级补 text 兜底（`lookup_prefix` 产出顺序不定）。
        if candidates.is_empty() {
            completion_pool.extend(engine_completion);
            if !completion_pool.is_empty() {
                // 与最终列表同一套排序，故同样先补 is_common（否则混输档位键在此退化为空操作）。
                self.mark_common(&mut completion_pool);
                // 补全池走同一条过滤链、同一个码。返回值并进 `user_pinned`：补全收口只在主列表为空时走，
                // 那时主列表那次 shadow 恒返回 false，判据全在这一次里。
                user_pinned |= self
                    .run_passes(
                        state,
                        &mut completion_pool,
                        crate::postprocess::profile::COMPLETION_POOL,
                        &pool_ctx,
                    )
                    .user_pinned;
                completion_pool.sort_by(|a, b| {
                    candidate_display_order(a, b, ignore_weight, mixed, &input_str)
                        .then_with(|| a.text.cmp(&b.text))
                });
                candidates.extend(completion_pool.into_iter().next());
            }
        }
        // ── 出简让全 ──────────────────────────────────────────────────────────
        // 有简码的字，在更长的码位上把首选让给词语。必须在 `apply_freq_rerank` 之后：4 码位不保护
        // 首选，先让位会被调频顶回去。判据取自本次输入沿途记录的各级简码位首选，零查询。
        // 通配组码两步都跳过：通配结果是「查码」列表，让位会挪走查询结果，记录会把查询串当简码位。
        if wildcard_pattern.is_none() {
            let yield_level = self.engine_mgr.codetable_settings().short_code_yield_level;
            // `user_pinned`：用户调过这个码的顺序就整码停手，候选调整优先于出简让全。让位被调频钉在
            // shadow 之后，优先级只能写成判据，见 `short_code_yield::apply`。
            short_code_yield::apply(
                &mut candidates,
                &state.input_buffer,
                &state.shortcode_tops,
                yield_level,
                user_pinned,
            );
            // 记在让位之后：记的是用户实际看到的首条。`apply` 扫全部级别，链式让位不会擦掉自己的前提。
            short_code_yield::record_top(
                &mut state.shortcode_tops,
                &state.input_buffer,
                &candidates,
            );
        }
        // ── 英文方案：头部候选（输入原文 + 大小写变形）──────────────────────────
        //
        // 英文引擎「输入即内容」：调频把某个词顶到首位后，想上屏原文就只剩回车，会打断输入流。
        // 钉在所有加工之后（重排、shadow、出简让全、补全收口只作用于词库候选），与临英同序
        // （见 `update_temp_english_candidates`）：`RawCandidateMode::InDict` 要看词库段的最终形态。
        // 配置与临英各自独立，产出共用 crate::english_candidates。
        if self.engine_mgr.active_is_english() {
            let (raw_mode, want_variants, follow_case) = {
                let en = &self.rt().config.schema.english;
                (en.raw_candidate, en.case_variants, en.case_follow_input)
            };
            // 取影子串而不是 `input_buffer`（恒为全小写），否则大小写传不进来，且毫无报错。
            let raw = crate::preedit_cursor::cased_or_buffer(
                &state.input_buffer,
                &state.input_buffer_cased,
            );
            // 词库候选跟随输入大小写。必须在词频重排与 shadow 之后：两者以候选 `text` 为键，先改写
            // 文本会让读写两端对不上。原文留在 `case_source` 里供记账。
            let mut cased = false;
            if follow_case {
                cased = crate::english_candidates::apply_english_case(
                    &mut candidates,
                    raw,
                    crate::english_candidates::CaseVariant::Default,
                );
            }
            // `InDict` 档的判据必须在投影之后求值：投影后 `hell` 才与所打的 `Hell` 字面相同，否则
            // 同一档会随另一个开关给出不同答案。词组分词生效时 `Always` 降级成 `InDict`
            // （见 `raw_mode_under_phrase_seg`）：含分词符的串是查询语法，但 `o'clock` 这类词库词不能误伤。
            let phrase_active = self.rt().config.schema.english.phrase_seg
                && raw.contains(wind_engine::english_phrase::PHRASE_SEPARATOR);
            let raw_mode =
                crate::english_candidates::raw_mode_under_phrase_seg(raw_mode, phrase_active);
            let blocked = self.blocked_words_in(
                &self
                    .engine_mgr
                    .data_schema_id(&self.engine_mgr.active_schema_id()),
            );
            let want_raw = crate::english_candidates::wants_raw_candidate(
                raw_mode,
                raw,
                &candidates,
                blocked.as_deref(),
                follow_case,
            );
            // 大小写变形跟着原文走：原文不产（`ip'pro`）变形也不产，判据统一成「原文产不产」。
            let want_variants = want_variants && (!phrase_active || want_raw);
            let head =
                crate::english_candidates::english_head_candidates(raw, want_raw, want_variants);
            if !head.is_empty() {
                // 与词库同名的头部格由词库词占据（保留来源 / 编码 / 释义），其余同名项被头部
                // 吃掉（同临英 / 快捷输入，见 `merge_head_with_dict`）。
                candidates = crate::english_candidates::merge_head_with_dict(
                    head,
                    std::mem::take(&mut candidates),
                );
            }
            // 档位（CapsLock 循环）作用于整列，含头部候选。档位非默认时投影不再参与（用户已显式指定形态）。
            if state.english_case_variant != crate::english_candidates::CaseVariant::Default {
                cased |= crate::english_candidates::apply_english_case(
                    &mut candidates,
                    raw,
                    state.english_case_variant,
                );
            }
            // 改写过才去重：投影会让词库词撞上头部原文候选，全大写档会把三条变形塌成一条。
            if cased {
                crate::english_candidates::dedup_by_text(&mut candidates);
            }
        }
        // ── Emoji 扩展：钉在所有加工之后 ────────────────────────────────
        // 理由见 `apply_emoji_suggestions`：放进过滤链里它会被静默滤光。
        self.apply_emoji_suggestions(&mut candidates);
        state.candidates = candidates;
        // 满码自动上屏「显示态」复评：引擎按未过滤候选判唯一，智能过滤后可能只剩唯一精确全码
        // 码表候选，据显示候选复评放行。惰性：仅在引擎未给意向时复评。通配结果不自动上屏（spec §3.2）。
        let auto_commit = if wildcard_pattern.is_some() {
            None
        } else {
            auto_commit.or_else(|| {
                self.engine_mgr
                    .recheck_auto_commit(&state.input_buffer, &state.candidates)
            })
        };
        // 复核：上屏目标须仍在最终候选中（未被 shadow 删除）。`$CC` 命令词条展开后 text 已是
        // display 标签，按 phrase_template 补匹配；`$` 模板按 `template_source` 补匹配。
        let outcome = match auto_commit
            .filter(|t| {
                state.candidates.iter().any(|c| {
                    (c.is_command && &c.phrase_template == t)
                        || Self::auto_commit_target_matches(c, t)
                })
            })
            // 短语侧否决：引擎在码表候选子集上判唯一，同码短语对它不可见，会出现「候选窗该有两条，
            // 却当场上屏码表那条」。判据见 [`Self::phrase_vetoes_auto_commit`]。惰性：`filter` 只在
            // 引擎已放行时才求值，全量扫短语码表的代价不落在每次按键上。
            .filter(|_| !self.phrase_vetoes_auto_commit(state, &state.input_buffer))
            // 追加候选否决（`input.emoji.block_auto_commit`，出厂关）：见 `emoji_vetoes_auto_commit`。
            .filter(|_| !self.emoji_vetoes_auto_commit(&state.candidates))
        {
            Some(_) => {
                // 自动上屏文本取实际显示的首候选，与空格/点选同源。仅当显示首选是码表来源时才上屏；
                // 被 shadow 置顶或过滤后首选成了拼音/英文时不上屏，留给用户继续选。
                match state.candidates.first() {
                    // 词库 `$CC` 命令词条：纯文本求值上屏 / 含副作用异步执行（与短语命令同分流）。
                    Some(c) if c.is_command && c.source == CandidateSource::CodeTable => {
                        self.command_auto_outcome(c, &state.input_buffer)
                    }
                    Some(c) if c.source == CandidateSource::CodeTable => {
                        InputOutcome::AutoCommit(c.text.clone())
                    }
                    _ => InputOutcome::Normal,
                }
            }
            // 满码空码清空：引擎只给资格，空不空按最终候选裁决（见 `clear_blocked_by_candidates`）。
            // 通配组码不清空（spec §3.2），这里是双保险。
            None if should_clear
                && wildcard_pattern.is_none()
                && !clear_blocked_by_candidates(
                    &state.candidates,
                    state.input_buffer.chars().count(),
                ) =>
            {
                InputOutcome::Clear
            }
            None => InputOutcome::Normal,
        };
        // 短语自动上屏：码表未给意向时补齐短语侧（引擎判据看不到短语）。通配组码不参与。
        let outcome = match outcome {
            InputOutcome::Normal if wildcard_pattern.is_none() => self
                .phrase_auto_commit(state)
                .unwrap_or(InputOutcome::Normal),
            other => other,
        };
        (engine_count, outcome)
    }

    /// 短语自动上屏（`schema.codetable.auto_commit_at_full` 开启时）：当前输入的唯一候选是
    /// 精确码短语且无更长后继（码表 + 短语两侧）→ 自动上屏。补齐引擎 `decide_auto_commit`
    /// 看不到的短语侧，判据与码表「全码唯一自动上屏」同构。
    ///
    /// - 普通短语 → 直接上屏其文本；
    /// - 纯文本命令（`$CC` 仅 `type` 文本）→ 同步求值上屏（与顶码 `eval_command_text_only` 同路）；
    /// - 含副作用命令 → [`InputOutcome::AutoCommand`]：清组合并异步执行；
    /// - `$SS`·`$AA` 组 / 前缀枚举短语 → 不自动上屏。
    ///
    /// `min_len` 管「够不够满码」，`has_longer_code` 管「还能不能接着打」，缺一不可。
    pub(crate) fn phrase_auto_commit(&self, state: &State) -> Option<InputOutcome> {
        let ct = self.engine_mgr.codetable_settings();
        if !ct.auto_commit_at_full {
            return None;
        }
        // 码表整句开启时满码上屏整体让位（引擎侧在 `CodeTableEngine::new` 关掉，这条读配置须同判）。
        if self.engine_mgr.sentence_input_enabled() {
            return None;
        }
        // 无「满码」概念的引擎（拼音 / 双拼）整体不参与：开关读的是 `schema.codetable`，拼音方案
        // 照样继承，而码长为 0 时最短码长闸恒放行，直通命令成了唯一候选就会当场执行。
        // 必须问满码长而不是 `Engine::max_code_length`：后者对混输刻意为 0，会把混输一并拦下。
        if self.engine_mgr.active_full_code_length() == 0 {
            return None;
        }
        // 最短码长闸：与引擎 decide_auto_commit 的 `input.chars().count() < min_len` 同构。
        if state.input_buffer.chars().count() < self.phrase_auto_commit_min_len(&ct) {
            return None;
        }
        // 唯一候选，不计 emoji 扩展候选：它们不是这个码检索出的词条，否则短语文本恰好命中 emoji
        // 表时本判据恒假。引擎侧 `decide_auto_commit` 按 `c.code == input` 筛，天然排除 emoji；
        // 两处判据不同源，别互相照抄。
        let c = sole_non_emoji(&state.candidates)?;
        // 同码表侧：用户选择让追加候选阻止自动上屏时，这里同口径否决。
        if self.emoji_vetoes_auto_commit(&state.candidates) {
            return None;
        }
        // 精确码短语（非前缀枚举 / 非组）。命令留待下方按纯文本/副作用分流。
        if !c.is_phrase || c.is_prefix || c.is_group {
            return None;
        }
        let input = &state.input_buffer;
        // 无更长后继：码表 + 短语两侧前缀扫描（避免短码短语打断更长输入）。
        if self.engine_mgr.has_longer_code(input) {
            return None;
        }
        let phrase_spec = self.phrase_spec_of(state);
        if self
            .phrases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .has_longer_code(input, &crate::schema_scope::phrase_scope(&phrase_spec))
        {
            return None;
        }
        // 命令 → 统一分流（纯文本求值上屏 / 含副作用异步执行）；普通短语 → 直接文本。
        if c.is_command {
            return Some(self.command_auto_outcome(c, input));
        }
        if c.text.is_empty() {
            return None;
        }
        Some(InputOutcome::AutoCommit(c.text.clone()))
    }

    /// 短语侧对顶码上屏的否决：整串已是精确码短语，或还能续打成更长短语 → 不许顶码。
    ///
    /// 引擎 `CodeTableEngine::handle_top_code` 只问码表，码长超过方案满码长的短语在码表侧
    /// 「既无精确匹配也无更长后继」，会被顶掉而永远打不出来。顶码在
    /// [`Coordinator::accumulate_code_char`] 里排在 `update_candidates` 之前且命中即返回，
    /// 判据必须补在这条路径上。调用点放在 `handle_top_code` 返回 Some 之后：本判据要全量扫
    /// 短语码表，而引擎首道闸极廉价。
    pub(crate) fn phrase_vetoes_top_code(&self, state: &State, input: &str) -> bool {
        self.phrase_owns_code(state, input)
    }

    /// 短语侧对全码唯一自动上屏的否决，判据与 [`Self::phrase_vetoes_top_code`] 同构。
    ///
    /// 引擎 `decide_auto_commit` 在码表候选子集上判唯一，短语候选 `code` 恒空且后追加，
    /// 同码短语 + 唯一码表字会被判成唯一直接上屏。`has_longer_code` 那半条同样不可省：
    /// 超满码长的短语在码表侧恰好呈现为「精确唯一 + 无更长后继」。
    pub(crate) fn phrase_vetoes_auto_commit(&self, state: &State, input: &str) -> bool {
        self.phrase_owns_code(state, input)
    }

    /// 「这个码位归短语管」的单一判据：整串已是精确码短语，或还能续打成更长短语。
    ///
    /// 顶码与全码自动上屏两条路径共用它，别各写一份。全量扫短语码表，调用方须放在
    /// 「引擎已决定上屏」之后作二道闸，不可每键必查。
    pub(crate) fn phrase_owns_code(&self, state: &State, input: &str) -> bool {
        // 这里最不能漏方案级作用域：漏了它，候选里不出短语，顶码与自动上屏却仍被否决，
        // 打字卡住且零日志。见 `docs/design/schema-scoped-behavior.md` §6.3。
        let spec = self.phrase_spec_of(state);
        let scope = crate::schema_scope::phrase_scope(&spec);
        let phrases = self.phrases.read().unwrap_or_else(|e| e.into_inner());
        phrases.has_exact_code(input, &scope) || phrases.has_longer_code(input, &scope)
    }

    /// 这串码是否恰好是一条短语的编码（[`wind_phrase::PhraseLayer::has_exact_code`] 直通）。
    ///
    /// 短语候选的 `code` 恒为空串，「是不是当前码的精确命中」无法从候选本身看出来（前缀命中也
    /// 以 `is_prefix=false` 出现在首位），必须回头问短语层；否则顶码会把前缀命中的短语当成首选上屏。
    pub(crate) fn phrase_has_exact_code(&self, state: &State, code: &str) -> bool {
        let spec = self.phrase_spec_of(state);
        let scope = crate::schema_scope::phrase_scope(&spec);
        self.phrases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .has_exact_code(code, &scope)
    }

    /// 短语自动上屏的最短码长门槛，当前跟随主码表的 `schema.codetable.auto_commit_min_len`，
    /// 否则同一个 `auto_commit_at_full` 开关下短语与码表行为分叉。日后要给短语独立门槛只需改这里。
    fn phrase_auto_commit_min_len(&self, ct: &wind_config::CodetableGlobal) -> usize {
        resolve_auto_commit_min_len(
            ct.auto_commit_min_len,
            self.engine_mgr.active_full_code_length(),
        )
    }

    /// 候选里有追加出来的 emoji / 附加模块符号，且用户开了 `input.emoji.block_auto_commit`
    /// ⇒ 否决全码唯一自动上屏（码表与短语两侧共用）。
    ///
    /// 判据是「列表里有追加候选」而不追溯宿主：走到自动上屏时列表通常只剩上屏目标这一条
    /// 词库候选，追加候选也就挂在它身上。
    fn emoji_vetoes_auto_commit(&self, candidates: &[Candidate]) -> bool {
        self.rt().config.input.emoji.block_auto_commit
            && candidates.iter().any(|c| c.is_emoji_suggestion)
    }

    /// `$CC` 命令候选的自动上屏结局分流（短语命令 / 词库命令词条共用）：
    /// - 纯文本命令（动作链全 Text）→ 同步求值其文本 [`InputOutcome::AutoCommit`]；
    /// - 含副作用命令 → [`InputOutcome::AutoCommand`]（经 `commit_command` 清组合后在独立线程执行：
    ///   Effect 会回调 coordinator 自锁方法，此刻持 state 锁不可同步跑）；
    /// - 求值文本为空 → Normal。
    pub(crate) fn command_auto_outcome(&self, c: &Candidate, input: &str) -> InputOutcome {
        match self.eval_command_text_only(&c.phrase_template, input) {
            Some(t) if !t.is_empty() => InputOutcome::AutoCommit(t),
            Some(_) => InputOutcome::Normal,
            None => InputOutcome::AutoCommand(Box::new(c.clone())),
        }
    }

    /// 常用字判定（只置 `is_common`，无过滤、无删除）。
    ///
    /// 必须在排序之前无条件调用，且不看 `filter_mode`：混输的拼音精确档拿 `is_common` 当提档
    /// 准入条件，若随 `apply_filter` 在 `FilterMode::Gb18030` 下被跳过，提档会无痕失效。
    /// 判定与过滤（`apply_filter`）因此拆开。
    pub(crate) fn mark_common(&self, candidates: &mut [Candidate]) {
        // 读锁在循环外取一次：逐候选取锁在这条热路径（每次按键 × 每个候选）上纯属浪费。
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        if cc.is_empty() {
            return;
        }
        // 字符类 registry 同理，循环外取一次。
        let cs = self.engine_mgr.charsets();
        for c in candidates.iter_mut() {
            // 短语保留（is_phrase 已置位）；其余按常用字表判定
            if !c.is_phrase {
                c.is_common = cc.is_string_common(&c.text, &cs);
                // 用户亲手降级的字与「出厂没收录」分开记：孤儿码位保底对前者不适用（见 `Candidate::user_rare`）。
                c.user_rare = cc.has_user_rare(&c.text);
            }
        }
    }

    /// 单字输入开启时，把词从候选里滤掉。
    ///
    /// 与 [`Self::apply_filter`]（检索范围）是两根正交的轴：那个按字符常用度裁剪、有孤儿码位
    /// 保底与临时放宽；本项按候选长度裁剪、没有任何保底。
    ///
    /// 位置在 `apply_filter` 之后、`apply_shadow` 之前：`ShadowPin.position` 是绝对下标，
    /// 记的是用户所见的列表，滤在 shadow 之后位次就对不上了。
    ///
    /// 被滤集直接丢弃、不留 `FilterOutcome`：刻意不给「翻到底还能翻出词」的出路，
    /// 与用户开单字的意图相反，要看词就关掉它。
    pub(crate) fn apply_single_char(&self, state: &State, candidates: &mut Vec<Candidate>) {
        if !self.effective_single_char(state) {
            return; // 热路径短路：绝大多数用户不开它，省掉逐候选的字素簇切分
        }
        candidates.retain(wind_candidate::single_char_admits);
    }

    /// 按当前检索范围过滤候选（`is_common` 由 `mark_common` 提前填好）。
    /// Gb18030 或数据缺失时不过滤（避免误删）。
    ///
    /// `common_chars.is_empty()` 这道检查不可省：常用字表未加载时全体 `is_common=false`，
    /// `General` 模式会把候选全部滤光。
    pub(crate) fn apply_filter(&self, state: &State, candidates: &mut Vec<Candidate>) {
        let mode = state.filter_mode;
        let table_missing = self
            .common_chars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty();
        if mode == wind_candidate::FilterMode::Gb18030 || table_missing {
            return;
        }
        // 含生僻字的词怎么处置（`input.rare_phrase`）：每次现读内存配置、不进 `State`。它没有菜单
        // 入口也没有临时态，进 State 只多一处热重载同步点（`filter_mode` 进 State 是因为菜单能切换）。
        let phrase =
            wind_candidate::RarePhrasePolicy::from_config(&self.rt().config.input.rare_phrase);
        let taken = std::mem::take(candidates);
        let outcome = wind_candidate::filter_candidates(taken, mode, phrase);
        *candidates = outcome.kept;
        // 临时放宽（末页再按向后翻页键触发）：被滤候选带 `is_scope_filtered` 追加到末尾，原有顺序
        // 不动。标记不可省：自动上屏计数靠它排除（见 `decide_auto_commit`）。
        // 不放宽时一切照旧是刻意的：智能档不该被任何自动补充改动，「不足一页」同样走翻页放宽这条路。
        // 例外：`input.scope_relax.always_append`（触屏端没有翻页键）在智能档下直接追加；
        // 辅助码模式不适用（放宽会绕过字形筛选，与 `try_relax_scope_on_page_end` 同判）。
        let always = self.rt().config.input.scope_relax.always_append
            && mode == wind_candidate::FilterMode::Smart
            && !matches!(state.active, Some(ModeKind::AuxCode));
        if state.scope_relaxed || always {
            candidates.extend(outcome.filtered.into_iter().map(|mut c| {
                c.is_scope_filtered = true;
                c
            }));
        }
    }

    /// 候选调整（shadow）规则的唯一取码口，读端、写端、菜单灰显三处都必须走它。
    ///
    /// 归一码非空（双拼）取归一码，否则落回击键缓冲。三处各取各的话失配完全静默
    /// （规则写进 `hc`、读的是 `hao`）。只服务主输入路：特殊模式的码在 `special_buffer`，
    /// 由 `candidate_op_scope` 自己分流。
    pub(crate) fn shadow_code_of(state: &State) -> &str {
        if state.shadow_code.is_empty() {
            &state.input_buffer
        } else {
            &state.shadow_code
        }
    }

    /// 应用 Shadow 规则：先按 deleted 过滤，再把 pinned 按目标位置重排。
    ///
    /// `schema_override` 指定生效方案（特殊模式用；`None` = 活跃方案）。生产路径经
    /// `postprocess::run_passes` 调用。
    ///
    /// 守卫是 `candidates.is_empty()` 而不是 `code.is_empty()`：空码是合法键位，特殊模式「进入即
    /// 展示」的浏览态正是空码 + 有候选，快符这类一码即上屏的方案只能在那时调候选。
    ///
    /// 返回本码是否有置顶规则就位，出简让全据此让路（见 `short_code_yield::apply`）。取值只此
    /// 一处，让位侧不得自己再查一遍规则，否则 schema 折叠与取码归一各复制一份，失配静默。
    pub(crate) fn apply_shadow_in(
        &self,
        schema_override: Option<&str>,
        candidates: &mut Vec<Candidate>,
        code: &str,
    ) -> bool {
        if candidates.is_empty() {
            return false;
        }
        let Some(store) = &self.store else {
            return false;
        };
        // 候选调整按 data_schema_id 归属（拼音族折叠共享；码表/混输各自独立）。
        let schema_id = schema_override
            .map(str::to_string)
            .unwrap_or_else(|| self.engine_mgr.active_schema_id());
        let schema = self.engine_mgr.data_schema_id(&schema_id);
        // 「屏蔽这个词」：按词库原文、不论编码与来源，兜住协调器自己并进来的候选（短语、快捷输入
        // 历史、英文召回）。混输只管拼音来源：拼音段与拼音方案共用屏蔽，码表段同文词不受影响。
        if let Some(blocked) = self.blocked_words_in(&schema) {
            candidates.retain(|c| !blocked.contains(c.freq_text()));
        } else if self.engine_mgr.schema_engine_type(&schema_id).as_deref() == Some("mixed")
            && let Some(blocked) = self.blocked_words_in(wind_engine::manager::PINYIN_DATA_SCHEMA)
        {
            candidates.retain(|c| {
                c.source != CandidateSource::Pinyin || !blocked.contains(c.freq_text())
            });
        }
        let rec = match store.get_shadow_rules(&schema, code) {
            Ok(Some(r)) => r,
            _ => return false,
        };
        // 纯重排逻辑在 wind_candidate（避免该 crate 依赖 wind-store）。`cand_id` 必须一并传：
        // 短语候选的 word 是写入当天的求值文本，只有 id 跨日稳定（见 `ShadowPinRule`）。
        let pinned: Vec<wind_candidate::ShadowPinRule> = rec
            .pinned
            .iter()
            .map(|p| wind_candidate::ShadowPinRule {
                word: p.word.clone(),
                cand_id: p.cand_id.clone(),
                position: p.position,
            })
            .collect();
        wind_candidate::apply_shadow(candidates, &rec.deleted, &pinned) > 0
    }

    /// 数据方案被「屏蔽这个词」的文字（候选调整里 `code = *` 的删除行，只有拼音与英文有）；
    /// 其它方案、或没有屏蔽词时 `None`。按键路径：只读内存（store 按表内容缓存）。
    pub(crate) fn blocked_words_in(
        &self,
        data_schema: &str,
    ) -> Option<std::sync::Arc<std::collections::HashSet<String>>> {
        if !wind_store::shadow::supports_block(data_schema) {
            return None;
        }
        self.store
            .as_ref()?
            .blocked_words(data_schema)
            .ok()
            .filter(|set| !set.is_empty())
    }

    /// 简繁 1对多变体展开：s2t 开启时，对最终候选列表中的单字候选，紧跟其后插入变体候选
    /// （「出」→「齣」，STCharacters 多值行）。变体 text 保持简体原字，输出走 `s2t_override`。
    ///
    /// 两条时机约束：必须在候选装配全部完成后（去重会误删同 text 的变体，重排会拆散它们）；
    /// 必须在自动上屏判定之后（看到变体会误判不唯一，自动上屏被静默否决）。
    /// 词级候选不展开，多字词由 STPhrases 词级最长匹配消歧。
    pub(crate) fn expand_s2t_variants(&self, state: &mut State) {
        if !state.s2t_enabled {
            return;
        }
        let guard = self.s2t.lock().unwrap_or_else(|e| e.into_inner());
        let Some(conv) = guard.as_ref() else {
            return;
        };
        let mut i = 0;
        while i < state.candidates.len() {
            let c = &state.candidates[i];
            let single = !c.is_command
                && !c.is_group
                && c.s2t_override.is_none()
                && c.text.chars().count() == 1;
            if !single {
                i += 1;
                continue;
            }
            let variants = conv.variants_of(&c.text);
            if variants.is_empty() {
                i += 1;
                continue;
            }
            // 默认转换结果已由原字候选呈现（显示层 maybe_convert），变体里滤掉它防止重复。
            let default_out = conv.convert(&c.text);
            let base = state.candidates[i].clone();
            let mut at = i + 1;
            for v in variants {
                if v == default_out {
                    continue;
                }
                let mut nc = base.clone();
                nc.s2t_override = Some(v);
                state.candidates.insert(at, nc);
                at += 1;
            }
            i = at;
        }
    }

    /// 光标限定查询的截断位（字节）：`Some(n)` = 本轮候选只按 `input_buffer[..n]` 去问引擎。
    ///
    /// 拼音「中间修正」：光标挪到某个音节后面，候选只剩这个音节（及其前缀）的字，选中后上屏、
    /// 后半截留在编码栏续打（复用分步上屏）。
    ///
    /// 默认关（[`cursor_by_syllable`]）：电脑端移光标不改候选是既有行为，这是给电脑端预备的机制，
    /// 接入时只需调 [`set_cursor_by_syllable`]；手机端不走它，用 [`crate::leading_reading`]。
    /// 仅限普通输入的拼音方案。
    fn query_cut(&self, state: &State) -> Option<usize> {
        cut_position(
            cursor_by_syllable(),
            &state.input_buffer,
            state.input_cursor_pos,
            Self::overlay_caret_parts(state).is_none() && self.engine_mgr.is_pinyin(),
        )
    }

    /// [`Self::build_candidates`] 的光标感知入口：光标在编码中间时只查光标前那一段。
    ///
    /// 临时把缓冲截到光标处喂给引擎、查完还原，读 `input_buffer` 的各处都看到一致的前缀。
    /// 还原后补两件事：候选 `consumed_length` 夹到截断位（否则被当成整串匹配、连后半截一起上屏）；
    /// 组合区拆分串换成整串的切分，否则编码栏只剩光标前半截。
    pub(crate) fn build_candidates_at_cursor(
        &self,
        state: &mut State,
        limit: usize,
    ) -> (usize, InputOutcome) {
        // 手机端「首单元读法」：选了非默认读法时，按改写后的查询串问引擎（见 `leading_reading`）。
        if let Some(applied) = state
            .leading
            .clone()
            .filter(|a| a.buffer == state.input_buffer)
        {
            return self.build_candidates_with_leading(state, limit, &applied);
        }
        let Some(cut) = self.query_cut(state) else {
            return self.build_candidates(state, limit);
        };
        let full = state.input_buffer.clone();
        let full_cased = state.input_buffer_cased.clone();
        let cased_ok = preedit_cursor::cased_is_valid(&full, &full_cased);
        state.input_buffer.truncate(cut);
        if cased_ok {
            state.input_buffer_cased.truncate(cut);
        }
        let result = self.build_candidates(state, limit);
        state.input_buffer = full;
        state.input_buffer_cased = full_cased;
        for c in &mut state.candidates {
            if c.consumed_length == 0 || c.consumed_length > cut {
                c.consumed_length = cut;
            }
        }
        let body = self
            .engine_mgr
            .convert(&state.input_buffer, 1)
            .preedit_pinyin;
        if !body.is_empty() {
            // 候选自带的切分只覆盖被查询的前缀且显示时优先，不补后半截编码栏就只剩光标前那几个字母。
            let tail = tail_after_letters(&body, cut);
            if !tail.is_empty() {
                for c in &mut state.candidates {
                    if let Some(own) = c.meta.preedit_split.as_deref() {
                        c.meta.preedit_split = Some(format!("{own}'{tail}").into());
                    }
                }
            }
            state.preedit_split_body = body;
        }
        result
    }

    /// 根据输入缓冲更新候选（动态分级加载：首次小批量，翻页到边界再扩展）。
    /// 返回输入结局（全码自动上屏 / 满码空码清空）；多数调用方忽略，仅正向输入字母时消费。
    pub(crate) fn update_candidates(&self, state: &mut State) -> InputOutcome {
        let outcome = self.refill_candidates(state);
        self.grid_resume(state);
        outcome
    }

    fn refill_candidates(&self, state: &mut State) -> InputOutcome {
        // 联想窗被新组合替换（联想态下打字母不经 `exit_assoc`）：注释翻转随旧窗作废。
        if state.assoc_active() {
            state.clear_comment_flips();
        }
        state.candidates.clear();
        state.preedit = state.input_buffer.clone();
        state.preedit_split_body.clear();
        state.preedit_fp_body.clear();
        state.preedit_abbrev_body.clear();
        state.preedit_codetable_body.clear();
        state.direct_aux_body.clear();
        state.shadow_code.clear();
        state.sentence_pool.clear();
        state.sentence_window = 0;
        state.sentence_base.clear();
        if state.input_buffer.is_empty() {
            state.has_more = false;
            state.candidate_input.clear();
            // 缓冲空但有已转换前缀（逐步转换中删空剩余拼音）：组合区仍显示前缀。
            state.preedit = state.committed_text.clone();
            return InputOutcome::Normal;
        }
        // 缓冲里混进了「入缓冲符号」（`input.buffer_symbol_chars`）：这串已不是本方案的编码，
        // 不问引擎、候选空着。否则拼音引擎会按合法前缀容错留下 `sun` 的候选，空格上屏「孙」，
        // 组合区还会多一个隔音符。判据与顶码否决共用 `buffer_has_literal_symbol`。
        if self.buffer_has_literal_symbol(state) {
            state.has_more = false;
            state.candidate_input = state.input_buffer.clone();
            state.candidate_limit = 0;
            // 候选已清空，页码与高亮必须复位：`current_page` 停在后面几页时，按页切片的各处
            // 会拿到越界的起点（翻页键非 `-` 时可达）。
            self.reset_candidate_view(state);
            // 不能直接 return：`state.preedit` 此时只是裸 `input_buffer`，要靠这个调用拼接
            // `committed_text`（分步上屏的已转换前缀）并把影子串的大写投影回显示串。
            // 候选已空、各 body 已清，它等价于 `committed_text + project_case(input_buffer)`。
            self.sync_preedit_to_highlight(state);
            return InputOutcome::Normal;
        }
        // 纯码表通配组码首批固定 100 条（见 `WILDCARD_INITIAL_LIMIT`）；混输维持原首批
        // （缩到 100 会让拼音保底只剩 20 席）。
        let limit = if self.wildcard_pattern_of(state).is_some()
            && !self.engine_mgr.active_wildcard_mixes_pinyin()
        {
            crate::wildcard::WILDCARD_INITIAL_LIMIT
        } else {
            self.initial_candidate_limit(&state.input_buffer)
        };
        let t0 = std::time::Instant::now();
        let (engine_count, outcome) = self.build_candidates_at_cursor(state, limit);
        let t_build = t0.elapsed();
        // 引导字母的「重复上屏」：输入恰为单个引导字母时，把最近一次上屏内容注入候选顶部。
        // 资格判定见 `leading_letter_repeat_text`。传当前帧是否已有候选：隐式来源只填空帧，
        // 否则活码字母上按空格会上屏上次内容。
        if let Some(last) =
            self.leading_letter_repeat_text(&state.input_buffer, state.candidates.is_empty())
        {
            state.candidates.insert(
                0,
                Candidate {
                    text: last,
                    natural_order: LEADING_REPEAT_ORDER,
                    ..Default::default()
                },
            );
        }
        state.candidate_input = state.input_buffer.clone();
        state.candidate_limit = limit;
        // 引擎返回数达到上限 → 可能还有更多未加载
        state.has_more = engine_count >= limit;
        // 候选变化：复位翻页与高亮（含清除鼠标悬停）
        self.reset_candidate_view(state);
        // 简繁 1对多变体展开（须在自动上屏判定之后——outcome 已定型，见函数文档）。
        self.expand_s2t_variants(state);
        // 组合区按高亮候选类型重算（混输高亮跟随；含已转换前缀拼接）。
        self.sync_preedit_to_highlight(state);
        if t0.elapsed() >= SLOW_STAGE {
            debug!(
                "refill_candidates 慢: build={:?} total={:?} buf='{}' n={}",
                t_build,
                t0.elapsed(),
                state.input_buffer,
                state.candidates.len()
            );
        }
        outcome
    }

    /// 单个引导字母那一帧的「重复上屏」文本（无资格 / 无历史 → None）。
    ///
    /// 它同时是「让位那一帧的反馈」：绑了动作的字母在本方案是活码前缀时按键让位给正常输入，
    /// 那一帧默认空无一物，用户分不清绑定是否生效。不靠降低短语枚举门槛来填：那会把 `zz*`
    /// 分组导航提前一整级，`min_prefix` 形同虚设。
    ///
    /// 资格的两个来源：
    /// - ① `z_key_repeat`：z 专有的显式开关，有候选也照样抢首选（这就是它的语义）；
    /// - ② 该字母绑了 `mix:<id>` 且那个 mix 的 members 含 `quick_input.repeat`：把 mix 空缓冲帧的
    ///   能力提前一格给出（让位路径永远到不了 mix 的空缓冲帧）。这条是隐式推导的，只填空帧
    ///   （`frame_empty`）：活码字母那一帧有真候选，插到顶上会让空格上屏上次的内容。
    fn leading_letter_repeat_text(&self, buffer: &str, frame_empty: bool) -> Option<String> {
        let mut it = buffer.chars();
        let (Some(c), None) = (it.next(), it.next()) else {
            return None; // 仅「恰好一个字符」那一帧
        };
        if !c.is_ascii_alphabetic() {
            return None;
        }
        // ① z 专有老开关（含 z_key_repeat 的开关判定与历史取数）。
        if buffer == "z"
            && let Some(t) = self.z_key_repeat_text()
        {
            return Some(t);
        }
        // ② 目标 mix 的 repeat 成员——仅填补空帧，不抢占正常候选。
        if !frame_empty {
            return None;
        }
        let vk = keymap::VK_A + (c.to_ascii_lowercase() as u32 - 'a' as u32);
        let Some(wind_config::BoundAction::Mix(id)) = self.bound_action_for(vk) else {
            return None;
        };
        let idx = self.mix_mode_idx(&id)?;
        let has_repeat = self
            .rt()
            .config
            .schema
            .mix_modes
            .get(idx as usize)
            .is_some_and(|m| {
                m.members
                    .iter()
                    .any(|s| s == wind_quick_input::MEMBER_REPEAT)
            });
        if !has_repeat {
            return None;
        }
        self.recent_commits_snapshot()
            .into_iter()
            .find(|t| !t.is_empty())
    }

    /// Z 键重复上屏：当前方案（码表/混输）启用 z_key_repeat 时返回最近一次上屏文本，否则 None。
    /// 混输继承主码表行为，故码表/混输统一读有效码表配置（全局 schema.codetable + 方案 override）。
    pub(crate) fn z_key_repeat_text(&self) -> Option<String> {
        let enabled = match self.engine_mgr.current_engine_type() {
            Some(wind_engine::EngineType::CodeTable) | Some(wind_engine::EngineType::Mixed) => {
                self.engine_mgr.codetable_settings().z_key_repeat
            }
            _ => false,
        };
        enabled
            .then(|| self.recent_commits_snapshot().into_iter().next())
            .flatten()
    }

    /// 扩展候选（翻页/下移到边界时调用）：上限翻倍（≤ [`wind_engine::engine::CANDIDATE_LIMIT_CAP`]）重新加载，保持当前页/高亮。
    pub(crate) fn expand_candidates(&self, state: &mut State) {
        // 反查模式的码在 `special_buffer`（`input_buffer` 恒空）：须在下面按 `input_buffer`
        // 判的守卫之前分流，否则会走主路 `build_candidates` 把反查候选清空。
        if matches!(state.active, Some(ModeKind::Reverse)) {
            return self.expand_reverse_candidates(state);
        }
        if !state.has_more || state.candidate_input != state.input_buffer {
            return;
        }
        // 辅助码模式：候选是「会话快照 + 辅助码筛选」的结果，扩容重建整池会绕过筛选，直接不扩展。
        if matches!(state.active, Some(ModeKind::AuxCode)) {
            return;
        }
        let new_limit =
            (state.candidate_limit.saturating_mul(2)).min(wind_engine::engine::CANDIDATE_LIMIT_CAP);
        if new_limit <= state.candidate_limit {
            state.has_more = false;
            return;
        }
        let prev_limit = state.candidate_limit;
        // 翻页扩展不消费全码自动上屏（仅正向输入字母时才上屏）。
        let (engine_count, _) = self.build_candidates_at_cursor(state, new_limit);
        // 重建后立刻重新展开变体（列表整份重建，变体须随之重展）。
        self.expand_s2t_variants(state);
        // 展开态正在按分类筛选：重建出来的是完整列表，重新筛一遍（见 `grid_reapply_filter`）。
        self.grid_reapply_filter(state);
        // 可见列表可能变短（同组常用字进来、放行的生僻字被滤走），页码/高亮须夹回范围。
        self.clamp_candidate_view(state);
        // 到底判据看引擎条数、不看可见条数（spec codetable-wildcard §11 契约 3）：新增一批若全被
        // 检索范围滤掉，按可见条数判会误判到底。`has_more` 只在 `engine_count >= limit` 时置真，
        // 故 `engine_count <= prev_limit` 即引擎没给出新东西，不必另存上一批条数。
        if engine_count <= prev_limit {
            state.has_more = false;
            return;
        }
        state.candidate_limit = new_limit;
        state.has_more = engine_count >= new_limit;
        // 保持当前页/高亮（只在越界时夹回过）；按当前高亮重算组合区（防御性同步）。
        self.sync_preedit_to_highlight(state);
    }

    /// 若 key_code 是配置的二/三候选键，返回页内候选偏移（1=次选/第2项，2=三选/第3项）。
    ///
    /// 数据源是 `keys.session_actions`（`select_key_groups` 已在 `normalize` 里折算进去）。
    /// 配置写的是序号，这里减 1 换成偏移，转换只在这一处发生。`include_printable = true`：
    /// `;` `'` 在所有模式下都查得到，要不要让位给输入字符由各模式的消费点判。
    pub(crate) fn select_key_offset(&self, key_code: u32) -> Option<usize> {
        self.session_action_for(key_code, false, true)?
            .candidate_ordinal()
            .map(|n| n as usize - 1)
    }

    /// 选中「当前页第 `offset` 项」（0=首选），按当前活跃模式派发到各自的选中出口。越界返回 `None`，
    /// 由调用方决定回落语义。只做派发，落点与各模式 keydown 路径是同一个函数，否则两种选法会分叉。
    pub(crate) fn select_page_candidate(
        &self,
        state: &mut State,
        offset: usize,
    ) -> Option<KeyAction> {
        let (start, end) = self.select_range(state);
        let gi = start + offset;
        if gi >= end {
            return None;
        }
        Some(match state.active {
            Some(ModeKind::TempPinyin) => {
                let cand = state.candidates[gi].clone();
                self.commit_temp_pinyin_selected(state, &cand, offset as i32)
            }
            Some(ModeKind::TempEnglish) => self.commit_temp_english_selected(state, gi),
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::Reverse) => {
                self.commit_special_candidate(state, gi)
            }
            Some(ModeKind::Mix(_)) => self.mix_select(state, offset),
            // 网址 / 邮箱模式有补全候选，但不能按序号选中：数字是网址与邮箱的合法字符。
            // 高亮靠导航键移动、上屏走空格/回车（`handle_url_key` / `handle_email_key`）。
            Some(ModeKind::Url) | Some(ModeKind::Email) => return None,
            // Unicode 模式同理：数字键全是十六进制位。上屏走空格/回车（`handle_unicode_key`）。
            Some(ModeKind::Unicode) => return None,
            // 走 `aux_code_committed` 而不是 `commit_selected`：部分消费时辅助码要留在模式内重建会话
            // 继续筛，与轻敲 Shift 选同一候选的行为保持一致。
            Some(ModeKind::AuxCode) => {
                let cand = state.candidates[gi].clone();
                self.aux_code_committed(state, cand, offset as i32)
            }
            None => {
                let cand = state.candidates[gi].clone();
                self.commit_selected(state, &cand, offset as i32)
            }
        })
    }

    /// 上屏当前高亮候选，按当前活跃模式派发：各模式空格键「有候选」那一支的唯一实现，也是
    /// 会话态动词 `commit_highlighted` 的执行体。无候选返回 `None`（空码空格各模式另有处置）。
    ///
    /// 空格臂也调它而不是各写一份：词频、上屏历史、统计、造词、补空格这些副作用各模式出口各有
    /// 一套，照抄一份就会漂移成「上屏的字一样、词频却不一样」。
    /// 与 [`Self::select_page_candidate`] 的分野：那个按页内序号选，本函数按高亮选，且网址 /
    /// 邮箱 / Unicode 在这里有出口（空格本就上屏它们的高亮补全项）。
    pub(crate) fn commit_highlighted(&self, state: &mut State) -> Option<KeyAction> {
        if state.candidates.is_empty() {
            return None;
        }
        // 联想态 + `space_commits = false`：用户明说了空格不选联想，绑定键同样不选，返回 `None`
        // 让键回落原语义。空格臂在调本函数之前自己判这一条（它还要照常出空格）。
        if state.assoc_active() && !self.assoc_config().space_commits {
            return None;
        }
        let (start, _) = self.page_range(state);
        let gi = self
            .highlighted_global_index(state)
            .min(state.candidates.len() - 1);
        let offset = gi - start;
        Some(match state.active {
            None => {
                let cand = state.candidates[gi].clone();
                self.commit_selected(state, &cand, offset as i32)
            }
            Some(ModeKind::TempPinyin) => {
                let cand = state.candidates[gi].clone();
                self.commit_temp_pinyin_selected(state, &cand, offset as i32)
            }
            Some(ModeKind::TempEnglish) => self.commit_temp_english_selected(state, gi),
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::Reverse) => {
                self.commit_special_candidate(state, gi)
            }
            // 重复上屏态不另开分支：`mix_select_at` 自己先判 `mix_repeat`，而重复态下候选恒只有一条，
            // 再判一次是测不到的冗余分支。前提由 `tests/commit_highlighted_key.rs` 的 `quick_input_repeat_*` 钉住。
            Some(ModeKind::Mix(_)) => self.mix_select(state, offset),
            // 部分消费时要留在模式内继续筛，见 `select_page_candidate` 的同名分支。
            Some(ModeKind::AuxCode) => {
                let cand = state.candidates[gi].clone();
                self.aux_code_committed(state, cand, offset as i32)
            }
            Some(ModeKind::Url) => self.commit_url(state, true),
            Some(ModeKind::Email) => self.commit_email(state, true),
            Some(ModeKind::Unicode) => self.commit_unicode(state),
        })
    }

    /// 修饰键（`select_key_groups` 里的 `lrshift` / `lrctrl`）作二三候选键的 keyup 入口。
    ///
    /// 在 keyup 判定的理由见 `hotkey::compile_select_modifier_group`；TSF 侧只在轻敲时才转发
    /// 这个 keyup，收到即可动作。与同一修饰键的中英文切换：有候选选词、无候选切换，返回 `None`
    /// 即让位给下游 toggle 判定，故本函数必须先于它调用。
    ///
    /// 越界（页内候选不足）吞键，既不选也不切换，也不套 `keys.overflow.select_key`（修饰键没有
    /// 字符可输出）。吞键是为了让「有候选时按它绝不切中英文」无例外。
    pub(crate) fn handle_select_key_up(&self, data: &KeyEventData) -> Option<KeyAction> {
        // 只认 keyup-only 键：可打印选词键在 keydown 路径消费，不能在这里重复选。
        // 判据取 `is_key_up_only_vk` 而非区间 `VK_LSHIFT..=VK_RCONTROL`：CapsLock 与那四个修饰键
        // 不连号，而它只有 keyup 一条路，漏掉就是配了完全没反应。
        if !keymap::is_key_up_only_vk(data.key_code) {
            return None;
        }
        let offset = self.select_key_offset(data.key_code)?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.chinese_mode || state.candidates.is_empty() {
            return None;
        }
        debug!(
            "select_key_up: code=0x{:02X} offset={} mode={:?}",
            data.key_code, offset, state.active
        );
        Some(
            self.select_page_candidate(&mut state, offset)
                .unwrap_or(KeyAction::Consumed),
        )
    }

    /// 临英作用域的词组分词符判定（`input.temp_english.phrase_seg`）。
    ///
    /// 与 [`Self::english_phrase_separator_key`] 是两个作用域各一份：临英与英文方案的开关本就独立。
    /// 不问活跃引擎类型（临英是 overlay，活跃引擎通常是五笔/拼音）；调用点自己保证在临英且缓冲非空。
    pub(crate) fn temp_english_phrase_separator_key(&self, key_code: u32) -> bool {
        key_code == wind_keys::keymap::VK_QUOTE && self.rt().config.input.temp_english.phrase_seg
    }

    /// 英文词组分词符判定：`key_code` 是否应作为分词符 `'` 压入缓冲。
    ///
    /// 与 [`Self::manual_separator_key`] 是两条相反策略，刻意不合并：拼音分隔符的 `auto` 档在 `'`
    /// 被占作选词键时避让；英文分词符开关打开就夺取。夺取的代价同
    /// `MixModeConfig::free_input_takes_select_keys`（`'` 选第 3 候选只是数字键的冗余别名），
    /// 但这边默认关（见 `EnglishGlobal::phrase_seg`）。
    ///
    /// 本谓词必须是唯一真相源：调用点是 `message_handler` 里 `VK_QUOTE | VK_BACKTICK` 臂的守卫，
    /// 位于选词分派之前，「收进缓冲」与「不再作选词键」是同一个判断的两面，别拆成两处各判。
    pub(crate) fn english_phrase_separator_key(&self, key_code: u32) -> bool {
        if key_code != wind_keys::keymap::VK_QUOTE {
            return false;
        }
        if !self.rt().config.schema.english.phrase_seg {
            return false;
        }
        // 只在英文引擎下夺取：那个开关属于英文方案的作用域，其余方案 `'` 维持选词键身份。
        self.engine_mgr
            .loaded_engine_type(&self.engine_mgr.active_schema_id())
            == Some(wind_engine::EngineType::English)
    }

    /// 手动分隔符判定的单一入口：`key_code` 是否应作为分隔符压入缓冲（活跃方案）。
    /// 规则见 [`Self::manual_separator_vk_of`]。缓冲是否为空由调用方判定（空缓冲维持标点路径）。
    pub(crate) fn manual_separator_key(&self, key_code: u32) -> bool {
        self.manual_separator_key_of(key_code, &self.engine_mgr.active_schema_id())
    }

    /// 同 [`Self::manual_separator_key`]，但判据取指定方案而非活跃方案。
    ///
    /// overlay 必须走这条：临拼在纯五笔方案下引用的是拼音方案，问活跃引擎恒为码表，分隔符会
    /// 静默失效。凡「按当前引擎类型决定行为」的判据，在 overlay 里都需要一个 `_of(&schema)` 版本。
    pub(crate) fn manual_separator_key_of(&self, key_code: u32, schema_id: &str) -> bool {
        self.manual_separator_vk_of(schema_id) == Some(key_code)
    }

    /// 指定方案此刻的手动分隔符是哪个键；方案不支持 / 策略为 `none` / 没有键可用时 `None`。
    ///
    /// 「是不是分隔符」（[`Self::manual_separator_key_of`]）与「宿主界面该给哪个键」
    /// （[`Self::manual_separator_char`]）都从这里取，键位判定有两份就会界面与引擎各认一个键。
    ///
    /// 规则（每次实时求值，`separator`、`select_key_groups` 热更新即时生效）：
    /// - 拼音族、码表整句放行，其余引擎 `None`（整句靠分隔符消歧：二简与两个一简的击键串同形）。
    /// - `none` → `None`；`quote` → `'`；`backtick` → 反引号。显式模式尊重用户指定值。
    /// - `auto`（默认 / 未知值）依次找：`'` 空闲（不是选词键、方案 `[key_actions]` 没绑）→ `'`；
    ///   反引号空闲 → 反引号；都不空闲时桌面不启用，移动端夺取 `'`。
    ///
    /// 桌面 `auto` 不动选词键（三选键是肌肉记忆，要断词显式写 `separator = "quote"`）；手机靠点选，
    /// `'` 没人按而「分词」键需要出路，故夺取，不愿丢的写 `separator = "none"`。
    /// 两端都不夺 `[key_actions]` 已绑的键，否则用户看到的是「配了没反应」。
    /// `separator` 本身也按方案取（见 `EngineManager::pinyin_separator_mode_of`）。
    pub(crate) fn manual_separator_vk_of(&self, schema_id: &str) -> Option<u32> {
        use wind_keys::keymap::{VK_BACKTICK, VK_QUOTE};
        let may_claim_quote = self.profile.separator_may_claim_quote;
        let pinyin_ok = self.engine_mgr.is_pinyin_of(schema_id);
        if !pinyin_ok && !self.engine_mgr.sentence_input_enabled_of(schema_id) {
            return None;
        }
        match self.engine_mgr.pinyin_separator_mode_of(schema_id).as_str() {
            "none" => None,
            "quote" => Some(VK_QUOTE),
            "backtick" => Some(VK_BACKTICK),
            _ => {
                let bound = |vk: u32| self.bound_action_in_schema(vk, schema_id).is_some();
                let free = |vk: u32| self.select_key_offset(vk).is_none() && !bound(vk);
                if free(VK_QUOTE) {
                    Some(VK_QUOTE)
                } else if free(VK_BACKTICK) {
                    Some(VK_BACKTICK)
                } else if may_claim_quote && !bound(VK_QUOTE) {
                    Some(VK_QUOTE)
                } else {
                    None
                }
            }
        }
    }

    /// 活跃方案的手动分隔符字符（`'` 或 `` ` ``），宿主界面据此决定「分词」键发什么；
    /// `None` = 此刻没有分隔符，宿主不该摆分词键。见 [`Self::manual_separator_vk_of`]。
    pub fn manual_separator_char(&self) -> Option<char> {
        use wind_keys::keymap::{VK_BACKTICK, VK_QUOTE};
        match self.manual_separator_vk_of(&self.engine_mgr.active_schema_id())? {
            VK_QUOTE => Some('\''),
            VK_BACKTICK => Some('`'),
            _ => None,
        }
    }

    /// 若 key_code 是配置的以词定字键，返回取字下标（0=取第 1 字，1=取第 2 字）。
    ///
    /// 数据源是 `keys.session_actions`（`select_char_keys` 已在 `normalize` 里折算进去）。
    /// 出厂 `select_char_keys` 为空 → 恒返回 None。与 [`Self::select_key_offset`] 共用一张表，
    /// 靠动词区分。
    pub(crate) fn select_char_index(&self, key_code: u32) -> Option<usize> {
        self.session_action_for(key_code, false, true)?
            .char_ordinal()
            .map(|n| n as usize - 1)
    }

    /// 当前页候选切片的 [start, end) 区间
    pub(crate) fn page_range(&self, state: &State) -> (usize, usize) {
        let pp = self.page_size(state);
        let len = state.candidates.len();
        // 起点也夹到 len：页码万一越界（本应由 `clamp_candidate_view` 挡住），得到空页而不是
        // `start > end` 的切片 panic。页码在范围内时 start < len，行为不变。
        let start = (state.current_page * pp).min(len);
        let end = (start + pp).min(len);
        (start, end)
    }

    /// 当前高亮候选的全局下标（页起点 + 页内高亮）
    pub(crate) fn highlighted_global_index(&self, state: &State) -> usize {
        let (start, _) = self.page_range(state);
        start + state.selected_index
    }

    /// 组合区「正文」形态选择（不含已转换前缀）：对齐微软五笔，按当前高亮候选的类型决定。
    /// - 无拆分形态（码表/无拼音，preedit_split_body 空）→ 恒原始码（input_buffer）。
    /// - 高亮候选为拼音来源 → 它自带的切分（`meta.preedit_split`），没有则用音节拆分串。
    /// - 高亮候选为码表/短语等非拼音 → 原始码（如 saaa 选「模式」时不拆）。
    fn effective_preedit_body<'a>(&self, state: &'a State) -> &'a str {
        // 码表整句：高亮到整句候选时按编码单元切分显示（`aawt'aawt`）。必须排在
        // `preedit_split_body.is_empty()` 守卫之前：码表方案下拼音拆分形态恒空，守卫会直接返回原始码。
        let highlighted = state.candidates.get(self.highlighted_global_index(state));
        if wants_codetable_split(&state.preedit_codetable_body, highlighted) {
            // 整句 N-best 各条自带切分，高亮到谁显示谁的；没带的（逆切分）用引擎给的那一份。
            return highlighted
                .and_then(|c| c.meta.preedit_split.as_deref())
                .unwrap_or(&state.preedit_codetable_body);
        }
        // 直接辅助码命中项：`前缀音节 + 空格 + 辅码`。排在「分段上屏中恒拆分」之前：分段后剩余
        // 缓冲同样可以带辅码。
        if !state.direct_aux_body.is_empty()
            && state
                .candidates
                .get(self.highlighted_global_index(state))
                .is_some_and(|c| c.is_direct_aux)
        {
            return &state.direct_aux_body;
        }
        // 拼音候选自带的切分（`pinan` 高亮「平安」是 `pin'an`、高亮「皮」是 `pi'nan`），见
        // `CandidateMeta::preedit_split`。排在「无拆分形态」守卫之前：混输下首选的切分与原始码同形时
        // 拆分串为空，别的拼音候选仍可能有自己的切法。
        let cand = state.candidates.get(self.highlighted_global_index(state));
        if let Some(split) = own_preedit_split(cand) {
            return split;
        }
        if state.preedit_split_body.is_empty() {
            return &state.input_buffer;
        }
        // 分段上屏进行中：剩余编码已被强制走拼音方案转换，恒按拼音拆分显示，不按候选来源切换
        // （否则高权重短语顶到首位时后段会显示成原始码）。
        if !state.committed_text.is_empty() {
            return &state.preedit_split_body;
        }
        // 下面几条「按高亮选形态」的分支必须在这里判，不能由引擎就地算定：翻页 / 移动高亮发生在
        // convert 之后，本函数由 `sync_preedit_to_highlight` 在每次高亮变化时重算。它们也与最后那条
        // `source == Pinyin` 判据分开：双拼流、全拼支路、简拼候选的来源同为 Pinyin。

        // 双拼下的简拼候选：编码栏按简拼切法显示（`w'b'w'n`），否则用户看不出自己打的是简拼。
        // 排在全拼降级之前：两个标记可能同真，那时简拼切法才对。
        if !state.preedit_abbrev_body.is_empty() && cand.is_some_and(|c| c.is_abbrev) {
            return &state.preedit_abbrev_body;
        }
        // 双拼下的全拼降级候选：编码栏按全拼切法显示（`zai'jian`），而不是双拼的 `za'ij'ia'n`。
        if !state.preedit_fp_body.is_empty() && cand.is_some_and(|c| c.is_fullpinyin_fallback) {
            return &state.preedit_fp_body;
        }
        let want_split = cand
            .map(|c| c.source == wind_candidate::CandidateSource::Pinyin)
            // 无候选（极少见）：有拆分形态则倾向拆分（纯拼音空候选边界）。
            .unwrap_or(true);
        if want_split {
            &state.preedit_split_body
        } else {
            &state.input_buffer
        }
    }

    /// 当前 overlay 模式的 (缓冲, 光标) 编辑视图。`None` = 普通输入（用 `input_buffer` 那套）。
    /// 五个 overlay 各有独立缓冲字段，这里是它们唯一的收敛点——缓冲编辑一律经此，勿裸 push/pop。
    pub(crate) fn overlay_buf_edit(state: &mut State) -> Option<preedit_cursor::BufEdit<'_>> {
        let st = state;
        Some(match st.active? {
            ModeKind::TempPinyin => {
                preedit_cursor::BufEdit::new(&mut st.temp_pinyin_buffer, &mut st.temp_pinyin_cursor)
            }
            ModeKind::TempEnglish => preedit_cursor::BufEdit::new(
                &mut st.temp_english_buffer,
                &mut st.temp_english_cursor,
            ),
            ModeKind::Url => preedit_cursor::BufEdit::new(&mut st.url_buffer, &mut st.url_cursor),
            ModeKind::Email => {
                preedit_cursor::BufEdit::new(&mut st.email_buffer, &mut st.email_cursor)
            }
            ModeKind::Unicode => {
                preedit_cursor::BufEdit::new(&mut st.unicode_buffer, &mut st.unicode_cursor)
            }
            // 生僻字 / 反查与 special 共用 `special_buffer`：按键处理是同一个 `handle_special_key`，
            // 另起字段的话退格与光标会作用在没人读的字段上，且无任何报错。
            ModeKind::Special(_) | ModeKind::RareChar | ModeKind::Reverse => {
                preedit_cursor::BufEdit::new(&mut st.special_buffer, &mut st.special_cursor)
            }
            ModeKind::Mix(_) => {
                preedit_cursor::BufEdit::new(&mut st.mix_buffer, &mut st.mix_cursor)
            }
            // 辅助码缓冲纯追加（尾增尾删，无光标编辑），不走 BufEdit。
            ModeKind::AuxCode => return None,
        })
    }

    /// overlay caret 换算的四要素 (只读前缀, 缓冲, 显示主体, 光标)，与各模式
    /// `update_*_candidates` 的 `state.preedit` 组装同源（preedit = 前缀 + 主体）。
    ///
    /// 临拼 / mix 的主体是引擎 `preedit_display`（含插入的音节分隔符，与缓冲不同形），取自
    /// `overlay_body`；临英 / 特殊 / URL 的主体恒等于自身缓冲，直接用缓冲。
    fn overlay_caret_parts(state: &State) -> Option<(String, &str, &str, usize)> {
        Some(match state.active? {
            ModeKind::TempPinyin => (
                format!("{}{}", state.temp_pinyin_prefix, state.committed_text),
                &state.temp_pinyin_buffer,
                &state.overlay_body,
                state.temp_pinyin_cursor,
            ),
            ModeKind::Mix(_) => (
                format!("{}{}", state.mix_prefix, state.committed_text),
                &state.mix_buffer,
                &state.overlay_body,
                state.mix_cursor,
            ),
            ModeKind::TempEnglish => (
                state.temp_english_prefix.clone(),
                &state.temp_english_buffer,
                &state.temp_english_buffer,
                state.temp_english_cursor,
            ),
            ModeKind::Special(_) | ModeKind::RareChar | ModeKind::Reverse => (
                state.special_prefix.clone(),
                &state.special_buffer,
                &state.special_buffer,
                state.special_cursor,
            ),
            ModeKind::Url => (
                String::new(),
                &state.url_buffer,
                &state.url_buffer,
                state.url_cursor,
            ),
            // 邮箱：缓冲里已含用户名与 `@`，只读前缀留空（那一段可退格删除）。
            ModeKind::Email => (
                String::new(),
                &state.email_buffer,
                &state.email_buffer,
                state.email_cursor,
            ),
            // Unicode：缓冲里已含触发前缀（`u+4e00` 整串），只读前缀留空。
            ModeKind::Unicode => (
                String::new(),
                &state.unicode_buffer,
                &state.unicode_buffer,
                state.unicode_cursor,
            ),
            // 辅助码：主体 = 辅助码缓冲，前缀 = 进入时拼好的显示前缀。光标恒在串尾。
            ModeKind::AuxCode => {
                let overlay = state.aux_code.as_ref().expect("aux 模式必持 overlay");
                let buf = overlay.session.buffer();
                (overlay.preedit_prefix.clone(), buf, buf, buf.len())
            }
        })
    }

    /// overlay 模式组合区光标的 TSF 位置（UTF-16 单元）。非 overlay 时回退为串尾。
    pub(crate) fn overlay_caret(&self, state: &State) -> u32 {
        match Self::overlay_caret_parts(state) {
            Some((prefix, buffer, body, cursor)) => {
                preedit_cursor::caret_utf16(&prefix, buffer, body, cursor)
            }
            None => state.preedit.chars().count() as u32,
        }
    }

    /// 组合区光标在 preedit 显示串内的字节偏移，供自绘候选窗画插入符。
    /// 统一普通输入与 overlay 两条路径（各自的前缀 / 主体来源不同，见两个 `*_caret_parts`）。
    /// 与 `state.preedit` 同源，故可安全用于 `&state.preedit[..n]` 切片。
    pub(crate) fn ui_caret_bytes(&self, state: &State) -> usize {
        match Self::overlay_caret_parts(state) {
            Some((prefix, buffer, body, cursor)) => {
                preedit_cursor::caret_display_bytes(&prefix, buffer, body, cursor)
            }
            // 普通输入：前缀 = 已转换前缀，主体 = 当前高亮所决定的显示形态。
            None => preedit_cursor::caret_display_bytes(
                &state.committed_text,
                &state.input_buffer,
                self.effective_preedit_body(state),
                state.input_cursor_pos,
            ),
        }
    }

    /// overlay 的音节停靠点：只有临拼 / 混输（显示串 `overlay_body` 带引擎拆的 `'`）有；
    /// 对不上缓冲时 `None`，调用方退回单字符。
    pub(crate) fn overlay_syllable_stops(state: &State) -> Option<Vec<usize>> {
        let buffer = match state.active? {
            ModeKind::TempPinyin => &state.temp_pinyin_buffer,
            ModeKind::Mix(_) => &state.mix_buffer,
            // 其余 overlay 没有音节切分，恒单字符。
            ModeKind::TempEnglish
            | ModeKind::Url
            | ModeKind::Email
            | ModeKind::Unicode
            | ModeKind::Special(_)
            | ModeKind::RareChar
            | ModeKind::Reverse
            | ModeKind::AuxCode => return None,
        };
        state
            .overlay_body
            .contains('\'')
            .then(|| preedit_cursor::syllable_stops(buffer, &state.overlay_body))
            .flatten()
    }

    /// overlay 模式的编码区光标移动（左右 / Home / End）。`None` = 该键不是光标移动键，
    /// 调用方继续分派。
    ///
    /// Delete 不在此处：各模式「删空缓冲」的收尾不同（退出模式 / 回退已转换段），故与各自的
    /// Backspace 臂合并处理。光标移动不重算候选（光标不参与引擎查询），只重发 caret。
    pub(crate) fn overlay_cursor_key(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        if !matches!(
            data.key_code,
            keymap::VK_LEFT | keymap::VK_RIGHT | keymap::VK_HOME | keymap::VK_END
        ) {
            return None;
        }
        let arrow = matches!(data.key_code, keymap::VK_LEFT | keymap::VK_RIGHT);
        // ←/→ 与主输入路径同一套语义（见 `move_caret_syllable`）：默认「边界整音节 / 中间单字符」，
        // Ctrl 总是整音节，Shift 总是单字符。只有临拼 / 混输有音节切分。
        let stops = if arrow && data.modifiers & MOD_SHIFT == 0 {
            Self::overlay_syllable_stops(state)
        } else {
            None
        };
        let smart = data.modifiers & MOD_SHORTCUT == 0;
        let moved = {
            let mut ed = Self::overlay_buf_edit(state)?;
            let right = data.key_code == keymap::VK_RIGHT;
            let jump = stops
                .and_then(|stops| preedit_cursor::syllable_jump(&stops, ed.pos(), right, smart));
            if let Some(pos) = jump {
                ed.set_pos(pos);
                true
            } else {
                match data.key_code {
                    keymap::VK_LEFT => ed.move_left(),
                    keymap::VK_RIGHT => ed.move_right(),
                    keymap::VK_HOME => ed.home(),
                    _ => ed.end(),
                }
            }
        };
        // 已在边界（含缓冲空、只剩只读前缀）：吃掉不透传，否则宿主光标会跳出组合区。
        if !moved {
            return Some(KeyAction::Consumed);
        }
        let text = state.preedit.clone();
        let caret_pos = self.overlay_caret(state);
        // 不重算候选，但仍须刷新 UI：自绘编码栏要据新 caret 重画插入符。
        self.notify_ui_update(state);
        Some(KeyAction::UpdateComposition { caret_pos, text })
    }

    /// 回退最后一个已转换段：把它消费的码并回剩余编码前部并重转候选。
    /// Backspace（段回退优先于光标）与 Delete（删空剩余编码后）共用，对齐 Go
    /// `handleBackspace` / `popConfirmedSegment`。
    ///
    /// 光标一律拉到剩余编码末尾：回退的码插在缓冲前部，光标留在原处会落进这段码中间，
    /// 语义不清。无段可退时（理论边界）吃掉按键，不透传。
    pub(crate) fn pop_committed_seg(&self, state: &mut State) -> KeyAction {
        // 并回缓冲的必须是 raw_code（原始输入空间），不是全拼 code：双拼下后者会把
        // `hao` 塞进击键缓冲，被重解析成 `ha|o` 而整串错乱。
        let Some(popped) = state.committed_segs.pop() else {
            return KeyAction::Consumed;
        };
        let raw_code = popped.raw_code;
        state.committed_text = state
            .committed_segs
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        // 回退段的码并回缓冲前部 → 影子串同步前置（并回的码来自已确认段，恒小写）。
        if !state.input_buffer_cased.is_empty() {
            state.input_buffer_cased = format!("{}{}", raw_code, state.input_buffer_cased);
        }
        state.input_buffer = format!("{}{}", raw_code, state.input_buffer);
        state.input_cursor_pos = state.input_buffer.len();
        // 缓冲整体换了一段：直接辅助码的前缀快照不再对应任何前缀。
        state.direct_aux_prev = None;
        self.update_candidates(state);
        let display = state.preedit.clone();
        let caret_pos = self.composition_caret(state);
        self.notify_ui_update(state);
        KeyAction::UpdateComposition {
            caret_pos,
            text: display,
        }
    }

    /// 普通模式组合区光标的 TSF 位置（UTF-16 单元），与 `sync_preedit_to_highlight` 同源：
    /// 二者都以 `committed_text` 为前缀、`effective_preedit_body` 为主体，故 caret 与所发的
    /// 组合区文本恒对齐（高亮在拼音↔码表候选间移动导致主体在拆分串↔原始码间切换时亦然）。
    pub(crate) fn composition_caret(&self, state: &State) -> u32 {
        preedit_cursor::caret_utf16(
            &state.committed_text,
            &state.input_buffer,
            self.effective_preedit_body(state),
            state.input_cursor_pos,
        )
    }

    /// 临拼 / mix 文本透镜的编码栏正文，按高亮候选选形态，优先级同主路
    /// [`Self::effective_preedit_body`]（候选自带切分 → 简拼 → 全拼降级 → 默认切分）。
    /// 没有任何形态（码表目标、数字透镜、引擎没给显示串）时是缓冲本身。
    fn overlay_effective_body<'a>(&self, state: &'a State, buffer: &'a str) -> &'a str {
        let cand = state.candidates.get(self.highlighted_global_index(state));
        let forms = &state.overlay_forms;
        if let Some(split) = own_preedit_split(cand) {
            return split;
        }
        if !forms.abbrev.is_empty() && cand.is_some_and(|c| c.is_abbrev) {
            return &forms.abbrev;
        }
        if !forms.fullpinyin.is_empty() && cand.is_some_and(|c| c.is_fullpinyin_fallback) {
            return &forms.fullpinyin;
        }
        if forms.split.is_empty() {
            buffer
        } else {
            &forms.split
        }
    }

    /// 临拼 / mix 按高亮重算 `state.preedit` 与供光标换算的 `overlay_body`（两者必须同步，
    /// 否则形态一变 caret 就错位）。组合区 = 只读前缀 + 已转换前缀 + 正文，同各自的候选刷新。
    fn sync_overlay_preedit_to_highlight(&self, state: &mut State) {
        let (prefix, buffer) = match state.active {
            Some(ModeKind::TempPinyin) => (&state.temp_pinyin_prefix, &state.temp_pinyin_buffer),
            Some(ModeKind::Mix(_)) => (&state.mix_prefix, &state.mix_buffer),
            None
            | Some(
                ModeKind::TempEnglish
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::RareChar
                | ModeKind::Reverse
                | ModeKind::AuxCode,
            ) => return,
        };
        // 空缓冲时组合区只剩前缀，由各自的刷新函数写好，不在这里动。
        if buffer.is_empty() {
            return;
        }
        let body = self.overlay_effective_body(state, buffer).to_string();
        state.preedit = format!("{prefix}{}{body}", state.committed_text);
        state.overlay_body = body;
    }

    /// 按当前高亮候选类型重算 `state.preedit`（混输高亮跟随）。含已转换前缀（逐步转换）拼接。
    /// 普通模式走 [`Self::effective_preedit_body`]；临拼 / mix 走各自的 overlay 形态
    /// （[`Self::sync_overlay_preedit_to_highlight`]）。其余 overlay 各自维护 preedit，这里什么都不做
    /// ——按普通模式重算会把编码栏改写成 `committed_text + input_buffer`，在那些模式下就是清空
    /// （分类标签点击 `grid_click_tab` 不分模式调本函数）。
    ///
    /// 辅助码（`AuxCode`）例外，同普通模式：它叠在主路缓冲上，部分上屏后由 `update_candidates`
    /// 按主路重建组合区（已转换前缀 + 剩余编码），辅码前缀随后才拼上去。
    pub(crate) fn sync_preedit_to_highlight(&self, state: &mut State) {
        match state.active {
            None | Some(ModeKind::AuxCode) => {}
            Some(ModeKind::TempPinyin | ModeKind::Mix(_)) => {
                self.sync_overlay_preedit_to_highlight(state);
                return;
            }
            Some(
                ModeKind::TempEnglish
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Special(_)
                | ModeKind::RareChar
                | ModeKind::Reverse,
            ) => return,
        }
        let body = self.effective_preedit_body(state).to_string();
        // 用户按 Shift+字母打出的大写只在影子串里，此处投影回显示串（拼音拆分形态含引擎插入
        // 的分隔符，故按贪心同步扫描投影而非整串替换）。大小写不改字符数，caret 换算不受影响。
        let body =
            preedit_cursor::project_case(&state.input_buffer, &state.input_buffer_cased, &body);
        state.preedit = if state.committed_text.is_empty() {
            body
        } else {
            format!("{}{}", state.committed_text, body)
        };
    }

    /// overlay 候选模式的导航分派：码表型（特殊/临拼，及无表达式来源的 mix）`-`/`=` 作翻页；
    /// 含表达式来源（`quick_input.calc/.date/.number`）的 mix 不把 `-`/`=` 当导航（那是运算符）。
    /// 判据是 `mix_has_quick_numeric`：只配了 `quick_input.repeat` 的 mix 仍把它们当翻页键。
    ///
    /// 临英逐键动态判定：只有该键实际产出的字符列入 `symbol_chars` 白名单时才让位输入，其余照旧
    /// 翻页。判据取字符而非键（`+` 是 Shift+=，同一个键的两个 shift 态分别管辖）。恒 false 会让
    /// 翻页键上屏首候选，整类读 `allow_symbols` 又过于宽泛。凡按 `ModeKind` 整类给值的门控，
    /// 都要复查它对每个模式的取值是否仍成立。
    pub(crate) fn handle_candidate_nav(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let include_printable = match state.active {
            // 生僻字模式与 special 取值必须一致：两者走同一个 `handle_special_key`。
            Some(ModeKind::Special(_))
            | Some(ModeKind::RareChar)
            | Some(ModeKind::Reverse)
            | Some(ModeKind::TempPinyin) => true,
            // mix 目前不经过本函数（`handle_mix_key` 直接调 `apply_session_action`）。保留本分支只为
            // 日后接过来时规则仍然对，取值统一问 `mix_nav_include_printable`。
            Some(ModeKind::Mix(idx)) => self.mix_nav_include_printable(idx),
            // 非可打印键（PgUp/方向键/Tab）`punct_char` 返回 None，恒作导航。
            Some(ModeKind::TempEnglish) => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                punct_char(data.key_code, shift)
                    .is_none_or(|ch| !self.temp_english_char_allowed(ch))
            }
            None
            | Some(ModeKind::Url | ModeKind::Email | ModeKind::Unicode | ModeKind::AuxCode) => {
                false
            }
        };
        self.apply_session_action(state, data, include_printable)
    }

    /// 提交某个候选（记录原始简体词频后清空状态），返回上屏文本（按需简繁转换）。
    /// `s2t_override`：1对多变体候选的输出覆盖；无候选实体（如自动上屏取首选文本）时传 None。
    ///
    /// `freq_code`：词频记账码，须为 [`Self::freq_code`] 的结果（按候选来源分流）。
    /// `freq_text`：词频记账文本，取候选的 [`Candidate::freq_text`]，无候选实体时同 `text`。
    /// 两者都做成显式入参，因为本函数只拿得到 `text`，同文多候选无从反查。
    pub(crate) fn commit_candidate(
        &self,
        state: &mut State,
        text: &str,
        freq_text: &str,
        s2t_override: Option<&str>,
        source: CandidateSource,
        freq_code: &str,
    ) -> String {
        // 满码 / 唯一候选自动上屏不经选词，这里是下一词学习的断点。不能只靠顶层兜底统计去清：
        // 移动端直调 `handle_key_event`，不走那道兜底。
        self.note_word_boundary();
        self.record_selection_cased_in(None, freq_code, freq_text, text, source);
        let mut out = match s2t_override {
            Some(t) => t.to_string(),
            None => self.maybe_convert(state, text),
        };
        if self.english_appends_space(
            state,
            source,
            text,
            crate::preedit_cursor::cased_or_buffer(&state.input_buffer, &state.input_buffer_cased),
        ) {
            out.push(' ');
        }
        state.input_buffer.clear();
        state.preedit.clear();
        state.candidates.clear();
        self.reset_candidate_view(state);
        out
    }

    /// 用户数据（词频 / 候选调整 / 用户词库）的生效方案：这一次按键的候选是哪个方案的引擎出的。
    /// 返回 `None` = 没有特殊归属，调用方走 active。
    ///
    /// - 特殊模式 = 它引用的方案：它有自己的词库和用户数据，按 active 归属会记到主方案头上。
    /// - 临英 = 英文方案（`ENGLISH_SCHEMA`）：同一份词库，归到同一个桶，临英里选过的词切到
    ///   英文方案也受益。判据是「模式是不是临英」，不能复用 `overlay_engine_schema`：那个在
    ///   `show_candidates = false` 时返回 `None`，词频会静默换到主方案的桶里。
    /// - 临拼 / 快捷输入不在此分流：它们在各自的读写点上取 `overlay_engine_schema`（临拼目标
    ///   方案折叠后落 `"pinyin"`，与全拼 / 双拼共享）。往这里加模式前先确认那条路径不够用。
    pub(crate) fn effective_data_schema(&self, state: &State) -> Option<String> {
        match state.active {
            Some(ModeKind::Special(idx)) => self.special_schema(idx),
            Some(ModeKind::TempEnglish) => Some(ENGLISH_SCHEMA.to_string()),
            None
            | Some(
                ModeKind::TempPinyin
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::RareChar
                | ModeKind::Reverse
                | ModeKind::Mix(_)
                | ModeKind::AuxCode,
            ) => None,
        }
    }

    /// 候选词条操作（右键菜单 / Ctrl+数字热键 / macOS 禁用位）的作用域；
    /// 返回 `None` = 当前状态不支持词库操作，调用方只保留复制。
    ///
    /// 三个调用方必须共用这一个函数：菜单可用性与写端落点各判各的，错配完全静默。
    /// 放行集合刻意与 [`Self::effective_data_schema`]（读端取归属的地方）绑死，另写一份清单
    /// 迟早漂移成「写进 A、读的是 B」。
    ///
    /// 空 `code`：主输入路拒绝（本就无候选）；特殊模式放行：`show_all_on_enter` 的浏览态空码也有
    /// 候选，快符这类一码即上屏的方案只能在那时调。读端 [`Self::apply_shadow_in`] 同样按候选非空放行。
    pub(crate) fn candidate_op_scope(&self, state: &State) -> Option<CandidateOpScope> {
        // 没有候选就没有作用域可言（如 show_all_on_enter 关闭时刚进入特殊模式的空白态）。
        if state.candidates.is_empty() {
            return None;
        }
        let (schema, code, raw_code, special) = match state.active {
            // 主输入路的 shadow 码取归一形态（见 `shadow_code_of`），与读端同源。
            None => (
                self.engine_mgr.active_schema_id(),
                Self::shadow_code_of(state).to_string(),
                state.input_buffer.clone(),
                false,
            ),
            // 特殊模式：编码在 special_buffer，归属是它引用的方案（与读端同源）。
            // 码表方案没有第二编码域，两个码恒同值。
            Some(ModeKind::Special(_)) => (
                self.effective_data_schema(state)?,
                state.special_buffer.clone(),
                state.special_buffer.clone(),
                true,
            ),
            // 生僻字模式：编码在 special_buffer，归属是当前活跃方案（`effective_data_schema` 返回
            // None 即「走 active」）。落点必须有：找到字后右键「设为常用字」正是它的主场景。
            // `special = false`：它没有 `show_all_on_enter` 浏览态。
            Some(ModeKind::RareChar) => (
                self.engine_mgr.active_schema_id(),
                state.special_buffer.clone(),
                state.special_buffer.clone(),
                false,
            ),
            // 反查模式：没有落点，右键只留复制。缓冲是查询串而不是码位，规则写进去模式内读不到，
            // 还会误伤主路同名字面输入。「设为常用字」不需要落点，在 `candidate_op` 准入之前分派。
            Some(ModeKind::Reverse) => return None,
            // 临时英文：归属恒是内置英文方案。码取小写化的缓冲：英文方案下 `input_buffer` 恒为全小写，
            // 不归一的话两个入口各存一份键。`raw_code` 保留原形。
            Some(ModeKind::TempEnglish) => (
                self.effective_data_schema(state)?,
                state.temp_english_buffer.to_lowercase(),
                state.temp_english_buffer.clone(),
                false,
            ),
            // 其余 overlay 没有独立词库落点（`effective_data_schema` 返回 None），放行会静默落回主方案。
            // 新增模式时先补落点、再放行，顺序不能反。
            Some(
                ModeKind::TempPinyin
                | ModeKind::Url
                | ModeKind::Email
                | ModeKind::Unicode
                | ModeKind::Mix(_)
                | ModeKind::AuxCode,
            ) => return None,
        };
        // 空码只在特殊模式（浏览态）合法；主输入路空码无候选。
        if code.is_empty() && !special {
            return None;
        }
        // 调位判据要问出这批候选的引擎是不是拼音：`current_engine_type()` 答的是主方案，
        // overlay 下照抄会误禁（或误放）调位。判据是「归属方案是不是 active」而非「是不是特殊模式」：
        // 临英的归属同样不是 active。
        let engine_type = if schema == self.engine_mgr.active_schema_id() {
            self.engine_mgr.current_engine_type()
        } else {
            self.engine_mgr.loaded_engine_type(&schema)
        };
        Some(CandidateOpScope {
            schema,
            code,
            raw_code,
            engine_type,
            special,
        })
    }

    /// 补空格的唯一对外入口：按语境分流到两份开关中的一份（英文方案 / 临英），
    /// 只有 `state` 能区分这两种语境，故所有出口都得走这里。
    pub(crate) fn english_space_enabled_in(&self, state: &State) -> bool {
        // 传的必须是「是不是临英」，不能传 `in_english_input_context`（英文方案或临英），
        // 否则英文方案也会取临英那份开关（见 `raw_code_space_commit_appends_space`）。
        self.english_space_for(state.active == Some(ModeKind::TempEnglish))
    }

    /// 「用户此刻正在打英文」：英文方案常驻，或临英 overlay。单一真相源。
    ///
    /// 问的是整个输入语境，不是某条候选的来源（混输 / 快捷输入里也有英文候选）。消费方是补空格
    /// （[`Self::english_space_enabled_in`]）与 CapsLock 大小写档位循环（`try_english_case_cycle`），
    /// 两者必须共用同一句判据。
    pub(crate) fn in_english_input_context(&self, state: &State) -> bool {
        self.engine_mgr.active_is_english() || state.active == Some(ModeKind::TempEnglish)
    }

    /// 补空格判据的单一真相源。两个作用域各一份开关，按语境取用：
    ///
    /// | 语境 | 开关 |
    /// |---|---|
    /// | 英文方案常驻 | `schema.english.commit_space` |
    /// | 临英 overlay | `input.temp_english.commit_space` |
    ///
    /// 分开的理由见 `TempEnglishConfig::commit_space`。先判临英：临英期间一律以临英那份为准。
    /// 两者并不互斥：「英文方案下不进临英」的门禁只管 Shift+字母一条路，绑键、顶字进入、
    /// z 键夺取回退都能在英文方案下进临英，那时读临英这一份仍是对的。
    fn english_space_for(&self, in_temp_english: bool) -> bool {
        if in_temp_english {
            return self.rt().config.input.temp_english.commit_space;
        }
        self.engine_mgr.active_is_english() && self.rt().config.schema.english.commit_space
    }

    /// 英文候选上屏后是否补一个空格：方案口径之上，再要求「这条候选算英文内容」。
    ///
    /// 判据本体在 [`english_text_counts_as_input`]（纯函数），三条：
    /// - `source == English`：英文方案里的短语等其它来源不补；
    /// - 上屏文本等于所打原码：头部原文候选刻意不带 `source`，只认 `source` 会漏补「空格上屏原码」；
    /// - `source == None` 且文本 ∈ `key_convert::en_case_variants(input)`：头部大小写变形同样补。
    ///   不能改成 `eq_ignore_ascii_case`：会把打 `usa` 选中的短语 `USA` 一并补上空格。
    ///
    /// `input` 必须是所打原码 `cased_or_buffer(input_buffer, input_buffer_cased)`，不能传
    /// `state.input_buffer`（恒为全小写，首字母大写的词会静静地不补）。
    ///
    /// 中文方案主路径里来自英文词库的候选（`source == English`）跟随 `input.temp_english.commit_space`，
    /// 与临英、快捷输入同属「中文里插一个英文词」；原码与短语不在此列。
    pub(crate) fn english_appends_space(
        &self,
        state: &State,
        source: CandidateSource,
        text: &str,
        input: &str,
    ) -> bool {
        // 必须用 `_in`（按语境取开关）：临英是中文方案之上的 overlay，只看英文方案那份恒 false。
        // 临英的点选已走 `commit_temp_english_selected`，不经本函数；保留 `_in` 是为了别的通用
        // 上屏点在临英语境下调到这里时仍读对开关。
        if english_text_counts_as_input(source, text, input) && self.english_space_enabled_in(state)
        {
            return true;
        }
        // 中文方案主路径里混进来的英文词库候选，跟随临英那份开关。只在主路径（无 overlay）：
        // 快捷输入有自己的出口（`mix_english_space`），走到这里会补两遍。
        source == CandidateSource::English
            && state.active.is_none()
            && !self.engine_mgr.active_is_english()
            && self.rt().config.input.temp_english.commit_space
    }

    /// 词频记账用的码，码表与拼音/英文口径不同：
    ///
    /// | 来源 | 用哪个码 | 理由 |
    /// |---|---|---|
    /// | 码表 | 输入码 | `d`/`de`/`def` 是独立码位，`ProtectPolicy` 按码长分级保护首选以此为前提 |
    /// | 拼音 / 英文 | 候选码（[`Self::cand_code`]） | 候选码是完整读音/拼写，跨码位共享才对 |
    ///
    /// 码表候选的 `code` 是词条全码（`de` 下的「有」带 `def`），拿它当键会让各码位互相串扰。
    /// 读写两侧（`record_selection` / `apply_freq_rerank`）必须都经本函数。
    pub(crate) fn freq_code(&self, buf: &str, cand: &Candidate) -> String {
        Self::freq_code_with(
            buf,
            cand,
            self.engine_mgr.freq_settings().english_code_by_input,
        )
    }

    /// [`Self::freq_code`] 的显式传参版本，供热路径使用：`apply_freq_rerank` 在循环外取一次
    /// 布尔量，避免每候选一次带锁的 `freq_settings` 查询。
    pub(crate) fn freq_code_with(buf: &str, cand: &Candidate, english_by_input: bool) -> String {
        match cand.source {
            CandidateSource::CodeTable => buf.to_string(),
            // 英文口径可配（内部项 `english_code_scope`）：英文几乎全是前缀匹配，
            // 跨码位共享与码位独立各有道理，尚未定论，先留旋钮。
            CandidateSource::English if english_by_input => buf.to_string(),
            _ => Self::cand_code(buf, cand),
        }
    }

    /// 拼音类「消费码」：候选自带 code（拼音段）则用之，否则退回整个输入缓冲。
    /// 全拼语义（双拼下为 `hao` 而非击键 `hc`），供词频记账与自动造词。
    /// 退格回退需要的是同域于缓冲的码，见 `raw_consumed_code`。
    pub(crate) fn cand_code(buf: &str, cand: &Candidate) -> String {
        if cand.code.is_empty() {
            buf.to_string()
        } else {
            cand.code.clone()
        }
    }

    /// 分步上屏的「回退码」：本次从缓冲实际切走的那一段原始输入（双拼下即击键 `hc`）。
    /// `consumed_length` 已由引擎回映射到原始输入空间，直接按它切；`partial=false` 时整个缓冲
    /// 都被消费。调用方须已确认 `consumed` 落在字符边界上。
    pub(crate) fn raw_consumed_code(buf: &str, consumed: usize, partial: bool) -> String {
        if partial {
            buf[..consumed].to_string()
        } else {
            buf.to_string()
        }
    }

    // 主输入路拼音选词：组合区逐步转换。部分匹配（候选只消费缓冲前缀）时汉字并入
    // `committed_text`、裁剪缓冲、重转剩余，留在组合区不上屏；完整匹配时整体上屏
    // `committed_text + 候选`、触发自动造词并清空。多行 / 过长候选的显示处理在渲染层
    // （`visible_whitespace`、`UiCandidateConfig::truncate_display`），数据层不改写上屏文本。

    /// 前缀导航候选选中：把输入缓冲补全到该组完整码并重查候选（展开成员/精确命令），
    /// 实现"敲 zz → 选标点 → 展开标点字符"的二级选择。返回新 preedit 显示文本。
    ///
    /// `group_key` 是组的源文本（`$SS(...)`/`$AA(...)`），登记为「用户已选中展开」——
    /// 重码折叠下重查时靠它展开该组（见 `State::expanded_group`）。
    pub(crate) fn complete_to_group_code(
        &self,
        state: &mut State,
        group_code: &str,
        group_key: &str,
    ) -> String {
        state.input_buffer = group_code.to_string();
        state.expanded_group = Some((group_code.to_string(), group_key.to_string()));
        state.input_cursor_pos = state.input_buffer.len();
        let _ = self.update_candidates(state);
        self.notify_ui_update(state);
        state.preedit.clone()
    }

    /// 展开态退一级：用户选中折叠组后（`expanded_group`）按 Backspace，先回到折叠视图而不删码。
    ///
    /// 清掉展开登记并重查；重查后同码确实出现该组的折叠项才算退了一级（返回动作，吞掉按键）。
    /// 否则（无重码，重查仍自动展开）返回 `None`，调用方按普通退格删字符，避免「死键」。
    pub(crate) fn collapse_expanded_group(&self, state: &mut State) -> Option<KeyAction> {
        if !self.rt().config.input.cmdbar.backspace_collapse_group {
            return None;
        }
        let (code, _) = state.expanded_group.take()?;
        if code != state.input_buffer {
            return None;
        }
        let _ = self.update_candidates(state);
        if !state
            .candidates
            .iter()
            .any(|c| c.is_group && c.group_code == code)
        {
            return None;
        }
        let text = state.preedit.clone();
        let caret_pos = self.composition_caret(state);
        self.notify_ui_update(state);
        Some(KeyAction::UpdateComposition { caret_pos, text })
    }

    pub(crate) fn commit_selected(
        &self,
        state: &mut State,
        cand: &Candidate,
        candidate_pos: i32,
    ) -> KeyAction {
        self.commit_selected_learn(state, cand, candidate_pos, true)
    }

    /// [`Self::commit_selected`] 的本体。`learn` = 整体上屏时是否造词、草稿转正并推 6b：
    /// 选词出口恒 `true`；顶屏类出口（次三选键越界 `commit_and_input`）传
    /// [`Self::top_commit_learns`]。
    pub(crate) fn commit_selected_learn(
        &self,
        state: &mut State,
        cand: &Candidate,
        candidate_pos: i32,
        learn: bool,
    ) -> KeyAction {
        // 前缀导航候选：补全输入到该组完整码并重查展开（二级选择，不上屏组名）。
        if cand.is_group {
            let code = cand.group_code.clone();
            // 组源文本：折叠的词库组记在 `group_template`，短语库的组导航记在 `phrase_template`。
            let key = if cand.group_template.is_empty() {
                cand.phrase_template.clone()
            } else {
                cand.group_template.clone()
            };
            let display = self.complete_to_group_code(state, &code, &key);
            return KeyAction::UpdateComposition {
                caret_pos: display.chars().count() as u32,
                text: display,
            };
        }
        // $CC 命令候选：执行动作而非上屏 display 标签。
        if cand.is_command {
            return self.commit_command(state, cand);
        }
        // 联想候选没有编码，以「输入码」为键的加工都不适用：词频会记进读端永远查不到的空码行，
        // 自动造词会凑出用户没打过的词。判据统一是来源，与短语「恒不记词频」同一先例。
        let from_assoc = cand.source == CandidateSource::Assoc;
        let total = state.input_buffer.len();
        let consumed = cand.consumed_length;
        let code = Self::cand_code(&state.input_buffer, cand);
        let partial =
            consumed > 0 && consumed < total && state.input_buffer.is_char_boundary(consumed);
        // 词频记账走 `freq_code`（码表按输入码，拼音/英文按候选码，分段时为前缀码）；
        // 上面的 `code` 仍供 `record_commit` 统计码长使用。
        if !from_assoc {
            self.record_selection_cand(&self.main_freq_code(&state.input_buffer, cand), cand);
        }
        // 下一词学习的上文：`record_commit` 一律清上文，选词是唯一接得上的出口，故先取出来；
        // 分步选的前段放回去，整体上屏交给 `learn_next_word`。
        let next_word_prev = self.take_next_word_prev();
        // 输入统计：每次选词记一段（分段各段各记一次）；两分支都经此处一次。
        self.record_commit(
            &cand.text,
            code.len() as u32,
            candidate_pos,
            wind_store::stats::CommitSource::Candidate,
        );
        let raw_code = Self::raw_consumed_code(&state.input_buffer, consumed, partial);
        if partial {
            self.restore_next_word_prev(next_word_prev);
            state.committed_segs.push(CommittedSeg {
                raw_code,
                code,
                text: cand.text.clone(),
                source: cand.source,
                boundary: cand.boundary,
                learn: cand.meta.learn_code.clone(),
            });
            state.committed_text.push_str(&cand.text);
            state.input_buffer = state.input_buffer[consumed..].to_string();
            // 缓冲切走了前段：直接辅助码的前缀快照（按旧缓冲的前缀取的）作废。
            state.direct_aux_prev = None;
            // 剩余码是缓冲的后缀 → 影子串同步掐头（大写随之左移，不错位也不丢）。
            preedit_cursor::keep_cased_tail(&state.input_buffer, &mut state.input_buffer_cased);
            // 分步确认消费掉前缀码：剩余编码整体左移，光标落到剩余码末尾（对齐 Go）。
            state.input_cursor_pos = state.input_buffer.len();
            let _ = self.update_candidates(state); // preedit 已含前缀（update_candidates 内拼接）
            let display = state.preedit.clone();
            let caret_pos = self.composition_caret(state);
            self.notify_ui_update(state);
            KeyAction::UpdateComposition {
                caret_pos,
                text: display,
            }
        } else {
            // 联想候选不进已转换段、不喂造词：两者都以「这一段是用什么码打出来的」为前提。
            if !from_assoc {
                state.committed_segs.push(CommittedSeg {
                    raw_code,
                    code: code.clone(),
                    text: cand.text.clone(),
                    source: cand.source,
                    boundary: cand.boundary,
                    learn: cand.meta.learn_code.clone(),
                });
            }
            // 联想的显示文本是整词（「中国」），屏幕上已有「中」，要补出去的只有 `commit_override`。
            let final_simplified = format!(
                "{}{}",
                state.committed_text,
                cand.commit_override.as_deref().unwrap_or(&cand.text)
            );
            let learned_code = if learn && !from_assoc {
                // 自动造词：多段组成的词，或一次选中的整句解（单段，靠 `is_sentence` 放行，
                // 见 `learn_phrase_on_commit`）。
                self.learn_phrase_on_commit(state, cand.is_synthesized)
            } else {
                None
            };
            // 6a-draft：选中草稿候选 → 跃迁进临时词库（「用过即转正」的第一跳）。过滤在使用端
            // 不在产生端，是滑窗模型敢产出大量杂词的前提。见 `docs/design/auto-phrase-draft-layer.md` §6。
            let promoted_draft = learn
                && cand.is_draft
                && self.promote_draft_on_commit(&code, &cand.text, cand.boundary);
            // 6b：选中临时层候选推进晋升计数（对齐 Go LearnWord-on-commit）。组 / 命令已在入口返回，
            // 短语无临时词晋升语义。刚跃迁的草稿要跳过：跃迁已记 count = 1，再点查一次就是 +2。
            if learn && !cand.is_phrase && !promoted_draft {
                let active = self.engine_mgr.active_schema_id();
                self.bump_selected_temp_word(&active, cand, &code, learned_code.as_deref());
            }
            // 变体候选（用户明选「齣」类 1对多变体）：末段用覆盖文本、前缀单独转换。普通候选保持整体
            // 转换：STPhrases 词级最长匹配可跨 committed/候选边界消歧（「一」+「出」→「一齣」）。
            let mut out = match &cand.s2t_override {
                Some(t) => format!("{}{}", self.maybe_convert(state, &state.committed_text), t),
                None => self.maybe_convert(state, &final_simplified),
            };
            // 英文补空格：本分支是英文方案下选词的唯一出口（空格 / 数字键 / 次三选键 / 修饰键 / 点选 /
            // 越界 overflow 全汇于此），一处接线即可。`partial` 分支不补（上屏的是前半段）；英文引擎
            // 恒不分段，这只是先把语义定死。补在 s2t 之后：提前补会让词级最长匹配断在空格上。
            if self.english_appends_space(
                state,
                cand.source,
                &cand.text,
                crate::preedit_cursor::cased_or_buffer(
                    &state.input_buffer,
                    &state.input_buffer_cased,
                ),
            ) {
                out.push(' ');
            }
            // 下一轮联想的上文取简体域的完整词，而不是上屏的 `out`：词库前缀检索是简体域的，
            // 且选中联想「中国」时 `out` 只是补出去的「国」。选的是联想候选就记「上文 → 整词」。
            if from_assoc {
                self.record_assoc_pick(cand);
            }
            // 下一词学习：「上一个整体上屏的词 → 这个词」，取词口径同上。
            self.learn_next_word(
                next_word_prev,
                if from_assoc {
                    &cand.text
                } else {
                    &final_simplified
                },
                from_assoc && cand.commit_override.is_some(),
            );
            let assoc_ctx = if from_assoc {
                cand.text.clone()
            } else {
                final_simplified.clone()
            };
            self.reset_pinyin_composition(state);
            // 联想入口。位置即契约：`reset_pinyin_composition` 刚把缓冲与候选清空，正是
            // `maybe_enter_assoc` 要的状态；六类选词触发全汇于本分支，一处接线即覆盖。
            // `assoc_may_chain`：一次性档下选中联想候选后不再续。`on_screen` 是选前缀延伸联想时整词里
            // 已在屏上的那截（「中国」的「中」）。
            let on_screen = match (from_assoc, cand.commit_override.as_deref()) {
                (true, Some(rest)) => cand.text.strip_suffix(rest).unwrap_or(""),
                _ => "",
            };
            if self.assoc_may_chain(from_assoc)
                && self.maybe_enter_assoc(state, &assoc_ctx, on_screen)
            {
                self.notify_ui_update(state);
                // 上屏后重开一个占位组合：这是联想态能收到后续按键的唯一可靠手段（宿主只有
                // `HasActiveComposition()` 是同步的，见 `Coordinator::assoc_composition`）。
                // 走 `commit_then_new_composition` 而不是自己拼 `InsertText`：它按 `top_commit_mode` 分流，
                // 进特殊模式走的也是这一条。
                return self.commit_then_new_composition(out, self.assoc_composition().to_string());
            }
            self.notify_ui_hide();
            Self::commit_action(out, true)
        }
    }

    /// 数字键选词统一入口（num 为 1-based：1-9 选页内对应候选，10 表示主键盘 `0` 选第 10 个）。
    /// 命中当前页范围 → 选词上屏；越界 → 走 overflow 策略（对齐 Go handleNumberKey）。
    pub(crate) fn handle_number_key_select(&self, state: &mut State, num: usize) -> KeyAction {
        let (start, end) = self.select_range(state);
        let idx = start + (num - 1);
        if idx < end {
            let cand = state.candidates[idx].clone();
            // 数字键页内位置 = num-1（候选首选率统计）。
            return self.commit_selected(state, &cand, (num - 1) as i32);
        }
        self.handle_overflow_number_key(state, num)
    }

    /// 数字键超出当前页候选范围时的处理（对齐 Go handleOverflowNumberKey）。
    /// 依 `keys.overflow.number_key`：ignore 吞键 / commit 上屏高亮候选 /
    /// commit_and_input 上屏高亮候选并追加数字字符 / clear_and_input 清空输入并上屏数字。
    /// 无候选或无有效高亮时一律吞键；联想态例外，见函数体。
    pub(crate) fn handle_overflow_number_key(&self, state: &mut State, num: usize) -> KeyAction {
        if state.candidates.is_empty() {
            return KeyAction::Consumed;
        }
        // 联想态：没有「码」可言，`0` / 超出联想条数的数字键就是用户要打的数字。收掉联想、只出该
        // 数字，不看 `keys.overflow.number_key`（`commit` 会把猜的首项一并上屏，`ignore` 会吞键）。
        // 与联想态下标点键同口径（见 `commit_highlight_then_char`）。
        if state.assoc_active() {
            self.exit_assoc(state, crate::handle_assoc::AssocExit::TopCommitKey);
            self.notify_ui_hide();
            let digit = self.overflow_digit_text(state, num);
            self.record_commit(&digit, 0, -1, wind_store::stats::CommitSource::Punctuation);
            return Self::commit_action(digit, true);
        }
        let hi = self.highlighted_global_index(state);
        if hi >= state.candidates.len() {
            return KeyAction::Consumed;
        }
        let behavior = self.rt().config.keys.overflow.number_key.clone();
        match behavior.as_str() {
            "commit" => {
                let cand = state.candidates[hi].clone();
                self.commit_selected(state, &cand, state.selected_index as i32)
            }
            "commit_and_input" => {
                let cand = state.candidates[hi].clone();
                let act = self.commit_selected(state, &cand, state.selected_index as i32);
                let digit = self.overflow_digit_text(state, num);
                Self::append_to_insert_text(act, &digit)
            }
            // 清空输入并上屏该数字：已打的码（含已转换前缀）与候选一并丢弃，不顶高亮候选。
            "clear_and_input" => {
                self.reset_pinyin_composition(state);
                self.notify_ui_hide();
                let digit = self.overflow_digit_text(state, num);
                self.record_commit(&digit, 0, -1, wind_store::stats::CommitSource::Punctuation);
                Self::commit_action(digit, true)
            }
            // "ignore" 及未知值：吞键无效（保留组合，不上屏）
            _ => KeyAction::Consumed,
        }
    }

    /// 数字键越界时要补出的那个数字（`num` 1-based，10 = `0`）。
    /// 小键盘恒半角：follow_main 下小键盘数字选词越界会落到这里，补的数字同样要跟着走半角。
    fn overflow_digit_text(&self, state: &State, num: usize) -> String {
        let digit = (b'0' + (num % 10) as u8) as char;
        if state.full_width && !self.numpad_raw_output(state) {
            wind_transform::fullwidth::to_full_width(&digit.to_string())
        } else {
            digit.to_string()
        }
    }

    /// 次/三选键（`;`/`'`）越界（页内候选不足以命中目标位次）时的处理（对齐 Go
    /// handleOverflowSelectKey）。须排在模式触发键判定之后调用：该键同时是模式触发键时，
    /// 候选不足应优先进模式。依 `keys.overflow.select_key`：ignore 吞键 / commit 上屏高亮候选 /
    /// commit_and_input 上屏高亮候选并追加（转换后的）触发键字符。`key_char` 为触发键产生的
    /// 字符，`prev_char` 为光标前字符（用于数字后智能标点）。
    pub(crate) fn handle_overflow_select_key(
        &self,
        state: &mut State,
        key_char: char,
        prev_char: u16,
    ) -> KeyAction {
        let behavior = self.rt().config.keys.overflow.select_key.clone();
        // 无候选（缓冲非空但无候选）：commit 清组合，commit_and_input 清组合并输出该字符。
        if state.candidates.is_empty() {
            return match behavior.as_str() {
                "commit" => {
                    self.reset_pinyin_composition(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                }
                "commit_and_input" => {
                    let piece = self.convert_punct(state, key_char, prev_char);
                    self.reset_pinyin_composition(state);
                    self.notify_ui_hide();
                    Self::commit_action(piece, state.chinese_mode)
                }
                _ => KeyAction::Consumed,
            };
        }
        let hi = self.highlighted_global_index(state);
        if hi >= state.candidates.len() {
            return KeyAction::Consumed;
        }
        match behavior.as_str() {
            "commit" => {
                let cand = state.candidates[hi].clone();
                self.commit_selected(state, &cand, state.selected_index as i32)
            }
            "commit_and_input" => {
                // 触发键字符按标点流水线转换（在提交前取，chinese_punct 等状态不受提交影响）。
                let cand = state.candidates[hi].clone();
                let piece = self.convert_punct_after(state, key_char, prev_char, &cand.text);
                // 顶屏类出口：造词 / 6b 跟随 `input.top_commit_learn`。
                let act = self.commit_selected_learn(
                    state,
                    &cand,
                    state.selected_index as i32,
                    self.top_commit_learns(),
                );
                Self::append_to_insert_text(act, &piece)
            }
            // "ignore" 及未知值：吞键无效（保留组合，不上屏）
            _ => KeyAction::Consumed,
        }
    }

    /// 以词定字：从当前高亮候选词中取第 `char_index` 个字符上屏（0-based，对齐 Go
    /// handleSelectChar）。返回 `None` 表示「无法以词定字」——无候选 / 无缓冲 / 候选词长度不足 /
    /// 命中的是未展开的组候选（组名不可作字源）——交调用方按 overflow 策略处理。
    pub(crate) fn handle_select_char(
        &self,
        state: &mut State,
        char_index: usize,
    ) -> Option<KeyAction> {
        if state.candidates.is_empty() || state.input_buffer.is_empty() {
            return None;
        }
        let hi = self.highlighted_global_index(state);
        if hi >= state.candidates.len() {
            return None;
        }
        // 逆切分次选只显示后段（「② 困难」）：以词定字按所见取字，被隐藏的前段视同已定、照样上屏，
        // 按 1 得「很困」。前段为空 = 普通候选。
        let (split_front, shown) = self.split_front_and_shown(state, &state.candidates[hi]);
        let cand = state.candidates[hi].clone();
        // 未展开的组候选（cand.text 是组名如「标点符号」）不可作字源 → 吞键，让用户先展开
        // （与 commit_selected 的组候选二级选择一致）。
        if cand.is_group {
            return Some(KeyAction::Consumed);
        }
        let runes: Vec<char> = shown.chars().collect();
        // 候选词长度不足 → None，由调用方按 overflow 处理
        if char_index >= runes.len() {
            return None;
        }
        // 词频学习记实际选的单字，且按这个字自己的编码记（整词码 + 单字是孤儿行）；取不到可靠编码时
        // 只记上屏历史（见 `select_char_freq_code`）。仅普通候选（无副作用命令 Action）才学。
        if cand.actions.is_empty() {
            let ch = runes[char_index].to_string();
            match Self::select_char_freq_code(&cand, runes.len(), char_index) {
                Some(freq_code) => self.record_selection(&freq_code, &ch, cand.source),
                // 被隐藏的前段是这条候选自己的上屏内容，历史要记全（「很困」而非「困」）；
                // 已确认段前缀 `committed_text` 照旧不记，口径不变。
                None => self.push_commit_history(&format!("{split_front}{ch}")),
            }
        }
        // 拼接已确认段前缀 + 选中单字，整体按简繁模式转换（与 commit_selected 一致）。
        let combined = format!(
            "{}{}{}",
            state.committed_text, split_front, runes[char_index]
        );
        let out = self.maybe_convert(state, &combined);
        let chinese = state.chinese_mode;
        self.reset_pinyin_composition(state);
        self.notify_ui_hide();
        Some(Self::commit_action(out, chinese))
    }

    /// 以词定字选出的第 `char_index` 个字（词共 `char_count` 字）自己的词频记账码；
    /// `None` = 拿不到可靠编码，只记历史不记词频。
    ///
    /// - 拼音：按词典真值音节边界（`boundary`）切开整词全拼，音节数与字数相等时取对应音节；
    ///   无边界或音节数对不上（儿化等）⇒ `None`，不猜切分。
    /// - 码表：词频按输入码位记，整词码与这个字的全码都不是它实际被打出的码位 ⇒ `None`。
    /// - 英文 / 短语 / 无来源：单字没有独立码位 ⇒ `None`。
    pub(crate) fn select_char_freq_code(
        cand: &Candidate,
        char_count: usize,
        char_index: usize,
    ) -> Option<String> {
        if cand.source != CandidateSource::Pinyin || cand.code.is_empty() {
            return None;
        }
        let syllables =
            wind_engine::pinyin::mixed_abbrev::syllables_from_boundary(&cand.code, cand.boundary)?;
        (syllables.len() == char_count)
            .then(|| syllables.get(char_index).map(|s| s.to_string()))
            .flatten()
    }

    /// 以词定字的完整流程，含 overflow 策略（对齐 Go handleSelectCharWithOverflow）。
    ///
    /// 仅在有候选时调用：无候选（空缓冲或空码）时这几个键回归标点身份，由调用方放行到标点臂、
    /// 交 `input.punct_on_empty_behavior` 处置。先尝试正常以词定字；失败（候选字数不足、高亮
    /// 越界等真正的越界）则按 `keys.overflow.select_char_key` 处理，三策与 select_key overflow 同构。
    pub(crate) fn handle_select_char_with_overflow(
        &self,
        state: &mut State,
        char_index: usize,
        key_code: u32,
        prev_char: u16,
    ) -> KeyAction {
        if let Some(act) = self.handle_select_char(state, char_index) {
            return act;
        }
        // None：候选词长度不足 / 空码。触发键字符用于 commit_and_input 追加。
        let key_char = crate::coordinator::punct_char(key_code, false);
        let behavior = self.rt().config.keys.overflow.select_char_key.clone();
        // 空码（缓冲非空但无候选）
        if state.candidates.is_empty() {
            return match behavior.as_str() {
                "commit" => {
                    self.reset_pinyin_composition(state);
                    self.notify_ui_hide();
                    KeyAction::ClearComposition
                }
                "commit_and_input" => {
                    let piece = key_char
                        .map(|c| self.convert_punct(state, c, prev_char))
                        .unwrap_or_default();
                    self.reset_pinyin_composition(state);
                    self.notify_ui_hide();
                    Self::commit_action(piece, state.chinese_mode)
                }
                _ => KeyAction::Consumed,
            };
        }
        let hi = self.highlighted_global_index(state);
        if hi >= state.candidates.len() {
            return KeyAction::Consumed;
        }
        match behavior.as_str() {
            "commit" => {
                let cand = state.candidates[hi].clone();
                self.commit_selected(state, &cand, state.selected_index as i32)
            }
            "commit_and_input" => {
                let cand = state.candidates[hi].clone();
                let piece = key_char
                    .map(|c| self.convert_punct_after(state, c, prev_char, &cand.text))
                    .unwrap_or_default();
                let act = self.commit_selected(state, &cand, state.selected_index as i32);
                Self::append_to_insert_text(act, &piece)
            }
            _ => KeyAction::Consumed,
        }
    }

    /// 把附加文本拼到 InsertText 结局尾部（用于 overflow commit_and_input 追加数字/标点）；
    /// 其它 KeyAction（如分段选择产生的 UpdateComposition）原样返回。
    pub(crate) fn append_to_insert_text(act: KeyAction, extra: &str) -> KeyAction {
        match act {
            KeyAction::InsertText {
                text,
                new_composition,
                mode_changed,
                chinese_mode,
                has_new_composition,
            } => KeyAction::InsertText {
                text: format!("{}{}", text, extra),
                new_composition,
                mode_changed,
                chinese_mode,
                has_new_composition,
            },
            other => other,
        }
    }

    /// $CC 命令候选选中：纯文本命令走同步上屏，含副作用命令清理组合区 + 独立线程异步执行。
    ///
    /// 纯文本命令（动作链全 `type()`）必须与普通短语同路：`commit_candidate` + `InsertText`，
    /// 在组合区仍活跃时提交。走异步的话宿主侧已无 composition，TSF 只能裸插入，Word/WPS 对
    /// 组合区提交会把 `\n` 规范成段落标记，裸插入则原样落进段落内部（带换行的文本挤成一行）。
    ///
    /// 含副作用命令必须异步：控制器会回调 handle_menu_command 等自锁方法，而此刻本线程仍持
    /// state 锁（非可重入），同线程重入即死锁。
    pub(crate) fn commit_command(&self, state: &mut State, cand: &Candidate) -> KeyAction {
        // 前缀列举选中的命令携完整码 group_code，用它作执行输入上下文；精确码命令用当前缓冲。
        let input = if cand.group_code.is_empty() {
            state.input_buffer.clone()
        } else {
            cand.group_code.clone()
        };
        // 纯文本命令 → 同步上屏（与 `command_auto_outcome` 同一判据）。求值为空时不走这里，
        // 交异步路径正常清组合收尾。
        if let Some(text) = self.eval_command_text_only(&cand.phrase_template, &input)
            && !text.is_empty()
        {
            let chinese_mode = state.chinese_mode;
            // 记账与 `commit_top_text` 的命令顶码分支同构：来源如实传（短语），`record_selection` 据此
            // 拦掉求值文本；记账文本取 `freq_text`（显示标签），与读端、满码自动上屏同键。
            let out =
                self.commit_candidate(state, &text, cand.freq_text(), None, cand.source, &input);
            self.notify_ui_hide();
            return Self::commit_action(out, chinese_mode);
        }
        self.reset_pinyin_composition(state);
        self.spawn_command_action(cand, input)
    }

    /// 该候选是否为纯文本 `$CC` 命令：动作链全 `type()`、无副作用、且求值文本非空。
    ///
    /// 鼠标路径要在分支判断之前问一次（键盘路径在 [`Self::commit_command`] 内部按求值结果分流）。
    /// 两处判据同源，避免鼠标与键盘对同一条命令给出不同的上屏语义。求值只读快照
    /// （见 [`Coordinator::eval_command_text_only`]），可在持 state 锁时调用。
    fn command_commits_text(&self, cand: &Candidate, input: &str) -> bool {
        cand.is_command
            && self
                .eval_command_text_only(&cand.phrase_template, input)
                .is_some_and(|t| !t.is_empty())
    }

    /// `$CC` 命令执行核心：隐藏 UI + 把命令源放独立线程异步执行，返回 `ClearComposition`。
    /// 不做任何缓冲/模式状态重置，调用方须先完成本路径的退出（正常路径经
    /// `reset_pinyin_composition`；overlay 经各自 `exit_*`）。`input` 为 `input()`/`code()`
    /// 求值上下文，须在退出前捕获。死锁规避见 [`Self::spawn_command`]。
    pub(crate) fn spawn_command_action(&self, cand: &Candidate, input: String) -> KeyAction {
        let src = cand.phrase_template.clone();
        self.notify_ui_hide();
        self.spawn_command(src, input);
        // ClearComposition 而非 Consumed：要清掉宿主里已输入的命令码。type() 的上屏文本由命令线程
        // 经 push 管道单独提交。
        KeyAction::ClearComposition
    }

    /// overlay 路径（特殊模式/临拼/临英/混输）选中候选的命令前置守卫：
    /// 若 `cand` 是 `$CC` 命令候选 → 先以 `code`（该 overlay 的编码缓冲）为上下文捕获，
    /// 执行退出闭包清 overlay 状态，再异步执行动作，返回 `Some(action)`；非命令 → `None`，
    /// 调用方按各自文本上屏语义继续。统一所有 overlay 的 `$CC` 选中执行入口。
    pub(crate) fn overlay_commit_command(
        &self,
        state: &mut State,
        cand: &Candidate,
        code: &str,
        exit: impl FnOnce(&Self, &mut State),
    ) -> Option<KeyAction> {
        if !cand.is_command {
            return None;
        }
        let input = if cand.group_code.is_empty() {
            code.to_string()
        } else {
            cand.group_code.clone()
        };
        exit(self, state);
        Some(self.spawn_command_action(cand, input))
    }

    /// 顶屏点统一命令分流：若当前高亮候选是 $CC 命令，执行命令（与空格选中同语义）并返回
    /// `Some(action)`；否则 `None`，调用方按普通候选顶屏。
    ///
    /// 标点 / 运算符 / 智能符号 Hold / 进其它模式等「顶高亮候选」路径都绕过了 `commit_selected`
    /// 的命令守卫，须经此处。命令候选被顶屏时触发键字符不再单独上屏（命令占据整段缓冲）。
    pub(crate) fn top_commit_command_guard(&self, state: &mut State) -> Option<KeyAction> {
        if state.candidates.is_empty() {
            return None;
        }
        let idx = self
            .highlighted_global_index(state)
            .min(state.candidates.len() - 1);
        if !state.candidates[idx].is_command {
            return None;
        }
        let cand = state.candidates[idx].clone();
        Some(self.commit_command(state, &cand))
    }

    /// 顶码「文本上屏 + 余码续打」收尾（码表候选 / 普通短语 / 纯文本命令 / 引擎回退文本共用）。
    /// 记账 → 设余码为缓冲 → 刷新候选 → 复位首显延迟 → 按 `top_commit_mode` 返回 `InsertText`
    /// （pre_confirm）或 `CommitThenDeferComposition`（direct_commit）。`top_text` 空时只刷新余码。
    ///
    /// - `source`：按被顶出的候选如实传入，不能一律当码表，否则短语的求值文本会写进 FREQ 表。
    /// - `top_text` 一律传简体原文，出屏转换在本函数内收口；记账必须吃简体原文（读端查的是简体）。
    /// - `s2t_override`：语义同 [`Self::cand_convert_text`]，按候选如实传，不依赖「顶码取不到变体」。
    /// - `freq_text`：被顶出候选的 [`Candidate::freq_text`]（`None` = 同 `top_text`），模板词按
    ///   展开结果记账次日即成孤儿行。
    /// - `draft_boundary`：顶出的首选是草稿候选时它的 `boundary`，表示按「草稿被用过」记账。
    ///
    /// 英文补空格不接本函数：英文引擎的码长上限取自最长单词、实际打不到顶码，且余码要继续组合。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_top_text(
        &self,
        state: &mut State,
        prefix: &str,
        top_text: String,
        freq_text: Option<&str>,
        s2t_override: Option<&str>,
        remainder: &str,
        source: CandidateSource,
        draft_boundary: Option<u64>,
    ) -> KeyAction {
        if !top_text.is_empty() {
            self.record_selection_cased_in(
                None,
                prefix,
                freq_text.unwrap_or(&top_text),
                &top_text,
                source,
            );
            // 顶码即上屏首选（pos=0），code_len=被顶出的前缀码长。
            self.record_commit(
                &top_text,
                prefix.len() as u32,
                0,
                wind_store::stats::CommitSource::Candidate,
            );
            // 草稿跃迁：判据是词落没落屏，顶码没有选择键可按；缺了这一跳，超满码长顶出的草稿永远转不了正。
            if let Some(boundary) = draft_boundary {
                let _ = self.promote_draft_on_commit(prefix, &top_text, boundary);
            }
        }
        // 出屏文本（记账之后取，见上）：变体候选用其覆盖文本，其余按需简繁转换。
        let out_text = match s2t_override {
            Some(t) => t.to_string(),
            None => self.maybe_convert(state, &top_text),
        };
        state.input_buffer = remainder.to_string();
        // 余码是缓冲的后缀 → 影子串同步掐头，否则大写会在这里静默丢掉。
        preedit_cursor::keep_cased_tail(&state.input_buffer, &mut state.input_buffer_cased);
        state.input_cursor_pos = state.input_buffer.len(); // 顶码后余码续打，光标在余码末尾
        // 首选已上屏，余码是新的一次组合：不继承上一次的展开与注释翻转（按键入口判不到，这一键按下
        // 时候选还在）。
        state.grid_keep = false;
        state.clear_comment_flips();
        let _ = self.update_candidates(state); // 余码候选（不再消费其结局）
        let preedit = state.preedit.clone();
        // 顶码后宿主光标已前移：复位首显延迟，使余码候选窗在 reflow 后的新坐标首显（对齐 Go）。
        self.reset_first_show();
        self.notify_ui_update(state);
        let has_comp = !remainder.is_empty();
        // direct_commit：真提交顶出文本，余码新组合延迟到触发键 keyup 才开（仅有余码时分叉）。
        if has_comp
            && self.rt().config.input.top_commit_mode == wind_config::TopCommitMode::DirectCommit
        {
            return KeyAction::CommitThenDeferComposition {
                commit_text: out_text,
                deferred_composition: preedit,
                timeout_ms: DEFERRED_COMPOSITION_FALLBACK_MS,
            };
        }
        KeyAction::InsertText {
            text: out_text,
            new_composition: has_comp.then_some(preedit),
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: has_comp,
        }
    }

    /// 顶屏「已转换前缀 + 高亮候选」的出屏文本，供进模式类顶屏（临英 / mix / 特殊模式 / 临时
    /// 拼音）共用；返回 `None` = 没有待上屏内容（空缓冲进入），调用方直接返回进模式动作。
    ///
    /// 必须收成一个函数：出屏文本只能有一个落点，复制出去的加工步骤（简繁转换、`record_commit`）
    /// 不会被一起想起来。两半转换方式不同：已转换前缀走 `maybe_convert`，高亮候选走
    /// [`Self::cand_convert_text`]（高亮可停在 1对多变体上，整串转换会打回默认结果）。
    /// 记账一律吃简体原文 `cand.text`。
    ///
    /// 联想态不顶屏（同 [`Self::commit_highlight_then_char`]）：高亮是输入法猜的，按引导键的意图
    /// 就是进模式。这里显式 `exit_assoc`：开软键盘这类非模式动作不清候选、编码栏标识与自动隐藏计时。
    pub(crate) fn take_committed_with_highlight(&self, state: &mut State) -> Option<String> {
        self.exit_assoc(state, crate::handle_assoc::AssocExit::TopCommitKey);
        // 顶屏进模式 = 旧候选窗关了，注释翻转不带进新模式。
        state.clear_comment_flips();
        // 顶屏的高亮候选并进已转换段后造词 / 推 6b（见 `learn_on_top_commit`），须在 `take_committed`
        // 清段之前；主输入路归属活跃方案。
        let highlight = self.highlight_for_top_commit(state);
        if let Some((_, cand)) = &highlight {
            let buf = state.input_buffer.clone();
            self.learn_on_top_commit(state, cand, &buf, None);
        }
        let prefix = self.take_committed(state);
        let mut out = self.maybe_convert(state, &prefix);
        let Some((idx, cand)) = highlight else {
            return (!out.is_empty()).then_some(out);
        };
        let (start, _) = self.page_range(state);
        // 记账码见 `main_freq_code`（码表按输入码，拼音/英文按候选码，通配组码记全码）。
        let freq_code = self.main_freq_code(&state.input_buffer, &cand);
        self.record_selection_cand(&freq_code, &cand);
        // 顶屏上屏的是一条来源候选（prefix 段已在选词时记过）。`saturating_sub` 只为不给纯展示量
        // 留下溢 panic。
        self.record_commit(
            &cand.text,
            state.input_buffer.len() as u32,
            idx.saturating_sub(start) as i32,
            wind_store::stats::CommitSource::Candidate,
        );
        out.push_str(&self.cand_convert_text(state, &cand));
        Some(out)
    }

    /// 顶屏类出口（标点顶屏 / 非码元字符与小键盘顶屏 / 进模式顶屏 / 次三选键越界
    /// `commit_and_input`）是否造词并推 6b。读内部配置 `input.top_commit_learn`（默认开）。
    pub(crate) fn top_commit_learns(&self) -> bool {
        self.rt().config.input.top_commit_learn
    }

    /// 顶屏时被顶的高亮候选：`(全局下标, 候选)`。无候选或联想态（联想没有码，不顶屏）时
    /// `None`。主输入路各顶屏出口共用，判据只写这一处。
    pub(crate) fn highlight_for_top_commit(&self, state: &State) -> Option<(usize, Candidate)> {
        if state.candidates.is_empty() || state.assoc_active() {
            return None;
        }
        let idx = self
            .highlighted_global_index(state)
            .min(state.candidates.len() - 1);
        Some((idx, state.candidates[idx].clone()))
    }

    /// 顶屏类出口的造词与 6b：顶屏即「选高亮那条」，与选词出口的整体上屏同口径，把被顶的候选
    /// 并进已转换段（码取 `cand_code`，回退码是整个 `buf`），再走 `learn_phrase_on_commit_in`
    /// 与 `bump_selected_temp_word`。
    ///
    /// `overlay`：`None` = 主输入路，临拼传目标方案、快捷输入传候选的成员方案。须在调用方
    /// `take_committed` / 退出模式之前调用（那两步会清空已转换段）。联想 / 组 / 命令 / emoji
    /// 候选不参与。高亮是分步候选时不造词、只推 6b：剩余码被丢弃，拼出的词不是用户打全的那串。
    pub(crate) fn learn_on_top_commit(
        &self,
        state: &mut State,
        cand: &Candidate,
        buf: &str,
        overlay: Option<&str>,
    ) {
        if !self.top_commit_learns()
            || cand.source == CandidateSource::Assoc
            || cand.is_group
            || cand.is_command
            || cand.is_emoji_suggestion
        {
            return;
        }
        let code = Self::cand_code(buf, cand);
        let consumed = cand.consumed_length;
        let partial = consumed > 0 && consumed < buf.len() && buf.is_char_boundary(consumed);
        let learned_code = if partial {
            None
        } else {
            state.committed_segs.push(CommittedSeg {
                raw_code: buf.to_string(),
                code: code.clone(),
                text: cand.text.clone(),
                source: cand.source,
                boundary: cand.boundary,
                learn: cand.meta.learn_code.clone(),
            });
            self.learn_phrase_on_commit_in(state, cand.is_synthesized, overlay)
        };
        // 草稿候选（主输入路才有）：同 `commit_selected`，用过即转正；刚转正的跳过 6b，
        // 否则同一次上屏计数 +2。
        let promoted_draft = overlay.is_none()
            && cand.is_draft
            && self.promote_draft_on_commit(&code, &cand.text, cand.boundary);
        if !cand.is_phrase && !promoted_draft {
            let owner = overlay
                .map(str::to_string)
                .unwrap_or_else(|| self.engine_mgr.active_schema_id());
            self.bump_selected_temp_word(&owner, cand, &code, learned_code.as_deref());
        }
    }

    /// 主输入路顶屏（标点顶屏的两条出口共用）：取高亮候选走 [`Self::learn_on_top_commit`]，
    /// 归属活跃方案。无候选 / 联想态（不顶屏）时什么也不做。
    pub(crate) fn learn_on_main_punct_top_commit(&self, state: &mut State) {
        let Some((_, cand)) = self.highlight_for_top_commit(state) else {
            return;
        };
        let buf = state.input_buffer.clone();
        self.learn_on_top_commit(state, &cand, &buf, None);
    }

    /// 「顶屏文本 + 进模式新组合」收尾（进特殊模式 / 临时拼音 / mix 融合共用），与顶码
    /// `commit_top_text` 同一 `top_commit_mode` 分流：direct_commit 且有新组合 →
    /// `CommitThenDeferComposition`（新组合延迟到触发键 keyup 才开）；pre_confirm → `InsertText`
    /// 聚合。新组合为空时两种模式都直接真提交（对齐顶码无余码分支）。
    ///
    /// `text` 必须是已过简繁转换的出屏文本，调用方一律经 [`Self::take_committed_with_highlight`]
    /// 取，勿自行拼 `prefix + cand.text`。
    pub(crate) fn commit_then_new_composition(&self, text: String, new_comp: String) -> KeyAction {
        if new_comp.is_empty() {
            return KeyAction::InsertText {
                text,
                new_composition: None,
                mode_changed: false,
                chinese_mode: true,
                has_new_composition: false,
            };
        }
        if self.rt().config.input.top_commit_mode == wind_config::TopCommitMode::DirectCommit {
            return KeyAction::CommitThenDeferComposition {
                commit_text: text,
                deferred_composition: new_comp,
                timeout_ms: DEFERRED_COMPOSITION_FALLBACK_MS,
            };
        }
        KeyAction::InsertText {
            text,
            new_composition: Some(new_comp),
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: true,
        }
    }

    /// 含副作用命令顶码：异步执行动作（消费 prefix 整段、无同步上屏文本），余码作为新一轮输入
    /// 缓冲走标准候选刷新 + 新组合。不走 direct_commit 延迟重开（无同步 commit 文本）。
    pub(crate) fn top_commit_command_with_remainder(
        &self,
        state: &mut State,
        cand: &Candidate,
        prefix: &str,
        remainder: &str,
    ) -> KeyAction {
        // 命令 input：nav 命令携完整码 group_code，否则用被顶出的前缀码 prefix（对齐 commit_command）。
        let input = if cand.group_code.is_empty() {
            prefix.to_string()
        } else {
            cand.group_code.clone()
        };
        // 无余码（理论边界）→ 退化为普通命令选中（清组合，异步执行）。
        if remainder.is_empty() {
            self.reset_pinyin_composition(state);
            return self.spawn_command_action(cand, input);
        }
        let src = cand.phrase_template.clone();
        // 余码是新的一次组合，不继承注释翻转（同 `commit_top_text`）。
        state.clear_comment_flips();
        state.input_buffer = remainder.to_string();
        preedit_cursor::keep_cased_tail(&state.input_buffer, &mut state.input_buffer_cased);
        state.input_cursor_pos = state.input_buffer.len(); // 顶码后余码续打，光标在余码末尾
        let _ = self.update_candidates(state); // 余码标准候选刷新
        let preedit = state.preedit.clone();
        self.reset_first_show();
        self.notify_ui_hide(); // 隐藏命令码 UI（余码候选窗随后由 notify_ui_update 重开）
        self.spawn_command(src, input); // 异步执行副作用（Effect 回调 coordinator 锁必须异步）
        self.notify_ui_update(state);
        KeyAction::InsertText {
            text: String::new(), // 空上屏：命令占 prefix，无同步文本
            new_composition: Some(preedit),
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: true,
        }
    }

    /// 在独立线程执行命令源（解析→求值→按序跑动作；type 文本经 push 提交、其余为副作用）。
    pub(crate) fn spawn_command(&self, src: String, input: String) {
        let Some(this) = self.self_weak.get().and_then(std::sync::Weak::upgrade) else {
            warn!("cmdbar: self_weak 未装配，命令跳过");
            return;
        };
        std::thread::spawn(move || {
            this.run_command_candidate(&src, &input);
        });
    }

    /// 把命令产生的文本经 push 管道提交给活动客户端（命令在独立线程执行，走 push 而非 KeyAction）。
    pub(crate) fn push_commit_text(&self, text: &str) {
        self.push_commit_text_as(text, "command");
    }

    /// 同 [`Self::push_commit_text`]，另指定给扩展事件的 `source`。
    pub(crate) fn push_commit_text_as(&self, text: &str, source: &str) {
        if text.is_empty() {
            return;
        }
        // push 路不经 `handle_key_event_policed`，换行改写要在这里各自接一次。
        let text = self.convert_commit_newline(text.to_string());
        let encoded = wind_ipc::codec::encode_commit_text(&text, None, false, true, false);
        self.push_server.push_commit_to_active(&encoded);
        self.hook_commit(&text, source);
    }

    /// 候选词条操作（右键菜单）：调整 Shadow 规则并即时重排重绘。编码与归属方案取自
    /// [`Self::candidate_op_scope`]。删除按候选来源分流（对齐 Go handleCandidateDelete）；
    /// 菜单已按同规则灰显，此处判定是热键路径的防御。
    pub(crate) fn candidate_op(&self, op: CandidateOp, page_local: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            return;
        }
        let (start, end) = self.select_range(&state);
        let idx = start + page_local;
        if idx >= end || idx >= state.candidates.len() {
            return;
        }
        self.candidate_op_in(&mut state, op, idx);
    }

    /// 同 [`Self::candidate_op`]，但按全量绝对下标定位（移动端候选长按菜单）。
    /// 不先翻页再调页内版：那会改掉用户可见的页状态。
    pub(crate) fn candidate_op_at(&self, op: CandidateOp, idx: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if idx >= state.candidates.len() {
            return;
        }
        self.candidate_op_in(&mut state, op, idx);
    }

    /// 词条操作执行体，`idx` 为全量绝对下标（调用方已做边界检查）。
    fn candidate_op_in(&self, state: &mut State, op: CandidateOp, idx: usize) {
        let cand = state.candidates[idx].clone();
        let word = cand.text.clone();

        // 常用/生僻标记落在全局字级覆盖表，不需要词库落点，故必须分派在作用域准入之前，否则
        // overlay / 空码浏览态下菜单给了入口而写端静默返回。判据与菜单侧 `common_char_mark` 同源。
        if matches!(op, CandidateOp::ToggleCommon) {
            self.toggle_common_char(state, &word);
            return;
        }

        // 作用域即准入：拿不到落点就什么都不做，与菜单侧「只给复制」是同一个判据。
        let Some(scope) = self.candidate_op_scope(state) else {
            return;
        };
        let CandidateOpScope {
            schema,
            code,
            raw_code,
            engine_type,
            special,
        } = scope;

        // $SS/$AA 展开成员：顺序/成员由短语定义决定，拒绝一切 shadow/删除双轨漂移。
        if crate::handle_menu::candidate_is_group_member(&cand) {
            return;
        }
        // emoji 扩展候选：一切词条操作拒绝（热键路径的同名守卫）。它 code 恒空，shadow 规则写进去
        // 读端永远命中不了。见 design/emoji-suggestion.md §9。
        if cand.is_emoji_suggestion {
            return;
        }
        // 单字母引导的重复上屏项不是这个码的候选，调位无意义（菜单侧同判据灰掉）。
        if crate::candidate_grid::is_leading_repeat(&cand)
            && matches!(
                op,
                CandidateOp::MoveTop
                    | CandidateOp::MoveUp
                    | CandidateOp::MoveDown
                    | CandidateOp::PinAt(_)
            )
        {
            return;
        }
        // 拼音普通候选放行置顶与固定到第 N 位，前移/后移仍禁；命令候选不受限。前移/后移是相对邻居
        // 的，而拼音的邻居随词频、学词、整句每天在变；置顶与固定名次是用户明说的绝对意图。
        // shadow 排在装配流水线最后，能翻过拼音的硬层级闸门，撤销只能靠「恢复默认」或设置页。
        if matches!(op, CandidateOp::MoveUp | CandidateOp::MoveDown)
            && !cand.is_command
            && matches!(engine_type, Some(wind_engine::EngineType::Pinyin))
        {
            return;
        }
        // 位置一律按完整列表计（分类筛选中 `idx` 只是子集下标），见 `reorder_positions`。
        let pos = self.reorder_positions(state, idx);
        // 已在首位：置顶是冗余规则，直接忽略（菜单已灰显，热键路径 defensive）。
        if matches!(op, CandidateOp::MoveTop) && pos.here == 0 {
            return;
        }
        // 固定到第 N 位：N 是界面名次，折算成读端 position（见 `pin_at_slots`）；已在该位、落不到的
        // 格子、英文引擎一律忽略。判据与菜单 `pin_at_submenu` 同源。
        let pin_at_pos = match op {
            CandidateOp::PinAt(n) => {
                if !pin_at_offered(engine_type) {
                    return;
                }
                match self.pin_at_slots(state, idx).position_for(n) {
                    Some(p) => p,
                    None => return,
                }
            }
            _ => 0,
        };
        if let Some(store) = &self.store {
            // 候选调整按 data_schema_id 归属（拼音族折叠）；Delete 分支仍传原始 schema，
            // 供 delete_candidate_by_source 对用户词/临时词按来源分流（混输）。
            let sh_schema = self.engine_mgr.data_schema_id(&schema);
            // 拼音 / 英文桶里 `*` 是「屏蔽这个词」的组键：缓冲恰为 `*` 时不写规则，免得混进屏蔽组。
            if wind_store::shadow::supports_block(&sh_schema)
                && code == wind_store::shadow::BLOCK_ALL_CODES
            {
                return;
            }
            // 规则键取词库原文（`freq_text`）：读端在大小写投影、模板展开之前比对。
            let word = cand.freq_text().to_string();
            // 稳定 id 必须一并传（码表/拼音静态候选为空 → 落回 word 匹配）：短语的 word 是当次求值
            // 结果，`date` 类短语次日即失配。
            let cand_id = (!cand.id.is_empty()).then_some(cand.id.as_str());
            let r = match op {
                CandidateOp::MoveTop => store.pin_shadow(&sh_schema, &code, &word, cand_id, 0),
                // 前移 / 后移是与邻居交换：邻居自己有固定规则时，把它改写到本条原来的位置，否则两条规则
                // 同位、后写的在前，连续两次后移换不回原序。
                CandidateOp::MoveUp | CandidateOp::MoveDown => {
                    let to = if op == CandidateOp::MoveUp {
                        pos.prev
                    } else {
                        pos.next
                    };
                    let nb = (to != pos.here)
                        .then(|| self.candidate_at_read_rank(state, to))
                        .flatten();
                    let neighbor = nb
                        .as_ref()
                        .map(|c| (c.freq_text(), (!c.id.is_empty()).then_some(c.id.as_str())));
                    store.move_shadow_swapping(
                        &sh_schema, &code, &word, cand_id, to, neighbor, pos.here,
                    )
                }
                // Delete 的短语 / 用户词分支走 `raw_code`（击键域，短语按 `input_buffer` 召回）；系统词落
                // shadow，与读端同取归一码 `code`。英文头部原文候选按对应词库词删，找不到就不删。
                CandidateOp::Delete => match self.delete_target(state, &schema, &cand) {
                    Some(t) => self.delete_candidate_by_source(&schema, &raw_code, &code, &t),
                    None => Ok(()),
                },
                // 只写当前输入码的删除行（键取归一码），只接按文字屏蔽的那类词，与菜单同判据。
                CandidateOp::HideHere => match self.delete_target(state, &schema, &cand) {
                    Some(t) if self.delete_blocks_word(&schema, &t) => {
                        store.delete_shadow(&sh_schema, &code, t.freq_text())
                    }
                    _ => Ok(()),
                },
                CandidateOp::PinAt(_) => {
                    store.pin_shadow(&sh_schema, &code, &word, cand_id, pin_at_pos)
                }
                CandidateOp::Reset => store.remove_shadow_rule(&sh_schema, &code, &word, cand_id),
                // 不可达（上面已 early-return）。不 panic：菜单 id 由 IPC 回传，越界值不该打崩输入法。
                CandidateOp::ToggleCommon => Ok(()),
            };
            if let Err(e) = r {
                warn!("candidate op failed: {}", e);
            }
        }

        // 原地重建：直接辅助码的前缀快照是写库前算的，作废（否则删掉的词仍被当命中项提到首位）。
        state.direct_aux_prev = None;
        // 重建候选（会重新应用 Shadow）并重绘，必须按模式分派：主路径 `update_candidates` 读
        // `input_buffer`，特殊模式下它恒空，走主路径会把候选窗当场清空。
        let grid = Self::grid_snapshot(state);
        if special {
            // 返回值是全码自动上屏意向，刻意丢弃：编码没变，用户只是在调顺序。
            let _ = self.update_special_candidates(state);
        } else if state.active == Some(ModeKind::TempEnglish) {
            // 临英的码在 `temp_english_buffer`，主路径读空的 `input_buffer` 会把候选窗当场清空。
            self.update_temp_english_candidates(state);
        } else {
            self.update_candidates(state);
        }
        // 展开态：别跳回首行、丢掉分类——用户正在第几行调的就留在第几行。
        if let Some(snap) = grid {
            self.grid_restore(state, snap);
        }
        // 删除诊断：「删掉了但同文系统词还在」与「根本没删到」屏幕上同形，要与上面的「删前命中」
        // 并排看才分得开。
        if matches!(op, CandidateOp::Delete | CandidateOp::HideHere) {
            debug!(
                "candidate_op({op:?}): 重建后候选仍在={} text={}",
                state.candidates.iter().any(|c| c.text == word),
                word
            );
        }
        self.notify_ui_update(state);
    }

    /// 右键「删除」按候选来源分流：
    /// - 短语 → `set_phrase_enabled(false)` 软禁用 + 重建短语层。code 优先取 `group_code`，
    ///   text 用原始记录文本 `phrase_template`（display 可能是模板展开后文本）。
    /// - 用户词/临时词 → store 真删；schema 取写归属 id，code 优先取候选自带存储码。两张表
    ///   各删各的：同文双记录时只删一张，剩下那张会继续供出同一条候选。
    /// - 其它（系统码表/拼音）→ shadow 软隐藏（仅该编码下隐藏，单字同样允许）。键取
    ///   `shadow_code`（读端同款归一码），不是击键 `code`。
    fn delete_candidate_by_source(
        &self,
        schema: &str,
        code: &str,
        shadow_code: &str,
        cand: &Candidate,
    ) -> anyhow::Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        if cand.is_phrase {
            let raw = if cand.phrase_template.is_empty() {
                cand.text.as_str()
            } else {
                cand.phrase_template.as_str()
            };
            let pcode = if cand.group_code.is_empty() {
                code
            } else {
                cand.group_code.as_str()
            };
            store.set_phrase_enabled(pcode, raw, false)?;
            debug!(
                "delete_candidate: 分支=短语禁用 code={} text={}",
                pcode, raw
            );
            self.rebuild_phrases();
            return Ok(());
        }
        if self.delete_blocks_word(schema, cand) {
            return if cand.source == CandidateSource::Pinyin {
                self.delete_pinyin_word(store, cand)
            } else {
                self.delete_english_word(store, cand)
            };
        }
        if cand.meta.is_user_dict || cand.meta.is_temp_dict {
            // 混在别的方案里的英文出自共享英文引擎，用户词与临时词在 `english` 桶，不能按宿主方案取桶。
            let sid = if cand.source == CandidateSource::English
                && self.engine_mgr.schema_engine_type(schema).as_deref() != Some("english")
            {
                Some(ENGLISH_SCHEMA.to_string())
            } else {
                self.engine_mgr.write_data_schema_id(schema, cand.source)
            };
            let Some(sid) = sid else {
                debug!("delete_candidate: 无法归因存储方案，跳过 '{}'", cand.text);
                return Ok(());
            };
            // 候选存储码按可信度排序去重：`meta.store_code`（引擎同文合并带过来的真值）→ 候选 `code`
            // → `merged_codes`（composite 去重丢弃那条的码位）→ 输入码兜底。逐个试：同文合并有两层，
            // 记录可能挂在被丢弃的那个码上。
            let mut codes: Vec<&str> = Vec::new();
            for c in [
                cand.meta.store_code.as_deref(),
                (!cand.code.is_empty()).then_some(cand.code.as_str()),
            ]
            .into_iter()
            .flatten()
            .chain(cand.merged_codes.iter().map(String::as_str))
            .chain((!code.is_empty()).then_some(code))
            {
                if !codes.contains(&c) {
                    codes.push(c);
                }
            }
            // 库里存的是词条原文（模板源文本、大小写投影前的形态），按 `text` 删恒 miss。
            let stored_text = cand.freq_text();
            // 删之前先查：redb `remove` 对不存在的 key 静默成功，任一段键错配都表现为「点了没反应」。
            let hit = |user: bool, s: &str, c: &str| -> bool {
                let recs = if user {
                    store.get_user_words(s, c)
                } else {
                    store.get_temp_words(s, c)
                };
                recs.unwrap_or_default()
                    .iter()
                    .any(|r| r.text == stored_text)
            };
            // 两个标记各删各的表：同文双记录时只删一张，剩下那张照样供出同一条候选。
            let mut removed: Vec<String> = Vec::new();
            let mut err: Option<anyhow::Error> = None;
            for (user, flagged) in [
                (true, cand.meta.is_user_dict),
                (false, cand.meta.is_temp_dict),
            ] {
                if !flagged {
                    continue;
                }
                let Some(c) = codes.iter().copied().find(|c| hit(user, &sid, c)) else {
                    continue;
                };
                let r = if user {
                    store.remove_user_word(&sid, c, stored_text)
                } else {
                    store.remove_temp_word(&sid, c, stored_text)
                };
                match r {
                    Ok(()) => {
                        removed.push(format!("{}({c})", if user { "用户词" } else { "临时词" }))
                    }
                    Err(e) => err = Some(e),
                }
            }
            debug!(
                "delete_candidate: 分支=store 真删 schema={} 标记=[user={} temp={}] 候选码={:?}（候选自带={} 输入码={}）text={} 已删={:?}",
                sid,
                cand.meta.is_user_dict,
                cand.meta.is_temp_dict,
                codes,
                cand.code,
                code,
                cand.text,
                removed
            );
            // 一条都没删到 = 用户眼里的「点了没反应」，这是唯一该报警的结局。顺带探出记录实际在哪个桶：
            // 错配来源是 schema 段（混输下三种解析各不相同）与 code 段（双拼、前缀补全时候选码≠存储码）。
            if removed.is_empty() && err.is_none() {
                let dsid = self.engine_mgr.data_schema_id(schema);
                let mut found: Vec<String> = Vec::new();
                for user in [true, false] {
                    for s in [sid.as_str(), dsid.as_str(), schema] {
                        for c in codes.iter().copied() {
                            if hit(user, s, c) {
                                found.push(format!(
                                    "{}/schema={s}/code={c}",
                                    if user { "用户词" } else { "临时词" }
                                ));
                            }
                        }
                    }
                }
                warn!(
                    "delete_candidate: 一条记录都没删到 text={} 试过 schema={} 码={:?}；实际命中={:?}（空=该词根本不在 store，候选来自系统词典/整句合成）",
                    cand.text, sid, codes, found
                );
            }
            return err.map_or(Ok(()), Err);
        }
        // 系统词软隐藏按 data_schema_id 归属（拼音族折叠），键取词库原文，与读端同形。
        let sh = self.engine_mgr.data_schema_id(schema);
        debug!(
            "delete_candidate: 分支=shadow 隐藏 schema={} code={} text={}（该候选未被判为用户词/临时词）",
            sh, shadow_code, cand.text
        );
        store.delete_shadow(&sh, shadow_code, cand.freq_text())
    }

    /// 删除是否按文字屏蔽（菜单文案「屏蔽该词」、并给第二项「在此编码下隐藏」）。
    ///
    /// 用户删的是「这个词」而不是「这个码下的它」（设计 §4a）。拼音只认写进 `pinyin` 桶的拼音
    /// 来源（拼音方案、混输的拼音段）；英文只在英文数据方案（英文方案、临英）里按文字屏蔽；
    /// 混在别处的英文与混输码表段仍按编码隐藏。短语与 emoji 扩展候选不算。写端分流与菜单装配
    /// 共用这一个判据。
    pub(crate) fn delete_blocks_word(&self, schema: &str, cand: &Candidate) -> bool {
        if cand.is_phrase
            || cand.is_emoji_suggestion
            || !wind_store::shadow::is_blockable_word(cand.freq_text())
        {
            return false;
        }
        match cand.source {
            CandidateSource::Pinyin => {
                self.engine_mgr
                    .write_data_schema_id(schema, CandidateSource::Pinyin)
                    .as_deref()
                    == Some(wind_engine::manager::PINYIN_DATA_SCHEMA)
            }
            CandidateSource::English => self.engine_mgr.data_schema_id(schema) == ENGLISH_SCHEMA,
            _ => false,
        }
    }

    /// 右键删除实际作用的候选。英文头部候选（所打原文 / 大小写变形，无来源）没有词库落点：
    /// 在英文数据方案里只认字面就是所打原文、且词库里有字面相同的词那一种，按那个词删；其余一律
    /// `None`，删除不可用（菜单灰掉、热键不执行）。
    ///
    /// 不放宽到忽略大小写：剩下的同码异形词是列表里另一行（打 `Help` 删原文条会屏蔽 `help`），
    /// 变形条更对应不到唯一一个词。其余候选原样返回。
    pub(crate) fn delete_target(
        &self,
        state: &State,
        schema: &str,
        cand: &Candidate,
    ) -> Option<Candidate> {
        if self.engine_mgr.data_schema_id(schema) != ENGLISH_SCHEMA {
            return Some(cand.clone());
        }
        let buf = if state.active == Some(ModeKind::TempEnglish) {
            state.temp_english_buffer.as_str()
        } else {
            crate::preedit_cursor::cased_or_buffer(&state.input_buffer, &state.input_buffer_cased)
        };
        if !crate::english_candidates::is_english_head_candidate(cand, buf) {
            return Some(cand.clone());
        }
        if cand.text != buf {
            return None;
        }
        self.english_exact_dict_word(schema, buf, buf)
            .filter(|w| w.text == buf)
    }

    /// 拼音右键删多字词（设计 §4a）：一律屏蔽这个词（写 `code = *` 的删除行，所有输入串、所有
    /// 来源都不再出现）。是用户词 / 临时词时先真删这段文字在拼音数据方案下所有编码的条目，连同
    /// 词频与联想记录：只屏蔽不删的话，分段上屏会把它再学回来、学过的整句也会被原样拼出来。
    /// 拼音的用户分库编进系统词典，候选上分不出来，按系统词处理（只屏蔽）。
    fn delete_pinyin_word(
        &self,
        store: &wind_store::Store,
        cand: &Candidate,
    ) -> anyhow::Result<()> {
        let ds = wind_engine::manager::PINYIN_DATA_SCHEMA;
        if cand.meta.is_user_dict || cand.meta.is_temp_dict {
            let n = store.purge_word(ds, cand.freq_text())?;
            debug!(
                "delete_candidate: 分支=拼音按文字真删 text={} 删掉 {} 条",
                cand.text, n
            );
            if n == 0 {
                warn!("delete_candidate: 拼音用户词 / 临时词一条都没删到");
            }
        }
        debug!("delete_candidate: 分支=拼音屏蔽这个词 text={}", cand.text);
        store.block_word(ds, cand.freq_text())
    }

    /// 英文右键删 ≥2 字母的词（§8 第 11 条）：屏蔽这个词（`english` 桶 `code = *` 的删除行，键是
    /// 词库原文、不归一大小写）。用户词 / 临时词先真删这段文字在所有编码下的条目、词频与联想，
    /// 理由同 [`Self::delete_pinyin_word`]：只删不屏蔽，自动造词会把它再学回来。
    fn delete_english_word(
        &self,
        store: &wind_store::Store,
        cand: &Candidate,
    ) -> anyhow::Result<()> {
        let text = cand.freq_text();
        if cand.meta.is_user_dict || cand.meta.is_temp_dict {
            let n = store.purge_word(ENGLISH_SCHEMA, text)?;
            debug!(
                "delete_candidate: 分支=英文按文字真删 text={} 删掉 {} 条",
                text, n
            );
            if n == 0 {
                warn!("delete_candidate: 英文用户词 / 临时词一条都没删到");
            }
        }
        debug!("delete_candidate: 分支=英文屏蔽这个词 text={}", text);
        store.block_word(ENGLISH_SCHEMA, text)
    }

    /// 候选词操作热键匹配（对齐 Go matchCandidateActionKey，但 `0` 扩展为第 10 候选）。
    /// 命中返回 1-based 页内序号(1-10)，否则 0。模板值域见 [`wind_config::hotkey::number_template_mods`]。
    ///
    /// 修饰位按「相等」判，不按「包含」判：模板值域里有子集关系（Ctrl ⊂ Ctrl+Alt），包含式判据
    /// 会让宽的劫走窄的，Ctrl+Alt+3 会在宿主快捷键生效的同时把第 3 个候选置顶（TSF 对这类组合
    /// 走 cleanup 通路，键照样发过来）。
    fn match_candidate_action_key(template: &str, modifiers: u32, key_code: u32) -> usize {
        // 0x30..=0x39 = '0'..'9'；'0' 映射为第 10 个候选。
        let num = match key_code {
            0x30 => 10,
            0x31..=0x39 => (key_code - 0x30) as usize,
            _ => return 0,
        };
        let Some(want) = wind_config::hotkey::number_template_mods(template) else {
            return 0; // "none" 或任何非法值 ＝ 未绑定
        };
        // 只比通用修饰位：左右具体位（MOD_LCTRL 等）由 TSF 附带，模板不区分左右。
        if modifiers & wind_config::hotkey::MOD_GENERIC_MASK == want {
            num
        } else {
            0
        }
    }

    /// Ctrl / Ctrl+Shift / Ctrl+Alt ＋数字 置顶/删除当前页候选（对齐 Go handle_key_event 候选热键段）。
    /// 可选值域见 [`wind_config::hotkey::number_template_mods`]。仅中文模式 + 有候选 + 有词库落点
    /// （见 `candidate_op_scope`）生效，命中即消费按键。复用 `candidate_op`。
    pub(crate) fn handle_candidate_action_hotkey(&self, data: &KeyEventData) -> Option<KeyAction> {
        use wind_ipc::protocol::MOD_CTRL;
        // 早退纯属省事：现值域里每一项都带 Ctrl。这是值域约束而非机制约束，真正的裁决在
        // `match_candidate_action_key` 的等值比较里。
        if data.modifiers & MOD_CTRL == 0 {
            return None;
        }
        let h = &self.rt().config.keys;
        // 删除优先匹配（与 Go 一致）。等值判据已保证不同模板互斥，顺序只兜「两项配了同一模板」的情形。
        let del =
            Self::match_candidate_action_key(&h.delete_candidate, data.modifiers, data.key_code);
        let pin = Self::match_candidate_action_key(&h.pin_candidate, data.modifiers, data.key_code);
        let (op, num) = if del > 0 {
            (CandidateOp::Delete, del)
        } else if pin > 0 {
            (CandidateOp::MoveTop, pin)
        } else {
            return None;
        };
        // 门控：中文态 + 有候选 + 有词库落点，落点判定与右键菜单同源（`candidate_op_scope`）。
        // 快捷输入历史候选没有词库落点但右键能删，热键同样走 `quick_history_at` 与同一个分发。
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.chinese_mode
                || state.candidates.is_empty()
                || (self.candidate_op_scope(&state).is_none()
                    && self.quick_history_at(&state, num - 1).is_none())
            {
                // overlay 里没有可操作的落点也吃掉：落到模式处理器的 `overlay_ctrl_alt_guard` 会放弃整段输入。
                // 主输入路照旧落到它自己的 Ctrl/Alt 兜底。
                return state.active.is_some().then_some(KeyAction::Consumed);
            }
        }
        // 分发自行重新加锁并做页范围/来源分流校验。
        self.candidate_or_quick_format_op(op, num - 1);
        Some(KeyAction::Consumed)
    }

    /// 快捷输入专用选词上屏键（`keys.quick_input_select`，出厂 Ctrl+数字）：上屏当前页第 N 个
    /// 候选，`0` 为第 10 个；展开态按序号顺延的范围选（见 `mix_select`）。
    ///
    /// 须先于 [`Self::handle_candidate_action_hotkey`]：出厂与置顶同为 `ctrl+number`，快捷输入里
    /// 归选词。命中即吃，一条候选都没有也吃：落到 mix 处理器的 `overlay_ctrl_alt_guard` 会放弃整段输入。
    ///
    /// 小键盘数字同样认（`numpad_behavior = direct` 时键码仍是 0x60..0x69），不认就落进那道守卫。
    /// 只在 NumLock 开、模板不带 Shift 时到得了这里，否则系统给的是导航键组合，按宿主快捷键处理。
    pub(crate) fn try_quick_input_select(&self, data: &KeyEventData) -> Option<KeyAction> {
        // 只取小键盘数字：`numpad_to_main` 也改写运算符键，那些不是序号。
        let key_code = match crate::key_convert::numpad_to_main(data.key_code) {
            Some((vk, false)) if (keymap::VK_0..=keymap::VK_9).contains(&vk) => vk,
            _ => data.key_code,
        };
        let num = Self::match_candidate_action_key(
            &self.rt().config.keys.quick_input_select,
            data.modifiers,
            key_code,
        );
        if num == 0 {
            return None;
        }
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let state: &mut State = &mut guard;
        if !matches!(state.active, Some(ModeKind::Mix(_))) {
            return None;
        }
        Some(self.mix_select(state, num - 1))
    }

    /// 上屏注释 / 拼音（`input.alt_commit`）：Alt+数字 N 上屏当前页第 N 个候选的拼音或注释，
    /// Alt+空格 取高亮那条。
    ///
    /// 守卫（任一不成立返回 `None`，按键落回原有语义）：功能开着 / 修饰位恰为 Alt / 不是小键盘
    /// 改写来的（`numpad_origin`：小键盘 Alt+数字是 Windows 的 Alt 码输入）/ 该组合没被
    /// `keys.key_actions` 绑走 / 中文态 + 有候选 + 不在快捷加词里。
    ///
    /// 有文本 ⇒ 已确认前缀 + 注释一并上屏，会话结束，余码丢弃，不记词频。没有可上屏的或序号越界
    /// ⇒ 吞键、会话保留；不回落成候选字（用户要的是注释）。
    pub(crate) fn try_alt_commit(
        &self,
        data: &KeyEventData,
        numpad_origin: bool,
    ) -> Option<KeyAction> {
        use wind_config::config::AltCommit;
        use wind_ipc::protocol::MOD_ALT;
        if numpad_origin || data.modifiers & wind_config::hotkey::MOD_GENERIC_MASK != MOD_ALT {
            return None;
        }
        // 生效档：注释总开关关着时 `comment` 档按 `off` 处理（见 `AltCommit::effective`）。
        let kind = AltCommit::effective(&self.rt().config);
        if kind == AltCommit::Off {
            return None;
        }
        let page_local = match data.key_code {
            keymap::VK_SPACE => None,
            0x30 => Some(9),
            k @ 0x31..=0x39 => Some((k - 0x31) as usize),
            _ => return None,
        };
        // 用户绑定让位：用户的动作在上游已分派，它因自身守卫没接时也不该被本功能顺手接走。
        let hash = (MOD_ALT << 16) | data.key_code;
        if self
            .rt()
            .compiled_hotkeys
            .match_key_down(hash)
            .is_some_and(|a| !a.is_empty())
        {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.chinese_mode || state.candidates.is_empty() || state.add_word_active {
            return None;
        }
        let idx = match page_local {
            None => self
                .highlighted_global_index(&state)
                .min(state.candidates.len() - 1),
            Some(n) => {
                let (start, end) = self.select_range(&state);
                if start + n >= end {
                    return Some(KeyAction::Consumed);
                }
                start + n
            }
        };
        let cand = state.candidates[idx].clone();
        let text = self.alt_commit_text(&state, &cand, kind);
        if text.is_empty() {
            debug!("alt_commit: 候选 #{idx} 没有可上屏的内容（{kind:?}），吞键");
            return Some(KeyAction::Consumed);
        }
        Some(self.commit_text_ending_session(&mut state, &text))
    }

    /// 上屏一段不是候选本身的文本并结束会话：已确认前缀连同 `text` 一并上屏，组合与候选窗清掉
    /// （同 Esc 的退出路径），未消费的余码丢弃。不记词频、不进联想。
    /// 调用方：上屏注释 / 拼音（Alt+数字）、悬停提示右键「上屏」。
    pub(crate) fn commit_text_ending_session(&self, state: &mut State, text: &str) -> KeyAction {
        let prefix = self.take_committed(state);
        let mut out = self.maybe_convert(state, &prefix);
        out.push_str(text);
        let _ = self.cancel_session(state);
        Self::commit_action(out, true)
    }

    /// 点击选词：提交页内第 N 个候选，经 push 管道异步上屏（对齐 Go PushCommitText）。
    ///
    /// 主输入路复用键盘选词的 [`Self::commit_selected`]，返回的 KeyAction 经
    /// [`Self::push_no_key_ctx_action`] 翻译成 push 消息，分步提交与数字键完全一致。
    /// overlay 模式走各模式自己的键盘出口（派发见 [`Self::select_candidate_at`]）；只有网址 /
    /// 邮箱 / Unicode（键盘侧不按序号选词）走「整串提交 + 彻底复位」。
    pub(crate) fn mouse_select(&self, page_local: usize) {
        let _ = self.mouse_select_action(page_local);
    }

    /// [`Self::mouse_select`] 的实现，返回实际推送的 KeyAction 供测试断言（`$CC` 命令 / 组折叠 /
    /// 越界返回 None）。页范围校验在此：桌面候选窗只画当前页，点到页外即坐标算错，必须拒绝。
    /// 移动端不是这样（见 [`Self::select_candidate_at`]）。
    fn mouse_select_action(&self, page_local: usize) -> Option<KeyAction> {
        // 展开态下 UI 回传的是网格平铺下标，先切到那一格再按页内下标选。
        // 点的是展开态的分类标签（tag 段见 `GRID_TAB_TAG_BASE`）：切分类，不选词。
        if let Some(tab) = self.grid_tab_of_event(page_local) {
            self.grid_click_tab(tab);
            return None;
        }
        // 切高亮时即重绘：选词可能不上屏（`$CC` 命令、组折叠），那时屏幕上要看到高亮已到这一格。
        let page_local = self.resolve_event_index(page_local, true)?;
        let (start, end) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.page_range(&state)
        };
        let idx = start + page_local;
        if idx >= end {
            return None;
        }
        self.select_candidate_at(idx)
    }

    /// `$CC` 命令候选的执行输入（无 `group_code` 时）：当前模式自己的编码缓冲。
    ///
    /// 必须与各模式键盘出口传给 [`Self::overlay_commit_command`] 的 `code` 同源，否则同一条命令
    /// 键盘选与鼠标点拿到的输入不同（overlay 下主路 `input_buffer` 恒空）。
    pub(crate) fn command_input_code<'a>(&self, state: &'a State) -> &'a str {
        match state.active {
            Some(ModeKind::TempEnglish) => &state.temp_english_buffer,
            Some(ModeKind::TempPinyin) => &state.temp_pinyin_buffer,
            Some(ModeKind::Mix(_)) => &state.mix_buffer,
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::Reverse) => {
                &state.special_buffer
            }
            Some(ModeKind::AuxCode) => self.aux_code_source_buffer(state),
            None | Some(ModeKind::Url | ModeKind::Email | ModeKind::Unicode) => &state.input_buffer,
        }
    }

    /// 按绝对下标选词：`mouse_select` 与移动端滚动候选栏的共同内核。
    ///
    /// 移动端候选栏是可滚动的长列表，没有「页」，桌面的选词入口却都以页内下标表达，合成数字键
    /// 永远选不到第 10 个以后。分页仍决定空格上屏的目标与数字键的语义，这里只是把「选哪一个」
    /// 从视图坐标里解放出来。
    ///
    /// 返回实际产生的 KeyAction（仅 `$CC` 命令、组折叠、越界返回 None）。
    pub(crate) fn select_candidate_at(&self, idx: usize) -> Option<KeyAction> {
        self.select_candidate_from(idx, crate::session_exit::ActionOrigin::Pointer)
    }

    /// 同 [`Self::select_candidate_at`]，按 `origin` 收尾（移动端候选栏是 `MobilePick`）。
    pub(crate) fn select_candidate_from(
        &self,
        idx: usize,
        origin: crate::session_exit::ActionOrigin,
    ) -> Option<KeyAction> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 联想候选就在 `candidates` 里，点选联想词与点选普通候选走同一条 `commit_selected`。
        if idx >= state.candidates.len() {
            return None;
        }
        // 非按键选词 ⇒ 解除智能符号待命态。这条通路不经按键出口，「非同键按键解除武装」看不见它，
        // 而选词会真往文档写字、清空缓冲，`smart_symbol_press2` 的「缓冲非空」判据也随之放行，
        // 再按同一标点会把刚上屏词的末字换掉。放在越界检查之后：点到页外什么都没发生。
        // 本行目前被其他解除点遮蔽、没有独立会红的用例，但它是「非按键写文档」唯一的解除点。
        self.disarm_smart_symbol();
        // 统计用的位置相对当前页首，与数字键的 `num-1` 同一量纲。移动端不翻页，页首恒为 0，
        // 于是如实记成绝对位次。
        let page_local = idx.saturating_sub(self.page_range(&state).0);
        // 主输入路（含辅助码）的判据，下方两处共用。
        let main_path = state.active.is_none() || state.active == Some(ModeKind::AuxCode);
        // 纯文本命令走下方 `commit_selected`，不进本分支：那里同步上屏，宿主侧仍是「组合区活跃时
        // 提交」；本分支先 ClearComposition 再裸插入，换行语义会变（见 [`Self::commit_command`]）。
        // overlay 仍按 `overlay_commit_command` 语义异步执行。求值输入与命令执行输入同源
        // （`command_input_code`），否则辅助码从临拼来时依赖 `code` 的纯文本命令会被误判成动作。
        let cmd_commits_text = main_path
            && self.command_commits_text(&state.candidates[idx], {
                let gc = &state.candidates[idx].group_code;
                if gc.is_empty() {
                    self.command_input_code(&state)
                } else {
                    gc
                }
            });
        // $CC 命令候选：执行动作而非上屏 display 标签（释放锁后异步执行，避免重入死锁）。
        if state.candidates[idx].is_command && !cmd_commits_text {
            let src = state.candidates[idx].phrase_template.clone();
            // 命令 nav 携完整码 group_code 作执行输入；精确码命令用当前缓冲。
            let gc = state.candidates[idx].group_code.clone();
            let input = if gc.is_empty() {
                self.command_input_code(&state).to_string()
            } else {
                gc
            };
            // 与键盘路径同样先复位组合再执行（`spawn_command_action` 不做状态重置）：否则陈旧候选会被
            // 下一次 `notify_ui_update` 推回屏幕，宿主里的命令码也没人收回。复位走 `cancel_session`
            // （按 `active` 分派各模式的退出函数），与 overlay 键盘路径的 `exit_*` 对齐。返回值丢弃：
            // 鼠标点击没有按键应答可承载它，下方手工推 `encode_clear_composition` 就是它的等价物。
            let _ = self.cancel_session(&mut state);
            drop(state);
            // 补上键盘路径由 KeyAction 承担的那一半：让宿主结束 composition。
            self.push_server
                .push_commit_to_active(&wind_ipc::codec::encode_clear_composition());
            self.spawn_command(src, input);
            return None;
        }
        // 主输入路：与数字键同一条提交路径（is_group 的二级选择亦由其内部处理）。辅助码并入本条：
        // 它的候选就是主路径候选，走 overlay 分支会直接清空缓冲，分步组句的剩余拼音会被丢掉。
        if main_path {
            let cand = state.candidates[idx].clone();
            let chinese_mode = state.chinese_mode;
            // 鼠标页内位置 = page_local（与数字键的 num-1 同义）。辅助码走 `aux_code_committed`
            // （三条选词路径的唯一收尾）：部分消费时留在模式内重建会话继续筛。
            let act = if state.active == Some(ModeKind::AuxCode) {
                self.aux_code_committed(&mut state, cand, page_local as i32)
            } else {
                self.commit_selected(&mut state, &cand, page_local as i32)
            };
            // 点选不经按键出口，收尾（补空格等）在推送之前做，见 `crate::session_exit`。
            let act = self.finish_session_action(state, origin, act);
            // commit_selected 已按分支自行 notify_ui_update / notify_ui_hide，此处不再重复。
            self.push_no_key_ctx_action(&act, chinese_mode, true);
            return Some(act);
        }
        // ── 以下为 overlay 模式（active != None）路径 ──
        // 与键盘选词走同一个出口，与 `select_page_candidate` 的派发逐臂对应（只是按绝对下标）：
        // 临英补空格与词频、临拼分步转换与造词、快捷输入按透镜分步 / 上屏、快符 / 生僻字的全码策略。
        // 返回值照主路一样带出来（分步时是 `UpdateComposition`），`debug_mouse_select` 由此可观测。
        // 派发写成穷尽 `match`、不留通用兜底：通用出口对任何 overlay 都是错的，新增模式时
        // 让编译器逼着在这里选出口。
        let page_local_pos = page_local as i32;
        let act = match state.active {
            Some(ModeKind::TempEnglish) => self.commit_temp_english_selected(&mut state, idx),
            Some(ModeKind::TempPinyin) => {
                let cand = state.candidates[idx].clone();
                self.commit_temp_pinyin_selected(&mut state, &cand, page_local_pos)
            }
            Some(ModeKind::Mix(_)) => self.mix_select_at(&mut state, idx, page_local_pos),
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::Reverse) => {
                self.commit_special_candidate(&mut state, idx)
            }
            // 网址 / 邮箱：鼠标点选须与空格走同一个出口（统计、学后缀 / 网址历史）。出口按高亮取候选，
            // 故先把高亮移到被点的那条（移动端 `idx` 可越出当页，页码一并换算）。
            Some(ModeKind::Url) | Some(ModeKind::Email) => {
                let pp = self.page_size(&state).max(1);
                state.current_page = idx / pp;
                state.selected_index = idx % pp;
                if state.active == Some(ModeKind::Url) {
                    self.commit_url(&mut state, true)
                } else {
                    self.commit_email(&mut state, true)
                }
            }
            // Unicode 恒至多一条候选，出口取首条即被点的那条。
            Some(ModeKind::Unicode) => self.commit_unicode(&mut state),
            // 主输入路（含辅助码）已在上方 `main_path` 分支返回。保守兜底：什么都不做，调试构建下暴露。
            None | Some(ModeKind::AuxCode) => {
                debug_assert!(
                    false,
                    "select_candidate_at: 主输入路应已在 main_path 分支返回"
                );
                return None;
            }
        };
        let chinese_mode = state.chinese_mode;
        let act = self.finish_session_action(state, origin, act);
        match &act {
            // 空文本上屏：只需让宿主结束 composition（同 $CC 命令分支）。
            KeyAction::ClearComposition => {
                self.push_server
                    .push_commit_to_active(&wind_ipc::codec::encode_clear_composition());
            }
            _ => self.push_no_key_ctx_action(&act, chinese_mode, false),
        }
        Some(act)
    }

    /// 鼠标点选页内第 N 个候选（测试/诊断用）：返回实际推送的 KeyAction
    /// （`UpdateComposition` = 分步提交，组合区留活；`InsertText` = 整串上屏）。
    pub fn debug_mouse_select(&self, page_local: usize) -> Option<KeyAction> {
        self.mouse_select_action(page_local)
    }

    /// 无按键上下文的 KeyAction → push 管道消息。
    ///
    /// 鼠标点击与 CapsLock 全局钩子（`handle_capslock_hook_press`）都不在 TSF 按键应答里，
    /// 只能自行编码经 push 管道定向投递（`push_commit_to_active`，非广播）。覆盖 `commit_selected`
    /// 的两种返回：
    /// - `UpdateComposition`（分步提交 / 二级选择）→ `CMD_UPDATE_COMPOSITION`，组合区留活；
    /// - `InsertText`（整串提交）→ `CMD_COMMIT_TEXT`。
    ///
    /// 其余 KeyAction（含 `ClearComposition`）被静默丢弃，宿主组合区会一直挂着。`select_candidate_at`
    /// 的 `$CC` 分支在调用点手工补了 `encode_clear_composition`；再有调用方需要它时应在此补一臂，
    /// 而不是在调用点再做特判（如 `handle_addword.rs` 的鼠标点选接进来时）。
    ///
    /// `main_path`：来自主输入路。组合区里的音节分隔符要照 `ui.candidate.syllable_separator` 换掉
    /// （键盘路径在 `handle_key_event_policed` 收口里换，这条通路不经过它）。
    pub(crate) fn push_no_key_ctx_action(
        &self,
        act: &KeyAction,
        chinese_mode: bool,
        main_path: bool,
    ) {
        let act = match self.syllable_separator_for(main_path) {
            Some(sep) => act.clone().with_syllable_separator(&sep),
            None => act.clone(),
        };
        match &act {
            KeyAction::UpdateComposition { text, caret_pos } => {
                let encoded = wind_ipc::codec::encode_update_composition(text, *caret_pos);
                self.push_server.push_commit_to_active(&encoded);
                debug!("push_no_key_ctx: 分步提交，组合区留活 preedit='{}'", text);
            }
            KeyAction::InsertText { text, .. } => {
                // 同 push_commit_text：push 路不经按键收口，换行改写要在这里接。组合区那一臂推的是编码
                // 不是正文，不参与。
                let text = self.convert_commit_newline(text.clone());
                let encoded =
                    wind_ipc::codec::encode_commit_text(&text, None, false, chinese_mode, false);
                self.push_server.push_commit_to_active(&encoded);
                self.hook_commit(&text, "candidate");
                debug!("push_no_key_ctx: committed '{}'", text);
            }
            other => {
                debug!("push_no_key_ctx: 无需推送的 KeyAction {:?}", other);
            }
        }
    }

    /// 上屏动作的常用构造。它不是唯一出口（另有约 10 处直接构造 `InsertText`），故自提交打点与
    /// 自动造词投喂不在这里做，统一在 `handle_key_event_policed` 按最终返回的 action 处理。
    pub(crate) fn commit_action(text: String, chinese_mode: bool) -> KeyAction {
        KeyAction::InsertText {
            text,
            new_composition: None,
            mode_changed: false,
            chinese_mode,
            has_new_composition: false,
        }
    }
}

/// [`Coordinator::english_appends_space`] 的「这条候选算不算英文内容」一半（不含开关）。
///
/// 三个判据的理由见该方法文档：英文词库来源 / 与所打原码逐字节相等 / 无来源的头部候选
/// 且文本是所打原码的某种大小写形态。
pub(crate) fn english_text_counts_as_input(
    source: CandidateSource,
    text: &str,
    input: &str,
) -> bool {
    if source == CandidateSource::English {
        return true;
    }
    if input.is_empty() {
        return false;
    }
    text == input
        || (source == CandidateSource::None
            && crate::key_convert::en_case_variants(input)
                .iter()
                .any(|v| v == text))
}

#[cfg(test)]
mod english_text_counts_as_input_tests {
    use super::english_text_counts_as_input;
    use wind_candidate::CandidateSource as S;

    /// 头部候选的大小写变形算原码。
    #[test]
    fn head_case_variants_count() {
        assert!(english_text_counts_as_input(S::None, "hel", "hel"));
        assert!(english_text_counts_as_input(S::None, "Hel", "hel"));
        assert!(english_text_counts_as_input(S::None, "HEL", "hel"));
        assert!(english_text_counts_as_input(S::None, "hel", "Hel"));
    }

    /// 反例：缩写类短语（打 `usa` 选 `USA`）文本恰好是全大写形，但来源是短语，不算。
    /// 没有这条，把判据简化成「只看文本」也不会有任何测试变红。
    #[test]
    fn phrase_with_case_variant_text_does_not_count() {
        assert!(!english_text_counts_as_input(S::Phrase, "USA", "usa"));
        assert!(!english_text_counts_as_input(S::Assoc, "Hel", "hel"));
    }

    /// 非大小写形态（混合大小写、别的词）不算；空输入一律不算。
    #[test]
    fn other_forms_do_not_count() {
        assert!(!english_text_counts_as_input(S::None, "hEl", "hel"));
        assert!(!english_text_counts_as_input(S::None, "help", "hel"));
        assert!(!english_text_counts_as_input(S::None, "", ""));
    }
}

#[cfg(test)]
mod auto_commit_min_len_tests {
    //! 最短码长归一：须与引擎 `CodeTableEngine::new` 的同名归一保持一致。
    use super::resolve_auto_commit_min_len;

    #[test]
    fn zero_follows_max_code_length() {
        // 0 = 跟随全码长（五笔 4 码）。
        assert_eq!(resolve_auto_commit_min_len(0, 4), 4);
    }

    #[test]
    fn explicit_value_wins_over_max_code_length() {
        assert_eq!(resolve_auto_commit_min_len(2, 4), 2);
        assert_eq!(resolve_auto_commit_min_len(6, 4), 6);
    }

    #[test]
    fn no_max_code_length_disables_gate() {
        // 拼音等引擎 max_code_length()=0 → 门槛 0 → 调用方 `len < 0` 恒假 → 不设闸。
        assert_eq!(resolve_auto_commit_min_len(0, 0), 0);
    }
}

#[cfg(test)]
#[path = "handle_candidate_sole_non_emoji_tests.rs"]
mod sole_non_emoji_tests;

#[cfg(test)]
#[path = "handle_candidate_plan_emoji_tests.rs"]
mod plan_emoji_tests;

#[cfg(test)]
#[path = "handle_candidate_emoji_tail_tests.rs"]
mod emoji_tail_tests;

#[cfg(test)]
#[path = "handle_candidate_finalize_candidates_tests.rs"]
mod finalize_candidates_tests;

/// 高亮候选自带的编码栏切分（`CandidateMeta::preedit_split`）。只认拼音来源；这道过滤目前是
/// 保险（引擎本就不给非拼音候选切分），防日后别的来源带上这个字段。
fn own_preedit_split(cand: Option<&Candidate>) -> Option<&str> {
    cand.filter(|c| c.source == wind_candidate::CandidateSource::Pinyin)?
        .meta
        .preedit_split
        .as_deref()
}

/// 组合区是否该显示码表的编码单元切分（整句 `aawt'aawt` / 逆切分 `hf'kn`）。
///
/// 两条都要：有切分串，且当前高亮的就是那条切分候选（翻到别的候选时留着切法比不切更糊涂）。
/// 整句与逆切分共用同一个出口（`ConvertResult::preedit_codetable`）且闸门互斥，同一次转换
/// 只会有一个非空。写成自由函数是为了不构造整个 Coordinator 就能测。
fn wants_codetable_split(body: &str, cand: Option<&Candidate>) -> bool {
    !body.is_empty()
        && cand.is_some_and(|c| {
            (c.is_sentence || c.is_split_composed) && c.source == CandidateSource::CodeTable
        })
}

#[cfg(test)]
#[path = "handle_candidate_clear_recheck_tests.rs"]
mod clear_recheck_tests;

#[cfg(test)]
#[path = "handle_candidate_dynamic_candidate_shadow_tests.rs"]
mod dynamic_candidate_shadow_tests;

#[cfg(test)]
#[path = "handle_candidate_english_placement_tests.rs"]
mod english_placement_tests;

#[cfg(test)]
#[path = "handle_candidate_sentence_block_tests.rs"]
mod sentence_block_tests;

#[cfg(test)]
#[path = "handle_candidate_sentence_cycle_tests.rs"]
mod sentence_cycle_tests;

#[cfg(test)]
#[path = "handle_candidate_mouse_command_overlay_tests.rs"]
mod mouse_command_overlay_tests;

#[cfg(test)]
#[path = "handle_candidate_select_char_freq_code_tests.rs"]
mod select_char_freq_code_tests;

#[cfg(test)]
#[path = "handle_candidate_expand_stop_tests.rs"]
mod expand_stop_tests;

/// 拼音拆分串（`fang'xiang`）里，跳过前 `letters` 个字母（及随后的隔音符）之后剩下的部分。
/// `tail_after_letters("fang'xiang", 4)` = `xiang`；字母数用尽 → 空串。
pub(crate) fn tail_after_letters(body: &str, letters: usize) -> String {
    let mut seen = 0usize;
    for (i, ch) in body.char_indices() {
        if seen >= letters {
            return body[i..].trim_start_matches('\'').to_string();
        }
        if ch != '\'' {
            seen += 1;
        }
    }
    String::new()
}

static CURSOR_BY_SYLLABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 打开 / 关闭「拼音按音节移光标、候选只查光标前一段」。默认关；电脑端的配置项接入时调用。
pub fn set_cursor_by_syllable(on: bool) {
    CURSOR_BY_SYLLABLE.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn cursor_by_syllable() -> bool {
    CURSOR_BY_SYLLABLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`Coordinator::query_cut`] 的判据（纯函数，便于测试）：只有开关打开（`cursor_by_syllable`）、
/// 光标严格落在编码中间且在字符边界、普通拼音输入时才截断。
pub(crate) fn cut_position(
    mobile: bool,
    buffer: &str,
    cursor: usize,
    plain_pinyin: bool,
) -> Option<usize> {
    (mobile
        && plain_pinyin
        && cursor > 0
        && cursor < buffer.len()
        && buffer.is_char_boundary(cursor))
    .then_some(cursor)
}

#[cfg(test)]
#[path = "handle_candidate_mobile_limit_tests.rs"]
mod mobile_limit_tests;

#[cfg(test)]
mod pin_at_offered_tests {
    use super::pin_at_offered;
    use wind_engine::EngineType;

    /// 按引擎类型判：英文类方案无论 id 叫什么都不给，其余都给。
    #[test]
    fn only_english_engine_is_excluded() {
        assert!(!pin_at_offered(Some(EngineType::English)));
        for t in [EngineType::Pinyin, EngineType::CodeTable, EngineType::Mixed] {
            assert!(pin_at_offered(Some(t)), "{t:?}");
        }
        assert!(pin_at_offered(None));
    }
}
