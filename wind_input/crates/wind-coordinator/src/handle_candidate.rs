//! 候选处理：生成 / 过滤 / shadow / 词频重排 / 翻页导航 / 选词上屏 / 右键操作。
//!
//! 从 coordinator.rs 拆出（同 crate 内 `impl Coordinator` 块，组织性重构，无逻辑变更）。

use crate::coordinator::{
    Coordinator, DEFERRED_COMPOSITION_FALLBACK_MS, InputOutcome, LEARN_ADD_WEIGHT, State,
    now_unix_secs, punct_char,
};
use crate::pipeline::ModeKind;
use crate::preedit_cursor;
use crate::short_code_yield;
use tracing::{debug, warn};
use wind_bridge::handler::{KeyAction, KeyEventData};
use wind_candidate::{Candidate, CandidateMeta, CandidateSource};
use wind_engine::manager::ENGLISH_SCHEMA;
use wind_ipc::protocol::MOD_SHIFT;
use wind_keys::keymap;
use wind_store::freq::FreqRecord;
use wind_ui_types::CandidateOp;

/// 候选词条操作（置顶 / 前移 / 后移 / 删除 / 恢复默认）的作用域快照。
/// 由 [`Coordinator::candidate_op_scope`] 解析，菜单构建、写端、macOS 禁用位三处共用。
pub(crate) struct CandidateOpScope {
    /// 词库 / shadow 归属方案 id（**原始 id，未经 `data_schema_id` 折叠**——Delete 分支
    /// 还要靠它走 `write_data_schema_id` 的按来源分流）。
    pub schema: String,
    /// **候选调整（shadow）专用码**：主输入路取归一形态（双拼 → 全拼码，见
    /// `shadow_code_of`），特殊模式 = `special_buffer`。只喂 shadow 三件套
    /// （`pin_shadow` / `shadow_has_rule` / `clear_shadow`）。
    pub code: String,
    /// **击键原码**：恒等于该模式的编码缓冲，不做任何归一。
    ///
    /// 与 `code` 分开，是因为二者服务的键空间不同，而双拼下它们**取值不同**：
    /// 短语按击键召回（`phrases.lookup(&state.input_buffer, ..)`），其主键就是击键串，
    /// 拿归一码去 `set_phrase_enabled` 会写到一个永远读不到的键上——短语删除静默失效。
    /// 全拼/码表下两者恒等，这条差异只在双拼激活时才显形，所以极易漏审。
    pub raw_code: String,
    /// 出这批候选的引擎类型（调位判据用）；引擎未加载时为 None。
    pub engine_type: Option<wind_engine::EngineType>,
    /// 特殊模式标记：重建候选须走 `update_special_candidates`——主路径的 `update_candidates`
    /// 读 `input_buffer`，在此模式下恒空，会把候选列表整个清掉。
    pub special: bool,
}

/// 候选统一层级排序（合并引擎候选 + 短语后的呈现序，与所选 `base_sort` 模式**同维度**）：
/// ① 非模糊优先于模糊；② 精确优先于前缀补全（`is_prefix`）；③ 完整匹配优先于子短语（`is_partial`）；
/// ④ 同层内编码精确匹配优先（`is_exact_code`）；⑤ 按权重降序（`ignore_weight` 时跳过）；
/// ⑥ 词库基序（`base_order`）升序；⑦ 自然序升序。
///
/// - `is_exact_code` 不可少：码表引擎已把「精确匹配优先」排好，但本函数会**无条件重排全部候选**，
///   若不复刻该键，引擎结果会在这里被按纯权重推翻——码表词组权重取自词频、单字取自字频，量纲
///   不可比，「新的」(usrq, 47487) 会压过简码「新」(usr, 11777)。须置于 `cmp_match_layers` 之后：
///   精确优先只在同匹配层内生效，前缀短语（`is_prefix=true`）仍留在其下层不受提拔——
///   哪怕它权重更高。
///
/// - `is_partial` 不可少：混输 ÷100 压缩权重后，高权重子串单字（平 w=58 is_partial=true）会靠
///   weight 反超低权重精确词组（平摊 w=4 is_partial=false）；且须在 `is_prefix` 之后（对齐 PinyinEngine）。
/// - `base_order` 不可少：与引擎 `candidate::better`/`by_natural` 一致——`natural_order` 是**每库局部
///   出现序**（各库从 0 起），只能在同 `base_order` 档内当 tiebreaker；跨库直接比会让小库靠前词条
///   （如一简次选库「有时」no=24）反超主库深处词条（如「一」no=57285）。`base_order` 隔离这种跨库
///   比较，必须排在 `natural_order` 之前（对齐引擎 weight→base_order→natural_order 分层）。
/// - `ignore_weight`：`base_sort = "natural"` 时为 true——引擎的 `by_natural` **完全忽略权重**，纯按
///   base_order→natural_order 呈现；协调器须同样跳过 weight 维度，否则合并短语后重排会与引擎发散
///   （如 natural 模式下高权重次选库条目仍会靠 weight 反超低权重主库条目）。此时短语仍靠其
///   base_order/natural_order 默认 0 浮于顶部。
///
/// - `consumed_length` 末级降序：对齐 `candidate::better`(candidate.rs) 的同名末级。紧随其后的
///   去重（按 `text` 保留排序后第一条）用的就是本函数的结果——若同文候选的消费长度不同而此级
///   缺失，留谁将由一个不含该字段的键随机决定，留下「消费整串」那条会让分段上屏把剩余拼音
///   一并吃掉。该级在 `better` 里早已存在，此前漏抄到本函数。
///
/// - `input`：本次输入的原始码串。取长度供档位判「消费整串」（字节长度与 `consumed_length`
///   同域，输入缓冲恒为 ASCII）；取全串供 `source_tier` 判「码 == 输入」（档 0）。
///   ⚠️ 「消费整串」不可省成 `!is_partial`：Viterbi 整句只解释部分输入时 `is_partial` 仍是
///   false（`aaw` → 「啊啊」只消费 2/3 键），会被误提档并抢走首位。
/// - `mixed`：当前是混输引擎时，在 `is_exact_code` 之后、权重之前插入**跨来源档位**
///   （[`wind_candidate::source_tier`]），档序为「码表精确/精确码短语 → 拼音精确 →
///   码表前缀补全 → 前缀短语 → 拼音其余/英文」。解决混输打 `xu` 时拼音「需」被码表 `xu*` 的
///   124 条前缀补全压到第 125 位。
///   ⚠️ **必须由调用方按引擎语境传入、不可恒 true**：纯拼音下全体候选同为 `Pinyin` 来源，档位会
///   退化成「`is_common` 优先」（拼音精确档 vs 拼音其余档），把含生僻字的多字词硬降到全部
///   常用单字之后。依赖 `mark_common` 已在排序前跑过（`is_common` 是拼音精确档的准入条件）。
///
/// 排序规则：Exact >> Sub-phrase >> Prefix >> Fuzzy。
pub(crate) fn candidate_display_order(
    a: &Candidate,
    b: &Candidate,
    ignore_weight: bool,
    mixed: bool,
    input: &str,
) -> std::cmp::Ordering {
    // `source_tier` 的档 0 判据是 `c.code == input`（完整串比较），故本函数收 `&str` 而非长度。
    let input_len = input.len();
    let by_weight = if ignore_weight {
        std::cmp::Ordering::Equal
    } else {
        b.weight.cmp(&a.weight)
    };
    // 混输专属层级：**跨来源档位**（`source_tier`，跨来源先后的唯一真相源）。
    //
    // 此前这里只用 `cmp_pinyin_exact_first`，即整个档位体系里只承认「是不是拼音精确档」
    // 一个二分。其余档次（码表精确 / 码表前缀 / 拼音其余）当时由混输引擎的权重加成表达
    // （`PHRASE_WEIGHT_BOOST` 等），于是同一语义分散在两处、且已经不一致
    // （混输加成给短语一律 +1M 不分精确/前缀，`source_tier` 把前缀短语单独降档）。
    //
    // 纯拼音/纯码表下必须为空操作——纯拼音时全体候选同源，档位会退化成「is_common 优先」
    // （拼音精确档 vs 拼音其余档），把含生僻字的多字词硬降到全部常用单字之后。
    let by_source_tier = if mixed {
        wind_candidate::source_tier(a, input).cmp(&wind_candidate::source_tier(b, input))
    } else {
        std::cmp::Ordering::Equal
    };
    // 消费输入长度是**首要**键，对齐 librime：其候选容器
    // `DictEntryCollector = map<size_t, DictEntryIterator>` 以「消费的输入长度」为 key，
    // `phrase_->rbegin()` 从最长开始遍历 ⇒ 消费更多输入者恒优先，先于词频、先于任何层级。
    //
    // 这个键此前排在链尾，前面六级早已分出胜负 ⇒ **等价于从未生效**。`buzhidaok` 的残码
    // `k` 被整串忽略正由此而来（「不知道看什么」原在第 136 位）。
    //
    // ⚠️ `consumed_length == 0` 是「引擎未标注 ⇒ 按整串算」的全仓约定（**码表候选恒为 0**），
    // 不归一化就直接降序会把码表候选整体甩到最后。
    //
    // ## ⚠️ 层级键必须原样复用 `cmp_match_layers`，不要在此另写一份
    //
    // 曾在这里写过一份「同构但忽略 `is_promoted_completion`」的副本，动机是：该标志本是
    // 引擎侧用来让高价值补全活过 `truncate` 的，协调器不截断、留着只会把 w=0 的冷僻补全
    // 提到高频词前面。**动机成立，但代价被漏算了**——层级键是布尔的，等价于惩罚 ∞，于是
    // 引擎侧一切「用 weight 表达的让位」在协调器全部失效：step 6.5b 把整句压到
    // `补全 weight - 1` 让位给「恰好用完残码的补全」，到这里因为补全停在 `is_prefix` 层而
    // 被整句反超，`nihaom` 首选从「你好吗」变成「你好们」、`beijingd` 变成「背景的」。
    //
    // 当初那个动机本身也已消失：`zhonghuar` 的「种花人」(w=0) 能登顶是因为当时没有候选
    // 消费到第 9 字节，残码补全整句（step 2c）落地后它自然被压下去。
    //
    // ⚠️ 本键与 `cmp_match_layers` 必须成对出现（判据抽在 `wind_candidate::cmp_by_consumed`，
    // 那里记着「只有一处带上本键」时长词在词频表里进出会导致候选忽隐忽现的事故）。
    let by_consumed = wind_candidate::cmp_by_consumed(a, b, input_len);

    by_consumed
        .then_with(|| wind_candidate::cmp_match_layers(a, b))
        // 音节数对齐者优先（`zaim` 先给 2 音节的「在吗/再买」，3 音节的「在美国」排其后）。
        // 置于层级之后、权重之前：层内分档，不跨层提拔。见 `cmp_completion_extra`。
        .then_with(|| wind_candidate::cmp_completion_extra(a, b))
        .then_with(|| wind_candidate::cmp_exact_first(a, b))
        .then(by_source_tier)
        .then(by_weight)
        .then(a.base_order.cmp(&b.base_order))
        .then(a.natural_order.cmp(&b.natural_order))
}

/// 满码空码清空的**最终复核**：候选列表里是否存在「拦得住清空」的候选。
///
/// 清空要穿过三道门，缺一不可：
/// 1. 码表 `clear_on_empty_max`（`CodeTableEngine`：满码 + 无候选 + 无更长后继）；
/// 2. 混输 `should_clear`（`MixedEngine`：两道拼音守护，受 `auto_commit_block_on_pinyin` 支配）；
/// 3. **本复核** —— 引擎在追加短语**之前**就算好了 `should_clear`，看不见协调器随后并入的短语
///    候选（`zzbd` 这类码表无字但短语命中），故须以最终列表复查。
///
/// 判据**不是**「列表非空」：**拼音的部分匹配不算匹配**。`nunl` 的「嫩」只解释了前 3 码 `nun`
/// （`consumed_length=3 < 4`），拿它当「有候选」等于让一个没解释完输入的候选替整串挡下清空，
/// 而用户看到的正是「满 4 码没打出东西、缓冲还赖着」。消费整串的拼音候选（`nuan`→「暖」，
/// `consumed_length=4`）是货真价实的匹配，照常拦住清空——否则关掉守护开关的用户就再也打不出
/// 那些码表无字、只有拼音出得来的字。
///
/// 曾经写作 `state.candidates.is_empty()`：那让第 2 道门的裁决被本道原样覆盖——引擎那句
/// 「开关关了就别管拼音」白说，因为拼音候选照样留在列表里把清空挡下（真机 `nunl` 不清空的
/// 直接原因）。同一语义分散在三处且无编译期强制同步，改任一道都要回头核另外两道。
///
/// `consumed_length == 0` = 引擎未标注（码表候选恒为 0）→ 视为整串匹配，与
/// `apply_freq_rerank` 的 `consumes_all` 同一约定。
fn clear_blocked_by_candidates(candidates: &[Candidate], input_len: usize) -> bool {
    candidates.iter().any(|c| {
        c.source != CandidateSource::Pinyin
            || c.consumed_length == 0
            || c.consumed_length >= input_len
    })
}

/// 自动上屏最短码长的归一（纯函数）：配置 0 = 跟随全码长。
///
/// 复刻引擎侧 `CodeTableEngine::new` 的同名归一——那份藏在引擎构造函数里、只作用于其私有
/// `opts`，协调器取不到，故短语侧须在此重算。两处语义必须一致。
///
/// `max_code_length` 为 0（拼音等无「全码」概念的引擎，见 `Engine::max_code_length` 默认实现）
/// 时结果为 0 → 调用方的 `len < 0` 恒假 → 不设闸，与引擎侧同构降级。
fn resolve_auto_commit_min_len(configured: usize, max_code_length: usize) -> usize {
    if configured > 0 {
        configured
    } else {
        max_code_length
    }
}

impl Coordinator {
    /// 记录一次选词到 redb FREQ（词频维度：count+1、last_used=now，按 schema+code+text）。
    /// 词频是与权重解耦的独立维度（frequency.md），仅记真实使用数据；redb 事务即时持久。
    /// 未开启「自动调频」（`schema.codetable/pinyin.frequency.enabled`）时不记录，避免关闭功能后仍写库。
    /// 记一条上屏历史（最近置前，限 16 条）。供命令栏 `last(n)`、加词推荐、
    /// z 键重复上屏与快捷输入的 `quick_input.repeat` 共用同一事实源。
    ///
    /// **与 [`Self::record_selection`] 分开**：那里记的是「用编码选中了某候选」（还要写词频），
    /// 而快捷输入数字透镜的上屏（计算结果、日期、金额）没有编码、恒不记词频，却同样是
    /// 一次上屏。历史点若只挂在选词上，「算完再按 ; 空格重复一次」永远取不到刚算的结果。
    pub(crate) fn push_commit_history(&self, text: &str) {
        if text.is_empty() {
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

    /// 记一次选词：上屏历史恒记，词频**按来源分流**。
    ///
    /// **短语（`CandidateSource::Phrase`）恒不记词频**：短语的候选集与其内部次序完全由短语
    /// 定义决定（`PhraseEntry` 的 weight/position），词频在这里没有排序职责；而它的上屏文本
    /// 可能是模板求值结果（`date` → `2026-07-29`、`time`、`clip()`），**每次上屏都是一个新
    /// 键**——记进 FREQ 表既永远不会被命中（次日查的是新文本），又逐日累积垃圾行。
    /// 读取端 `apply_freq_rerank` 对应地跳过短语候选，两端必须同时成立。
    pub(crate) fn record_selection(&self, code: &str, text: &str, source: CandidateSource) {
        self.record_selection_in(None, code, text, source);
    }

    /// 同 [`Self::record_selection`]，但可指定**生效方案**（特殊模式用，见
    /// [`Self::effective_data_schema`]）。`None` = 按 active 归属。
    ///
    /// ⚠️ 开关也跟着生效方案走（`freq_settings_for`）：特殊方案的调频开关在它自己的
    /// 方案文件里，拿主方案的开关来判断，会出现「主方案开着 → 往特殊方案记账」这种
    /// 用户没配过的行为。
    pub(crate) fn record_selection_in(
        &self,
        schema_override: Option<&str>,
        code: &str,
        text: &str,
        source: CandidateSource,
    ) {
        if text.is_empty() {
            return;
        }
        self.push_commit_history(text);
        if source == CandidateSource::Phrase {
            return;
        }
        if let Some(store) = &self.store {
            let owner = schema_override
                .map(str::to_string)
                .unwrap_or_else(|| self.engine_mgr.active_schema_id());
            // 未开启「自动调频」则不记录（配置说关、代码却记的潜在 bug，对齐 apply_freq_rerank 的开关检查）。
            let settings = self.engine_mgr.freq_settings_for(&owner);
            if !settings.enabled {
                return;
            }
            // 排除区块（`schema.frequency.exclude_blocks`，出厂为空）：emoji 这类候选不学词频。
            //
            // ⚠️ 位置必须在 `push_commit_history` **之后**——那是「重复上屏」功能的数据源，
            // 与词频是两条独立通路。在它之前 return 会让刚上屏的 emoji 无法被 `;` 重复调出，
            // 而用户只会觉得重复上屏偶尔失灵。同理 `record_commit`（统计）也不受本项影响。
            //
            // 读端 `apply_freq_rerank_in` 调**同一个** `excluded_from_freq`：只跳过这一端的话，
            // 库里既有的 emoji 记录照旧参与重排，开关看起来毫无反应。
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

    /// 词频重排（独立维度，**绝不改 weight**）：按 redb 词频记录做档位感知的 used-first 稳定
    /// 重排——用过的候选（count>0）按策略上浮，未用候选保持基础(权重)序。对齐 frequency.md §3。
    ///
    /// 策略（engine.codetable.freq_strategy）：
    /// - `step`（默认/逐次提升）：count 降序、last_used 降序 tiebreak（累积使用才爬升，抗误选）。
    /// - `top`（一次到顶/MRU）：last_used 降序、count 降序 tiebreak（最近选的置该档之首）。
    ///
    /// 主开关 `learning.freq.enabled` 关闭则完全不重排（修"配置说关、代码却排"的潜在 bug）。
    /// 引擎类型分流：码表/混输走永久 used-first（§3），纯拼音走衰减软置前（§4）。
    /// 注：每候选一次 redb 点查（mmap 微秒级）；后续可下沉到引擎排序层。
    pub(crate) fn apply_freq_rerank(&self, candidates: &mut [Candidate], code: &str) {
        self.apply_freq_rerank_in(None, candidates, code);
    }

    /// 同 [`Self::apply_freq_rerank`]，但可指定**生效方案**（特殊模式用）。
    ///
    /// ★ 读端的方案必须与写端 [`Self::record_selection_in`] 取自同一处
    /// （[`Self::effective_data_schema`]），否则「写进 qsym、读的是 wubi86」——
    /// 记账看着成功，候选顺序永远不动。
    pub(crate) fn apply_freq_rerank_in(
        &self,
        schema_override: Option<&str>,
        candidates: &mut [Candidate],
        code: &str,
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
        // 归属方案解析：非混输单次折叠（现行为，零回归）；混输预解析两个子方案归属 id，
        // 循环内按候选来源选用（热路径纪律：非混输不走逐候选分支）。
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
        // 取每个"消费整串"候选的词频记录。分段子候选（consumed_length < 整串，如「nihao」里的「你」
        // 只消费「ni」）的词频归属其自身前缀码，不能被整串码的历史计数上浮——否则单字会浮到整句
        // 「你好」之上。consumed_length==0 表示引擎未标注，视为整串匹配。
        //
        // 注：码表候选**大多**为 0，但已非全部——混输超码长回捞的前缀候选如实标注（见
        // `mixed/engine.rs` 的 `convert_overflow`），故它与拼音分段子候选同样不参与本次重排。
        // 这是有意的：它只解释得了前 N 码，让它靠历史计数浮到消费整串的候选之上并不正确。
        //
        // ⚠️ 查询码**按来源分流**，见 [`Self::freq_code`]：
        //
        // **拼音 / 英文**用 `cand_code`（候选存储码 = 全拼扁平域），不能用击键缓冲——二者在
        // 下列输入下不相等，用错即恒 miss、词频整体失效：双拼缓冲 `siyr` vs 候选码 `siyuan`；
        // 带分隔符 `xi'an` vs `xian`；前缀补全 `si` vs `sikao`。全仓 code 域标准（用户词库
        // key、`generate_word_pinyin` 造词码、加词 `calc_add_word_code`）同为全拼扁平码。
        //
        // **码表**反过来用输入码。码表候选的 code 是**词条全码**（`de` 下的「有」带的是
        // `def`），拿它当 key 会让 `d`/`de`/`def` 三个码位互相串扰——真机实测在 `de` 选中后
        // 打 `d` 时它也跟着前移。而码表的码位本就彼此独立（`ProtectPolicy` 按输入码长分级
        // 保护首选正以此为前提）。
        //
        // 写入端 `record_selection` 的各调用点同样经 `freq_code`，**两侧必须同口径**。
        // 先收键再**一次事务**批量查：逐候选 `get_freq` 会为每个候选开一次 redb 读事务，
        // 五笔单字母下 78+ 候选即 78 次，是每键的固定开销（见 `get_freq_batch`）。
        let mut probe: Vec<(String, String, String)> = Vec::new();
        for c in candidates.iter() {
            // 短语不参与词频维度（写入端 `record_selection` 对称跳过）：其次序由短语定义的
            // weight/position 决定，且求值型短语的文本逐日变化，点查恒 miss——白花一次
            // redb 查询，还会让人误以为词频在这里生效。
            if c.is_phrase {
                continue;
            }
            // 排除区块（写端 `record_selection_in` 调同一个判据）：跳过即不进 probe，
            // 于是也不占 `get_freq_batch` 的一个条目——**这道判断在热路径上是净省**，
            // 它省掉的是三次 String 分配加一次批量查条目，而自身只是一次移位加与运算。
            if settings.excluded_from_freq(&c.text) {
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
                c.text.clone(),
            ));
        }
        let found = store.get_freq_batch(&probe).unwrap_or_default();
        // 只为**命中**的候选 clone 文本：五笔单字母下 78 个候选往往只有个位数有词频记录，
        // 先收一份全量文本副本再筛是白 clone 七十多次。
        let recs: std::collections::HashMap<String, FreqRecord> = probe
            .iter()
            .map(|(_, _, text)| text)
            .zip(found)
            .filter_map(|(t, r)| match r {
                Some(r) if r.count > 0 => Some((t.clone(), r)),
                _ => None,
            })
            .collect();
        if recs.is_empty() {
            return;
        }
        // 词频重排归属 engine 排序层（frequency.md §5/§7）：本协调器只负责取词频记录、按引擎
        // 类型分流到纯函数。码表/混输永久 used-first（§3），纯拼音走等效权重
        // （docs/design/freq-rerank-model.md）。
        if self.engine_mgr.is_pinyin() {
            let profile = self.engine_mgr.pinyin_freq_profile();
            // （此处曾给「未消费整串的整句」标 `is_sentence_unanchored` 以摘掉顶部锚定：
            //  锚定是硬闸门而本次调用是最后一道整体排序，`buzhidaok` 下只消费 8/9 键的
            //  「不知道」若锚定，一有任何词频记录就会把按消费长度排在首位的「不知道看」
            //  挤到第二，P0 的 by_consumed 被整个推翻。整句锚定已整体移除，该标记与字段
            //  随之回收。）
            //
            // 位置提升模型（docs/design/freq-rerank-model.md）：候选按**位次**前移，
            // 不比权重，故无需引擎的量纲基准——位次天然与词库分布、混输降档都无关。
            //
            // ⚠️ 依赖入参已按 `candidate_display_order` 排好（base_pos 取的就是入参下标）。
            // 本调用位于 display_order → filter 之后、shadow 之前，是最后一道整体排序。
            wind_engine::freq_rerank::rerank_pinyin_positional(
                candidates,
                &recs,
                now_unix_secs(),
                profile,
                settings.promote_prefix,
                input_len,
            );
        } else {
            // `strategy = position` 时走与拼音同一套位置提升（档位仍是硬约束，只在档内
            // 提升）；`top`/`step` 仍是布尔 used-first，不读 profile / promote_prefix。
            //
            // profile 取**生效方案**那份（英文方案取英文段、其余取该方案折叠后的码表段），
            // 与上面 `freq_settings_for` 同一个 id——一个按方案取 strategy、另一个按 active
            // 取 half_life 的话，特殊方案会用自己的策略配上主方案的衰减速度。不用拼音
            // 那份。此前这里直接用 `pinyin_freq_profile()`，等于改拼音的半衰期会连带改码表
            // position 的衰减速度，而码表段根本没有这个旋钮。
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

    /// 短语候选的**稳定 id**（`Candidate::id`）：`phrase:{code}:{原始记录文本}`，对齐 Go
    /// `dict.phraseCandID`。供 shadow 规则跨日精准匹配——短语的显示文本可能是模板求值结果
    /// （`date` 的 `$Y-$MM-$DD` → `2026-07-29`），以文本为键的规则次日必失配。
    ///
    /// `code` 用**短语自身的完整码**（前缀导航候选取 `nav_code`，精确命中取输入缓冲），与
    /// shadow 规则的存储键 code 同源；`raw` 为 store 里的 `PhraseEntry.text`（模板未展开）。
    /// `raw` 为空（测试直构的 `PhraseHit::plain` / `$AA` 字面元素）→ 返回空 id，表示该候选
    /// 无稳定身份，shadow 落回文本匹配。
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
        Self::initial_candidate_limit_of(self.engine_mgr.current_engine_type(), input)
    }

    /// 同 [`Self::initial_candidate_limit`]，但按**指定引擎类型**分级。
    ///
    /// overlay 类模式的候选来自它自己引用的方案，而 `current_engine_type()` 报的是**主方案**
    /// （临英下常是五笔/混输）——拿它分级，等于给英文词库套上码表那档「单字母只取 100 条」。
    /// 临拼早有同型分流（见 `temp_pinyin_limit`），临英此前漏了、写死 50 条。
    ///
    /// 真机现象：英文方案下打 `t` 能出刚用过的 `then`，临英下同一个 `t` 出不来，而打到 `th`
    /// 又正常。根因是 `then` 在该词库 `t` 的 top-k 里排第 86（`t` 前缀有 232 条同为最高
    /// weight，按 `order` 兜底），卡在临英的 50 与主路径的 300 之间；`th` 下它排第 15，
    /// 两个上限都够得着。**词频重排只能重排已在池中的候选**，取不到就无从谈起。
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

    /// 用给定上限转换并构建候选（引擎 + 词频 boost + 短语 + 排序去重）。
    /// 返回引擎候选数（不含短语），供判断 has_more。不复位翻页/高亮。
    /// 返回 (引擎候选数, 输入结局)。结局含全码自动上屏 / 满码空码清空；自动上屏文本经
    /// shadow 复核后才放行，避免上屏被置顶删词移除的候选。调用方仅在「正向输入字母」时消费。
    /// 词库候选 value 内嵌特殊语法（`$CC` 命令 / `$Y` 模板 / `$AA`·`$SS` 组 / `{..}` 插值）的
    /// **统一展开汇聚点**。所有候选生成路径（正常 / 特殊模式 / 混输 overlay / 临拼 / 临英）在写入
    /// `state.candidates` 前均须过此点，保证 `$` 语法在全部输入方案一致生效（对齐 Go
    /// `dict.ValueExpander`；见 docs/redesign/unified-candidate-value-expansion.md）。
    ///
    /// - `$CC` → 标 `is_command`（选中由 `select_candidate`、顶屏由 `top_commit_command_guard` 执行动作）；
    /// - `$AA`/`$SS` 组 → **精确码**（候选码 == 当前输入）时逐成员炸开；**前缀**（候选码更长）时折叠为
    ///   单个组名候选（`is_group`，`group_code` = 完整码），选中经 `complete_to_group_code` 补全到完整码
    ///   重查 → 精确 → 展开（二级选择，与短语前缀分组一致）；
    /// - 模板 / 花括号插值 → 直接以展开文本上屏；
    /// - 普通候选（不含 `$` 与 `{`）经廉价预检零开销原样返回。
    ///
    /// `input` 为该路径当前编码缓冲（供 cmdbar 语法内 `input()` 求值）。已是 `is_phrase`/`is_command`
    /// 的候选（短语命中）跳过二次展开。
    pub(crate) fn finalize_candidates(&self, raw: Vec<Candidate>, input: &str) -> Vec<Candidate> {
        // 快路径：无任一候选含特殊语法（普通词库/拼音结果）→ 零拷贝原样返回。
        if !raw.iter().any(|c| {
            !c.is_phrase && !c.is_command && (c.text.contains('$') || c.text.contains('{'))
        }) {
            return raw;
        }
        let now = chrono::Local::now();
        let recent = self.recent_commits_snapshot();
        // 走 **_cached**：本闭包在每次按键的候选构建期求值，只用于拼显示标签，绝不能
        // 卡按键线程（阻塞版 clipboard_get_text 打不开时会 sleep 重试至 40ms）。真正执行
        // 动作时另有 CmdbarCtx 用非缓存版取值。
        let clip = |_n: i64| -> String { self.host_services().clipboard_get_text_cached() };
        let reverse = |text: &str, fmt: &str| -> String { self.reverse_render(text, fmt) };
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
                // 是特殊语法但这次求值为空（如剪贴板空 / 反查查不到）→ 整条不出。
                // **不能**退回原候选：那会把 `{dict.rev(clip())}` 这串源码当文本显示并上屏。
                wind_phrase::DictExpansion::Drop => {}
                wind_phrase::DictExpansion::Single {
                    display,
                    command_src,
                } => {
                    let mut c = cand;
                    c.text = display;
                    if let Some(src) = command_src {
                        c.phrase_template = src;
                        c.is_command = true;
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

    pub(crate) fn build_candidates(
        &self,
        state: &mut State,
        limit: usize,
    ) -> (usize, InputOutcome) {
        // 出简让全的沿途记录：淘汰与当前码无关的那些。放在函数最前面，因为下面每一条
        // 返回路径都可能不走到记录点，而陈旧记录留着比没有更危险（会让位错的字）。
        //
        // 这是**唯一**的失效点：`input_buffer.clear()` 有十余个散落调用点，逐个接线必漏一处；
        // 按前缀关系统一淘汰后，缓冲清空、光标中间编辑、方案切换全被这一条覆盖。
        short_code_yield::evict_stale(&mut state.shortcode_tops, &state.input_buffer);
        // 分段上屏进行中**且最后一段来自拼音选词**：剩余编码强制按混输方案的拼音子方案转换，
        // 避免混输让五笔抢首选（你↑选后 hao→虚）。拼音方案 id 取当前混输方案的
        // [engine.mixed].secondary_schema（如 wubi86_pinyin → "pinyin"）。注意不用全局
        // primary_pinyin——那是给「临时拼音↔临时双拼」切换用的，对混输不适用。
        //
        // ⚠️ 判据曾写作 `!state.committed_text.is_empty()`，理由是「committed 前缀非空 ⟺ 来自
        // 拼音选词——五笔候选 consumed_length=0 永不部分匹配」。**该等价关系已不成立**：混输
        // 超码长回捞的码表前缀候选现在如实带 `consumed_length`（见 `mixed/engine.rs` 的
        // `convert_overflow`），码表也会进入分段态。沿用旧判据的话，`yijga` 选「就是」后剩下的
        // `a` 会被强制按拼音解释，用户看到五笔码出拼音候选。改看**最后一段的来源**：谁刚被选走，
        // 剩余编码就大概率还是谁的。
        let last_seg_is_pinyin = state
            .committed_segs
            .last()
            .is_some_and(|(_, _, _, src, _)| *src == CandidateSource::Pinyin);
        let pinyin_schema = if last_seg_is_pinyin {
            let active = self.engine_mgr.active_schema_id();
            self.engine_mgr
                .schema_merged(&active)
                .map(|s| s.engine.mixed.secondary_schema.clone())
                .filter(|s| !s.is_empty())
        } else {
            None
        };
        let result = match pinyin_schema {
            Some(ps) if self.engine_mgr.ensure_schema(&ps) => {
                self.engine_mgr
                    .convert_with(&ps, &state.input_buffer, limit)
            }
            _ => self.engine_mgr.convert(&state.input_buffer, limit),
        };
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
        let engine_count = result.candidates.len();
        // 引擎给出的全码自动上屏意向（基于引擎候选；下方 shadow 后复核存活性）。
        let auto_commit = if result.should_commit && !result.commit_text.is_empty() {
            Some(result.commit_text.clone())
        } else {
            None
        };
        let should_clear = result.should_clear;

        // 词库候选 value 内嵌特殊语法统一展开（汇聚点：所有路径共用，见
        // finalize_candidates / docs/redesign/unified-candidate-value-expansion.md）。
        // 精确匹配空码补全的两个候选源，在下方「补全收口」处统一判空后择一采纳：
        // - `engine_completion`：码表引擎备下的更长编码首选（`ConvertResult::completion_hint`）；
        // - `completion_pool`：短语侧前缀命中（仅精确模式抑制了枚举时才装填）。
        // ⚠️ `completion_hints` **必须同样过汇聚点**：它直接来自引擎（词库原始 value），而
        // `result.candidates` 在下一行走了 finalize。漏掉的表现是补出来的直通命令候选原样
        // 显示成 `$CC(...)` 源码——同一张码表里的同一条词条，正常命中时显示标签、被当作
        // 补全兜底时显示源码。
        let engine_completion =
            self.finalize_candidates(result.completion_hints, &state.input_buffer);
        let mut completion_pool: Vec<Candidate> = Vec::new();
        let mut candidates = self.finalize_candidates(result.candidates, &state.input_buffer);
        // 方案级短语作用域：`[phrases] enabled/categories/exclude_categories`。
        // 归属方案取 `effective_data_schema`（临英归 english），见 phrase_spec_of。
        let phrase_spec = self.phrase_spec_of(state);
        let phrase_scope = crate::schema_scope::phrase_scope(&phrase_spec);
        let phrases = self.phrases.read().unwrap_or_else(|e| e.into_inner());
        if !phrases.is_empty() && !phrase_scope.is_closed() {
            let recent = self.recent_commits_snapshot();
            // 剪贴板读取回调注入 wind-phrase（其不依赖平台 UI 层）：精确码命令 display
            // 含 {clip()}（如 coad）时按需读取；非 windows 返回空。
            // 走 **_cached**：这里是每次按键都会经过的候选构建期，只为拼 display 标签——
            // 用会 sleep 重试的 `get_clipboard_text` 等于把最坏 40ms 摊到按键线程上。
            let clip = |_n: i64| -> String {
                // cfg 外壳保持：macOS 此处返回空串是既定行为（见上注释），
                // 勿因 trait 化「顺手统一」成读 Pasteboard。
                #[cfg(windows)]
                {
                    self.host_services().clipboard_get_text_cached()
                }
                #[cfg(not(windows))]
                {
                    String::new()
                }
            };
            // 反查渲染（`dict.rev`）：走词库与反查表，无平台调用，三平台一致。
            //
            // ⚠️ 它与上面的 `clip` 不同——`clip` 在非 Windows 恒空是**既定行为**，
            // 而反查没有那个约束。但二者串起来的后果要清楚：`{dict.rev(clip())}`
            // 在 macOS 上因 `clip` 恒空而查无可查，整条候选不出现。
            let reverse = |text: &str, fmt: &str| -> String { self.reverse_render(text, fmt) };
            let host = wind_phrase::PhraseHost {
                clip: &clip,
                reverse: &reverse,
            };
            for hit in phrases.lookup(&state.input_buffer, &recent, &host, &phrase_scope) {
                let is_command = hit.command_src.is_some();
                let is_system = hit.is_system;
                // 稳定 id 取**模板原文**：text 可能是求值结果（date/time/clip），逐日变化。
                let cand_id = Self::phrase_cand_id(&state.input_buffer, &hit.source_text);
                candidates.push(Candidate {
                    // text 存**完整原文，含真换行**——它就是上屏文本，不得在此改写。
                    //
                    // 这里曾调 `clamp_candidate_display` 做「一行化」（换行/制表→空格），
                    // 目的是杜绝多行候选撑破候选窗。那是 ↵ 可见符出现之前的权宜之计，
                    // 代价是**把上屏文本一起改了**——用户词库不走这步，于是同样一条含换行的
                    // 词条，用户词能上屏换行、短语却上屏成空格。
                    // 现在「杜绝多行候选」由渲染层的 `wind_ui::candidate_window::
                    // visible_whitespace` 负责（换行→↵、制表→⇥），那是显示投影、不动数据。
                    //
                    // ★ 一行化是**显示层关注点**，放在数据层就必然殃及上屏。
                    text: hit.text,
                    // 短语自身权重，**不加类别硬顶**。此处曾是 `PHRASE_WEIGHT_BASE(40M) + w`，
                    // 于是精确码短语在纯码表下恒赢每一条码表精确候选（混输/纯拼音见下）。
                    //
                    // ## 为什么现在可以直接比
                    //
                    // 全仓自产权重早已在同一条轴上，只是被 40M 遮住从未真正比较过：
                    // 短语默认 1000（`wind_phrase` 的 `unwrap_or(1000)`）、系统短语实测 800~2000、
                    // 五笔主库 median 941 / p99 9000 / max 9999、`LEARN_ADD_WEIGHT` 800、
                    // `PROMOTED_WEIGHT` 1000、约定上界 `WEIGHT_RANGE_MAX` 10000。
                    // 短语 w=1000 击败 54% 的五笔条目，w=2000 击败 92%——这是一次有意义的比较。
                    //
                    // ## ⚠️ 前提：码表权重必须守约
                    //
                    // Rime 生态导入的方案常是未归一的原始词频（虎码 p99=343,880），不配方案级
                    // `[weight_spec]` 就会让短语全线沉底，且**没有任何报错**——用户只看到「短语没了」。
                    // 护栏在 `wind_dict::SystemDictLayer::effective_weight`（查询期越界告警），
                    // 体检走 `wind_input dict weight-check`。
                    //
                    // ## 「精确码短语 vs 码表精确候选」现在处处由权重裁决
                    //
                    // 沿 `candidate_display_order` 倒推每种模式的实际裁决者：
                    // - **纯码表**（mixed=false）：两者 `is_exact_code` 同为 true ⇒ `cmp_exact_first`
                    //   平局 ⇒ 落到 `by_weight`。40M 在此恒赢，删除它改的就是这条；
                    // - 混输（mixed=true）：`source_tier` 曾把二者分作档 0/档 1（码表恒先），40M
                    //   因此被档位覆盖、不起作用。该档已合并（同为档 0）⇒ 现在同样落到 `by_weight`；
                    // - 纯拼音：拼音候选 `is_exact_code` 恒 false ⇒ `cmp_exact_first` 已让短语居前，
                    //   与权重无关。
                    //
                    // 合档的动因不在这里，而在 `freq_rerank::freq_tier`——它复用 `source_tier` 且是
                    // 调频重排的首要键，二者分档会造成「开调频码表恒赢、关调频按权重比」的
                    // 开关依赖不一致。见 `wind_candidate::source_tier` 函数文档。
                    weight: hit.weight,
                    is_phrase: true,
                    // $CC 命令短语：标记 is_command，phrase_template 暂存命令源；
                    // 选中时由 commit_selected 拦截，执行动作而非上屏 display 标签。
                    // 非命令短语 phrase_template 存原始记录文本（source_text，模板未展开），
                    // 供右键「禁用短语」按 (code, 原文) 定位 store 记录（对齐 Go PhraseTemplate）。
                    is_command,
                    // `lookup` 查的是**精确码短语**（短语编码与输入完全相等），按定义即精确匹配，
                    // 须与码表精确候选同层竞争（同层内按权重定先后，见上方 `weight`）。漏标会被
                    // `cmp_exact_first` 压到码表精确候选之下——如短语 skce 会输给五笔「可能」(skce)。
                    // 下面 `lookup_prefix` 的前缀枚举则不标，留在精确层之下。
                    is_exact_code: true,
                    phrase_template: hit.command_src.unwrap_or(hit.source_text),
                    id: cand_id,
                    // 来源如实标注为短语：`record_selection` 据此跳过词频记账（短语的上屏文本
                    // 可能逐次不同，记了永不命中、只污染 FREQ 表），排序层不受影响——层级键
                    // 走的是 `is_phrase`（见 `freq_tier` / `cmp_match_layers`）。
                    source: CandidateSource::Phrase,
                    meta: CandidateMeta {
                        is_system_phrase: is_system,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
            // 前缀导航：敲 `zz`/`co` 等前缀（长度 ≥ min_prefix_length）列出所有该前缀的
            // marker 短语。**$CC 命令** → is_command（选中直接执行，group_code 作执行输入
            // 上下文）；**$SS/$AA 组** → is_group（选中补全到完整码再展开成员，二级选择）。
            let min_prefix = self.rt().config.input.phrase.min_prefix;
            // 精确匹配模式（`single_code_input`，仅纯码表方案）：默认抑制短语前缀枚举，只保留上面的
            // 精确码短语（`lookup`）——与码表引擎跳过 `search_prefix` 的行为对齐。混输不适用：其拼音半边
            // 恒前缀匹配，切精确会与拼音割裂（见 `EngineManager::is_codetable`）。
            // 例外——镜像码表引擎 `single_code_complete`：当前无任何候选（码表 + 精确短语均空）且未满码时，
            // 放行一次前缀枚举作**补全候选源**，避免精确模式下彻底无候选。
            let ct = self.engine_mgr.codetable_settings();
            // 引擎模式：纯码表 vs 拼音/混输。全局短语的前缀命中按**来源**（来源=短语库、全局、
            // 不与方案挂钩）统一处理，不按语法类型（`$CC`/`$SS`/静态）区分——见下方三个前缀分支
            // 共用的 `phrase_prefix_is_prefix`。此处只承载「码表用 is_exact_code 分档、拼音用
            // is_prefix 分层」这一引擎差异：两种引擎表达「短语该降到方案精确候选之下」的标志不同。
            // （方案内词库词条走引擎/`finalize_candidates`，按方案权重排，不经这里，故第①类天然正确。）
            let codetable_mode = self.engine_mgr.is_codetable();
            let exact_only = codetable_mode && ct.single_code_input;
            // 空码补全的短语侧取数闸门：仅在精确模式抑制了前缀枚举时才可能触发。此处的
            // `candidates.is_empty()` 已是「码表候选 + 精确短语」之和——码表引擎不再抢先把
            // 补全候选塞进来（见 `ConvertResult::completion_hint`），故这个「空」判得准。
            let complete_fallback = exact_only
                && ct.single_code_complete
                && candidates.is_empty()
                && state.input_buffer.chars().count() < self.engine_mgr.active_max_code_length();
            let prefix_hits = if !exact_only || complete_fallback {
                phrases.lookup_prefix(&state.input_buffer, &recent, min_prefix, &phrase_scope)
            } else {
                Vec::new()
            };
            // 前缀命中 → 候选的构造，正常枚举与补全池两条去向共用：补全要取的是「若开启前缀
            // 匹配，本会显示在最前的那一条」，故它必须与正常枚举**构造得一模一样**，才能用同一个
            // 显示排序器（`candidate_display_order`）比出真正的首条。
            //
            // 三个前缀分支（`$CC`/`$SS`·`$AA`/静态）的**排序标志一律相同**——只按「来源=全局短语 +
            // 前缀匹配」处理，不按语法类型区分（语法只决定 is_command/is_group 的**选中行为**）：
            // - `is_exact_code=false`：前缀非完全匹配，不进精确档（完全匹配走上面的 `lookup`，仍抬升）；
            // - `is_prefix=!codetable_mode`：码表下与更长编码补全同档、按权重竞争；拼音/混输下降到
            //   拼音精确候选（is_prefix=false）之下；
            // - `weight=hit.weight`：按短语自身权重排。与上方 `lookup` 分支同口径——类别硬顶
            //   `PHRASE_WEIGHT_BASE`(40M) 已整体删除，两条通路不再有量级差异。
            let phrase_prefix_is_prefix = !codetable_mode;
            let mut built: Vec<Candidate> = Vec::new();
            for hit in prefix_hits {
                // 完整原文，含真换行：它就是上屏文本。一行化由渲染层的 `visible_whitespace`
                // 承担（见上方 `lookup` 分支的同源说明），此处不得改写。
                let text = hit.text;
                let is_system = hit.is_system;
                let phrase_meta = || CandidateMeta {
                    is_system_phrase: is_system,
                    ..Default::default()
                };
                if let Some(src) = hit.command_src {
                    // $CC 命令短语：选中直接执行，不二级展开。
                    let code = hit.nav_code.unwrap_or_default();
                    // 稳定 id 的 code 取**短语自身完整码**（nav_code）而非当前输入前缀：
                    // 同一条短语在敲 `co` 与敲 `coad` 时都应是同一个身份。
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
        let mixed =
            self.engine_mgr.current_engine_type() == Some(wind_engine::engine::EngineType::Mixed);
        // 供跨来源档位判「消费整串」与「码 == 输入」。缓冲恒 ASCII，字节长度与 `consumed_length` 同域。
        let input_str = state.input_buffer.clone();
        // 空码补全的**择一推迟到全部过滤之后**（见下方「空码补全收口」）——判空必须落在
        // 真正的最终列表上，而 `apply_filter` / `apply_shadow` 都在下面、都可能把列表清空。
        // 候选层级排序：合并引擎候选 + 短语后按统一层级重排（见 `candidate_display_order`）。
        // base_sort=natural 时忽略权重，对齐引擎 by_natural（否则合并短语后重排会与引擎发散）。
        let ignore_weight = self.engine_mgr.active_base_sort_ignores_weight();
        // ⚠️ 常用字判定必须**先于排序**：混输的拼音精确档拿 `is_common` 作提档准入条件
        // （见 `mark_common` 与 `wind_candidate::is_pinyin_exact_tier`）。过滤仍在下面按模式进行。
        self.mark_common(&mut candidates);
        candidates.sort_by(|a, b| candidate_display_order(a, b, ignore_weight, mixed, &input_str));
        // 按 text 去重。**不能用 `retain` + `HashSet`**：被丢弃那条所占的码位要并进幸存者，
        // 否则下一步的检索范围过滤按 (source, code) 分组时会丢掉「该码位下有常用字」这一事实
        // ——同一个字打前缀出、打全码反而不出（见 `Candidate::merged_codes`）。
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut deduped: Vec<Candidate> = Vec::with_capacity(candidates.len());
        for c in std::mem::take(&mut candidates) {
            if let Some(&idx) = seen.get(&c.text) {
                deduped[idx].absorb_codes_from(&c);
                continue;
            }
            seen.insert(c.text.clone(), deduped.len());
            deduped.push(c);
        }
        candidates = deduped;
        // 检索范围过滤（按模式裁剪；is_common 已由上面的 mark_common 填好）
        self.apply_filter(state, &mut candidates);
        // 用户词频重排（独立维度，used-first，绝不改 weight；frequency.md §3）
        //
        // ⚠️ 自动补充的候选（`is_scope_filtered`，恒在末尾）**排除在重排之外**：沉底是硬约束。
        // 否则用户误选一次补充来的生僻字，used-first 就会把它顶到常用字前面——码表侧
        // **不衰减**，误选一次即永久翻转（见 codetable-freq-short-code-protection.md），
        // 与「自动补充不影响原有排序」的承诺直接相悖。
        // 用 take_while 而非 partition_point：不预设列表整体有序，只截断到首个补充候选为止。
        let rerank_len = candidates
            .iter()
            .take_while(|c| !c.is_scope_filtered)
            .count();
        self.apply_freq_rerank(&mut candidates[..rerank_len], &state.input_buffer);
        // Shadow 的取码口与写端 `candidate_op_scope` 同源（见 `shadow_code_of`）——双拼下
        // 是归一后的全拼码，其余恒为击键。⚠️ 与上一行的词频记账**刻意不同域**：那条链有
        // 自己的 `freq_code`（码表按输入码、拼音按候选码），两者别互相照抄。
        let shadow_code = Self::shadow_code_of(state).to_string();
        // Shadow 规则：删除过滤 + 置顶/移动重排（优先级最高，排序后应用）。
        //
        // ⚠️ 「优先级最高」这句现在由**返回值**兑现，而不再靠「排在最后」：出简让全排在它
        // 之后（那是 `apply_freq_rerank` 强加的次序，见下），若不额外让路就会反过来覆盖
        // 用户的调整。`user_pinned` 就是那条让路判据，取值只此一处。
        let mut user_pinned = self.apply_shadow(&mut candidates, &shadow_code);
        // ── 空码补全收口 ──────────────────────────────────────────────────────
        // 精确匹配模式下「一条候选都没有」时补一条兜底。判据必须落在**最终列表**上：码表引擎
        // 与短语层各自只看得见自己那一半，谁先跑谁就会拿子集的空当成全局的空——引擎抢先补一条，
        // 屏幕上短语旁边就多出无关的后续编码；反过来引擎补的那条又会让短语侧误判「已有候选」
        // 而放弃补全。故两边都只交候选源（`completion_hint` / `completion_pool`），在此统一判空、
        // 统一取一条。
        //
        // ⚠️ **必须在过滤链末尾**（原先在合并短语之后、`apply_filter`/`apply_shadow` 之前）：
        // 「最终列表」这个判据，只有走完全部过滤才算数。某码下的候选被检索范围滤光或被用户
        // 全部隐藏时，早判的版本看到的是「还有候选」⇒ 不补 ⇒ 过滤后空屏。
        //
        // ⚠️ 补全池**自己也要走同一条过滤链**。尤其是 shadow：补进来的候选同样显示在当前码
        // 的候选窗里，用户右键隐藏的往往正是它——不过滤的话，隐藏完当场又被补回来。
        // （早判的版本里补全候选是先并进主列表再一起过滤的，故过滤语义与此等价，只是次序不同。）
        //
        // 取哪一条：**若开启前缀匹配，本会显示在最前的那一条**——用与最终列表同一个
        // `candidate_display_order` 排序，不另立跨来源的优先级规则，将来前缀模式排序改了这里自动跟随。
        // 末级补 text 兜底：`lookup_prefix` 由 HashMap 遍历产出、顺序不定（见 wind-phrase
        // lookup_prefix_at），而 `candidate_display_order` 无文本末级，同分时取到的会是随机一条。
        if candidates.is_empty() {
            completion_pool.extend(engine_completion);
            if !completion_pool.is_empty() {
                // 与最终列表同一套排序 ⇒ 同样先补 is_common（否则混输档位键在此退化为空操作，
                // 上面「本会显示在最前的那一条」这句承诺就不成立了）。
                self.mark_common(&mut completion_pool);
                self.apply_filter(state, &mut completion_pool);
                // 补全池必须走同一条过滤链、**同一个码**：主列表隐藏掉的词若在这里被原码
                // 补回来，用户看到的就是「删了又冒出来」。
                // 返回值并进 `user_pinned`：补全收口只在主列表为空时走，那时上面那次
                // `apply_shadow` 因空列表直接返回 false，判据全在这一次里。
                user_pinned |= self.apply_shadow(&mut completion_pool, &shadow_code);
                completion_pool.sort_by(|a, b| {
                    candidate_display_order(a, b, ignore_weight, mixed, &input_str)
                        .then_with(|| a.text.cmp(&b.text))
                });
                candidates.extend(completion_pool.into_iter().next());
            }
        }
        // ── 出简让全 ──────────────────────────────────────────────────────────
        // 有简码的字，在更长的码位上把首选让给词语。**必须在 `apply_freq_rerank` 之后**：
        // 4 码位的 `ProtectPolicy.fallback` 是 0（不保护首选），先让位会被调频原样顶回去。
        //
        // 判据取自本次输入沿途记录的各级简码位首选（见 `short_code_yield`），零查询——
        // 打 khtk 必然逐键经过 k/kh/kht，那时的首选已经记下了。
        let yield_level = self.engine_mgr.codetable_settings().short_code_yield_level;
        // `user_pinned`：用户右键调过这个码的顺序就整码停手——**候选调整优先于出简让全**。
        // 让位没法简单地挪到 `apply_shadow` 之前来表达这个优先级：它被 `apply_freq_rerank`
        // 钉在后面（4 码位 `ProtectPolicy.fallback = 0`，先让位会被调频原样顶回去），
        // 于是优先级只能写成判据。详见 `short_code_yield::apply` 内的论证。
        short_code_yield::apply(
            &mut candidates,
            &state.input_buffer,
            &state.shortcode_tops,
            yield_level,
            user_pinned,
        );
        // 记在让位**之后**：记的是用户实际看到的首条，让位本身也是用户所见的一部分。
        // 简码位因此可能记到词（该级被让位了），而更短那级仍记着字——`apply` 扫全部级别，
        // 故链式让位不会把自己的前提擦掉。
        short_code_yield::record_top(&mut state.shortcode_tops, &state.input_buffer, &candidates);
        // ── 英文方案：头部候选（输入原文 + 大小写变形）──────────────────────────
        //
        // 英文引擎的「输入即内容」：输入串本身就是可上屏文本，而调频一旦把某个词顶到首位，
        // 想上屏所打原文就只剩回车——终结性动作，打断连续输入流。码表方案没有这个问题
        // （`aaaa` 不是可上屏文本），故这条只对英文引擎生效。
        //
        // ★ 钉在**所有加工之后**：重排、shadow、出简让全、空码补全收口全都只作用于词库
        // 候选，原文才不会被挤走。手法与临英 `split_off(dict_start)` 同型，位置不同是因为
        // 主路径的加工链更长、没有一个「词库段起点」可切。
        //
        // 配置与临英各自独立（`schema.english.*` vs `input.temp_english.*`，默认值还刻意
        // 相反），但产出共用同一个函数——见 crate::english_candidates 模块文档。
        if self.engine_mgr.active_is_english() {
            let (want_raw, want_variants) = {
                let en = &self.rt().config.schema.english;
                (en.raw_candidate, en.case_variants)
            };
            let head = crate::english_candidates::english_head_candidates(
                &state.input_buffer,
                want_raw,
                want_variants,
            );
            if !head.is_empty() {
                // 精确去重：词库里字面相同的那条被头部候选吃掉（同临英）。**不是**小写去重
                // ——`hello` 不该把词库里的 `Hello` 一起抹掉。
                let heads: std::collections::HashSet<&str> =
                    head.iter().map(|c| c.text.as_str()).collect();
                candidates.retain(|c| !heads.contains(c.text.as_str()));
                let mut merged = head;
                merged.append(&mut candidates);
                candidates = merged;
            }
        }
        state.candidates = candidates;
        // 满码自动上屏「显示态」复评：引擎按未过滤候选判唯一（生僻同码字致不唯一被否决），
        // 但智能过滤后可能只剩唯一精确全码码表候选 → 据显示候选复评放行（逻辑与显示一致）。
        // 惰性：仅在引擎未给出上屏意向时复评。
        let auto_commit = auto_commit.or_else(|| {
            self.engine_mgr
                .recheck_auto_commit(&state.input_buffer, &state.candidates)
        });
        // 复核：仅当上屏目标在最终候选中仍存在（未被 shadow 删除）才放行自动上屏。
        // 词库 `$CC` 命令词条经 finalize_candidates 展开后 text 已改写为 display 标签，而引擎
        // 意向 commit_text 是原始 `$CC` 源 → 按 phrase_template 补匹配（否则意向恒被误否决）。
        let outcome = match auto_commit
            .filter(|t| {
                state
                    .candidates
                    .iter()
                    .any(|c| &c.text == t || (c.is_command && &c.phrase_template == t))
            })
            // 短语侧否决：引擎的「唯一」判在**码表候选子集**上跑（`decide_auto_commit` 按
            // `c.code == input` 筛，而短语候选的 code 恒为空串、且在引擎 convert 之后才由协调器
            // 追加）⇒ 同码短语对它完全不可见。真机现场 `aqgy`：短语「东乌珠穆沁旗」+ 码表
            // 「葡」共两条候选，却被判成唯一而自动上屏——**关掉开关有两条候选、开了反而只剩
            // 一条**，显示与处置对不上，用户配的短语连露面的机会都没有。
            //
            // 判据与 [`Self::phrase_vetoes_top_code`] 同构（共用 `phrase_owns_code`）：整串已是
            // 精确码短语，或还能续打成更长短语 → 这个码位归用户的短语管，不许引擎替他做主。
            // 后半条不可省——**码长超过方案满码长的短语**（5 码短语落在 4 码方案里）在码表侧
            // 恰是「精确唯一 + 无更长后继」，正是自动上屏最爱命中的形态，一上屏那条短语就
            // 永远打不出来（顶码路径为同一原因补过同一道闸）。
            //
            // 惰性：`filter` 只在引擎已放行（`Some`）时才求值，故全量扫短语码表的代价不落在
            // 每次按键上——与 `phrase_vetoes_top_code` 的调用点选择同源。
            .filter(|_| !self.phrase_vetoes_auto_commit(state, &state.input_buffer))
        {
            Some(_) => {
                // 一致性：自动上屏文本取「实际显示的首候选」，与空格/点选同源，杜绝
                // "显示藏、全码上屏駏"的漂移（首候选已由档位排序保证是五笔精确全码）。
                // 守护：仅当显示首选是**码表来源**时才自动上屏；若显示首选是拼音/英文（被 shadow
                // 置顶，或码表精确字被智能过滤后仅剩拼音），则不自动上屏——上屏须与显示一致、
                // 非码表类不上屏，留给用户继续选。
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
            // 满码空码清空：`should_clear` 由引擎在追加短语**之前**计算，故此处须以叠加短语后的
            // 最终候选复查（判据见 `clear_blocked_by_candidates`——不是简单的「列表非空」）。
            None if should_clear
                && !clear_blocked_by_candidates(
                    &state.candidates,
                    state.input_buffer.chars().count(),
                ) =>
            {
                InputOutcome::Clear
            }
            None => InputOutcome::Normal,
        };
        // 短语自动上屏：码表未给出上屏意向（Normal）时，补齐短语侧——引擎判据看不到短语，
        // 唯一精确码短语 + 无更长后继时也应自动上屏（与码表「全码唯一自动上屏」对齐）。
        let outcome = match outcome {
            InputOutcome::Normal => self
                .phrase_auto_commit(state)
                .unwrap_or(InputOutcome::Normal),
            other => other,
        };
        (engine_count, outcome)
    }

    /// 短语自动上屏（`schema.codetable.auto_commit_at_full` 开启时）：当前输入的**唯一**候选是
    /// 精确码短语，且**无更长后继**（码表前缀扫描 + 短语码前缀扫描）→ 自动上屏。引擎的
    /// `decide_auto_commit` 只认码表候选（短语在引擎 convert 后由协调器追加、且候选 `code` 为空），
    /// 故短语从不进码表判据；此处补齐短语侧，判据与码表「全码唯一自动上屏」同构。
    ///
    /// - 普通短语 → 直接上屏其文本；
    /// - 纯文本命令（`$CC` 仅 `type` 文本、无副作用）→ 同步求值上屏其文本（与顶码 `eval_command_text_only` 同路）；
    /// - 含副作用命令 → [`InputOutcome::AutoCommand`]：清组合并异步执行（与空格选中命令同语义）；
    /// - `$SS`·`$AA` 组 / 前缀枚举短语 → 排除（不自动上屏，避免误展开/打断输入）。
    ///
    /// 门槛为「最短码长 + 唯一 + 无更长后继（含短语）」四闸串联，与引擎 `decide_auto_commit`
    /// 同构——两道缺一不可：`min_len` 管「够不够满码」，`has_longer_code` 管「还能不能接着打」。
    pub(crate) fn phrase_auto_commit(&self, state: &State) -> Option<InputOutcome> {
        let ct = self.engine_mgr.codetable_settings();
        if !ct.auto_commit_at_full {
            return None;
        }
        // 最短码长闸：与引擎 decide_auto_commit 的 `input.chars().count() < min_len` 同构。
        // 短语此前不设此闸，致 3 码短语（如 ocd）在 4 码方案里绕过「满码」语义直接上屏/执行。
        if state.input_buffer.chars().count() < self.phrase_auto_commit_min_len(&ct) {
            return None;
        }
        // 唯一候选。
        let [c] = &state.candidates[..] else {
            return None;
        };
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

    /// 短语侧对**顶码上屏**的否决：整串已是精确码短语，或还能续打成更长短语 → 不许顶码。
    ///
    /// 与 [`Self::phrase_auto_commit`] 的两道闸同源，补的是引擎侧够不到的那一半：
    /// `CodeTableEngine::handle_top_code` 的 `has_full_input_match` / `has_longer_code`
    /// 都只问 `DictManager`（码表），而短语层归协调器持有。于是**码长超过方案满码长**的
    /// 短语（5 码短语落在 4 码方案里）在码表侧「既无精确匹配也无更长后继」，被判成
    /// 「溢出该顶字」→ 顶掉前 N 码的显示首选、余码续打，那条短语**永远打不出来**。
    ///
    /// ⚠️ 顶码在 [`Coordinator::accumulate_code_char`] 里**排在 `update_candidates` 之前**
    /// 且命中即 `return`，故 `phrase_auto_commit` 补得再全也救不回来——判据必须补在先手
    /// 这条路径上。出厂 `schema.codetable.top_code_commit = true`，真机默认走的就是这条。
    ///
    /// 调用点刻意放在 `handle_top_code` **返回 Some 之后**：本判据要全量扫短语码表，而
    /// 引擎侧首道闸（开关 + 码长 ≤ 满码长）极廉价且绝大多数按键都在那里返回 None。
    pub(crate) fn phrase_vetoes_top_code(&self, state: &State, input: &str) -> bool {
        self.phrase_owns_code(state, input)
    }

    /// 短语侧对**全码唯一自动上屏**的否决。判据与 [`Self::phrase_vetoes_top_code`] 同构，
    /// 补的同样是引擎侧够不到的那一半。
    ///
    /// 引擎的 `decide_auto_commit` 在**码表候选子集**上判唯一（按 `c.code == input` 筛），而
    /// 短语候选的 `code` 恒为空串、且在引擎 `convert` 之后才由协调器追加 ⇒ 短语对那道判据
    /// 完全不可见。于是同码短语 + 唯一码表字会被判成「唯一候选」直接上屏：**关掉开关时
    /// 候选面有两条、开了反而只剩一条**，显示与处置对不上。
    ///
    /// 真机现场（v0.118.0）：用户短语 `aqgy → 东乌珠穆沁旗`（w=1000）与五笔 `aqgy → 葡`
    /// （w=1379）同码，敲完第 4 码当场上屏「葡」，候选窗从未显示（日志里只有 HideCandidates、
    /// 无 UpdateCandidates），用户根本没有按空格的机会。
    ///
    /// ⚠️ 两条判据缺一不可，理由与顶码那侧完全一致：`has_longer_code` 管的是**码长超过方案
    /// 满码长的短语**（5 码短语落在 4 码方案里），它在码表侧恰好呈现为「精确唯一 + 无更长
    /// 后继」——正是自动上屏最爱命中的形态，一旦上屏那条短语就永远打不出来。
    pub(crate) fn phrase_vetoes_auto_commit(&self, state: &State, input: &str) -> bool {
        self.phrase_owns_code(state, input)
    }

    /// 「这个码位归短语管」的单一判据：整串已是精确码短语，或还能续打成更长短语。
    ///
    /// 顶码与全码自动上屏两条路径**共用**它——两者都是「引擎替用户做主上屏」，短语层的
    /// 否决条件本就是同一个。曾各写一份，分叉只是时间问题（自动上屏那份一开始压根没写，
    /// 于是同一个缺口在第二条路径上原样重现了一次）。
    ///
    /// 全量扫短语码表，调用方须把它放在「引擎已决定上屏」之后作二道闸，不可每键必查。
    pub(crate) fn phrase_owns_code(&self, state: &State, input: &str) -> bool {
        // ★ 这一处**最不能漏**方案级作用域：漏了它，英文方案下短语候选不出现（上面两处
        // 已过滤），但顶码与自动上屏仍被短语层否决 ⇒ 打字卡住不上屏，且零日志。
        // 见 `docs/design/schema-scoped-behavior.md` §6.3。
        let spec = self.phrase_spec_of(state);
        let scope = crate::schema_scope::phrase_scope(&spec);
        let phrases = self.phrases.read().unwrap_or_else(|e| e.into_inner());
        phrases.has_exact_code(input, &scope) || phrases.has_longer_code(input, &scope)
    }

    /// 这串码是否**恰好**是一条短语的编码（[`wind_phrase::PhraseLayer::has_exact_code`] 直通）。
    ///
    /// ⚠️ **短语候选的 `code` 字段恒为空串**（协调器在引擎 convert 之后追加，不填码），
    /// 故「这条短语候选是不是当前这串码的精确命中」**无法从候选本身看出来**——必须回头问
    /// 短语层。真机现场：5 码短语 `zzsfz` 在敲到 `zzsf` 时就以 `is_prefix=false` 的形态出现在
    /// 候选首位（前缀命中不打标记），顶码把它当成「`zzsf` 的首选」兑现，于是打 `zzsfa`
    /// （短语里根本没有这条码）反而上屏了 `zzsfz` 的内容。
    pub(crate) fn phrase_has_exact_code(&self, state: &State, code: &str) -> bool {
        let spec = self.phrase_spec_of(state);
        let scope = crate::schema_scope::phrase_scope(&spec);
        self.phrases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .has_exact_code(code, &scope)
    }

    /// 短语自动上屏的最短码长门槛。
    ///
    /// **当前跟随主码表**的 `schema.codetable.auto_commit_min_len`：短语虽是独立体系，但
    /// 「满码自动上屏」的规格应与主码表一致，否则同一个 `auto_commit_at_full` 开关下短语与
    /// 码表行为分叉（原 bug：3 码短语在 4 码方案里直接上屏）。
    ///
    /// 预留：日后若要给短语独立门槛（如 `schema.phrase.auto_commit_min_len`），只需改本方法
    /// 的取值来源，`phrase_auto_commit` 的判据结构无需改动。
    fn phrase_auto_commit_min_len(&self, ct: &wind_config::CodetableGlobal) -> usize {
        resolve_auto_commit_min_len(
            ct.auto_commit_min_len,
            self.engine_mgr.active_max_code_length(),
        )
    }

    /// `$CC` 命令候选的自动上屏结局分流（短语命令 / 词库命令词条共用）：
    /// - 纯文本命令（动作链全 Text）→ 同步求值其文本 [`InputOutcome::AutoCommit`]；
    /// - 含副作用命令 → [`InputOutcome::AutoCommand`]（消费点经 `commit_command` 清组合 +
    ///   独立线程异步执行——Effect 回调 coordinator 自锁方法，此刻持 state 锁不可同步跑）；
    /// - 求值文本为空 → Normal（无可上屏内容，继续组合）。
    pub(crate) fn command_auto_outcome(&self, c: &Candidate, input: &str) -> InputOutcome {
        match self.eval_command_text_only(&c.phrase_template, input) {
            Some(t) if !t.is_empty() => InputOutcome::AutoCommit(t),
            Some(_) => InputOutcome::Normal,
            None => InputOutcome::AutoCommand(Box::new(c.clone())),
        }
    }

    /// 常用字**判定**（只置 `is_common`，无过滤、无删除）。
    ///
    /// ⚠️ **必须在排序之前无条件调用**，且刻意**不看 `filter_mode`**：混输的拼音精确档
    /// （`wind_candidate::is_pinyin_exact_tier`）拿 `is_common` 当提档准入条件，而本判定原先
    /// 写在 `apply_filter` 内部、`FilterMode::Gb18030` 时随那道 early-return 一起被跳过 ——
    /// 沿用那个位置会让提档在 Gb18030 下因 `is_common` 恒假而**整体失效且无任何痕迹**。
    ///
    /// 判定（纯计算）与过滤（按模式裁剪，见 `apply_filter`）因此拆开：判定无条件跑，
    /// 过滤仍留在原步骤、语义不变。
    pub(crate) fn mark_common(&self, candidates: &mut [Candidate]) {
        // 读锁在循环外取一次：逐候选取锁在这条热路径（每次按键 × 每个候选）上纯属浪费。
        let cc = self.common_chars.read().unwrap_or_else(|e| e.into_inner());
        if cc.is_empty() {
            return;
        }
        for c in candidates.iter_mut() {
            // 短语保留（is_phrase 已置位）；其余按常用字表判定
            if !c.is_phrase {
                c.is_common = cc.is_string_common(&c.text);
                // 用户亲手降级的字要与「出厂就没收录」分开记：智能档的孤儿码位保底
                // 对前者不适用（见 `Candidate::user_rare`）。
                c.user_rare = cc.has_user_rare(&c.text);
            }
        }
    }

    /// 按当前检索范围过滤候选（`is_common` 由 `mark_common` 提前填好）。
    /// Gb18030 或数据缺失时不过滤（避免误删）。
    ///
    /// ⚠️ `common_chars.is_empty()` 这道检查**不可省**：常用字表未加载时全体 `is_common=false`，
    /// `General` 模式会把候选**全部滤光**。
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
        let taken = std::mem::take(candidates);
        let outcome = wind_candidate::filter_candidates(taken, mode);
        *candidates = outcome.kept;
        // 临时放宽（末页再按向后翻页键触发）：把被滤候选带 `is_scope_filtered` 标记
        // **追加到末尾**，原有候选顺序纹丝不动。标记不可省——自动上屏计数靠它排除，
        // 否则放宽期间满码上屏会静默退化（见 `decide_auto_commit`）。
        //
        // **不放宽时一切照旧，这是刻意的**。曾实现过「候选不足一页就自动补充」，实测下来
        // 平白改变了智能档的既有观感（用户没要求却凭空多出生僻字），已删除：智能档的表现
        // 不该被任何自动行为改动，**用户不主动按翻页键就什么都不变**。
        // 「不足一页」并未因此失去出路——只有一页时 `page_next` 同样翻不动，落到同一条
        // 放宽分支，一条路径覆盖两种场景。
        if state.scope_relaxed {
            candidates.extend(outcome.filtered.into_iter().map(|mut c| {
                c.is_scope_filtered = true;
                c
            }));
        }
    }

    /// 候选调整（shadow）规则的**唯一取码口**——读端、写端、菜单灰显三处都必须走它。
    ///
    /// 归一码非空（双拼）取归一码，否则落回击键缓冲。之所以要有这么个单点，是因为三处
    /// 若各取各的，失配**完全静默**：规则写进 `hc`、读的是 `hao`，界面上看不出任何异常，
    /// 只表现为「置顶了但没反应」。同款教训见 `candidate_op_scope` 的文档。
    ///
    /// 只服务**主输入路**：特殊模式的码在 `special_buffer`（码表方案，无第二编码域），
    /// 由 `candidate_op_scope` 自己分流。
    pub(crate) fn shadow_code_of(state: &State) -> &str {
        if state.shadow_code.is_empty() {
            &state.input_buffer
        } else {
            &state.shadow_code
        }
    }

    /// 应用 Shadow 规则：先按 deleted 过滤，再把 pinned 按目标位置重排。
    pub(crate) fn apply_shadow(&self, candidates: &mut Vec<Candidate>, code: &str) -> bool {
        self.apply_shadow_in(None, candidates, code)
    }

    /// 同 [`Self::apply_shadow`]，但可指定**生效方案**（特殊模式用）。
    ///
    /// ⚠️ 守卫是 `candidates.is_empty()` 而**不是** `code.is_empty()`：空码是合法键位
    /// （shadow key = `"{schema}\0{code}"`，空 code 与任何非空码互不冲突），特殊模式的
    /// 「进入即展示」浏览态正是**空码 + 有候选**。此前按空码 return，导致那批候选的调整
    /// 规则写得进去却永远读不出来；而 `max_code_length=1` + `auto_commit_at_full` 的快符
    /// 方案敲一码即上屏，浏览态是用户**唯一**能右键的时机，等于该方案完全不能调候选。
    /// 主路径不受影响：空码时候选本就为空，首行即 return，零额外开销。
    ///
    /// # 返回值：本码是否有置顶规则**就位**（用户对这个码的顺序表达过主张）
    ///
    /// 出简让全据此让路，见 `short_code_yield::apply` 的 `user_pinned`。取值只此一处——
    /// 让位侧不得自己再查一遍规则，那把 `data_schema_id` 折叠与 `shadow_code_of` 归一
    /// 各复制一份，失配是完全静默的。
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
        let schema = self.engine_mgr.data_schema_id(
            &schema_override
                .map(str::to_string)
                .unwrap_or_else(|| self.engine_mgr.active_schema_id()),
        );
        let rec = match store.get_shadow_rules(&schema, code) {
            Ok(Some(r)) => r,
            _ => return false,
        };
        // 纯重排逻辑下沉 wind_candidate（镜像结构解耦，避免该 crate 依赖 wind-store）。
        // `cand_id` 必须一并传下去：短语候选的 word 记的是写入当天的求值文本，只有 id
        // 跨日稳定（匹配契约见 `ShadowPinRule`）。
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

    /// 简繁 1对多变体展开：s2t 开启时，对最终候选列表中的**单字**候选，紧跟其后插入
    /// 变体候选（如「出」→ 追加「齣」，STCharacters 多值行，全表 276 字）。变体候选
    /// text 保持简体原字、输出走 `s2t_override`（见 `cand_s2t_text`）。
    ///
    /// 调用时机有两条硬约束：
    /// - 必须在候选装配**全部完成后**（排序/去重/词频重排/shadow 之后）：去重按 text
    ///   会把 text 相同的变体误删；重排会把变体与原字拆散。
    /// - 必须在自动上屏判定**之后**：满码「唯一候选」判定若看到变体会误判不唯一，
    ///   顶码/满码自动上屏被静默否决。
    ///
    /// 词级候选不展开：多字词的 1对多由 STPhrases 词级最长匹配消歧（一出戏→一齣戲）。
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
            // 默认转换结果已由原字候选呈现（显示层 maybe_s2t），变体里滤掉它防止重复。
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

    /// 根据输入缓冲更新候选（动态分级加载：首次小批量，翻页到边界再扩展）。
    /// 返回输入结局（全码自动上屏 / 满码空码清空）；多数调用方忽略，仅正向输入字母时消费。
    pub(crate) fn update_candidates(&self, state: &mut State) -> InputOutcome {
        state.candidates.clear();
        state.preedit = state.input_buffer.clone();
        state.preedit_split_body.clear();
        state.preedit_fp_body.clear();
        state.preedit_abbrev_body.clear();
        state.preedit_codetable_body.clear();
        state.shadow_code.clear();
        if state.input_buffer.is_empty() {
            state.has_more = false;
            state.candidate_input.clear();
            // 缓冲空但有已转换前缀（逐步转换中删空剩余拼音）：组合区仍显示前缀。
            state.preedit = state.committed_text.clone();
            return InputOutcome::Normal;
        }
        let limit = self.initial_candidate_limit(&state.input_buffer);
        let (engine_count, outcome) = self.build_candidates(state, limit);
        // 引导字母的「重复上屏」：输入恰为单个引导字母时，把最近一次上屏内容注入候选顶部
        // （对齐 Go），供「引导键 + 选词」重复上一次输入。资格判定见
        // `leading_letter_repeat_text`——它同时承担「让位那一帧不能空无一物」这个职责。
        //
        // 传**当前帧是否已有候选**：隐式来源（mix 的 repeat 成员）只填空帧，不抢首选，
        // 否则活码字母上按空格会上屏上次内容而非用户刚打的字。
        if let Some(last) =
            self.leading_letter_repeat_text(&state.input_buffer, state.candidates.is_empty())
        {
            state.candidates.insert(
                0,
                Candidate {
                    text: last,
                    natural_order: -1,
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
        outcome
    }

    /// 单个引导字母那一帧的「重复上屏」文本（无资格 / 无历史 → None）。
    ///
    /// # ★★ 它同时是「让位那一帧的反馈」
    ///
    /// 绑了动作的字母按下后，若该字母在本方案是活码前缀（`has_code_prefix`），按键**让位**
    /// 给正常输入——这是对的，否则那个字母开头的编码在本方案彻底打不出来。但让位那一帧
    /// 默认空无一物：五笔 86 的 z 本身是死码，`zz*` 短语又够不着 `input.phrase.min_prefix`
    /// （默认 2）。用户按下 z 只看到一个光秃秃的 `z`，分不清「绑定没生效」还是「还要再按
    /// 一键」（2026-08-08 真机反馈）。
    ///
    /// ⚠️ 曾试图把绑定字母的**短语枚举门槛**降到 1 来填这一帧，被推翻：真机上 `zz*` 是
    /// `1 标点 2 数字 3 字母 4 偏旁` 这样的 `$SS` 分组导航，按 z 就弹出整屏等于把 `zz` 那
    /// 一级的导航提前了一整级，用户设的 `min_prefix` 形同虚设（2026-08-09 用户反馈）。
    /// **反馈和短语是两回事**——短语门槛管「显示策略」，这一帧的反馈另找来源。
    ///
    /// # 资格的两个来源（合并在此，不再各判各的）
    ///
    /// - `z_key_repeat`：z 专有的老开关（repeat 功能本就绑死在 z 上）；
    /// - 该字母绑了 `mix:<id>` 且那个 mix 的 members 含 `quick_input.repeat`：按下去本就是
    ///   要进那个模式，把它**空缓冲帧**的能力提前一格给出来。没有这条，走让位路径的引导键
    ///   永远到不了 mix 的空缓冲帧（夺取路径的 `mix_buffer` 恒等于残余码，至少一个字符），
    ///   那个成员对这类配置形同虚设——用户报的「z 进的快捷输入没有重复输入功能」正是它。
    ///
    /// `frame_empty` = 这一帧的正常候选是否为空，**只约束来源 ②**：
    ///
    /// - ① `z_key_repeat` 是用户显式打开的开关，「按 z 重复上屏」就是它的全部语义，
    ///   有候选也照样抢首选——那是用户要的。
    /// - ② mix members 那条是**隐式**推导的（用户只写了 `z = "mix:…"`，没要求 repeat），
    ///   职责仅限于填补让位后的空帧。绑了动作的字母**恰恰可能是活码**（那正是它让位的
    ///   原因），此时那一帧有真候选，再插到顶上就会让用户按空格上屏上次的内容而不是
    ///   自己打的字。
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

    /// 扩展候选（翻页/下移到边界时调用）：上限翻倍（≤5000）重新加载，保持当前页/高亮。
    pub(crate) fn expand_candidates(&self, state: &mut State) {
        if !state.has_more || state.candidate_input != state.input_buffer {
            return;
        }
        // 辅助码模式：候选列表是「会话快照 + 辅助码筛选」的结果，翻页放宽重建整池会绕过
        // 辅助码筛选、把未过滤候选塞回来（过滤结果丢失）。辅助码按字形筛、结果通常已很小，
        // 不需要放宽，直接不扩展（保持过滤结果）。
        if matches!(state.active, Some(ModeKind::AuxCode)) {
            return;
        }
        let new_limit = (state.candidate_limit.saturating_mul(2)).min(5000);
        if new_limit <= state.candidate_limit {
            state.has_more = false;
            return;
        }
        let prev_len = state.candidates.len();
        // 翻页扩展不消费全码自动上屏（仅正向输入字母时才上屏）。
        let (engine_count, _) = self.build_candidates(state, new_limit);
        // 重建后立刻重新展开变体：prev_len 取自展开后的旧列表，两边须同口径比较，
        // 否则 s2t 开启时新增量会被变体数抵消、误判「已到底」。
        self.expand_s2t_variants(state);
        if state.candidates.len() <= prev_len {
            // 没有新增 → 已到底
            state.has_more = false;
            return;
        }
        state.candidate_limit = new_limit;
        state.has_more = engine_count >= new_limit;
        // 保持当前页/高亮不变（build_candidates 未改动它们）；按当前高亮重算组合区
        // （输入/高亮未变 → 形态不变，仅防御性同步）。
        self.sync_preedit_to_highlight(state);
    }

    /// 若 key_code 是配置的二/三候选键，返回页内候选偏移（1=次选/第2项，2=三选/第3项）。
    ///
    /// 数据源是 `keys.session_actions`（`select_key_groups` 已在 `normalize` 里折算进去）。
    /// 配置里写的是**序号**（`select_candidate:2` = 第 2 个候选），这里减 1 换成偏移——
    /// 配置面向人、偏移面向数组，转换只在这一处发生。
    ///
    /// `include_printable = true`：`;` `'` 这类可打印选词键在**所有**模式下都查得到，
    /// 与折算前逐字一致——各模式要不要让位给输入字符，由各自的消费点判（如临英的
    /// `temp_english_char_allowed`），不在这里一刀切。
    pub(crate) fn select_key_offset(&self, key_code: u32) -> Option<usize> {
        self.session_action_for(key_code, false, true)?
            .candidate_ordinal()
            .map(|n| n as usize - 1)
    }

    /// 选中「当前页第 `offset` 项」（0=首选，1=次选，2=三选），按当前活跃模式派发到各自的
    /// 选中出口。越界（页内不足）返回 `None`，由调用方决定回落语义。
    ///
    /// 各模式的选中语义差异很大（临拼要分步转换、mix 要按透镜、临英/特殊要先判命令候选），
    /// 故本函数**只做派发**，落点全是各模式 keydown 路径上用的同一个函数——两条路径若哪天
    /// 分叉，就会出现「按 `;` 和按 Ctrl 选同一个候选，结果不一样」。
    pub(crate) fn select_page_candidate(
        &self,
        state: &mut State,
        offset: usize,
    ) -> Option<KeyAction> {
        let (start, end) = self.page_range(state);
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
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) => {
                self.commit_special_candidate(state, gi)
            }
            Some(ModeKind::Mix(_)) => self.mix_select(state, offset),
            // 网址模式无候选列表（不出候选窗），没有可选中的东西。
            Some(ModeKind::Url) => return None,
            // 走 `aux_code_committed` 而**不是** `commit_selected` + 无条件收尾：
            // 部分消费（候选只吃掉缓冲前缀）时辅助码要留在模式内重建会话继续筛，
            // 否则「没时间」这类分步组句在按 `2` 时能继续、轻敲 Shift 选同一个候选
            // 却直接退出模式——正是本函数文档里那条「两条路径分叉」的实例。
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

    /// 修饰键（`select_key_groups` 里的 `lrshift` / `lrctrl`）作二三候选键的 **keyup** 入口。
    ///
    /// 为什么在 keyup：见 `hotkey::compile_select_modifier_group`——纯修饰键的 keydown 既不能
    /// 吃（宿主要看到修饰键），在 keydown 上判定又会让 `Ctrl+A` 的第一下 Ctrl 误选候选。
    /// TSF 侧只在「轻敲」（<500ms、中途没按别的键）时才把这个 keyup 转发过来，故收到即可动作。
    ///
    /// 与「同一个修饰键也被配成中英文切换键」的优先级：**有候选选词、无候选切换**。
    /// 返回 `None` 即让位给下游的 toggle 判定，故本函数必须先于它调用。
    ///
    /// 越界（页内候选不足 2/3 项）**吞键**，既不选也不切换，且不套 `keys.overflow.select_key`：
    /// 那三档里有两档要「输出该键的字符」，而修饰键没有字符可输出，套过来只剩半截语义。
    /// 吞键而非落到切换，是为了让「有候选时按它绝不切中英文」成为无例外的规则——否则
    /// 候选恰好只有 1 个的那次会突然切走中英文，是最难复现也最恼人的一类不确定行为。
    pub(crate) fn handle_select_key_up(&self, data: &KeyEventData) -> Option<KeyAction> {
        // 只认 keyup-only 键。可打印选词键（`;` `'`）在 keydown 路径消费，万一哪天 TSF
        // 也转发它们的 keyup，不能在这里被重复选一次。
        //
        // ⚠️ 判据取 `is_key_up_only_vk` 而非区间 `VK_LSHIFT..=VK_RCONTROL`：CapsLock（0x14）
        // 与那四个修饰键**不连号**，用区间写就把它漏在门外了——而 CapsLock 是这批键里唯一
        // 连 keydown 都拿不到的（C++ 压根不发，钩子路径合成的也是 keyup）。漏掉的表现是
        // `capslock = "select_candidate:3"` 配了完全没反应：`apply_session_action` 对选词类
        // 动词一律 `return None`（选词带 overflow 语义，要落到各自的既有消费点执行），而
        // CapsLock 的两条路都没有「继续往下走」，None 就是终点。见 project_modifier_key_as_function_key。
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

    /// 手动分隔符判定的单一入口：`key_code` 是否应作为分隔符 `'` 压入缓冲。
    ///
    /// 每次按键实时求值（不缓存），使 `separator` 或 `select_key_groups` 热更新即时生效。
    /// 规则（对齐 Go `pinyin_mode_shared.go` 真 `auto` 语义）：
    /// - **码表整句**方案同样放行：分隔符是整句的消歧手段（二简「旬 qj」与一简
    ///   「我 q」+「是 j」的击键串完全同形，打分无从区分），且键位判定与拼音**完全共用**
    ///   —— 用户不该为两个方案记两套键。
    /// - 其余非拼音引擎 → 恒 false。**双拼已放行**（引擎侧支持见
    ///   `docs/design/shuangpin-separator.md`），但出厂 `separator = "none"`。
    /// - `none` → false；`quote` → 仅引号键(VK_QUOTE)；`backtick` → 仅反引号键(VK_BACKTICK)。
    ///   显式模式尊重用户指定值，不做动态判定（显式 quote 即用户自选覆盖选键行为）。
    /// - `auto`（默认/未知值）→ 动态挑一个**空闲**符号键：`'`(VK_QUOTE) 未被占作选词键
    ///   且未被本方案 `[key_actions]` 绑定时用它；否则退而看反引号，同样两项都空闲才用。
    ///   两个都不空闲时 auto 不启用分隔符（双拼出厂即如此：`'` 是选词键、反引号归辅助码）。
    ///
    /// 缓冲是否为空由调用方判定（空缓冲维持标点路径）。
    pub(crate) fn manual_separator_key(&self, key_code: u32) -> bool {
        self.manual_separator_key_of(key_code, &self.engine_mgr.active_schema_id())
    }

    /// 同 [`Self::manual_separator_key`]，但判据取**指定方案**而非活跃方案。
    ///
    /// ★★ overlay 必须走这条。临拼在纯五笔方案下引用的是拼音方案，而
    /// `engine_mgr.is_pinyin()` 问的是**活跃**引擎（= 码表）⇒ 恒 false ⇒ 临拼下分隔符
    /// 永远不生效，且完全静默。这与 `update_temp_pinyin_candidates` 里
    /// 「`ignore_weight` 必须按临拼目标方案取，不能用 `active_base_sort_ignores_weight()`」
    /// 是**同一个错误的两个实例**——凡「按当前引擎类型决定行为」的判据，在 overlay 里
    /// 都需要一个 `_of(&schema)` 版本，否则拿主方案的性质去裁决 overlay 的行为。
    ///
    /// `separator` 本身也按方案取（见 `EngineManager::pinyin_separator_mode_of`）：
    /// 临拼引用哪个拼音方案，就用那个方案生效的分隔符策略。
    pub(crate) fn manual_separator_key_of(&self, key_code: u32, schema_id: &str) -> bool {
        use wind_keys::keymap::{VK_BACKTICK, VK_QUOTE};
        // 码表整句与拼音共用下面那套键位判定；两者都不成立时该键维持原本语义。
        //
        // ★ 双拼同样放行（原先在此早退，理由「buffer 会与 preedit 发散」）。发散那件事
        // 已在引擎侧解决：`ShuangpinConverter::convert` 把 `'` 当配对硬边界自行消化，
        // preedit 逐字节重建使手动与自动的分隔符合流，`consumed_length` 也把那一键算进去。
        // 见 `docs/design/shuangpin-separator.md`。
        //
        // ⚠️ 放行的是**键位判定**，不是默认开启：双拼出厂 `separator = "none"`
        // （反引号在双拼里归辅助码，见 `shuangpin.schema.toml`），要用得显式配成 `quote`。
        let pinyin_ok = self.engine_mgr.is_pinyin_of(schema_id);
        if !pinyin_ok && !self.engine_mgr.sentence_input_enabled_of(schema_id) {
            return false;
        }
        match self.engine_mgr.pinyin_separator_mode_of(schema_id).as_str() {
            "none" => false,
            "quote" => key_code == VK_QUOTE,
            "backtick" => key_code == VK_BACKTICK,
            // auto（及其它未知值兜底）：挑一个当前**空闲**的符号键作分隔符。
            //
            // 两类占用都要避让：
            //  ① 选词键——出厂 `semicolon_quote` 含 `'`，夺走它等于让三选不可用；
            //  ② 本方案 `[key_actions]` 已绑的功能键——分隔符臂在按键裁决里排在 key_actions
            //     之前，夺走的话那条绑定**只剩一条 warn 日志**，用户看到的是「配了没反应」。
            //
            // ★ 两个键都不空闲时 auto 退化成「不启用」。双拼正是这种情况：`'` 是选词键、
            // 反引号出厂给了辅助码（`shuangpin.schema.toml`），于是双拼虽然支持分隔符，
            // auto 下却不会自动占用任何键——要用得显式写 `separator = "quote"`。
            //
            // ⚠️ 这条避让**不能改成「双拼一律不给分隔符」**：那会让用户显式配的 `quote`
            // 也一并失效，而显式模式的含义就是「我知道我在做什么」。判据的维度是
            // 「这个键空不空闲」，不是「这是什么引擎」。
            _ => {
                let free = |vk: u32| {
                    self.select_key_offset(vk).is_none()
                        && self.bound_action_in_schema(vk, schema_id).is_none()
                };
                if free(VK_QUOTE) {
                    key_code == VK_QUOTE
                } else {
                    key_code == VK_BACKTICK && free(VK_BACKTICK)
                }
            }
        }
    }

    /// 若 key_code 是配置的以词定字键，返回取字下标（0=取第 1 字，1=取第 2 字）。
    ///
    /// 数据源是 `keys.session_actions`（`select_char_keys` 已在 `normalize` 里折算进去）。
    /// 出厂 `select_char_keys` 为空 → 折算不出 `select_char:*` → 恒返回 None（功能禁用）。
    ///
    /// ⚠️ 折算前这里与 [`Self::select_key_offset`] 用的是**两张不同的键组表**，曾被张冠
    /// 李戴过一次（用选词键组的解析器解以词定字配置，`brackets` 静默失效）。收编进同一张
    /// 表后，两者靠**动词**区分而不再靠解析器区分，那类错配从结构上消失了。
    pub(crate) fn select_char_index(&self, key_code: u32) -> Option<usize> {
        self.session_action_for(key_code, false, true)?
            .char_ordinal()
            .map(|n| n as usize - 1)
    }

    /// 当前页候选切片的 [start, end) 区间
    pub(crate) fn page_range(&self, state: &State) -> (usize, usize) {
        let pp = self.per_page(state.active);
        let start = state.current_page * pp;
        let end = (start + pp).min(state.candidates.len());
        (start, end)
    }

    /// 当前高亮候选的全局下标（页起点 + 页内高亮）
    pub(crate) fn highlighted_global_index(&self, state: &State) -> usize {
        let (start, _) = self.page_range(state);
        start + state.selected_index
    }

    /// 组合区「正文」形态选择（不含已转换前缀）：对齐微软五笔——按**当前高亮候选**的类型决定。
    /// - 无拆分形态（码表/无拼音，preedit_split_body 空）→ 恒原始码（input_buffer）。
    /// - 高亮候选为拼音来源 → 音节拆分串（preedit_split_body，如 baoan 的拼音 / saaa 的 sa'a'a）。
    /// - 高亮候选为码表/五笔（或短语等非拼音）→ 原始码（input_buffer，如 saaa 选「模式」时不拆）。
    fn effective_preedit_body<'a>(&self, state: &'a State) -> &'a str {
        // 码表整句：高亮到整句候选时按**编码单元**切分显示（`aawt'aawt`）。
        //
        // ★ 必须排在 `preedit_split_body.is_empty()` 那道守卫**之前**：码表方案下拼音
        // 拆分形态恒空，守卫会直接 return 原始码，后面的分支一条都走不到。
        // （混输方案下两者可能同时非空 —— 那时正是靠下面各分支按高亮候选各选各的。）
        if wants_codetable_split(
            &state.preedit_codetable_body,
            state.candidates.get(self.highlighted_global_index(state)),
        ) {
            return &state.preedit_codetable_body;
        }
        if state.preedit_split_body.is_empty() {
            return &state.input_buffer;
        }
        // 分段上屏进行中（committed 前缀非空）：剩余编码已被 build_candidates 强制走拼音方案
        // 转换，故恒按拼音拆分显示，不再按候选来源切换——否则高权重短语候选顶到首位时，
        // 后段会被显示成原始码形态（看似「又以五笔处理」）。与 build_candidates 强制拼音对齐。
        if !state.committed_text.is_empty() {
            return &state.preedit_split_body;
        }
        let hi = self.highlighted_global_index(state);
        let cand = state.candidates.get(hi);
        // ★ 下面三条「按高亮选形态」的分支都必须在这里判，不能由引擎就地算定：引擎每次
        // 按键只 convert 一次，而翻页 / 方向键移动高亮都发生在那之后，就地算定的形态不会
        // 跟着变。本函数由 `sync_preedit_to_highlight` 在每次高亮变化时重算，是唯一能跟住
        // 的位置。三条也都与最后那条 `source == Pinyin` 判据分开：双拼流、全拼支路、简拼
        // 候选的**来源同为 Pinyin**，那条分不开它们；且它的两个分支（拆分串 / 原始码）
        // 都不是这里要的切法。

        // 双拼下的**简拼候选**：编码栏按简拼切法显示（`w'b'w'n`），而不是双拼的 `wbwn`／
        // `wf'wt`——四个字的候选「万般无奈」配着一段两键式编码，用户看不出自己打的是简拼。
        //
        // 排在全拼降级那条**之前**：双拼的全拼降级支路把击键当全拼再查一遍，查出来的词条
        // 同样可能是简拼命中，于是两个标记同真。那时简拼切法才是对的——全拼切法
        // (`compose_segment`) 会把 `wbwn` 按音节最大匹配乱切一气。
        if !state.preedit_abbrev_body.is_empty() && cand.is_some_and(|c| c.is_abbrev) {
            return &state.preedit_abbrev_body;
        }
        // 双拼下的**全拼降级候选**：编码栏按全拼切法显示（`zai'jian`），而不是双拼的
        // `za'ij'ia'n`——三段编码配着两字候选「再见」，用户看不懂，退格时更会以为光标错位。
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
            // 生僻字模式与 special 共用 `special_buffer`：按键处理走的是同一个
            // `handle_special_key`，缓冲另起一个字段的话，退格与光标会作用在一个没人读的
            // 字段上——组合区不动、候选不变，且没有任何报错。
            ModeKind::Special(_) | ModeKind::RareChar => {
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
            ModeKind::Special(_) | ModeKind::RareChar => (
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
            // 辅助码：主体 = 辅助码缓冲，前缀 = overlay 里进入时拼好的显示前缀（基线 + 分隔符，
            // 只写一遍）。光标恒在串尾。
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

    /// 组合区光标在 preedit 显示串内的**字节偏移**，供自绘候选窗画插入符。
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
        let moved = {
            let mut ed = Self::overlay_buf_edit(state)?;
            match data.key_code {
                keymap::VK_LEFT => ed.move_left(),
                keymap::VK_RIGHT => ed.move_right(),
                keymap::VK_HOME => ed.home(),
                _ => ed.end(),
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

    /// 回退最后一个已转换段：把它消费的码并回剩余编码**前部**并重转候选。
    /// Backspace（段回退优先于光标）与 Delete（删空剩余编码后）共用，对齐 Go
    /// `handleBackspace` / `popConfirmedSegment`。
    ///
    /// 光标一律拉到剩余编码末尾：回退的码插在缓冲前部，光标留在原处会落进这段码中间，
    /// 语义不清。无段可退时（理论边界）吃掉按键，不透传。
    pub(crate) fn pop_committed_seg(&self, state: &mut State) -> KeyAction {
        // 并回缓冲的必须是 raw_code（原始输入空间），不是全拼 code：双拼下后者会把
        // `hao` 塞进击键缓冲，被重解析成 `ha|o` 而整串错乱。
        let Some((raw_code, _, _, _, _)) = state.committed_segs.pop() else {
            return KeyAction::Consumed;
        };
        state.committed_text = state
            .committed_segs
            .iter()
            .map(|(_, _, t, _, _)| t.as_str())
            .collect();
        // 回退段的码并回缓冲**前部** → 影子串同步前置（并回的码来自已确认段，恒小写）。
        if !state.input_buffer_cased.is_empty() {
            state.input_buffer_cased = format!("{}{}", raw_code, state.input_buffer_cased);
        }
        state.input_buffer = format!("{}{}", raw_code, state.input_buffer);
        state.input_cursor_pos = state.input_buffer.len();
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

    /// 按当前高亮候选类型重算 `state.preedit`（混输高亮跟随）。含已转换前缀（逐步转换）拼接。
    /// 仅普通模式（active==None）有意义；覆盖层模式各自维护 preedit，不应调用此方法。
    pub(crate) fn sync_preedit_to_highlight(&self, state: &mut State) {
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
    /// 含表达式来源（`quick_input.calc/.date/.number`）的 mix 不把 `-`/`=` 当导航——那里
    /// 它们是运算符输入。由 active 自判。
    ///
    /// 判据是 `mix_has_quick_numeric` 而非 `mix_has_quick_input`：只配了 `quick_input.repeat`
    /// 的 mix 没有表达式录入，`-`/`=` 仍应是翻页键。
    ///
    /// 临英**逐键**动态判定，既不是恒排除也不是整类开关：
    /// - 恒 `false`（最初版）：`allow_symbols` 关时符号本就进不了缓冲，`-`/`=` 不承担任何
    ///   输入语义，却落到 `_ =>` 标点臂被判成「上屏高亮候选 + 标点 → 退出」——用户按 `=`
    ///   想翻页，实得首候选被直接上屏。
    /// - 读 `allow_symbols`（第二版）：整类让位。为了打 `e-mail` 的一个 `-`，`=`/`[`/`]`/
    ///   `,`/`.` 的翻页能力全部一起赔进去，即用户所说的「过于宽泛」。
    /// - 读 `symbol_chars` 白名单（本版）：只有该键实际产出的字符被列入时它才让位输入，
    ///   同键组的另一半（列了 `-` 没列 `=`）照旧翻页。判据必须取**字符**而非键——`+` 是
    ///   Shift+=、`@` 是 Shift+2，同一个键的两个 shift 态归白名单分别管辖。
    ///
    /// ★ 这是「静态类别判据 vs 动态配置判据」错配的第二次修正：上次把恒 false 改成读开关，
    /// 这次把读开关改成读字符。凡按 `ModeKind` 整类给值的门控，都要复查它对每个模式的取值
    /// 是否仍成立——`include_printable` 的历史注释里写死过「临英要输符号」这个静态假设。
    pub(crate) fn handle_candidate_nav(
        &self,
        state: &mut State,
        data: &KeyEventData,
    ) -> Option<KeyAction> {
        let include_printable = match state.active {
            // 生僻字模式与 special 取值必须一致：两者走的是同一个 `handle_special_key`，
            // 这里给不同的值就会出现「同一套按键处理，翻页键在两个模式里表现不同」。
            Some(ModeKind::Special(_)) | Some(ModeKind::RareChar) | Some(ModeKind::TempPinyin) => {
                true
            }
            // mix 目前**不经过本函数**（`handle_mix_key` 直接调 `apply_session_action`）。保留本
            // 分支只为「日后有人把 mix 接过来时规则仍然对」，取值统一问
            // `mix_nav_include_printable`——此前这里独立写了一份且与活代码取值相反。
            Some(ModeKind::Mix(idx)) => self.mix_nav_include_printable(idx),
            // 非可打印键（PgUp/方向键/Tab）`punct_char` 返回 None，恒作导航——它们本就不在
            // `printable` 绑定里，`include_printable` 对其取何值都不改变结果。
            Some(ModeKind::TempEnglish) => {
                let shift = data.modifiers & MOD_SHIFT != 0;
                punct_char(data.key_code, shift)
                    .is_none_or(|ch| !self.temp_english_char_allowed(ch))
            }
            _ => false,
        };
        self.apply_session_action(state, data, include_printable)
    }

    /// 提交某个候选（记录原始简体词频后清空状态），返回上屏文本（按需简繁转换）。
    /// `s2t_override`：1对多变体候选的输出覆盖（`Candidate::s2t_override`）；来源无候选
    /// 实体（如自动上屏取首选文本）时传 None。
    ///
    /// `freq_code`：词频记账码，须为 [`Self::freq_code`] 的结果——**按候选来源分流**：拼音/英文
    /// 取候选存储码（全拼扁平域；双拼/分隔符/前缀补全下与输入缓冲不同域），码表取输入码
    /// （`d`/`de`/`def` 是独立码位，用词条全码会让它们串扰）。
    ///
    /// **刻意做成显式入参而非在此取 `state.input_buffer`**：本函数只拿得到 `text`，
    /// 同文多候选无从反查，交由每个调用点交代码来源（同 `add_user_word` 的 `boundary`
    /// 入参先例）。
    pub(crate) fn commit_candidate(
        &self,
        state: &mut State,
        text: &str,
        s2t_override: Option<&str>,
        source: CandidateSource,
        freq_code: &str,
    ) -> String {
        self.record_selection(freq_code, text, source);
        let mut out = match s2t_override {
            Some(t) => t.to_string(),
            None => self.maybe_s2t(state, text),
        };
        if self.english_appends_space(source, text, &state.input_buffer) {
            out.push(' ');
        }
        state.input_buffer.clear();
        state.preedit.clear();
        state.candidates.clear();
        self.reset_candidate_view(state);
        out
    }

    /// 用户数据（词频 / 候选调整 / 用户词库）的**生效方案**：这一次按键的候选是哪个方案的
    /// 引擎出的。
    ///
    /// 普通输入 = `active`；**特殊模式 = 它引用的方案**——特殊方案与主方案同层级，有自己的
    /// 词库和用户数据，只是用特殊按键进入，按 `active` 归属会把它的账全记到主方案头上。
    ///
    /// **临英 = 英文方案**（`ENGLISH_SCHEMA`）：临英与英文方案打的是同一份词库、同一个方案
    /// id，差别只在进入方式（Shift+字母 overlay vs 常驻方案）。归属到同一个桶，才谈得上
    /// 「临英里选过的词，切到英文方案也受益」。不给它落点的话，读写两端会同时缺席——
    /// 词频记不进、候选调整存不下，而这两件事各自失效都是完全静默的。
    ///
    /// ★ 判据是**模式是不是临英**，不能图省事复用 `overlay_engine_schema`：那个在
    /// `show_candidates = false` 时返回 `None`（它回答的是「要不要出候选」），拿它当落点，
    /// 用户一关候选显示，词频就静默换到主方案的桶里去。
    ///
    /// ⚠️ **临拼 / 快捷输入刻意不在此分流**（2026-08-04 用户拍板）：它们走
    /// `write_data_schema_id` 的按候选来源分流，实测行为正确（临拼记进 `"pinyin"`、全拼
    /// 双拼共享一份），改动风险大于收益。往这里加模式前先确认那条路径不够用。
    ///
    /// 返回 `None` = 没有特殊归属，调用方走原有的 active 路径。
    pub(crate) fn effective_data_schema(&self, state: &State) -> Option<String> {
        match state.active {
            Some(ModeKind::Special(idx)) => self.special_schema(idx),
            Some(ModeKind::TempEnglish) => Some(ENGLISH_SCHEMA.to_string()),
            _ => None,
        }
    }

    /// 候选词条操作（右键菜单 / Ctrl+数字热键 / macOS 禁用位）的**作用域**；
    /// 返回 `None` = 当前状态不支持词库操作，调用方只保留复制。
    ///
    /// # 为什么三个调用方必须共用这一个函数
    ///
    /// 菜单可用性与写端落点若各判各的，错配是**完全静默**的。快符模式此前正是如此：
    /// 菜单被 `overlay` 判据整块拒开（只剩复制），而写端 `candidate_op` 又因
    /// `input_buffer` 恒空而首行 return——两处独立地失效，修好任一处都看不出效果。
    ///
    /// # 放行集合刻意与 [`Self::effective_data_schema`] 绑死
    ///
    /// 那里正是读端（`apply_shadow_in` / `apply_freq_rerank_in` / `record_selection_in`）
    /// 取归属方案的地方。「落点存在」与「操作可行」本就是同一件事；若在此另写一份
    /// 「哪些模式允许」的清单，两份判据迟早漂移成「写进 A、读的是 B」——记账看着成功，
    /// 候选顺序永远不动。
    ///
    /// # 空 `code`：主输入路拒绝，特殊模式放行
    ///
    /// 主输入路空码本就无候选，无从操作。特殊模式开 `show_all_on_enter` 时**空码也会枚举出
    /// 候选**，这批候选必须可调——`max_code_length=1` + `auto_commit_at_full` 的快符方案敲
    /// 一码即上屏，浏览态是用户**唯一**能右键的时机。空码是合法的 shadow 键位，读端
    /// [`Self::apply_shadow_in`] 已按候选非空（而非码非空）放行，读写两端就此对齐。
    pub(crate) fn candidate_op_scope(&self, state: &State) -> Option<CandidateOpScope> {
        // 没有候选就没有作用域可言（如 show_all_on_enter 关闭时刚进入特殊模式的空白态）。
        if state.candidates.is_empty() {
            return None;
        }
        let (schema, code, raw_code, special) = match state.active {
            // 主输入路的 shadow 码取**归一形态**（见 `shadow_code_of`）：双拼下是全拼码，
            // 与读端 `apply_shadow` 同源。全拼/码表下它恒等于 `input_buffer`，行为不变。
            None => (
                self.engine_mgr.active_schema_id(),
                Self::shadow_code_of(state).to_string(),
                state.input_buffer.clone(),
                false,
            ),
            // 特殊模式（快符 / 生僻字等自定义码表方案）：编码在 special_buffer，归属是它
            // 自己引用的方案——与读端同源取值，见上方 effective_data_schema。
            // 码表方案没有第二编码域，两个码恒同值。
            Some(ModeKind::Special(_)) => (
                self.effective_data_schema(state)?,
                state.special_buffer.clone(),
                state.special_buffer.clone(),
                true,
            ),
            // 生僻字模式：编码在 special_buffer（与 special 共用），归属是**当前活跃方案**
            // ——它就是用这个方案的编码在输入，`effective_data_schema` 对它返回 None 正是
            // 「没有特殊归属、走 active」的意思，故这里直接取 active 而不是调那个函数。
            //
            // 落点必须补：用户在生僻字模式里找到字之后，最想做的一件事恰恰是右键「设为
            // 常用字」——那正是常用字覆盖功能的主场景。不补落点就只剩一个复制。
            // `special = false`：它没有 `show_all_on_enter` 浏览态，空码时本就没有候选。
            Some(ModeKind::RareChar) => (
                self.engine_mgr.active_schema_id(),
                state.special_buffer.clone(),
                state.special_buffer.clone(),
                false,
            ),
            // 临时英文：归属恒是内置英文方案（与读写两端同源，见 effective_data_schema）。
            // 码取**小写化的缓冲**——临英缓冲带大写（Shift+H 进入即 `H`），而英文方案下
            // `input_buffer` 恒为全小写；不归一的话「临英里置顶的词，切到英文方案不生效」，
            // 反过来也一样，两个入口各自存了一份键。`raw_code` 保留原形供展示。
            Some(ModeKind::TempEnglish) => (
                self.effective_data_schema(state)?,
                state.temp_english_buffer.to_lowercase(),
                state.temp_english_buffer.clone(),
                false,
            ),
            // 其余 overlay（临拼 / 混输 / 网址）没有独立词库落点：`effective_data_schema`
            // 对它们返回 None（2026-08-04 用户拍板，见其文档），放行会静默落回主方案。
            // 新增模式时**先补落点、再放行**，顺序不能反。
            _ => return None,
        };
        // 空码只在特殊模式（浏览态）合法；主输入路空码无候选。
        if code.is_empty() && !special {
            return None;
        }
        // 调位判据要问的是「**出这批候选的**引擎是不是拼音」。`current_engine_type()` 答的
        // 是主方案——overlay 下照抄它，会在「主方案是拼音 + 快符是码表」时把本该可调位的
        // 快符候选整体误禁（反之亦然）。`loaded_engine_type` 正是为 overlay 分流准备的。
        //
        // 判据是「归属方案是不是 active」而非「是不是特殊模式」：临英的归属同样不是 active
        // （主方案可能是五笔），按后者分流会拿五笔的引擎类型去判英文候选。两者相等时
        // 两个取值本就同源，故对既有的特殊模式路径逐字节等价。
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

    /// 英文上屏后补空格的**方案口径**（`schema.english.commit_space` + 当前是英文方案）。
    ///
    /// 供**无候选可依**的出口使用——「空格上屏原码」上屏的是输入缓冲本身
    /// （`CandidateSource::None`），拿不到来源，只能按方案判定。
    ///
    /// 判「用户此刻正在打英文」这一条不可省：`CandidateSource::English` 在混输、快捷输入里
    /// 同样出现，而那些场景用户正在写中文句子，插个英文词后面平白多个空格是错的。
    ///
    /// **临时英文算在内**：它与英文方案打的是同一份词库，用户意图同样是「连着打英文词」，
    /// 差别只在进入方式。混输/快捷里的英文候选则不算——判据问的是**当前整个输入语境**是不是
    /// 英文，不是这一条候选来自哪里。
    ///
    /// ⚠️ 注意本项与同段的 `frequency.code_scope` **判据相反**：那个按**候选来源**生效
    /// （英文候选走到哪都该按同一口径记账），这个按**输入语境**。改动其一时别照着另一个抄。
    pub(crate) fn english_space_enabled(&self) -> bool {
        self.english_space_for(false)
    }

    /// 同 [`Self::english_space_enabled`]，但把 overlay 语境一并算进去。
    ///
    /// 有 `state` 的出口一律用这个；`english_space_enabled` 留给 DLL 侧 IPC 排水那条
    /// **拿不到模式上下文**的路径。两者共用下面的单一真相源，免得日后漂移成
    /// 「键盘上屏补了、排水路径没补」。
    pub(crate) fn english_space_enabled_in(&self, state: &State) -> bool {
        self.english_space_for(state.active == Some(ModeKind::TempEnglish))
    }

    /// 补空格判据的单一真相源：开关 + 「英文语境」（英文方案常驻 或 临英 overlay）。
    fn english_space_for(&self, in_temp_english: bool) -> bool {
        self.rt().config.schema.english.commit_space
            && (self.engine_mgr.active_is_english() || in_temp_english)
    }

    /// 英文**候选**上屏后是否补一个空格：方案口径之上，再要求「这条候选算英文内容」。
    ///
    /// # 两个来源，两条理由
    ///
    /// - `source == English`——英文方案里同样会出现短语等其它来源的候选，那些不该补空格；
    /// - `text == input`（上屏的就是**所打原码**）——`commit_space` 的生效范围里本就写着
    ///   「**空格上屏原码**（打了词库里没有的词）」。头部候选（见 `crate::english_candidates`）
    ///   刻意不带 `source`（带了就会往词频表里写读端永远查不中的孤儿键），于是它把「上屏
    ///   原码」这条路从「无候选兜底分支」变成了「选中首候选」——只认 `source` 的话，这条
    ///   既有契约就静默漏补了（`english_commit_space.rs` 六条一起红）。
    ///
    /// ★ 第二个判据落在**上屏文本是否等于当前输入串**，而不是「这条候选有没有 `source`」
    /// ——后者是实现细节，前者才是「这是不是原码」的定义。
    ///
    /// ⚠️ 非英文方案不会误中：`english_space_enabled` 已经要求 `active_is_english()`。
    pub(crate) fn english_appends_space(
        &self,
        source: CandidateSource,
        text: &str,
        input: &str,
    ) -> bool {
        (source == CandidateSource::English || (!input.is_empty() && text == input))
            && self.english_space_enabled()
    }

    /// 词频记账用的码——**码表与拼音/英文口径不同**，这是两类方案的语义差异。
    ///
    /// | 来源 | 用哪个码 | 理由 |
    /// |---|---|---|
    /// | 码表 | **输入码** | `d`/`de`/`def` 是三个**独立码位**，各自的首选独立调整 |
    /// | 拼音 / 英文 | 候选码（[`Self::cand_code`]） | 候选码是词的读音/拼写，跨码位共享才对 |
    ///
    /// 码表候选的 `code` 是**词条全码**而非用户输入：`de` 下的「有」带的是 `def`
    /// （前缀补全）。拿它当词频 key，会让「在任一码位选中」影响「所有能召回它的码位」
    /// ——真机实测在 `de`/`def` 下选「有」后，打 `d` 时它也跟着前移。而
    /// `ProtectPolicy` 按输入码长分级保护首选，前提正是「码位彼此独立」。
    ///
    /// 拼音反过来：打 `d` 选「东西」后，打 `dongxi` 也该受益——那里候选码恒是完整读音，
    /// 跨码位共享是想要的行为。英文同理（打 `hel` 选 `hello`，打 `he` 也该受益）。
    ///
    /// ⚠️ **读写两侧必须同口径**：写入端 `record_selection` 与读取端 `apply_freq_rerank`
    /// 都要经本函数，否则记了查不到、或查到不该查的。
    pub(crate) fn freq_code(&self, buf: &str, cand: &Candidate) -> String {
        Self::freq_code_with(
            buf,
            cand,
            self.engine_mgr.freq_settings().english_code_by_input,
        )
    }

    /// [`Self::freq_code`] 的显式传参版本，供**热路径**使用。
    ///
    /// `apply_freq_rerank` 要对整页候选逐个取码，走实例方法会每候选取一次 `freq_settings`
    /// （带锁的缓存查询）。那里在循环外取一次布尔量传进来即可。
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
    /// **全拼语义**（双拼下为 `hao` 而非击键 `hc`），供词频记账与自动造词。
    /// 退格回退需要的是同域于缓冲的码，见 `raw_consumed_code`。
    pub(crate) fn cand_code(buf: &str, cand: &Candidate) -> String {
        if cand.code.is_empty() {
            buf.to_string()
        } else {
            cand.code.clone()
        }
    }

    /// 分步上屏的「回退码」：本次从缓冲**实际切走**的那一段原始输入。
    /// 双拼下即击键（`hc`），与 `cand_code` 的全拼码（`hao`）不同域——`consumed_length`
    /// 已由引擎回映射到原始输入空间，故直接按它切缓冲即得。`partial=false`（消费整串）
    /// 时整个缓冲都被消费。调用方须已确认 `consumed` 落在字符边界上。
    pub(crate) fn raw_consumed_code(buf: &str, consumed: usize, partial: bool) -> String {
        if partial {
            buf[..consumed].to_string()
        } else {
            buf.to_string()
        }
    }

    // 主输入路拼音选词 —— 组合区逐步转换（C）。此段说明本节整体行为，不属于下面任何单个
    // 函数（原属的函数已迁走），故用普通注释：
    // 部分匹配（候选只消费缓冲前缀）：把汉字并入 `committed_text` 前缀、裁剪缓冲、重转剩余，
    // **留在组合区不上屏到应用**，返回 UpdateComposition。
    // 完整匹配（消费整串）：整体上屏 `committed_text + 候选` 到应用，触发自动造词（L），清空。
    // 此处原有 `clamp_candidate_display`（换行/制表→空格 + 可选截断），已随「短语候选
    // 不再一行化」删除：它唯一的两个调用点都传 max=0，实际只在做一行化，而一行化会连
    // 上屏文本一起改掉。杜绝多行候选现由渲染层 `wind_ui` 的 `visible_whitespace` 承担
    // （换行→↵、制表→⇥，只投影不改数据），长度截断由 `UiCandidateConfig::truncate_display`
    // 负责——两个显示层关注点都已各归其位，数据层不该再有同类改写。

    /// 前缀导航候选选中：把输入缓冲补全到该组完整码并重查候选（展开成员/精确命令），
    /// 实现"敲 zz → 选标点 → 展开标点字符"的二级选择。返回新 preedit 显示文本。
    pub(crate) fn complete_to_group_code(&self, state: &mut State, group_code: &str) -> String {
        state.input_buffer = group_code.to_string();
        state.input_cursor_pos = state.input_buffer.len();
        let _ = self.update_candidates(state);
        self.notify_ui_update(state);
        state.preedit.clone()
    }

    pub(crate) fn commit_selected(
        &self,
        state: &mut State,
        cand: &Candidate,
        candidate_pos: i32,
    ) -> KeyAction {
        // 前缀导航候选：补全输入到该组完整码并重查展开（二级选择，不上屏组名）。
        if cand.is_group {
            let code = cand.group_code.clone();
            let display = self.complete_to_group_code(state, &code);
            return KeyAction::UpdateComposition {
                caret_pos: display.chars().count() as u32,
                text: display,
            };
        }
        // $CC 命令候选：执行动作而非上屏 display 标签。
        if cand.is_command {
            return self.commit_command(state, cand);
        }
        // 联想候选**没有编码**（它是上屏之后按上文给的），凡以「输入码」为 key 的加工
        // 都不适用：词频会记进一条空码行（读端 `apply_freq_rerank` 要求码非空，永远查不到，
        // 只是逐日累积垃圾），自动造词会拿它去凑一个用户从没打过的词。
        // 判据统一是来源，与短语「有文本无码位、恒不记词频」同一先例。
        let from_assoc = cand.source == CandidateSource::Assoc;
        let total = state.input_buffer.len();
        let consumed = cand.consumed_length;
        let code = Self::cand_code(&state.input_buffer, cand);
        let partial =
            consumed > 0 && consumed < total && state.input_buffer.is_char_boundary(consumed);
        // 词频记账走 `freq_code` 而非上面的 `code`：码表按**输入码**（码位独立），
        // 拼音/英文按候选码（分段时为前缀码，如「ni」而非整串「nihao」）。
        // 上面的 `code` 仍供 `record_commit` 统计码长使用，那是另一套语义。
        if !from_assoc {
            self.record_selection(
                &self.freq_code(&state.input_buffer, cand),
                &cand.text,
                cand.source,
            );
        }
        // 输入统计：每次选词记一段（分段逐字选各段各记一次，不重复整串）；
        // 在 partial 分支之前，两分支都经此处一次。
        self.record_commit(
            &cand.text,
            code.len() as u32,
            candidate_pos,
            wind_store::stats::CommitSource::Candidate,
        );
        let raw_code = Self::raw_consumed_code(&state.input_buffer, consumed, partial);
        if partial {
            state.committed_segs.push((
                raw_code,
                code,
                cand.text.clone(),
                cand.source,
                cand.boundary,
            ));
            state.committed_text.push_str(&cand.text);
            state.input_buffer = state.input_buffer[consumed..].to_string();
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
            // 联想候选不进已转换段、不喂造词：那两者都以「这一段是用什么码打出来的」为
            // 前提，而联想没有码。
            if !from_assoc {
                state.committed_segs.push((
                    raw_code,
                    code.clone(),
                    cand.text.clone(),
                    cand.source,
                    cand.boundary,
                ));
            }
            // 上屏文本：联想的**显示文本是整词**（「中国」），而屏幕上已经有「中」了，
            // 真正要补出去的只有 `commit_override` 里那半截。见 `Candidate::commit_override`。
            let final_simplified = format!(
                "{}{}",
                state.committed_text,
                cand.commit_override.as_deref().unwrap_or(&cand.text)
            );
            let learned_code = if !from_assoc {
                // 自动造词：多段组成的词，或一次选中的整句解（后者只有一段，
                // 靠 `is_sentence` 放行——见 `learn_phrase_on_commit` 的「为什么单段整句要单独放行」）。
                self.learn_phrase_on_commit(state, cand.is_synthesized)
            } else {
                None
            };
            // 6b: 临时词使用累积（对齐 Go LearnWord-on-commit）：选中临时层候选也推进晋升计数。
            // 点查代替候选层标记：一次 redb 读，未命中即非临时词，零成本略过。
            // is_group/is_command 已在 commit_selected 入口提前返回；is_phrase 由本条件显式过滤
            //（短语无临时词晋升语义），此处均为普通候选。
            //
            // **刚由造词写入的那条要跳过**：单段整句时造词的 key 与这里的点查完全相同，
            // 不跳就是同一次上屏 count +2（见 `learn_phrase_on_commit` 的返回值说明）。
            if !cand.is_phrase
                && learned_code.as_deref() != Some(code.as_str())
                && let Some(store) = &self.store
            {
                let active = self.engine_mgr.active_schema_id();
                if let Some(schema) = self.engine_mgr.write_data_schema_id(&active, cand.source)
                    && let Ok(Some(_)) = store.get_temp_word(&schema, &code, &cand.text)
                {
                    let promote_count = if self.engine_mgr.is_pinyin() {
                        self.engine_mgr.auto_learn_settings().promote_count
                    } else {
                        self.engine_mgr
                            .codetable_settings()
                            .auto_phrase
                            .promote_count
                    };
                    // 选中已存在的临时词：learn_temp_word 内部沿用旧 boundary，仅当旧值为 0
                    // （v1 遗留/无信息）时用候选自带的边界补上。
                    if let Ok(count) = store.learn_temp_word(
                        &schema,
                        &code,
                        &cand.text,
                        LEARN_ADD_WEIGHT,
                        cand.boundary,
                    ) {
                        self.maybe_promote_temp(
                            store,
                            &schema,
                            &code,
                            &cand.text,
                            count,
                            promote_count,
                        );
                    }
                }
            }
            // 变体候选（用户明选「齣」类 1对多变体）：末段用覆盖文本、前缀单独转换。
            // 普通候选保持**整体**转换——STPhrases 词级最长匹配可跨 committed/候选边界
            // 消歧（「一」+「出」→「一齣」），拆开会丢掉跨段词级命中。
            let mut out = match &cand.s2t_override {
                Some(t) => format!("{}{}", self.maybe_s2t(state, &state.committed_text), t),
                None => self.maybe_s2t(state, &final_simplified),
            };
            // 英文补空格（`schema.english.commit_space`）：本分支是英文方案下选词的**唯一**
            // 出口——空格 / 数字键 / 次三选键 / 修饰键选词 / 鼠标点选 / 数字键越界 overflow
            // 六类触发全汇于此，故一处接线即可覆盖，不必按触发键分别接。
            //
            // 只在整串分支补、`partial`（分步提交）分支不补：那里上屏的是词的前半段，后面
            // 还要接着打。英文引擎不设 `consumed_length`（恒 0）⇒ `partial` 恒 false，英文
            // 实际走不到那儿，此处的不对称只是把「万一日后英文也分段」的语义先定死。
            //
            // 补在 s2t 之后：空格不参与简繁转换，且提前补会让 STPhrases 的词级最长匹配断在
            // 空格上。
            if self.english_appends_space(cand.source, &cand.text, &state.input_buffer) {
                out.push(' ');
            }
            // 下一轮联想的**上文**：取简体域的完整文本，而不是上屏的 `out`。
            //
            // ★ 两处差别都要紧：
            //   ① **简体**——词库前缀检索是简体域的；开着简繁时 `out` 是「中國」，
            //      拿它去查一条也查不到。（标点联想两种都行，但没理由分叉。）
            //   ② **完整词**——选中联想「中国」时 `out` 只是补出去的「国」，而屏幕上
            //      是「中国」。拿「国」当上文，续联想会从错误的前缀接下去。
            let assoc_ctx = if from_assoc {
                cand.text.clone()
            } else {
                final_simplified.clone()
            };
            self.reset_pinyin_composition(state);
            // 联想入口。**位置即契约**：`reset_pinyin_composition` 刚把缓冲与候选清空，
            // 正是 `maybe_enter_assoc` 往 `candidates` 里填之前要的状态；再往后就只剩返回。
            //
            // 六类选词触发（空格/数字键/次三选键/修饰键选词/鼠标点选/数字键越界）全部汇于
            // 本分支——与紧邻上方英文补空格的收口理由完全相同，一处接线即覆盖。
            //
            // `assoc_may_chain`：一次性档下，选中联想候选后**不再续**（否则联想会一直
            // 接龙下去，那是持续档的语义）。
            if self.assoc_may_chain(from_assoc) && self.maybe_enter_assoc(state, &assoc_ctx) {
                self.notify_ui_update(state);
                // ★ 上屏后**重开一个占位组合**——这是联想态能收到后续按键的唯一可靠手段。
                // 宿主的会话判据里，只有 `HasActiveComposition()` 是同步的；靠应答异步
                // 回填的 `_hasCandidates` 赢不了下一次 OnTestKeyDown 的竞速（见
                // `handle_assoc::ASSOC_COMPOSITION` 里的真机日志铁证）。
                //
                // 走 `commit_then_new_composition` 而不是自己拼 `InsertText`：它按
                // `top_commit_mode` 分流，direct_commit 下把新组合延到 keyup 才开，
                // 躲开「真提交 + 同位置重开」被 diff 式宿主误读成替换。进特殊模式走的
                // 也是这一条。
                return self.commit_then_new_composition(
                    out,
                    crate::handle_assoc::ASSOC_COMPOSITION.to_string(),
                );
            }
            self.notify_ui_hide();
            Self::commit_action(out, true)
        }
    }

    /// 数字键选词统一入口（num 为 1-based：1-9 选页内对应候选，10 表示主键盘 `0` 选第 10 个）。
    /// 命中当前页范围 → 选词上屏；越界 → 走 overflow 策略（对齐 Go handleNumberKey）。
    pub(crate) fn handle_number_key_select(&self, state: &mut State, num: usize) -> KeyAction {
        let (start, end) = self.page_range(state);
        let idx = start + (num - 1);
        if idx < end {
            let cand = state.candidates[idx].clone();
            // 数字键页内位置 = num-1（候选首选率统计）。
            return self.commit_selected(state, &cand, (num - 1) as i32);
        }
        self.handle_overflow_number_key(state, num)
    }

    /// 数字键超出当前页候选范围时的处理（对齐 Go handleOverflowNumberKey）。
    /// 依 `input.overflow.number_key`：ignore 吞键 / commit 上屏高亮候选 /
    /// commit_and_input 上屏高亮候选并追加数字字符。无候选或无有效高亮时一律吞键。
    pub(crate) fn handle_overflow_number_key(&self, state: &mut State, num: usize) -> KeyAction {
        if state.candidates.is_empty() {
            return KeyAction::Consumed;
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
                // 小键盘恒半角：follow_main 下小键盘数字选词越界会落到这里，顶字之后
                // 补的那个数字同样要跟着走半角。
                let full_width = state.full_width && !self.numpad_raw_output(state);
                let cand = state.candidates[hi].clone();
                let act = self.commit_selected(state, &cand, state.selected_index as i32);
                let digit = (b'0' + (num % 10) as u8) as char;
                let digit = if full_width {
                    wind_transform::fullwidth::to_full_width(&digit.to_string())
                } else {
                    digit.to_string()
                };
                Self::append_to_insert_text(act, &digit)
            }
            // "ignore" 及未知值：吞键无效（保留组合，不上屏）
            _ => KeyAction::Consumed,
        }
    }

    /// 次/三选键（`;`/`'`）越界（页内候选不足以命中目标位次）时的处理（对齐 Go
    /// handleOverflowSelectKey）。须排在模式触发键判定之后调用——若该键同时是模式触发键
    /// （如 `;` 触发快捷输入），候选不足时应优先进模式而非走此 overflow。
    /// 依 `input.overflow.select_key`：ignore 吞键 / commit 上屏高亮候选 /
    /// commit_and_input 上屏高亮候选并追加（转换后的）触发键字符。`key_char` 为触发键产生的
    /// 字符（如 `'`），`prev_char` 为光标前字符（用于数字后智能标点）。
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
                let piece = self.convert_punct(state, key_char, prev_char);
                let cand = state.candidates[hi].clone();
                let act = self.commit_selected(state, &cand, state.selected_index as i32);
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
        let cand = state.candidates[hi].clone();
        // 未展开的组候选（cand.text 是组名如「标点符号」）不可作字源 → 吞键，让用户先展开
        // （与 commit_selected 的组候选二级选择一致）。
        if cand.is_group {
            return Some(KeyAction::Consumed);
        }
        let runes: Vec<char> = cand.text.chars().collect();
        // 候选词长度不足 → None，由调用方按 overflow 处理
        if char_index >= runes.len() {
            return None;
        }
        // 词频学习：以词定字应记实际选的「单字」（非整词），否则造词策略会误判为多字词；
        // 仅普通候选（无副作用命令 Action）才学（对齐 Go len(cand.Actions)==0）。
        if cand.actions.is_empty() {
            // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
            let freq_code = self.freq_code(&state.input_buffer, &cand);
            self.record_selection(&freq_code, &runes[char_index].to_string(), cand.source);
        }
        // 拼接已确认段前缀 + 选中单字，整体按简繁模式转换（与 commit_selected 一致）。
        let combined = format!("{}{}", state.committed_text, runes[char_index]);
        let out = self.maybe_s2t(state, &combined);
        let chinese = state.chinese_mode;
        self.reset_pinyin_composition(state);
        self.notify_ui_hide();
        Some(Self::commit_action(out, chinese))
    }

    /// 以词定字的完整流程，含 overflow 策略（对齐 Go handleSelectCharWithOverflow）。
    ///
    /// **仅在有候选时调用**：无候选（空缓冲、或打了码但一个候选都没有的「空码」）时这几个键
    /// 回归标点身份，由调用方放行到标点臂、交 `input.punct_on_empty_behavior` 处置。
    ///
    /// 先尝试正常以词定字；失败则按 `keys.overflow.select_char_key` 处理，三策与 select_key
    /// overflow 同构：ignore 吞键 / commit 上屏高亮 / commit_and_input 追加字符。
    ///
    /// ⚠️ 「失败」现在只剩**真正的越界**：候选词字数不足、联想态无 `input_buffer`、高亮下标
    /// 越界。**空码不再走到这里**——它不是「以词定字越界」，而是「此刻这个键不该算以词定字
    /// 键」。曾经把空码也算进 overflow，后果是 `punct_on_empty_behavior` 对这几个键整个失效。
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
                let piece = key_char
                    .map(|c| self.convert_punct(state, c, prev_char))
                    .unwrap_or_default();
                let cand = state.candidates[hi].clone();
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

    /// $CC 命令候选选中：**纯文本命令走同步上屏**，含副作用命令清理组合区 + 独立线程异步执行。
    ///
    /// # 纯文本命令必须与普通短语同路
    ///
    /// 动作链全为 `type()` 的命令（≈带模板的词条）没有任何需要回调 coordinator 的副作用，
    /// 求值只读快照 —— 于是它可以、也**必须**走与普通短语完全相同的同步上屏：
    /// `commit_candidate` + 返回 `InsertText`，由 TSF 在**本次按键的组合区仍活跃时**
    /// 经 `SetText + EndComposition` 提交。
    ///
    /// 走异步会实质改变上屏语义，而不只是慢一点：`spawn_command_action` 先返回
    /// `ClearComposition` 结束 composition，命令线程稍后再推一个裸 `CMD_COMMIT_TEXT`，
    /// 此刻宿主侧**已无 composition**，TSF 只能退到 `InsertTextAtSelection` 裸插入。
    /// 两条分支对宿主的含义不同：composition 提交被 Word/WPS 当作「输入法上屏」处理，
    /// 文本里的 `\n` 规范化成段落标记；裸插入是纯字符流，`\n` 原样落进段落内部。
    /// 真机现场：同一首带换行的诗，做成普通短语词条正常分段，封进 `$CC` 的 `type()`
    /// 就挤成一行 —— 文本内容与调用参数完全一致，差别只在「组合区还在不在」。
    ///
    /// # 含副作用命令：异步仍是必须的
    ///
    /// 控制器经 Weak 回调 handle_menu_command 等自锁方法，而此刻本线程仍持 state 锁
    /// （std::sync::Mutex 非可重入），同线程重入即死锁——交独立线程待本次按键处理释放锁
    /// 后再跑（对齐 Go「不在 SearchCommand 持锁路径里再 Lock」的约束）。
    pub(crate) fn commit_command(&self, state: &mut State, cand: &Candidate) -> KeyAction {
        // 命令 nav（从前缀列举选中）携完整码 group_code，用它作执行输入上下文
        // （让 code()/input() 等按完整码求值）；精确码命令 group_code 空 → 用当前缓冲。
        let input = if cand.group_code.is_empty() {
            state.input_buffer.clone()
        } else {
            cand.group_code.clone()
        };
        // 纯文本命令 → 同步上屏（与顶码 / 自动上屏的 `command_auto_outcome` 同一判据）。
        // 求值出空文本时**不**走这里：那是「选中了却没内容」，交异步路径正常清组合收尾。
        if let Some(text) = self.eval_command_text_only(&cand.phrase_template, &input)
            && !text.is_empty()
        {
            let chinese_mode = state.chinese_mode;
            // 记账码传 `input`、来源如实传 `cand.source`（短语）——与 `commit_top_text` 的
            // 命令顶码分支同构：`record_selection` 据来源拦掉短语，求值文本不进 FREQ 表。
            let out = self.commit_candidate(state, &text, None, cand.source, &input);
            self.notify_ui_hide();
            return Self::commit_action(out, chinese_mode);
        }
        self.reset_pinyin_composition(state);
        self.spawn_command_action(cand, input)
    }

    /// 该候选是否为**纯文本 `$CC` 命令**——动作链全 `type()`、无副作用、且求值文本非空。
    ///
    /// 键盘路径不需要它（[`Self::commit_command`] 内部直接按求值结果分流，只求值一次）；
    /// 鼠标路径必须先于分支判断问一次，因为「进不进命令特判分支」这个决定要在求值之前做出。
    /// 两处判据同源于此，避免鼠标与键盘对同一条命令给出不同的上屏语义。
    ///
    /// 求值本身只读快照（见 [`Coordinator::eval_command_text_only`] 的锁说明），
    /// 可在持 state 锁时调用。
    fn command_commits_text(&self, cand: &Candidate, input: &str) -> bool {
        cand.is_command
            && self
                .eval_command_text_only(&cand.phrase_template, input)
                .is_some_and(|t| !t.is_empty())
    }

    /// `$CC` 命令执行核心：隐藏 UI + 把命令源放独立线程异步执行，返回 `ClearComposition`。
    /// **不做**任何缓冲/模式状态重置——调用方须在调用前完成本路径的退出（正常路径经
    /// `commit_command` 的 `reset_pinyin_composition`；overlay 路径经各自 `exit_*`）。
    /// `input` 为命令 `input()`/`code()` 求值上下文（正常路径=输入缓冲；overlay=其编码缓冲，
    /// 须在退出前捕获）。异步执行的死锁规避见 [`Self::spawn_command`]。
    pub(crate) fn spawn_command_action(&self, cand: &Candidate, input: String) -> KeyAction {
        let src = cand.phrase_template.clone();
        self.notify_ui_hide();
        self.spawn_command(src, input);
        // ClearComposition 而非 Consumed：清掉应用里已输入的命令码（如 "coen"），
        // 否则 composition 残留（Consumed 仅吞键、不结束 composition）。type() 的上屏文本
        // 由命令线程经 push 管道单独提交。
        KeyAction::ClearComposition
    }

    /// overlay 路径（特殊模式/临拼/临英/混输）选中候选的**命令前置守卫**：
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

    /// 顶屏点统一命令分流：若当前高亮候选是 $CC 命令，执行命令（异步，语义与按空格
    /// `commit_selected` 一致——上屏命令动作结果而非 display 标签），返回 `Some(action)`；
    /// 否则返回 `None`，调用方按普通候选顶屏。
    ///
    /// 用于标点 / 运算符 / 智能符号 Hold / 进其它模式等所有「顶高亮候选」路径，修复命令候选被
    /// 顶屏时错把 display 文本当普通文本上屏的问题（这些路径绕过了 `commit_selected` 的命令守卫）。
    /// 命令候选被顶屏时按「执行命令」处理，触发键（标点 / 模式键）字符不再单独上屏——与空格选中
    /// 命令候选行为一致（命令占据整段缓冲，无独立前缀）。
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
    /// 记账 → 设余码为缓冲 → 刷新候选 → 复位首显延迟 → 按 `top_commit_mode`
    /// 返回 `InsertText`（pre_confirm）或 `CommitThenDeferComposition`（direct_commit，余码
    /// keyup 延迟重开）。`top_text` 空（理论边界）时跳过记账、仅刷新余码组合。
    ///
    /// `source` 由调用方按被顶出的候选如实传入，**不能一律当码表**：本函数的三条来路里有两条
    /// 是短语（普通短语顶码、`$CC` 纯文本命令顶码），谎报成码表会让短语的求值文本被写进 FREQ
    /// 表——那正是 `record_selection` 要拦掉的逐日新键。顶码机制本身归属码表不改变候选的来源。
    ///
    /// 英文补空格（`schema.english.commit_space`）**不接本函数**：顶码的触发条件是输入超过
    /// 方案码长上限，而英文引擎的码长上限取自词典最长单词，实际打不到；且顶码后余码要继续
    /// 组合，中间插空格会把一个词劈成两截。已判定为不可达 + 语义不合，不是漏接。
    ///
    /// # 简繁转换在本函数内收口
    ///
    /// `top_text` 一律传**简体原文**（内部唯一事实），出屏文本由本函数转换——三条来路各自
    /// `maybe_s2t` 曾经全部漏掉，顶码上屏简体而空格上屏繁体（2026-08-20 反馈）。压进来之后
    /// 新增来路想漏也漏不掉。顺序不可对调：`record_selection` / `record_commit` 必须吃简体
    /// 原文，否则词频表会长出一套繁体键，而读端 `apply_freq_rerank` 查的是简体，永远查不到。
    ///
    /// `s2t_override` 由调用方按被顶出的候选**如实传入**（语义同 [`Self::cand_s2t_text`]）。
    /// 现实中三条来路都取不到 1对多变体候选——顶码取 `candidates.first()`，而变体恒插在原字
    /// **之后**（见 [`Self::expand_s2t_variants`]）——但判据写在候选身上而非「反正取不到」的
    /// 推断上：顶码哪天改取高亮候选，这里不必跟着想起来改。
    pub(crate) fn commit_top_text(
        &self,
        state: &mut State,
        prefix: &str,
        top_text: String,
        s2t_override: Option<&str>,
        remainder: &str,
        source: CandidateSource,
    ) -> KeyAction {
        if !top_text.is_empty() {
            self.record_selection(prefix, &top_text, source);
            // 顶码即上屏首选（pos=0），code_len=被顶出的前缀码长。
            self.record_commit(
                &top_text,
                prefix.len() as u32,
                0,
                wind_store::stats::CommitSource::Candidate,
            );
        }
        // 出屏文本（记账之后取，见上）：变体候选用其覆盖文本，其余按需简繁转换。
        let out_text = match s2t_override {
            Some(t) => t.to_string(),
            None => self.maybe_s2t(state, &top_text),
        };
        state.input_buffer = remainder.to_string();
        // 余码是缓冲的后缀 → 影子串同步掐头，否则大写会在这里静默丢掉。
        preedit_cursor::keep_cased_tail(&state.input_buffer, &mut state.input_buffer_cased);
        state.input_cursor_pos = state.input_buffer.len(); // 顶码后余码续打，光标在余码末尾
        let _ = self.update_candidates(state); // 余码候选（不再消费其结局）
        let preedit = state.preedit.clone();
        // 顶码 = 部分上屏 + 余码续组合：宿主光标因 top_text 插入而前移，余码组合起点已变。
        // 复位首显延迟，使余码候选窗延迟到 reflow 后的新坐标首显、重锁组合起点（对齐 Go）。
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

    /// 顶屏「已转换前缀 + 高亮候选」的**出屏文本**，供进模式类顶屏（临英 / mix / 特殊模式 /
    /// 临时拼音）共用；返回 `None` = 没有待上屏内容（空缓冲进入），调用方直接返回进模式动作。
    ///
    /// # 为什么必须收成一个函数
    ///
    /// 这段逻辑曾以逐字相同的形态复制在四处，四份**全部**漏掉了简繁转换（顶屏出简体、空格
    /// 出繁体，2026-08-20 反馈），其中三份还漏掉了 `record_commit`（输入统计少记一条上屏）。
    /// 复制出去的加工步骤不会被一起想起来——出屏文本只能有一个落点。
    ///
    /// # 两半的转换方式不同，不可合并成整串 `maybe_s2t`
    ///
    /// 已转换前缀走 `maybe_s2t`，高亮候选走 [`Self::cand_s2t_text`]。整串一起转会把 1对多
    /// 变体候选打回默认转换结果（用户高亮的是「齣」，上屏却成「出」）。此处高亮**可以**停在
    /// 变体候选上——与顶码取 `candidates.first()` 恒取不到变体的情形不同，见
    /// [`Self::commit_top_text`]。
    ///
    /// 记账（`record_selection` / `record_commit`）一律吃简体原文 `cand.text`，同一口径。
    ///
    /// # ★ 联想态**不顶屏**
    ///
    /// 与 [`Self::commit_highlight_then_char`] 同款守卫、同一个理由：顶屏的语义前提是「用户
    /// 打了码、还没选词，按这个键意味着『就选高亮那条吧』」。联想态**没有码**——高亮那条是
    /// 输入法猜的，不是用户在选，此刻按引导键的意图就是进模式。
    ///
    /// 不必显式 `exit_assoc`：联想候选就住在 `state.candidates` 里，进模式各 `enter_*` 都会
    /// 清空候选，联想随之隐式退出（见 `handle_assoc` 模块文档）。
    pub(crate) fn take_committed_with_highlight(&self, state: &mut State) -> Option<String> {
        let prefix = self.take_committed(state);
        let mut out = self.maybe_s2t(state, &prefix);
        if state.candidates.is_empty() || state.assoc_active() {
            return (!out.is_empty()).then_some(out);
        }
        let (start, _) = self.page_range(state);
        let idx = self
            .highlighted_global_index(state)
            .min(state.candidates.len() - 1);
        let cand = state.candidates[idx].clone();
        // 记账码：码表按输入码（码位独立），拼音/英文按候选码。见 `freq_code`。
        let freq_code = self.freq_code(&state.input_buffer, &cand);
        self.record_selection(&freq_code, &cand.text, cand.source);
        // 顶屏上屏的是一条来源候选（prefix 段已在选词时记过）。
        // `saturating_sub`：`page_range` 保证 start < len，钳制后 idx < start 不可达，
        // 但页内位置这种纯展示用的量不值得为它留一个下溢 panic。
        self.record_commit(
            &cand.text,
            state.input_buffer.len() as u32,
            idx.saturating_sub(start) as i32,
            wind_store::stats::CommitSource::Candidate,
        );
        out.push_str(&self.cand_s2t_text(state, &cand));
        Some(out)
    }

    /// 「顶屏文本 + 进模式新组合」收尾（进特殊模式 / 临时拼音 / mix 融合共用）：与顶码
    /// `commit_top_text` 同一 `top_commit_mode` 分流——direct_commit 且有新组合（引导键
    /// 前缀）→ `CommitThenDeferComposition` 真提交、新组合延迟到触发键 keyup 才开；
    /// pre_confirm → `InsertText` 聚合（文本并入 TSF `_pendingCommitPrefix`、留组合内）。
    /// 新组合为空（直达热键进入无引导符）时无组合可重开、无 diff 合并之虞，
    /// 两种模式都直接真提交（对齐顶码无余码分支）。
    ///
    /// `text` 必须是**已过简繁转换的出屏文本**——本函数只管 `top_commit_mode` 分流，不碰文本。
    /// 四个调用方一律经 [`Self::take_committed_with_highlight`] 取，勿自行拼 `prefix + cand.text`。
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

    /// 含副作用命令（`$CC` 里带 shell/key/clip 等 Effect）顶码：异步执行动作（消费 prefix 整段、
    /// 无同步上屏文本），余码作为新一轮输入缓冲走标准候选刷新 + 新组合。副作用多为开应用 /
    /// 切设置——前者焦点变化自动取消余码组合（无害），后者不改焦点、余码组合正常续打。
    /// 不走 direct_commit 延迟重开（无同步 commit 文本，无 diff 合并之虞）。
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
        if text.is_empty() {
            return;
        }
        let encoded = wind_ipc::codec::encode_commit_text(text, None, false, true, false);
        self.push_server.push_commit_to_active(&encoded);
    }

    /// 候选词条操作（右键菜单）：调整 Shadow 规则并即时重排重绘。
    /// 编码与归属方案取自 [`Self::candidate_op_scope`]（主输入路 = 输入码 + active 方案；
    /// 特殊模式 = 其编码缓冲 + 它引用的方案），按方案隔离。
    /// 删除按候选来源分流（对齐 Go handleCandidateDelete）：短语软禁用 / 用户词・临时词真删 /
    /// 系统词 shadow 隐藏。菜单已按同规则灰显，此处判定为 defensive（热键路径也经此）。
    pub(crate) fn candidate_op(&self, op: CandidateOp, page_local: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.candidates.is_empty() {
            return;
        }
        let (start, end) = self.page_range(&state);
        let idx = start + page_local;
        if idx >= end || idx >= state.candidates.len() {
            return;
        }
        let cand = state.candidates[idx].clone();
        let word = cand.text.clone();

        // 常用/生僻标记落在全局字级的覆盖表，**不需要词库落点**，故必须分派在下面那道
        // 作用域准入之前。放在后面的话，临拼/临英/混输/空码浏览态下菜单给了入口而写端
        // 直接 return —— 用户点得动、毫无反应、没有任何日志，是最难查的一类错配。
        // 判据与菜单侧 `common_char_mark` 同源（都由它决定给不给这一项）。
        if matches!(op, CandidateOp::ToggleCommon) {
            self.toggle_common_char(&mut state, &word);
            return;
        }

        // 作用域即准入：拿不到落点（无词库归属的 overlay / 空码浏览态）就什么都不做，
        // 与菜单侧「只给复制」是同一个判据的两副面孔。
        let Some(scope) = self.candidate_op_scope(&state) else {
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
        // 拼音普通候选**只放行置顶**，前移/后移仍禁；命令候选不受限。
        // 引擎类型取自 scope（特殊模式问的是它引用的方案，不是主方案）。
        //
        // 分野在于位置语义是否稳定：`position=0` 恒等于「第一个」，这个承诺与候选集怎么变
        // 无关；而 `position=3` 一旦词频衰减、模糊音开关或词库变动改了候选集，指的就是另一
        // 条候选了——那正是当初对拼音整体禁调位的理由，它对置顶从来不成立。
        //
        // ⚠️ shadow 排在装配流水线**最后**（sort → dedup → filter → freq_rerank → shadow），
        // 因此它能翻过拼音 `cmp_match_layers` 的硬层级闸门——置顶的效力比调频强，是硬规则。
        // 用户要撤销只能靠右键「恢复默认」或设置页的规则列表。
        if matches!(op, CandidateOp::MoveUp | CandidateOp::MoveDown)
            && !cand.is_command
            && matches!(engine_type, Some(wind_engine::EngineType::Pinyin))
        {
            return;
        }
        // 已在首位：置顶是冗余规则，直接忽略（菜单已灰显，热键路径 defensive）。
        if matches!(op, CandidateOp::MoveTop) && idx == 0 {
            return;
        }
        let last = state.candidates.len().saturating_sub(1);
        if let Some(store) = &self.store {
            // 候选调整按 data_schema_id 归属（拼音族折叠）；Delete 分支仍传原始 schema，
            // 供 delete_candidate_by_source 对用户词/临时词按来源分流（混输）。
            let sh_schema = self.engine_mgr.data_schema_id(&schema);
            // 稳定 id：短语候选有（`phrase:{code}:{模板原文}`），码表/拼音静态候选无（空串 →
            // None → 落回 word 匹配）。**必须在此传下去**——`word` 记的是当次求值结果，
            // `date`/`time` 类短语次日即失配，规则表现为「昨天调好、今天被还原」。
            // redb 事务持久，无需显式落盘。
            let cand_id = (!cand.id.is_empty()).then_some(cand.id.as_str());
            let r = match op {
                CandidateOp::MoveTop => store.pin_shadow(&sh_schema, &code, &word, cand_id, 0),
                CandidateOp::MoveUp => {
                    store.pin_shadow(&sh_schema, &code, &word, cand_id, idx.saturating_sub(1))
                }
                CandidateOp::MoveDown => {
                    store.pin_shadow(&sh_schema, &code, &word, cand_id, (idx + 1).min(last))
                }
                // ⚠️ Delete 走 `raw_code`（击键）而非归一码：它落的是短语表 / 用户词库，
                // 两者的键空间都是击键域（短语按 `input_buffer` 召回），与 shadow 不同。
                // 双拼下混用会让短语删除静默失效——写进 `hao`、读的是 `hc`。
                CandidateOp::Delete => self.delete_candidate_by_source(&schema, &raw_code, &cand),
                CandidateOp::Reset => store.remove_shadow_rule(&sh_schema, &code, &word, cand_id),
                // 不可达：它在上面就 early-return 了（落点是全局常用字覆盖表，不是 shadow）。
                // 保持不 panic —— 菜单 id 由 IPC 回传，让一个越界值把输入法打崩不值当。
                CandidateOp::ToggleCommon => Ok(()),
            };
            if let Err(e) = r {
                warn!("candidate op failed: {}", e);
            }
        }

        // 重新构建候选（会重新应用 Shadow）并重绘。
        // **必须按模式分派**：主路径的 `update_candidates` 读 `input_buffer`，而特殊模式下它
        // 恒为空 —— 在快符里走主路径的后果不是「不刷新」而是候选窗当场被清空。
        if special {
            // 返回值是「全码策略请求自动上屏」的意向，此处**刻意丢弃**：编码一个字没变，
            // 用户只是在调整候选顺序，凭空上屏是错的。
            let _ = self.update_special_candidates(&mut state);
        } else {
            self.update_candidates(&mut state);
        }
        // 用户看到的那一半判据。「记录删掉了、但同文的系统词条目还在」与「根本没删到」
        // 在屏幕上完全同形（都是点了没反应），只有把这一行和上面的「删前命中」并排看
        // 才分得开——单看任何一条都会误诊。
        if matches!(op, CandidateOp::Delete) {
            debug!(
                "candidate_op(Delete): 重建后候选仍在={} text={}",
                state.candidates.iter().any(|c| c.text == word),
                word
            );
        }
        self.notify_ui_update(&state);
    }

    /// 右键「删除」按候选来源分流：
    /// - 短语 → `set_phrase_enabled(false)` 软禁用（设置页可恢复）+ 重建短语层即时生效；
    ///   code 优先取导航完整码 `group_code`，text 用原始记录文本 `phrase_template`（display
    ///   可能是模板展开后文本，直接用会在 store 里 miss）。
    /// - 用户词/临时词 → store 真删；schema 取写归属 id（混输按来源分流、拼音族折叠共享），
    ///   code 优先取候选自带存储码（双拼下 input_buffer 是双拼串、存储码是全拼）。
    ///   **两个标记各删各的表**：同文双记录（先自动学过、后来又手动加词——`add_user_word`
    ///   不清临时表）时只删一张，剩下那张继续供出同一条候选，表现与没删一模一样。
    /// - 其它（系统码表/拼音）→ shadow 软隐藏；单字同样允许（旧版单字保护已取消：
    ///   shadow 按 code+word 键控，仅该编码下隐藏，设置页可恢复）。
    fn delete_candidate_by_source(
        &self,
        schema: &str,
        code: &str,
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
        if cand.meta.is_user_dict || cand.meta.is_temp_dict {
            let Some(sid) = self.engine_mgr.write_data_schema_id(schema, cand.source) else {
                debug!("delete_candidate: 无法归因存储方案，跳过 '{}'", cand.text);
                return Ok(());
            };
            // 候选存储码，按可信度排序去重：
            // ① `meta.store_code` —— 引擎层同文合并时从 store 记录原样带过来的真值；
            // ② 候选自带 `code` —— 未经合并时它就是存储码（双拼下也已是全拼）；
            // ③ `merged_codes` —— `CompositeDict::merge_search` 去重时被丢弃那条的码位；
            // ④ 输入码 —— 候选无码时的最后兜底。
            //
            // 逐个试而非只认一个：**同文合并有两层**（composite 去重、引擎 store 层并入），
            // 每层都只留得住一个 code，而记录可能挂在被丢弃的那个码上。
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
            // 删之前先查。**没有这一步，三种结局在日志里长得一模一样**：`redb` 的 `remove`
            // 对不存在的 key 静默成功，而 key 由 schema+code+text 三段拼成，任一段错配的
            // 表现都是「点了删除、界面毫无变化、词还在词库里」。
            let hit = |user: bool, s: &str, c: &str| -> bool {
                let recs = if user {
                    store.get_user_words(s, c)
                } else {
                    store.get_temp_words(s, c)
                };
                recs.unwrap_or_default().iter().any(|r| r.text == cand.text)
            };
            // 两个标记**各删各的表**：同文双记录时只删一张，剩下那张照样供出同一条候选。
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
                    store.remove_user_word(&sid, c, &cand.text)
                } else {
                    store.remove_temp_word(&sid, c, &cand.text)
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
            // 一条都没删到 = 用户眼里的「点了没反应」。这是**唯一**该报警的结局，
            // 降到 debug 就等于把它藏进只有开了调试才看得见的地方（本 bug 正是这么活了一个月）。
            // 顺带把「记录到底在哪个桶」探出来，省掉下一轮复现：错配来源有二——schema 段
            //（混输下 `write_data_schema_id` / `data_schema_id` / active 自身解析各不相同）
            // 与 code 段（双拼、前缀补全、展示域带空格时候选码≠存储码）。
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
        // 候选调整（系统词软隐藏）按 data_schema_id 归属（拼音族折叠）。
        let sh = self.engine_mgr.data_schema_id(schema);
        debug!(
            "delete_candidate: 分支=shadow 隐藏 schema={} code={} text={}（该候选未被判为用户词/临时词）",
            sh, code, cand.text
        );
        store.delete_shadow(&sh, code, &cand.text)
    }

    /// 候选词操作热键匹配（对齐 Go matchCandidateActionKey，但 `0` 扩展为第 10 候选）。
    /// 命中返回 1-based 页内序号(1-10)，否则 0。模板值域见 [`wind_config::hotkey::number_template_mods`]。
    /// 数字键 1-9 → 序号 1-9；`0` → 序号 10（候选窗最多 10 项，与主键盘/小键盘选词一致）。
    ///
    /// ★★ **修饰位按「相等」判，不按「包含」判**。此前这里写的是
    /// `"ctrl+number" if has_ctrl && !has_shift`，压根不看 Alt ⇒ **Ctrl+Alt+3 会命中
    /// `ctrl+number` 那条臂、把第 3 个候选静默置顶**，而 TSF 侧对「有会话 + Ctrl/Alt +
    /// 非注册热键」走的是 cleanup 通路（`KeyEventSink.cpp` 的 `isCtrlAltCleanup`）——键照样
    /// 发过来、之后又 `pfEaten=FALSE` 交还宿主，于是宿主快捷键与置顶**同时**发生。
    ///
    /// ⇒ 通则：**模板值域里一旦有两项存在子集关系，包含式判据就必然让宽的那项劫走窄的那项。**
    /// 相等判据还有一个附带好处：值域再加取值也不必回头补 `!has_xxx` 排他条件。
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

    /// Ctrl / Ctrl+Shift / Ctrl+Alt ＋数字 置顶/删除当前页候选（对齐 Go handle_key_event
    /// 候选热键段）。可选值域见 [`wind_config::hotkey::number_template_mods`]。
    /// 仅中文模式 + 有候选 + 有词库落点（主输入路或特殊模式，见 `candidate_op_scope`）生效；
    /// 命中即消费按键。
    /// 复用 `candidate_op`（页内序号驱动的 shadow 改写 + 重排重绘）。
    pub(crate) fn handle_candidate_action_hotkey(&self, data: &KeyEventData) -> Option<KeyAction> {
        use wind_ipc::protocol::MOD_CTRL;
        // 早退纯属省事：现值域里每一项都带 Ctrl（见 `number_template_mods`）。这是**值域约束**
        // 而非机制约束——真正的裁决在 `match_candidate_action_key` 的等值比较里，值域若哪天
        // 收进不带 Ctrl 的模板，删掉这一段即可，不必改判据。
        if data.modifiers & MOD_CTRL == 0 {
            return None;
        }
        let h = &self.rt().config.keys;
        // 删除优先匹配（与 Go 顺序一致：DeleteCandidate 先于 PinCandidate）。
        // ⚠️ 这个顺序现在只用来兜「两项配了同一个模板」的退化情形：等值判据已经保证不同模板
        // 互斥，不再像包含式判据那样靠先后顺序来「侥幸不出错」。
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
        // 门控：中文态 + 有候选 + 有词库落点。落点判定交给 `candidate_op_scope`，与右键菜单
        // 同源——此前这里写的是 `state.active.is_some()` 整类屏蔽，于是特殊模式接上词库管理
        // 之后，右键能删而热键仍然无效（同一能力的两条通路各判各的）。
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.chinese_mode
                || state.candidates.is_empty()
                || self.candidate_op_scope(&state).is_none()
            {
                return None;
            }
        }
        // candidate_op 自行重新加锁并做页范围/来源分流校验。
        self.candidate_op(op, num - 1);
        Some(KeyAction::Consumed)
    }

    /// 点击选词：提交页内第 N 个候选，经 push 管道异步上屏（对齐 Go PushCommitText）。
    ///
    /// 主输入路（`active == None`）复用键盘选词的 [`Self::commit_selected`]，其返回的 KeyAction
    /// 经 [`Self::push_no_key_ctx_action`] 翻译成 push 消息——分步提交（候选只消费缓冲前缀，如
    /// 「nihao」选「你」）由此与数字键完全一致：组合区留活、剩余码续查候选。此前鼠标独走
    /// `commit_candidate`（无条件清空缓冲），故点选分段候选会丢弃剩余编码、丢失已确认前缀段，
    /// 并把词频错记到整串码上。
    ///
    /// overlay 模式（临拼/特殊/临英/混输，`active != None`）在键盘侧由各自的专用处理器接管、
    /// 不经 `commit_selected`（见 coordinator 内 `state.active` 的单点分派），故仍走原
    /// 「整串提交 + 彻底复位」路径，不向其引入未定义的分段语义。
    pub(crate) fn mouse_select(&self, page_local: usize) {
        let _ = self.mouse_select_action(page_local);
    }

    /// [`Self::mouse_select`] 的实现，返回主输入路实际推送的 KeyAction 供测试断言
    /// （overlay / `$CC` 命令 / 越界等不经 push 的路径返回 None）。
    ///
    /// 页内下标 → 绝对下标的换算在此，**页范围校验也在此**：桌面候选窗只画当前页，
    /// 点到页外即为坐标算错，必须拒绝。移动端不是这样（见 [`Self::select_candidate_at`]）。
    fn mouse_select_action(&self, page_local: usize) -> Option<KeyAction> {
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

    /// **按绝对下标选词**：`mouse_select` 与移动端滚动候选栏的共同内核。
    ///
    /// 移动端的候选栏是一条可滚动的长列表，没有"页"这个视觉概念，用户想点第几个就点第几个。
    /// 而桌面的选词入口全部以**页内下标**表达（数字键 1-9、鼠标点击当前页），移动端此前
    /// 只能合成数字键去凑，于是**永远选不了第 10 个及以后的候选**。
    ///
    /// 分页仍然保留、也仍然有意义：它决定空格上屏的目标与数字键的语义。这里只是把
    /// 「选哪一个」从视图坐标里解放出来。
    ///
    /// @return 主输入路实际产生的 KeyAction（overlay / `$CC` 命令 / 越界返回 None）
    pub(crate) fn select_candidate_at(&self, idx: usize) -> Option<KeyAction> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // 联想候选就住在 `candidates` 里，故本函数**原样适用**——鼠标点选联想词与点选
        // 普通候选走的是同一条 `commit_selected`，无需分支。移动端的候选栏全靠这条通路。
        if idx >= state.candidates.len() {
            return None;
        }
        // 统计用的位置：**相对当前页首**，与数字键的 `num-1` 保持同一量纲（既有约定）。
        // 移动端不翻页（滚动条不动 `current_page`），页首恒为 0，于是如实记成绝对位次
        // ——「用户选了第 37 个」正是首选率统计要回答的问题。
        let page_local = idx.saturating_sub(self.page_range(&state).0);
        // 主输入路（含辅助码）的判据，下方两处共用。
        let main_path = state.active.is_none() || state.active == Some(ModeKind::AuxCode);
        // **纯文本命令走下方 `commit_selected`**，不进本分支：其 `is_command` 守卫经
        // `commit_command` 同步上屏产出 `InsertText`，由 `push_no_key_ctx_action` 编成
        // `CMD_COMMIT_TEXT`，宿主侧仍是「组合区活跃时提交」。走本分支则先 ClearComposition
        // 再裸插入，换行等语义随之改变（原委见 [`Self::commit_command`]）。
        // overlay 路径不在此列：它有各自的退出闭包，仍按 `overlay_commit_command` 语义异步执行。
        let cmd_commits_text = main_path
            && self.command_commits_text(&state.candidates[idx], {
                let gc = &state.candidates[idx].group_code;
                if gc.is_empty() {
                    &state.input_buffer
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
                state.input_buffer.clone()
            } else {
                gc
            };
            // 与键盘路径（`commit_command`）同样先复位组合再执行。
            //
            // 此前这里只清了 `state.active`，缓冲与候选列表原封不动，于是：
            //   1. 候选窗被 `notify_ui_hide` 藏起来，但下一次 `notify_ui_update`
            //      （光标上报、模式变化…任何一处）又把那批陈旧候选推回屏幕；
            //   2. 宿主里已输入的命令码（如 "coen"）没人收回——键盘路径靠返回
            //      `ClearComposition` 清掉，而鼠标点击不在按键应答里，没有那个出口。
            // `spawn_command_action` 的文档早写明「不做任何状态重置，调用方须先退出」，
            // 这条鼠标通路是唯一漏做的。
            self.reset_pinyin_composition(&mut state);
            state.active = None;
            drop(state);
            self.notify_ui_hide();
            // 补上键盘路径由 KeyAction 承担的那一半：让宿主结束 composition。
            self.push_server
                .push_commit_to_active(&wind_ipc::codec::encode_clear_composition());
            self.spawn_command(src, input);
            return None;
        }
        // 主输入路：与数字键同一条提交路径（is_group 的二级选择亦由其内部处理）。
        //
        // **辅助码并入本条**：它的候选就是主路径候选（只是被字形筛过一轮），选中要走
        // `commit_selected` 才有分步转换——走下面的 overlay 分支会 `commit_candidate`
        // 直接清空 `input_buffer`，于是「没时间」这类分步组句在鼠标点「没」时把剩余
        // 拼音一并丢掉，而键盘选同一个候选却能继续组句。同一候选两种入口两种结果。
        if main_path {
            let cand = state.candidates[idx].clone();
            let chinese_mode = state.chinese_mode;
            // 鼠标页内位置 = page_local（候选首选率统计，与数字键的 num-1 同义）。
            //
            // 辅助码走 `aux_code_committed`（三条选词路径的唯一收尾，见该函数）：部分消费
            // 时留在模式内重建会话继续筛，完整消费才退出。此前这里是就地抄的一份，抄漏了
            // 重建之后的 `notify_ui_update` —— 鼠标点掉「没」之后候选窗还停在旧那一屏。
            let act = if state.active == Some(ModeKind::AuxCode) {
                self.aux_code_committed(&mut state, cand, page_local as i32)
            } else {
                self.commit_selected(&mut state, &cand, page_local as i32)
            };
            drop(state);
            // commit_selected 已按分支自行 notify_ui_update / notify_ui_hide，此处不再重复。
            self.push_no_key_ctx_action(&act, chinese_mode);
            return Some(act);
        }
        // ── 以下为 overlay 模式（active != None）路径 ──
        // 前缀导航候选：补全输入到完整码并重查展开（二级选择，鼠标点击同键盘选中）。
        if state.candidates[idx].is_group {
            let code = state.candidates[idx].group_code.clone();
            self.complete_to_group_code(&mut state, &code);
            return None;
        }
        let text = state.candidates[idx].text.clone();
        let s2t_override = state.candidates[idx].s2t_override.clone();
        let source = state.candidates[idx].source;
        // 记账码按来源分流（见 `freq_code`）：码表用输入码，拼音/英文用候选存储码。
        let code = self.freq_code(&state.input_buffer, &state.candidates[idx]);
        let chinese_mode = state.chinese_mode;
        let out = self.commit_candidate(&mut state, &text, s2t_override.as_deref(), source, &code);
        // 鼠标提交后彻底复位各输入模式，避免遗留状态
        state.active = None;
        state.temp_pinyin_buffer.clear();
        state.temp_pinyin_prefix.clear();
        state.temp_english_buffer.clear();
        drop(state);

        self.notify_ui_hide();
        let encoded = wind_ipc::codec::encode_commit_text(&out, None, false, chinese_mode, false);
        // 仅推给活动客户端，避免广播导致多个 TSF 端重复上屏
        self.push_server.push_commit_to_active(&encoded);
        debug!(
            "mouse_select: overlay 整串提交 '{}' (page_local={})",
            out, page_local
        );
        None
    }

    /// 鼠标点选页内第 N 个候选（测试/诊断用）：返回主输入路实际推送的 KeyAction
    /// （`UpdateComposition` = 分步提交，组合区留活；`InsertText` = 整串上屏）。
    pub fn debug_mouse_select(&self, page_local: usize) -> Option<KeyAction> {
        self.mouse_select_action(page_local)
    }

    /// 无按键上下文的 KeyAction → push 管道消息。
    ///
    /// 键盘选词把 KeyAction 交回 TSF 按键管线应答，**鼠标点击**（不在 OnKeyDown 的应答里）
    /// 与 **CapsLock 全局钩子**（`handle_capslock_hook_press`，事件根本不经 TSF）都没有那个
    /// 上下文，只能自行编码经 push 管道投递——两者是同一处境，故共用本函数而不是各写一份。
    /// 仅覆盖 `commit_selected` 的两种返回：
    /// - `UpdateComposition`（分步提交 / 二级选择）→ `CMD_UPDATE_COMPOSITION`，组合区留活。
    ///   C++ 侧 IPCClient 异步 reader 与 TextService 的 `SetUpdateCompositionCallback` 自 Go 版
    ///   起就在位（注释即写 "mouse click partial confirm"），Rust 侧此前从未发过此包。
    /// - `InsertText`（整串提交）→ `CMD_COMMIT_TEXT`。
    ///
    /// 两者均带副作用，故一律 `push_commit_to_active` 定向投递（非广播），避免多个 TSF 端重复。
    ///
    /// ⚠️ **其余 KeyAction 一律落 `other` 臂被静默丢弃，`ClearComposition` 也在其中。**
    /// 键盘路径靠按键应答把它带回宿主，这条通路没有那个出口，漏掉的现象是「点了之后
    /// 动作确实跑了，但宿主里的组合区一直挂着」。`select_candidate_at` 的 `$CC` 分支
    /// 正是踩过这个坑之后、在**调用点**手工补了一行 `encode_clear_composition`
    /// （那条路 `return None`，根本不经过本函数）。
    ///
    /// 也就是说这里的缺口仍在：`handle_addword.rs` 里有七处返回 `ClearComposition`，
    /// 加词面板的鼠标点选哪天接进本函数就会再踩一次。届时应当**在此补一臂**，而不是
    /// 在调用点做第三次特判 —— 修现象不修机制，每次修复都正确，每次都不减少下次的概率。
    pub(crate) fn push_no_key_ctx_action(&self, act: &KeyAction, chinese_mode: bool) {
        match act {
            KeyAction::UpdateComposition { text, caret_pos } => {
                let encoded = wind_ipc::codec::encode_update_composition(text, *caret_pos);
                self.push_server.push_commit_to_active(&encoded);
                debug!("push_no_key_ctx: 分步提交，组合区留活 preedit='{}'", text);
            }
            KeyAction::InsertText { text, .. } => {
                let encoded =
                    wind_ipc::codec::encode_commit_text(text, None, false, chinese_mode, false);
                self.push_server.push_commit_to_active(&encoded);
                debug!("push_no_key_ctx: committed '{}'", text);
            }
            other => {
                debug!("push_no_key_ctx: 无需推送的 KeyAction {:?}", other);
            }
        }
    }

    /// 上屏动作的常用构造。**注意它不是唯一出口**——另有约 10 处直接构造 `InsertText`
    /// 的路径（顶码/智能符号/临拼等），故自提交打点与自动造词投喂**不在这里**做，
    /// 而是统一在 `handle_key_event_policed` 按最终返回的 action 处理
    /// （与 `record_input_stats` 同一收口思路）。
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
mod finalize_candidates_tests {
    //! 候选值展开汇聚点 `finalize_candidates`：所有输入方案共用，保证 `$` 语法一致生效。
    use super::*;
    use std::sync::Arc;
    use wind_config::config::Config;

    fn coord() -> Arc<Coordinator> {
        Coordinator::new_headless(Config::default(), None)
    }

    fn cand(text: &str) -> Candidate {
        Candidate {
            text: text.to_string(),
            ..Default::default()
        }
    }

    fn cand_code(text: &str, code: &str) -> Candidate {
        Candidate {
            text: text.to_string(),
            code: code.to_string(),
            ..Default::default()
        }
    }

    /// 词频记账的码：**码表按输入码，拼音/英文按候选码**。
    ///
    /// 真机现场：五笔下 `de` 的「有」带的是词条全码 `def`（前缀补全）。若拿它当词频 key，
    /// 在 `de`/`def` 下选中后，打 `d` 时「有」也跟着前移——而 `d`/`de`/`def` 是三个独立
    /// 码位，各自首选本该独立（`ProtectPolicy` 按输入码长分级保护正以此为前提）。
    ///
    /// 拼音反向：候选码是完整读音，打 `d` 选「东西」后打 `dongxi` 也该受益。
    #[test]
    fn freq_code_splits_codetable_from_pinyin() {
        let mut ct = cand_code("有", "def");
        ct.source = CandidateSource::CodeTable;
        assert_eq!(
            Coordinator::freq_code_with("de", &ct, false),
            "de",
            "码表须按输入码记账，否则 d/de/def 三个码位会互相串扰"
        );
        assert_eq!(Coordinator::freq_code_with("d", &ct, false), "d");
        assert_eq!(Coordinator::freq_code_with("def", &ct, false), "def");

        let mut py = cand_code("东西", "dongxi");
        py.source = CandidateSource::Pinyin;
        assert_eq!(
            Coordinator::freq_code_with("d", &py, false),
            "dongxi",
            "拼音按候选码——跨码位共享才是想要的行为"
        );
        assert_eq!(
            Coordinator::freq_code_with("d", &py, true),
            "dongxi",
            "英文开关不得波及拼音"
        );

        // 候选无码时退回输入缓冲（`cand_code` 的既有语义，两侧一致）
        let mut bare = cand_code("啊", "");
        bare.source = CandidateSource::Pinyin;
        assert_eq!(Coordinator::freq_code_with("a", &bare, false), "a");
    }

    /// 英文记账码口径可配（内部项 `english_code_scope`）：英文几乎全是前缀匹配，
    /// 跨码位共享与码位独立各有道理，尚未定论，故留旋钮而非写死。
    #[test]
    fn freq_code_english_scope_is_switchable() {
        let mut en = cand_code("hello", "hello");
        en.source = CandidateSource::English;
        assert_eq!(
            Coordinator::freq_code_with("hel", &en, false),
            "hello",
            "默认 candidate：打 hel 选 hello 后，打 he 也该受益"
        );
        assert_eq!(
            Coordinator::freq_code_with("hel", &en, true),
            "hel",
            "切到 input：各前缀独立学习，与码表同侧"
        );
    }

    /// 守门：**所有** `record_selection` 生产调用点的记账码都必须经 [`Coordinator::freq_code`]。
    ///
    /// 这道机械检查是必需的，不是洁癖。漏掉一处的后果是**静默的**：写进 FREQ 表的是个
    /// 孤儿键（双拼击键 `siyr` / 带分隔符 `xi'an` / 前缀补全 `si`），读取端按候选码
    /// `siyuan` 查，永远查不到——用户选了词却毫无变化，而任何常规测试都不会失败，因为
    /// 写入本身成功了。三处漏网（标点顶屏 ×2、临拼小键盘 ×1）都是这么藏到真机上的。
    ///
    /// 用 `include_str!` 编译期嵌入而非运行时读盘：CI 的 test 跑 Linux 宿主、clippy 交叉编
    /// Windows，运行时拼源码路径在两边语义不同，嵌入则与执行环境无关。
    #[test]
    fn every_record_selection_call_goes_through_freq_code() {
        // 判据是「实参文本里出现 freq_code」，这顺带强制**中转变量必须命名为 `freq_code`**
        // ——名字即声明，读代码时不必回溯赋值处就知道它已分流过。
        //
        // 白名单：确有理由不走 freq_code 的调用点，键为其首个实参的源码文本。
        const ALLOWED: &[(&str, &str)] = &[(
            "prefix",
            "commit_top_text：顶码机制归属码表，prefix 即被顶出的输入码，本就是码表口径",
        )];
        let sources: &[(&str, &str)] = &[
            ("coordinator.rs", include_str!("coordinator.rs")),
            ("handle_candidate.rs", include_str!("handle_candidate.rs")),
            ("handle_mode.rs", include_str!("handle_mode.rs")),
            ("handle_special.rs", include_str!("handle_special.rs")),
            ("handle_temp.rs", include_str!("handle_temp.rs")),
        ];
        let mut checked = 0usize;
        let mut bad: Vec<String> = Vec::new();
        for (name, src) in sources {
            // 只看 `#[cfg(test)]` 之前的部分：测试自己用字面量直接构造键是合法的。
            let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
            for (off, _) in prod.match_indices(".record_selection(") {
                let args = &prod[off + ".record_selection(".len()..];
                // 切出第一个实参：按括号深度找顶层逗号（实参可能是 `self.freq_code(a, b)`）。
                let mut depth = 0i32;
                let mut end = args.len();
                for (i, ch) in args.char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' if depth == 0 => {
                            end = i;
                            break;
                        }
                        ')' => depth -= 1,
                        ',' if depth == 0 => {
                            end = i;
                            break;
                        }
                        _ => {}
                    }
                }
                let arg = args[..end].trim().trim_start_matches('&').trim();
                checked += 1;
                if arg.contains("freq_code") {
                    continue;
                }
                if !ALLOWED.iter().any(|(a, _)| *a == arg) {
                    // 收齐再报：逐个 assert 只暴露第一处，改完重跑才发现还有下一处。
                    bad.push(format!("{name}: `{arg}`"));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "以下 record_selection 的记账码未经 freq_code：\n  {}\n\
             码表须按输入码、拼音/英文须按候选码——直接传击键缓冲会在双拼/分隔符/\n\
             前缀补全下写成读不到的孤儿键。改用 `&self.freq_code(buf, &cand)`（中转变量\n\
             命名为 `freq_code`），确有理由不走的请加进本测试的 ALLOWED 并写明理由。",
            bad.join("\n  ")
        );
        // 反向保证：若哪天调用点被重命名/重构掉，本测试不得退化成空跑而静默变绿。
        //
        // 下限随**有意的**收口下调过一次：进模式顶屏（临英 / mix / 特殊模式 / 临拼）原本各持
        // 一份逐字相同的记账 + 拼接代码，合并进 `take_committed_with_highlight` 后四处并作一处，
        // 12 → 9。下调前务必确认是合并而非漏调——这条断言的用途正是逼人回来说明减少的原因。
        assert!(
            checked >= 9,
            "只扫到 {checked} 个 record_selection 调用点，远少于预期——\
             调用点被改名或本测试的扫描方式失效了，先修测试再说"
        );
    }

    /// 方案归属的**读 / 写 / 调试三处必须取自同一处**（`effective_data_schema`）。
    ///
    /// 不一致的后果分两种，都很隐蔽：读写不同源 ⇒ 写进 qsym、读的是 wubi86，记账看着成功
    /// 而候选顺序永远不动；调试不同源 ⇒ 调试段显示的计数与排序实际用的不是同一个 key，
    /// 排查时被它带偏。
    ///
    /// 判据是「首个实参 ∈ 允许集合」，这顺带强制中转变量必须叫 `owner`——名字之外的任何
    /// 变量名都会被拦下，逼新增调用点的人回到这里说明它取的是哪个方案。
    #[test]
    fn schema_scoped_reads_writes_and_debug_share_one_source() {
        // `None` = 显式走 active 路径（普通输入）；其余必须能追溯到 effective_data_schema。
        const ALLOWED: &[&str] = &[
            "None",
            "owner.as_deref()",
            "self.effective_data_schema(state).as_deref()",
            // 转发层：`record_selection` / `apply_freq_rerank` / `apply_shadow` 三个薄包装
            // 把自己的参数原样传下去。
            "schema_override",
        ];
        const CALLS: &[&str] = &[
            ".record_selection_in(",
            ".apply_freq_rerank_in(",
            ".apply_shadow_in(",
            ".build_debug_schema_ctx(",
        ];
        let sources: &[(&str, &str)] = &[
            ("coordinator.rs", include_str!("coordinator.rs")),
            ("handle_candidate.rs", include_str!("handle_candidate.rs")),
            ("handle_special.rs", include_str!("handle_special.rs")),
        ];
        let mut checked = 0usize;
        let mut bad: Vec<String> = Vec::new();
        for (name, src) in sources {
            let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
            for call in CALLS {
                for (off, _) in prod.match_indices(call) {
                    let args = &prod[off + call.len()..];
                    // 切首个实参：按括号深度找顶层逗号（实参可能自带括号调用）。
                    let mut depth = 0i32;
                    let mut end = args.len();
                    for (i, ch) in args.char_indices() {
                        match ch {
                            '(' => depth += 1,
                            ')' if depth == 0 => {
                                end = i;
                                break;
                            }
                            ')' => depth -= 1,
                            ',' if depth == 0 => {
                                end = i;
                                break;
                            }
                            _ => {}
                        }
                    }
                    let arg = args[..end].trim();
                    checked += 1;
                    if !ALLOWED.contains(&arg) {
                        bad.push(format!("{name}: {call}…) 的方案实参 `{arg}`"));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "以下调用点的方案归属不是取自 effective_data_schema：\n  {}\n\
             读(apply_freq_rerank_in) / 写(record_selection_in) / 调试(build_debug_schema_ctx)\n\
             三处必须同源，否则「写进 A、读的是 B」——记账看着成功，排序永远不动。",
            bad.join("\n  ")
        );
        // 反向保证：调用点被改名或扫描失效时，本测试不得退化成空跑而静默变绿。
        assert!(
            checked >= 6,
            "只扫到 {checked} 个方案归属调用点，少于预期——扫描方式失效了，先修测试"
        );
    }

    /// **shadow 的码必须处处归一**：读端 `apply_shadow` 与写端 `candidate_op_scope` 若各取
    /// 各的，失配是**完全静默**的——双拼下规则写进 `hao`、读的却是击键 `hc`，用户看到的只是
    /// 「置顶了没反应」，日志、界面、返回值全都正常。
    ///
    /// 判据是「第二个实参 ∈ 允许集合」，顺带强制中转变量必须叫 `shadow_code`：任何别的名字
    /// 都会被拦下，逼新增调用点的人回到这里说明它取的是哪个码域。这套机械扫描是本仓的既有
    /// 先例（见上面两个测试），因为「N 个调用点都要做同一件事」这类不变量靠注释守不住——
    /// `freq_code` 那次连红四次才把四处遗漏抓干净。
    #[test]
    fn every_shadow_read_goes_through_normalized_code() {
        // 主输入路：一律走 `shadow_code_of` 取出的中转变量。
        // 特殊模式：码在 `special_buffer`（码表方案，无第二编码域），恒等即正确。
        // 空串：特殊模式浏览态的合法键位，见 `apply_shadow_in` 的守卫。
        const ALLOWED: &[&str] = &[
            "shadow_code",
            "state.special_buffer",
            "\"\"",
            // 转发层：`apply_shadow` 是 `apply_shadow_in` 的薄包装，原样传自己的参数。
            "code",
            // 菜单灰显走 `candidate_op_scope` 的产物，其 `code` 已在那里归一（同写端一处取值）。
            "scope.code",
        ];
        const CALLS: &[&str] = &[
            ".apply_shadow(",
            ".apply_shadow_in(",
            // 「恢复默认」的可用性判据也必须同码，否则菜单项灰着而规则其实存在。
            ".shadow_has_rule(",
        ];
        let sources: &[(&str, &str)] = &[
            ("coordinator.rs", include_str!("coordinator.rs")),
            ("handle_candidate.rs", include_str!("handle_candidate.rs")),
            ("handle_special.rs", include_str!("handle_special.rs")),
            ("handle_menu.rs", include_str!("handle_menu.rs")),
        ];
        let mut checked = 0usize;
        let mut bad: Vec<String> = Vec::new();
        for (name, src) in sources {
            let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
            for call in CALLS {
                // `apply_shadow_in` 多一个前置的 schema 实参，码是第 3 个；`apply_shadow` 是第 2 个。
                let code_pos = if *call == ".apply_shadow_in(" { 2 } else { 1 };
                for (off, _) in prod.match_indices(call) {
                    let args = &prod[off + call.len()..];
                    // 按括号深度切顶层逗号，取第 code_pos 个实参（实参可能自带括号调用）。
                    let mut depth = 0i32;
                    let mut cur = 0usize;
                    let mut start = 0usize;
                    let mut found: Option<&str> = None;
                    for (i, ch) in args.char_indices() {
                        match ch {
                            '(' => depth += 1,
                            ')' if depth == 0 => {
                                if cur == code_pos {
                                    found = Some(&args[start..i]);
                                }
                                break;
                            }
                            ')' => depth -= 1,
                            ',' if depth == 0 => {
                                if cur == code_pos {
                                    found = Some(&args[start..i]);
                                    break;
                                }
                                cur += 1;
                                start = i + 1;
                            }
                            _ => {}
                        }
                    }
                    let Some(arg) = found else { continue };
                    let arg = arg.trim().trim_start_matches('&').trim();
                    checked += 1;
                    if !ALLOWED.contains(&arg) {
                        // 收齐再报：逐个 assert 只暴露第一处，改完重跑才发现还有下一处。
                        bad.push(format!("{name}: {call}…) 的码实参 `{arg}`"));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "以下 shadow 读取点的码未经归一：\n  {}\n\
             双拼与全拼折叠为同一个 schema，码若仍取击键，`hc` 与 `hao` 会落成两个互不相认\n\
             的键。改用 `Self::shadow_code_of(state)`（中转变量命名为 `shadow_code`），\n\
             确有理由不走的请加进本测试的 ALLOWED 并写明理由。",
            bad.join("\n  ")
        );
        assert!(
            checked >= 5,
            "只扫到 {checked} 个 shadow 读取点，少于预期——调用点被改名或扫描失效了，先修测试"
        );
    }

    #[test]
    fn aa_group_expands_inline_when_code_absent() {
        let c = coord();
        // 无码信息（code 空）→ 视为精确，逐成员炸开。
        let out = c.finalize_candidates(vec![cand(r#"$AA("数字", "①②③")"#)], "sz");
        let texts: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["①", "②", "③"], "$AA 应一对多炸开为逐字符候选");
        assert!(out.iter().all(|c| !c.is_command && !c.is_group));
    }

    #[test]
    fn aa_group_expands_inline_at_exact_code() {
        let c = coord();
        // 精确码（候选码 == 输入 "arrx"）→ 逐成员炸开。
        let out = c.finalize_candidates(vec![cand_code(r#"$AA("箭头", "←↑→↓")"#, "arrx")], "arrx");
        let texts: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["←", "↑", "→", "↓"]);
    }

    #[test]
    fn aa_group_collapses_to_name_at_prefix() {
        let c = coord();
        // 前缀（候选码 "arrx" 长于输入 "arr"）→ 折叠为组名候选，不炸开。
        let out = c.finalize_candidates(vec![cand_code(r#"$AA("箭头", "←↑→↓")"#, "arrx")], "arr");
        assert_eq!(out.len(), 1, "前缀应折叠为单个组名候选");
        assert_eq!(out[0].text, "箭头", "折叠候选显示组名");
        assert!(out[0].is_group, "折叠候选标 is_group");
        assert_eq!(
            out[0].group_code, "arrx",
            "group_code 为完整码，选中补全后重查展开"
        );
        assert!(!out[0].is_command);
    }

    #[test]
    fn marks_cc_command_and_keeps_source() {
        let c = coord();
        let src = r#"$CC("切简繁", ime.toggle("s2t"))"#;
        let out = c.finalize_candidates(vec![cand(src)], "co");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "切简繁", "$CC display 作候选文本");
        assert!(out[0].is_command, "$CC 应标 is_command");
        assert_eq!(out[0].phrase_template, src, "命令源留存供选中执行");
    }

    #[test]
    fn plain_candidates_pass_through_unchanged() {
        let c = coord();
        // 普通词 + 含 $ 但非语法文本（价格$5）：均原样保留，零干预。
        let out = c.finalize_candidates(vec![cand("你好"), cand("价格$5")], "nh");
        let texts: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["你好", "价格$5"]);
        assert!(out.iter().all(|c| !c.is_command));
    }

    #[test]
    fn already_command_candidate_is_not_re_expanded() {
        let c = coord();
        // 已是 is_command 的候选（短语命中）：跳过二次展开，原样保留。
        let mut pre = cand(r#"$AA("x","ab")"#);
        pre.is_command = true;
        let out = c.finalize_candidates(vec![pre], "x");
        assert_eq!(out.len(), 1, "已标命令不应被再炸开");
        assert!(out[0].is_command);
    }

    /// 本组用例的输入串。传给 `candidate_display_order` 的跨来源档位：取长度判「候选是否
    /// 消费了整串」、取全串判「码 == 输入」（档 0）；`mixed=false` 的用例里该档不参与，
    /// 取值无影响。
    const XU_LEN: &str = "xu";

    fn cand_ordered(text: &str, base_order: i32, natural_order: i32, weight: i32) -> Candidate {
        Candidate {
            text: text.into(),
            code: "y".into(),
            base_order,
            natural_order,
            weight,
            ..Default::default()
        }
    }

    /// 混输打 `xu` 的三档现场（`per_page=7` 时「需」原本在第 125 位 / 第 18~20 页）：
    /// 码表精确「弱」(`xu`, 二简码) → 拼音精确「需」(`code==xu`, 该音节最高频字) →
    /// 码表前缀补全「弹幕」(`xuaj`, 要打满 4 码才精确)。
    ///
    /// 权重取真实值：码表带 `PARTIAL_MATCH_BOOST`(500K)、拼音已 `÷PINYIN_TIER_SCALE`(100)，
    /// 所以「需」纯按权重必输（69 vs 501,554）——这一条锁的正是「层级先于权重」。
    fn xu_scene() -> (Candidate, Candidate, Candidate) {
        let ct_exact = Candidate {
            text: "弱".into(),
            code: "xu".into(),
            weight: 9950, // 真实词频（混输引擎已不再加成，见 truncation_tier）
            is_common: true,
            is_exact_code: true,
            source: CandidateSource::CodeTable,
            ..Default::default()
        };
        let py_exact = Candidate {
            text: "需".into(),
            code: "xu".into(),
            weight: 69, // 6999 / 100
            is_common: true,
            source: CandidateSource::Pinyin,
            ..Default::default()
        };
        let ct_prefix = Candidate {
            text: "弹幕".into(),
            code: "xuaj".into(),
            weight: 1554, // 真实词频（混输引擎已不再加成，见 truncation_tier）
            is_common: true,
            source: CandidateSource::CodeTable,
            ..Default::default()
        };
        (ct_exact, py_exact, ct_prefix)
    }

    /// 混输：拼音精确档插在「码表精确」与「码表前缀补全」之间。
    #[test]
    fn mixed_pinyin_exact_sits_between_codetable_exact_and_prefix() {
        let (ct_exact, py_exact, ct_prefix) = xu_scene();
        // 故意以最不利顺序放入，确保结果由排序而非原序决定。
        let mut cands = [ct_prefix, py_exact, ct_exact];
        cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
        let order: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            order,
            vec!["弱", "需", "弹幕"],
            "混输应为「码表精确 → 拼音精确 → 码表前缀补全」"
        );
    }

    /// ★ 反向锁一：`mixed=false`（纯拼音/纯码表）时本档**不得生效**。
    /// 纯拼音下全体候选同为 Pinyin 来源，该键会退化成「is_common 优先」，把含生僻字的多字词
    /// 硬降到全部常用单字之后 —— 那是明显回归。此处以码表权重序验证键未参与。
    #[test]
    fn non_mixed_keeps_weight_order_for_pinyin() {
        let (_, py_exact, ct_prefix) = xu_scene();
        let mut cands = [py_exact, ct_prefix];
        cands.sort_by(|a, b| candidate_display_order(a, b, false, false, XU_LEN));
        assert_eq!(
            cands[0].text, "弹幕",
            "非混输时应回落到纯权重序（1554 > 69），拼音精确档不得生效"
        );
    }

    /// ★ **前缀短语排在码表前缀补全之后**，且不再依赖权重量级。
    ///
    /// 这条次序一直存在，但从前是**加成的副产品**：混输引擎给码表前缀补全 +500K，而前缀短语
    /// 拿原始权重（`PHRASE_WEIGHT_BASE` 只加给精确码短语），于是码表恒赢，`source_tier` 里
    /// 二者「同档比 weight」那一步从来没有真正裁决过。加成拆掉后它才第一次生效——而那是
    /// 「码表词频 vs 用户设定的短语权重」，两个不同量纲的数硬比。故拆成两档写死。
    ///
    /// 用**短语权重高于码表**的取值，确保测的是档位而非权重。
    #[test]
    fn mixed_prefix_phrase_sits_below_codetable_prefix() {
        let (_, _, ct_prefix) = xu_scene();
        let prefix_phrase = Candidate {
            text: "短语正文".into(),
            weight: 99_999, // 远高于码表的 1554
            is_phrase: true,
            is_exact_code: false, // lookup_prefix 命中
            source: CandidateSource::Phrase,
            ..Default::default()
        };
        let mut cands = [prefix_phrase, ct_prefix];
        cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
        assert_eq!(
            cands[0].text, "弹幕",
            "码表前缀补全须先于前缀短语，且与两者的权重取值无关"
        );
    }

    /// ★ 反向锁二：生僻拼音字（`is_common=false`）不提档，仍留在码表前缀补全之后。
    /// 这一条锁住「不设条数上限、由检索范围把关」的设计——`xu` 有 329 条同音字，
    /// 若生僻字一并提档会把码表候选整片挤出候选配额。
    #[test]
    fn mixed_rare_pinyin_char_stays_below_codetable_prefix() {
        let (_, py_exact, ct_prefix) = xu_scene();
        let rare = Candidate {
            text: "𬣙".into(),
            weight: 0,
            is_common: false,
            ..py_exact
        };
        let mut cands = [rare, ct_prefix];
        cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
        assert_eq!(
            cands[0].text, "弹幕",
            "生僻同音字不得越过码表前缀补全（否则 329 条同音字会挤爆配额）"
        );
    }

    /// 档内仍按权重竞争：两条都在拼音精确档时，高频字在前（本键在同档不表态）。
    #[test]
    fn mixed_within_pinyin_exact_tier_weight_still_decides() {
        let (_, py_exact, _) = xu_scene();
        let xu_low = Candidate {
            text: "序".into(),
            weight: 13, // 1321 / 100
            ..py_exact.clone()
        };
        let mut cands = [xu_low, py_exact];
        cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
        assert_eq!(cands[0].text, "需", "同档内仍按权重降序（69 > 13）");
    }

    /// 回归：跨词库排序须以 `base_order` 隔离，`natural_order` 只在同档内当 tiebreaker。
    /// 复刻 flypy「y」现场——主库「一」(base_order=0, natural_order 大) vs 一简次选库「有时」
    /// (base_order=2, natural_order 小)：修复前协调器仅按 natural_order 升序会把「有时」拉到首位，
    /// 修复后「一」（更小 base_order）应稳居首位。两种模式（含/不含权重）都成立（此处权重均 0）。
    #[test]
    fn base_order_wins_over_cross_dict_natural_order() {
        let yi = cand_ordered("一", 0, 57285, 0);
        let youshi = cand_ordered("有时", 2, 24, 0);
        for ignore_weight in [false, true] {
            // 故意以「有时」在前的顺序放入，确保是排序而非原序决定结果。
            let mut cands = [youshi.clone(), yi.clone()];
            cands.sort_by(|a, b| candidate_display_order(a, b, ignore_weight, false, XU_LEN));
            assert_eq!(
                cands[0].text, "一",
                "base_order=0 主库候选应排在 base_order=2 次选库候选之前（ignore_weight={ignore_weight}）"
            );
            assert_eq!(cands[1].text, "有时");
        }
    }

    /// natural 模式忽略权重：主库低权重条目（base_order 0）须排在次选库高权重条目（base_order 1）
    /// 之前，与引擎 `by_natural` 一致；weight 模式则相反（高权重靠前）。证明 ignore_weight 生效。
    #[test]
    fn natural_mode_ignores_weight_weight_mode_respects_it() {
        let main_low = cand_ordered("主低", 0, 100, 1); // 主库、低权重
        let extra_high = cand_ordered("扩高", 1, 5, 999); // 次选库、高权重
        // weight 模式：权重降序主导 → 扩高(999) 在前。
        let mut w = [main_low.clone(), extra_high.clone()];
        w.sort_by(|a, b| candidate_display_order(a, b, false, false, XU_LEN));
        assert_eq!(w[0].text, "扩高", "weight 模式高权重应靠前");
        // natural 模式：忽略权重 → base_order 升序主导，主库(0) 在前。
        let mut n = [main_low, extra_high];
        n.sort_by(|a, b| candidate_display_order(a, b, true, false, XU_LEN));
        assert_eq!(
            n[0].text, "主低",
            "natural 模式忽略权重、按 base_order 升序，主库应靠前"
        );
    }

    /// 回归：码表精确匹配须先于高权重前缀词组，且该优先级**不得跨匹配层提拔**。
    ///
    /// 复刻 usr 现场（古精86五笔）：简码「新」(usr, 11777) vs 前缀词组「新的」(usrq, 47487)。
    /// 引擎已排好序，但本函数会无条件重排全部候选——若不复刻 `is_exact_code` 键，引擎结果
    /// 会在这里被按纯权重推翻（原始 bug）。同时验证前缀枚举短语（`is_prefix=true`）仍留在
    /// 精确层之下，不因本键而上浮。
    ///
    /// ## ⚠️ 精确档**内部**的先后已改由权重裁决
    ///
    /// 引导键组短语（`$SS`/`$AA`）此前带 `PHRASE_WEIGHT_BASE`(40M)，在精确档内**恒**居首；
    /// 那个常量已整体删除（见 `lookup` 分支的 `weight` 注释）。现在它与码表精确候选真比权重，
    /// 于是先后**取决于配置**：系统组短语实测 1230，会输给 11777 的简码；调到简码之上就赢。
    ///
    /// 下方两个方向都断言。**只测一个方向证明不了裁决者是权重**——若某个更靠前的键碰巧
    /// 也给出同样次序，单向断言会一路绿到底（本仓踩过：混输六个加成里有恒赢的偏置，
    /// 拆掉后一批从未执行过的比较才第一次生效，而全套测试当时一条没红）。
    #[test]
    fn exact_code_outranks_prefix_but_stays_within_match_layer() {
        let exact = Candidate {
            text: "新".into(),
            code: "usr".into(),
            weight: 11777,
            // natural_order 拉开档次，使 ignore_weight（natural 模式）下精确档内也有确定序，
            // 否则平局落到稳定排序取输入序，断言会随入表顺序漂移。
            natural_order: 10,
            is_exact_code: true,
            ..Default::default()
        };
        let prefix_word = Candidate {
            text: "新的".into(),
            code: "usrq".into(),
            weight: 47487,
            natural_order: 20,
            ..Default::default()
        };
        // 前缀枚举短语：is_prefix=true 使其落在更低匹配层。权重取真实量级（系统短语 800~2000），
        // 高于「新」也不该把它拉到精确候选之上——`cmp_match_layers` 在权重之前。
        let prefix_phrase = Candidate {
            text: "短语".into(),
            weight: 2000,
            is_phrase: true,
            is_prefix: true,
            ..Default::default()
        };
        // 引导键导航候选（$SS/$AA 组）：三个匹配层标志均为 false，须显式属精确档，
        // 否则会被每一条码表精确候选压下去——用户按引导键时首选会变成五笔单字。
        let guide_group = |weight: i32| Candidate {
            text: "组名".into(),
            weight,
            is_phrase: true,
            is_group: true,
            is_exact_code: true,
            ..Default::default()
        };
        // 故意以「最不利」的顺序放入，确保结果由排序而非原序决定。
        let order_of = |group: Candidate, ignore_weight: bool| -> Vec<String> {
            let mut cands = vec![
                prefix_phrase.clone(),
                prefix_word.clone(),
                exact.clone(),
                group,
            ];
            cands.sort_by(|a, b| candidate_display_order(a, b, ignore_weight, false, XU_LEN));
            cands.into_iter().map(|c| c.text).collect()
        };

        // ① 系统组短语的真实权重（1230）低于该简码 → 组名落到简码之后，但仍在精确档内、
        //    整体先于前缀词组。这是删掉 40M 后的**新行为**。
        assert_eq!(
            order_of(guide_group(1230), false),
            vec!["新", "组名", "新的", "短语"],
            "精确档内按权重：组短语 1230 < 简码 11777 ⇒ 排其后；两者仍先于前缀词组"
        );
        // ② 反向对照：把组短语权重调到简码之上，它就回到首位。证明裁决者确实是 weight。
        assert_eq!(
            order_of(guide_group(20000), false),
            vec!["组名", "新", "新的", "短语"],
            "精确档内按权重：组短语 20000 > 简码 11777 ⇒ 回到首位"
        );
        // ③ natural 模式忽略权重 ⇒ 精确档内退化为 base_order/natural_order（组名 0 < 新 10），
        //    两个权重取值结果相同——这一档的次序与权重无关。
        for w in [1230, 20000] {
            assert_eq!(
                order_of(guide_group(w), true),
                vec!["组名", "新", "新的", "短语"],
                "natural 模式下精确档按 natural_order，与权重 {w} 无关"
            );
        }
    }
}

/// 组合区是否该显示**码表整句的编码单元切分**。
///
/// 判据两条都要：有切分串（本次确实解出了整句），且**当前高亮的就是那条整句候选**。
/// 后者不能省——用户翻到别的候选时，屏幕上留着一个不对应它的切法比不切更糊涂。
///
/// 抽成自由函数是为了可测：`effective_preedit_body` 收 `&self`，要构造整个 Coordinator
/// 才测得到，而这里真正要锁的只是这两条判据。
fn wants_codetable_split(body: &str, cand: Option<&Candidate>) -> bool {
    !body.is_empty()
        && cand.is_some_and(|c| c.is_sentence && c.source == CandidateSource::CodeTable)
}

#[cfg(test)]
mod clear_recheck_tests {
    //! 满码空码清空的**第三道门**（`clear_blocked_by_candidates`）。
    //!
    //! 前两道在引擎内（码表 `clear_on_empty_max` → 混输 `should_clear` 的拼音守护），
    //! 见 `wind-engine` 的 `mixed_trailing_partial_pinyin` 与 `mixed::engine::tests`。
    //! 本模块只锁这一道：哪些候选**拦得住**清空。
    use super::*;

    fn pinyin(text: &str, consumed: usize) -> Candidate {
        Candidate {
            text: text.into(),
            source: CandidateSource::Pinyin,
            consumed_length: consumed,
            ..Default::default()
        }
    }

    fn codetable(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            source: CandidateSource::CodeTable,
            // 码表候选恒不标注消费长度（选词即消费整串）。
            consumed_length: 0,
            ..Default::default()
        }
    }

    #[test]
    fn codetable_split_shows_only_on_highlighted_sentence() {
        let sentence = Candidate {
            text: "工作工作".into(),
            source: CandidateSource::CodeTable,
            is_sentence: true,
            ..Default::default()
        };
        let plain = codetable("工作");

        assert!(
            wants_codetable_split("aawt'aawt", Some(&sentence)),
            "高亮整句候选时应显示切分"
        );
        assert!(
            !wants_codetable_split("aawt'aawt", Some(&plain)),
            "高亮普通码表候选时不得显示切分——那个切法不对应它"
        );
        assert!(
            !wants_codetable_split("", Some(&sentence)),
            "没有切分串时不显示"
        );
        assert!(!wants_codetable_split("aawt'aawt", None), "无候选时不显示");
        // 拼音整句不走这条：它有自己的音节拆分形态。
        let pinyin_sentence = Candidate {
            text: "工作".into(),
            source: CandidateSource::Pinyin,
            is_sentence: true,
            ..Default::default()
        };
        assert!(!wants_codetable_split("aawt'aawt", Some(&pinyin_sentence)));
    }

    #[test]
    fn empty_list_never_blocks() {
        assert!(!clear_blocked_by_candidates(&[], 4), "空列表不得拦清空");
    }

    /// 真机现象 `nunl`：拼音候选「嫩」只解释了前 3 码 `nun`，没解释完整串 → 不算匹配。
    /// 这是修复前 `state.candidates.is_empty()` 那版的直接失效点。
    #[test]
    fn partial_pinyin_candidate_does_not_block() {
        let cands = vec![pinyin("嫩", 3)];
        assert!(
            !clear_blocked_by_candidates(&cands, 4),
            "拼音部分匹配（consumed 3 < 4）不得替整串挡下清空"
        );
    }

    /// 反向锁：消费整串的拼音候选（`nuan`→「暖」，码表无字）是货真价实的匹配，必须拦住——
    /// 否则关掉守护开关的用户再也打不出那些只有拼音出得来的字。
    #[test]
    fn full_pinyin_candidate_blocks() {
        let cands = vec![pinyin("暖", 4)];
        assert!(
            clear_blocked_by_candidates(&cands, 4),
            "拼音消费整串 → 有效匹配，必须拦住清空"
        );
    }

    /// 非拼音来源（短语 / 码表 / 英文）一律拦住：引擎算 `should_clear` 时看不见协调器
    /// 随后追加的短语，本道门存在的原始理由就是它。
    #[test]
    fn non_pinyin_candidate_always_blocks() {
        assert!(
            clear_blocked_by_candidates(&[codetable("工")], 4),
            "码表候选（consumed_length=0 未标注）须视为整串匹配并拦住清空"
        );
        let phrase = Candidate {
            text: "地址".into(),
            is_phrase: true,
            ..Default::default()
        };
        assert!(
            clear_blocked_by_candidates(&[phrase], 4),
            "短语候选必须拦住清空（zzbd 等码表无字但短语命中）"
        );
    }

    /// 混合列表：只要有**一条**有效候选就拦住，部分匹配的拼音不参与计数。
    #[test]
    fn mixed_list_blocks_only_on_effective_candidate() {
        let only_partial = vec![pinyin("嫩", 3), pinyin("女", 2)];
        assert!(
            !clear_blocked_by_candidates(&only_partial, 4),
            "全是拼音部分匹配 → 不拦"
        );
        let with_full = vec![pinyin("嫩", 3), pinyin("暖", 4)];
        assert!(
            clear_blocked_by_candidates(&with_full, 4),
            "混有一条消费整串的拼音候选 → 拦住"
        );
    }
}

#[cfg(test)]
mod dynamic_candidate_shadow_tests {
    //! 求值型候选（`date`/`time` 等短语）的 shadow 接线：规则须按**稳定 id** 落键与匹配，
    //! 否则次日文本一变即失配——用户侧表现为「候选调整昨天设了、今天被还原」。
    use super::*;
    use std::sync::Arc;
    use wind_config::config::Config;
    use wind_store::store::Store;

    fn store_at(name: &str) -> Arc<Store> {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        Arc::new(Store::open(&p).unwrap())
    }

    fn coord_with(store: Arc<Store>) -> Arc<Coordinator> {
        Coordinator::new_headless_with_store(Config::default(), None, store)
    }

    /// 一条纯文本候选（拼音/码表静态候选，无稳定 id → 规则按 word 匹配）。
    fn plain(text: &str) -> Candidate {
        Candidate {
            text: text.to_string(),
            ..Default::default()
        }
    }

    /// 一条 `date` 短语候选：`text` 是当日求值结果，`id` 是模板身份。
    fn date_cand(text: &str, template: &str) -> Candidate {
        Candidate {
            text: text.to_string(),
            is_phrase: true,
            is_exact_code: true,
            phrase_template: template.to_string(),
            id: Coordinator::phrase_cand_id("date", template),
            source: CandidateSource::Phrase,
            ..Default::default()
        }
    }

    /// id 构造：模板原文入 id，空模板 → 空 id（无稳定身份，落回文本匹配）。
    #[test]
    fn phrase_cand_id_shape() {
        assert_eq!(
            Coordinator::phrase_cand_id("date", "$Y-$MM-$DD"),
            "phrase:date:$Y-$MM-$DD"
        );
        assert_eq!(Coordinator::phrase_cand_id("date", ""), "");
    }

    /// 核心回归：昨天置顶的 `date` 格式，今天（文本已全变）依然置顶。
    ///
    /// ⚠ 判别力全在「写规则用的文本」与「今天候选的文本」**不相同**——若两者相同，
    /// 按 text 匹配的旧实现也会全绿，这个测试就成了假测试。
    #[test]
    fn pinned_date_format_survives_next_day() {
        let store = store_at("wind_coord_shadow_date.redb");
        let c = coord_with(store.clone());
        let schema = c
            .engine_mgr
            .data_schema_id(&c.engine_mgr.active_schema_id());
        let tpl = "$Y-$MM-$DD";

        // 昨天：用户把 `$Y-$MM-$DD` 那条置顶，规则 word = 昨天的求值文本。
        store
            .pin_shadow(
                &schema,
                "date",
                "2026-07-28",
                Some(&Coordinator::phrase_cand_id("date", tpl)),
                0,
            )
            .unwrap();

        // 今天：候选文本全变，且被置顶的那条排在第二位。
        let mut cands = vec![
            date_cand("2026年7月29日", "$Y年$M月$D日"),
            date_cand("2026-07-29", tpl),
            date_cand("2026.07.29", "$Y.$MM.$DD"),
        ];
        c.apply_shadow(&mut cands, "date");

        assert_eq!(
            cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["2026-07-29", "2026年7月29日", "2026.07.29"],
            "按 cand_id 匹配，昨天的置顶今天仍生效"
        );
    }

    /// 反向：规则不带 id（存量规则 / 手工添加）时仍按文本匹配，老行为不回归。
    #[test]
    fn legacy_rule_without_id_still_matches_by_text() {
        let store = store_at("wind_coord_shadow_legacy.redb");
        let c = coord_with(store.clone());
        let schema = c
            .engine_mgr
            .data_schema_id(&c.engine_mgr.active_schema_id());
        store.pin_shadow(&schema, "aaaa", "敬", None, 0).unwrap();

        let mut cands = vec![
            Candidate {
                text: "工".into(),
                ..Default::default()
            },
            Candidate {
                text: "敬".into(),
                ..Default::default()
            },
        ];
        c.apply_shadow(&mut cands, "aaaa");
        assert_eq!(
            cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["敬", "工"]
        );
    }

    /// **双拼击键读得到全拼下写的规则**——这是「拼音候选置顶」在双拼/全拼共用词库时
    /// 能成立的全部理由。
    ///
    /// `data_schema_id` 早已把两种方案折叠成同一个 schema，缺的一直是 code 维度：读写两端
    /// 都取 `input_buffer`（击键域），于是双拼的 `hc` 与全拼的 `hao` 落成两个互不相认的键。
    ///
    /// 反向对照不可省：末尾那段用击键码再读一次，**必须读不到**。没有它，这个测试在
    /// 「归一码根本没接线、而 apply_shadow 恰好对空列表无操作」之类的情形下也会绿。
    #[test]
    fn shuangpin_keystroke_reads_rule_written_under_full_pinyin() {
        let store = store_at("wind_coord_shadow_norm.redb");
        let c = coord_with(store.clone());
        let schema = c
            .engine_mgr
            .data_schema_id(&c.engine_mgr.active_schema_id());

        // 全拼下把「好」置顶（或等价地：另一台机器/另一次会话用全拼写下的存量规则）。
        store.pin_shadow(&schema, "hao", "好", None, 0).unwrap();

        // 双拼击键 `hc`，引擎给出归一码 `hao`（见 ConvertResult::shadow_code）。
        let code = {
            let mut state = c.state.lock().unwrap_or_else(|e| e.into_inner());
            state.input_buffer = "hc".into();
            state.shadow_code = "hao".into();
            Coordinator::shadow_code_of(&state).to_string()
        };
        assert_eq!(code, "hao", "归一码非空时必须优先于击键缓冲");

        let mut cands = vec![plain("耗"), plain("好"), plain("号")];
        c.apply_shadow(&mut cands, &code);
        assert_eq!(
            cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["好", "耗", "号"],
            "双拼 hc 应读到全拼 hao 下的置顶规则"
        );

        // 反向对照：拿击键码去读，规则不该命中——证明上面的绿来自归一，而非别的原因。
        let mut raw = vec![plain("耗"), plain("好"), plain("号")];
        c.apply_shadow(&mut raw, "hc");
        assert_eq!(
            raw.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["耗", "好", "号"],
            "击键码 hc 下没有规则，顺序必须原样——否则上面那条断言证明不了归一起了作用"
        );
    }

    /// 全拼路径**恒等**：归一码为空时落回击键缓冲，存量规则一条都不用迁。
    #[test]
    fn full_pinyin_shadow_code_is_identity() {
        let c = coord_with(store_at("wind_coord_shadow_identity.redb"));
        let state = c.state.lock().unwrap_or_else(|e| e.into_inner());
        // 默认 State：input_buffer 与 shadow_code 均空。
        assert_eq!(Coordinator::shadow_code_of(&state), "");
        drop(state);

        let mut state = c.state.lock().unwrap_or_else(|e| e.into_inner());
        state.input_buffer = "xi'an".into();
        state.shadow_code.clear(); // 全拼引擎恒给空串
        assert_eq!(
            Coordinator::shadow_code_of(&state),
            "xi'an",
            "全拼须原样取击键（含手动分隔符——`'` 是硬边界，剥掉会与 xian 撞 key）"
        );
    }

    /// 短语上屏不写 FREQ：求值型文本每次都是新键，记了永不命中、只堆垃圾。
    /// 同一条 code 下的码表候选照常记账，证明拦的是来源而非整条路径。
    #[test]
    fn phrase_commit_does_not_record_freq() {
        let store = store_at("wind_coord_phrase_freq.redb");
        let c = coord_with(store.clone());
        let schema = c
            .engine_mgr
            .data_schema_id(&c.engine_mgr.active_schema_id());

        c.record_selection("date", "2026-07-29", CandidateSource::Phrase);
        assert!(
            store
                .get_freq(&schema, "date", "2026-07-29")
                .unwrap()
                .is_none(),
            "短语候选不得写入词频"
        );

        // 上屏历史仍应记（`z` 键重复上屏 / cmdbar last() 依赖它）。
        assert_eq!(
            c.recent_commits_snapshot().first().map(|s| s.as_str()),
            Some("2026-07-29"),
            "不记词频不等于不记上屏历史"
        );
    }
}
