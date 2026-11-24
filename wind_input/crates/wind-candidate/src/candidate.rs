//! 候选词数据类型
//!
//! 与 Go 版本 `wind_input/internal/candidate/candidate.go` 对齐。

use serde::{Deserialize, Serialize};

/// 候选词来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CandidateSource {
    #[serde(rename = "")]
    #[default]
    None,
    #[serde(rename = "codetable")]
    CodeTable,
    #[serde(rename = "pinyin")]
    Pinyin,
    #[serde(rename = "english")]
    English,
    #[serde(rename = "phrase")]
    Phrase,
    /// 联想候选：上屏之后按刚上屏的内容给出的下一批候选，没有编码。
    ///
    /// 以输入码为 key 的加工（词频记账、自动造词、码表调序）都必须按这个来源值跳过它，
    /// 同 [`CandidateSource::Phrase`]「有文本无码位」恒不记词频。
    #[serde(rename = "assoc")]
    Assoc,
}

/// 正确拼音提示：候选**引擎文字**第 `char_index` 个字（0 起）对应的音节与用户所敲不同，
/// 正确音节为 `correct`，来源见 `kind`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinHint {
    pub char_index: u16,
    pub correct: String,
    pub kind: HintKind,
}

/// 提示来源：模糊音 / 读错的音 / 拼写纠错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    Fuzzy,
    Misread,
    Correction,
}

/// 候选词元数据
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateMeta {
    pub lexicon_name: String,
    pub is_user_dict: bool,
    pub is_temp_dict: bool,
    pub raw_weight: i32,
    pub freq_boost: i32,
    /// 当前 `weight` 由哪个词库层贡献，供悬停调试段标出来源（`权 5000 ←ext`）。
    ///
    /// 跨层同词合并时权重取各层最大值（`composite::merge_search`），`code`/`natural_order`/
    /// `base_order` 却仍属首个出现层；本字段随 weight 一起继承，否则无从区分「合并没取到 max」
    /// 与「取到了但被别的排序维度盖过」。用 `Arc<str>` 是因为热路径上每条候选都要填。
    /// `None` = 不来自词库层（短语 / 命令 / 联想等）。
    #[serde(skip)]
    pub weight_layer: Option<std::sync::Arc<str>>,
    /// 短语来源归属：`is_phrase` 候选时有意义，true=系统短语，false=用户短语。
    /// 仅供悬停调试提示区分来源（`wind_phrase::PhraseHit::is_system` 透传而来）。
    #[serde(default)]
    pub is_system_phrase: bool,
    /// 用户词/临时词在 store 里的真实存储码（`user_words`/`temp_words` 的 key 中段）。
    ///
    /// 同文合并（`pinyin/mod.rs` 第 6 步）保留已有候选的 `code`/`boundary`、只盖上来源标记，
    /// 于是 `code` 可能是系统词的码；删除若拿 `code` 拼 key 会删个空（redb 静默成功）。
    /// 只服务按 key 回写 store（删除），排序/边界不读。
    /// `None` = 不来自 store 层，或本就是新增分支（此时 `code` 即存储码）。
    #[serde(skip)]
    pub store_code: Option<std::sync::Arc<str>>,
    /// 造词用的规范词条编码与它自己的音节边界；`None` = 与候选 `code`/`boundary` 相同。
    ///
    /// 两者分家的来源：模糊音命中（敲 `senri`、词典是 `shengri`）、简拼节点（`bzdgailun` →
    /// `buzhidaogailun`）、双拼的全拼降级支路。候选 `code` 必须留用户所打那份
    /// （`consumed_length` 按 `query.starts_with(&c.code)` 判，preedit 跟随与词频记账也绑它），
    /// 而写进词库的码得是用户下次真能打出来的词典码。边界一并带：模糊命中的 `boundary` 恒 0，
    /// 没有这份真值造出的词进不了简拼索引的正常分组（`abbrev_index::group_of`）。
    /// 只服务造词，排序/去重/显示不读。
    #[serde(skip)]
    pub learn_code: Option<(String, u64)>,
    /// 模糊命中走的那条**所打码**（候选 `code`）切分，各音节起点的 bitmask；`0` = 不是模糊
    /// 命中，或命中词条没有边界真值。
    ///
    /// 模糊命中的 `boundary` 恒 0（见 `learn_code`），而 `pinan` 的「平安」走的是 `pin|an`、
    /// 不是 `maximum_match` 的 `pi|nan`。只服务编码栏切分（见 `preedit_split`），排序/造词/边界核对一律不读。
    #[serde(skip)]
    pub fuzzy_typed_boundary: u64,
    /// 「纠错音节 + 简拼尾」候选的编码栏切分：**整串所打击键**的各段起点 bitmask（纠错前缀的
    /// 各音节起点、外加尾巴的起点）。`0` = 不是这类候选。候选 `code` 是词典码、与所打击键不同域，
    /// 取 `boundary` 切不对（`yogny` 显示成 `yo'gny`）。只服务编码栏切分。
    #[serde(skip)]
    pub tail_typed_boundary: u64,
    /// 高亮到这条候选时编码栏的拼音正文（`pi'nan`）：在本次 `convert` 的输入串里按这条候选
    /// 自己走的切分插 `'`，候选只覆盖前缀时尾部按最大匹配接着切。`None` = 拿不到可信切分
    /// （手动分隔符、双拼击键候选、无边界真值、补全等），编码栏退回首选那份。双拼下只有全拼
    /// 降级候选有（击键即全拼码）。
    ///
    /// 只服务显示，排序/造词/边界核对一律不读。`Arc<str>`：同一切分的候选共用一份。
    #[serde(skip)]
    pub preedit_split: Option<std::sync::Arc<str>>,
    /// 连续辅助码：本候选吃掉的辅码在**击键坐标**的位置 `(起点, 长度)`，按起点升序；`None` =
    /// 没用辅码。带它的候选 `preedit_split` 是辅码形态（`ni hn'hc`），`code` / `boundary` 是路径
    /// 实际经过的音节（去辅码）的全拼码与边界，与 `consumed_length` 覆盖同一段击键。
    ///
    /// 只服务显示与上屏后的辅码定位；排序 / 去重 / 造词一律不读。引擎内的双拼边界校验据它豁免
    /// （码与双拼转换结果不同域，同简拼、全拼降级）。
    #[serde(skip)]
    pub aux_keys: Option<std::sync::Arc<[(u16, u8)]>>,
    /// 「英文前缀补全」（`english_merge.complete_min_length` 等）补出来的英文候选。
    ///
    /// 只由 `wind_engine::english_merge::lookup_prefix_complete` 置位；协调器
    /// `place_english_after_common_exact` 据此让它们排在整串拼音真词之后。不能拿「code 比输入
    /// 长」代替：英文方案、混输自己的前缀候选也长那样，却不该被挪动。
    #[serde(skip)]
    pub from_prefix_complete: bool,
    /// 正确拼音提示（空 = 无提示）。引擎在提示开关（`HintSwitch`）开时填充，各来源一并给出，
    /// 按来源勾选取舍由协调器做。
    #[serde(skip)]
    pub pinyin_hints: Vec<PinyinHint>,
    /// 带提示候选的整词正确音节（一字一个，`["zhong", "guo"]`）；与 `pinyin_hints` 同时由引擎
    /// 填，`pinyin_hints` 空时也空。切法与提示同源（词条码无边界时按音节图现切），协调器的
    /// `${correct_pinyin}` 直接用它，不另行切分。
    #[serde(skip)]
    pub hint_syllables: Vec<String>,
}

/// 命令栏动作
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub kind: ActionKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionKind {
    /// 文本插入
    Text,
    /// 副作用（不插入文本）
    Effect,
}

/// 候选词
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub text: String,
    pub pinyin: String,
    pub code: String,
    /// 本次检索中该文本还占用过的其它码位（同文本去重时，被丢弃那条的 `code` 并入此处）。
    ///
    /// `filter_smart` 按 `(source, code)` 分组判「组内有无常用字」；去重若丢掉码位，过滤会不单调
    /// （打 `siv` 时「档」的 `sivg` 码位被去重丢掉，`sivg` 组只剩生僻「桜」而被放行）。累积是
    /// 无条件的，因为去重时 `is_common` 还没填。
    ///
    /// 新增按 text 去重、且结果流向 `filter_candidates` 的地方，必须调用
    /// [`Candidate::absorb_codes_from`]。当前四处：`composite` 跨层合并、码表 `convert` 精确/前缀
    /// 两循环、混输 `sort_dedup_truncate`、协调器 `build_candidates`。刻意不接的：跨方案合并
    /// （码不同域）、现造候选（`code` 恒空）、拼音模糊变体（用的是 `LookupHit`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_codes: Vec<String>,
    pub weight: i32,
    /// 词库层级基序档位（`[[dictionaries]].base_order`，默认 0）。排序时作为独立层级：
    /// weight 之后、natural_order 之前（见 `better`/`by_natural`），小整数即可把整库排到另一库前后。
    pub base_order: i32,
    pub natural_order: i32,
    pub comment: String,
    pub is_common: bool,
    /// 用户显式把这个字标成了生僻（词库管理 / 候选右键），不是出厂字表里没有它。
    ///
    /// 与 `is_common == false` 的区别：带这一位的字不吃智能档的「孤儿码位」保底（见
    /// `filter_smart`），否则用户降级的字会原样回到首位。被滤的仍可在末页按翻页键放宽调出。
    /// 只对单字成立：出厂 `input.rare_phrase = "keep"` 下多字候选在此之前就已放行（`RarePhrasePolicy`）。
    pub user_rare: bool,
    /// 该候选按当前检索范围本应被滤掉，因用户按翻页键临时放宽才留在列表里
    /// （见 `docs/design/smart-filter-scope-relax.md`）。
    ///
    /// 语义只是这个事实，不编码排序或显示决策，各消费者自定行为：
    /// - `decide_auto_commit`：计数时跳过，否则满码即上屏的字会退化成要按空格；
    /// - 排序：追加在列表末尾，原有顺序不动；
    /// - 词频重排：排除在外（码表侧 used-first 不衰减，误选一次就会顶到常用字前）；
    /// - UI：加 `input.scope_relax.prefix` 前缀（默认 `·`）。
    ///
    /// 别把「沉底」写进本字段语义：来源属性与排序决策必须分开。
    #[serde(default)]
    pub is_scope_filtered: bool,
    /// 该候选是按已成形候选的文本查表追加的 emoji，不是词库条目
    /// （见 `docs/design/emoji-suggestion.md`）。
    ///
    /// 与 [`Self::is_scope_filtered`] 同构，语义只是这个事实：
    /// - 自动上屏：计数时跳过。协调器 `phrase_auto_commit` 判的是列表长度为 1，不跳过则短语打全码
    ///   不再自动上屏（引擎侧 `decide_auto_commit` 按 `code == input` 筛，天然免疫，两处判据不同源）；
    ///   开了 `input.emoji.block_auto_commit` 时反过来，有它就不自动上屏；
    /// - 排序：位置由 `input.emoji.show_as` 决定；
    /// - 词频：恒不参与（它在词频重排之后才插入，`record_selection_cand`）；
    /// - UI：可加标注前缀。
    ///
    /// 不复用 `is_scope_filtered`：两件无关的事，一个字段承担两种含义改一个必碰坏另一个。
    #[serde(default)]
    pub is_emoji_suggestion: bool,
    pub is_phrase: bool,
    pub is_command: bool,
    /// 是否来自模糊音变体命中（非原拼音精确匹配）。
    ///
    /// 这是召回来源标记，不是层级：不参与 `cmp_match_layers`，模糊候选与精确候选同层竞争，
    /// 惩罚由引擎施加在 weight 上（见 `pinyin::FUZZY_WEIGHT_SCALE`）。别把它加回层级键：
    /// 那等价于惩罚无穷大，模糊候选会被压出候选上限之外。
    pub is_fuzzy: bool,
    /// 该候选**只有经字母换位纠错才得到**（`gogn` → 「工」）：同一文字另有非纠错来源时
    /// 不标（那条非纠错候选留下）。整句里只要有一个音节经纠错就算。
    ///
    /// 与 [`Self::is_scope_filtered`] 同构，语义只是这个事实，不编码排序或显示决策：
    /// - 排序：存在非纠错的完整候选时不占首选（[`protect_top_from_corrected`]）；
    /// - 自动上屏：计数时跳过；
    /// - UI：加 `input.correction.marker` 前缀（默认 `*`）。
    ///
    /// 纠错命中照旧带 `is_fuzzy`（它决定召回与罚分），本字段与之并列。
    #[serde(default)]
    pub is_corrected: bool,
    /// 该候选**只因误读表召回**（用户打了表里的错误读音，词库读不出这个词，如 `xinxin` →
    /// 「莘莘」）。词库已有同文字候选时不召回，那条不带本标记。
    ///
    /// 与 [`Self::is_corrected`] 同为低置信候选，消费者按同一口径对待，语义只是这个事实：
    /// - 排序：引擎给同页最低权重再低一档；存在完整的普通候选时不占首选
    ///   （[`protect_top_from_corrected`]）；
    /// - 自动上屏：计数时跳过；
    /// - UI：不加纠错标记，正确拼音由提示（`HintKind::Misread`）给出。
    #[serde(default)]
    pub is_misread_recall: bool,
    /// 是否为简拼（声母缩写）候选，如 `nh`→「你好」。排序时整体降到全拼候选之后。
    ///
    /// 不复用 `is_fuzzy`：简拼与模糊音是两种无关的召回方式。本字段标的是用户/临时词简拼
    /// （step6）；系统词库简拼（step5，查 wdat `AbbrevSection`）仍走 `is_prefix`，改它会动到
    /// 前缀补全层的既有序。
    #[serde(default)]
    pub is_abbrev: bool,
    /// 是否为**全拼降级**候选：双拼方案下把击键串当全拼解释所得（`nihao` → 「你好」）。
    ///
    /// 服务「多人共用一台机器」——主力用户打双拼，偶尔来的人只会全拼。开关见
    /// `schema.pinyin.shuangpin.allow_full_pinyin`；关闭时本字段全域恒 `false`。
    ///
    /// 这是来源标记，不是层级键：沉不沉底由 [`cmp_match_layers`] 叠加置信度判定（精确整词且
    /// 消费整串不沉底，前缀补全 / 子短语沉底）。别把「是否沉底」编进本字段，下面三处豁免认的
    /// 正是这个标记。同一个词双拼也解释得出时由同文去重保留双拼那条（`recall_full_pinyin`）。
    ///
    /// 与 `is_abbrev` 同构，`code` 是全拼码、与双拼击键不同域，`pinyin/mod.rs` 里三处必须一并豁免：
    /// `sp_boundary_mask` 边界校验（不豁免则整批候选被静默滤光）、`map_consumed_length` 回映射、
    /// `build_raw_preedit`。照抄 `is_abbrev` 的写法即可。
    ///
    /// 非双拼方案 / 开关关闭时恒 `false`。引擎内部用于排序，不推送 UI。
    #[serde(skip)]
    pub is_fullpinyin_fallback: bool,
    /// 是否为**零权重读音**的精确单字：该字的这个读音在词库里权重为 0（几乎没人这么读，
    /// 如 𤭢 cei、塞 sei、嫩 nun、这 zhei）。拼音引擎在截断**之后**置位，
    /// [`cmp_match_layers`] 把它并入**简拼层**，于是双拼两键 `cw` 首选是简拼词「成为」
    /// 而不是 𤭢，但 𤭢 仍在候选里；有权重的读音（嗲 dia=182）不置位、照旧首选。
    ///
    /// 简拼层在前缀补全层、子短语层之下，故同消费长度下它也沉到这些之后，层内按 weight 排到简拼词后。
    ///
    /// 用户词 / 临时词里有的字不置位。有词频记录的字由 `freq_rerank::rerank_pinyin_positional`
    /// 摘掉标记；它每次转换重算、不落盘，关闭调频或记录衰减殆尽后会重新生效。不推送 UI。
    #[serde(skip)]
    pub is_zero_weight_reading: bool,
    /// 是否为前缀补全候选（候选编码比输入更长，如输入 si 补全出「思考」(sikao)）。
    /// 排序时前缀补全整体降到精确匹配（code==输入）之后，使等长精确候选优先
    /// （如输入 si 时单字「四」优先于补全词「思考」），对齐 Go 的 Exact>>Partial 层级。
    pub is_prefix: bool,
    /// 是否为子短语候选（候选编码是输入的真前缀、比输入短，如输入 baoan 时「报」(bao)）。
    /// 供分段上屏（你好→你）使用，但排序时整体降到完整匹配之后，避免高频单字插进
    /// 完整词之间（如 baoan 时「报/宝」塞在「保安」「报案」之间）。对齐 Go 的 coverage
    /// 分层：完整覆盖输入的词恒先于只覆盖部分输入的子短语单字。
    pub is_partial: bool,
    /// 是否属于**精确匹配档**：候选 `code` 与本次输入完全相等（如五笔简码 `usr`→「新」），
    /// 区别于编码更长的前缀补全（`usrq`→「新的」）。排序时精确档整体先于前缀补全。
    ///
    /// 谁置位（新增候选来源时须对照，漏标即被压到精确候选之下）：
    /// - 码表引擎 `CodeTableEngine::convert`：按 `code == input`，覆盖文件词库/用户词/临时词及英文引擎；
    /// - 混输 overflow 分支：以完整输入重新归一；
    /// - 协调器精确码短语（`phrases.lookup`）；
    /// - 协调器引导键导航候选（`$CC`/`$SS`/`$AA`）：按设计恒置顶，唯一的「非 code==input」成员，
    ///   故语义是「精确档」而非字面的编码相等；
    /// - 拼音引擎不置位：混输下码表精确恒先于拼音（同 `freq_rerank::freq_tier`）。
    ///
    /// 不复用 `is_prefix`：协调器 `build_candidates` 已把前缀短语标成 `is_prefix=true`，复用会让短语与
    /// 码表词组同层。需要独立层级而非靠权重：词组权重来自词频、单字来自字频，量纲不可比。
    #[serde(default)]
    pub is_exact_code: bool,
    /// 是否为**引擎合成的整句解**（Viterbi 多词拼接，或超长词典整词的等价整句分）。
    ///
    /// 语义 = 「引擎对整串输入的一种解读」，是来源属性；权重只表达多重要，别再用权重阈值代替本字段。
    /// 同文合并时也会补标到词典词条上（见 [`Self::is_synthesized`]）。
    #[serde(default)]
    pub is_sentence: bool,
    /// 整句解**已让位于精确整词**（降级，非销毁）。
    ///
    /// 触发条件：合成的整句不是词典词条，而候选中有覆盖同一段输入的严格精确整词（非模糊命中）。
    /// 整句仍可选，只是不占首位（由 step 6.5/6.5b 记录）。
    ///
    /// 不直接清 `is_sentence`：那是来源属性，降级是排序决策。不复用 `is_exact_code`：拼音引擎
    /// 约定不置位它，置位会让拼音候选在混输下整体越过码表候选。
    ///
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_sentence_demoted: bool,
    /// 整句 N-best 里的名次：`0` = 不是 N-best 整句；`1` = 最优解；`2..` = 备选。
    ///
    /// 只在 `schema.pinyin.sentence_count > 1` 时有消费者：协调器据此把整句块连续摆到最前
    /// （`handle_candidate.rs::place_sentence_block`）；出厂 `count = 1` 时照样标但无人读。
    /// 不复用 `is_sentence`：那个同文合并时也会补标到词典词条上，与 N-best 名次正交。
    ///
    /// 引擎/协调器内部用，不推送 UI。
    #[serde(skip)]
    pub sentence_rank: u8,
    /// 该整句是引擎新合成的解读，词库里没有以它为整体的词条。自动造词据此判断「词库里有没有」。
    ///
    /// 不能用 [`Self::is_sentence`] 代替：同文合并（`pinyin/mod.rs` step 2 / 2b / 6.2）也会给已有
    /// 词典候选补标 `is_sentence`，照它放行会把系统词一条条抄进临时词库。也别改成协调器侧查词库：
    /// 反查索引（`reverse_index_for`）只缓存两份，造词与悬停交替查会反复重建，落在按键路径上。
    ///
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_synthesized: bool,
    /// 该候选由**逆切分**拼成：满码长的串被切成两段、各取词典候选后拼接
    /// （`hfkn` → `hf`(很可) + `kn`(能) → 「很可能」）。
    /// 见 `docs/design/codetable-split-input.md` 与 `codetable::engine::decode_split`。
    ///
    /// 不复用 [`Self::is_sentence`]：它已有让位、整句名次统计等消费者，借用会把切分候选混进去。
    ///
    /// 消费者：`wants_codetable_split`（高亮时组合区显示 `hf'kn`）；`candidate_display_order` 的
    /// 沉底层（`split_trigger = "no_exact"` 档下排在既有候选之后）。
    ///
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_split_composed: bool,

    /// 该候选来自**自动造词的草稿层**：滑窗切出、尚未被任何人用过的猜测词
    /// （`docs/design/auto-phrase-draft-layer.md`）。
    ///
    /// 草稿杂词率高，但必须能被召回才有机会转正，故靠两道闸调和：草稿层只响应精确查询
    /// （`StoreDraftLayer`），本标记让它在候选里恒沉底（`candidate_display_order`）。
    /// 不复用 [`Self::is_split_composed`]：那个已有自己的消费者。
    ///
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_draft: bool,
    /// 前缀补全已被提升进完整匹配层（排序决策，与 `is_prefix` 的「码更长」结构事实正交）。
    ///
    /// `is_prefix` 恒表结构事实，别为了上浮把补全词硬标成 `is_prefix=false`。`cmp_match_layers`
    /// 的有效前缀层 = `is_prefix && !本字段`，提升后的补全在层级比较中等价于非补全。
    ///
    /// 生产方：拼音引擎 step4（系统词残码上浮）/ step6（用户·临时词长词上浮）。
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_promoted_completion: bool,
    /// 该候选是**直接辅助码**（双拼）的命中项：输入末 1～2 位被当作辅码，本条取自对前缀
    /// 单独解码的结果、经字形筛选命中后并入主候选（`docs/design/aux-code-direct.md`）。
    ///
    /// 命中项的 `consumed_length` 已改成整串（上屏连辅码一起吃掉），`code` 仍是**前缀**的
    /// 拼音码（调频记在前缀下）。本标记只服务**组码区**：高亮它时显示「前缀 + 空格 + 辅码」，
    /// 让人看出末尾被当成了辅码（协调器 `effective_preedit_body`）。
    ///
    /// 不复用任何既有层级字段：它不参与排序（插入发生在全部排序之后），只是来源标记。
    /// 协调器内部用，不推送 UI。
    #[serde(skip)]
    pub is_direct_aux: bool,
    /// 该候选来自码表**通配查询**（`CodeTableEngine::convert_wildcard`）。
    ///
    /// 只服务显示档位：通配下 `is_exact_code` 的语义是「与 pattern 等长」，`code` 必然 ≠ 输入
    /// （输入里带着通配键），[`source_tier`] 的 `code == input` 认不出它。见
    /// `docs/design/codetable-wildcard.md` §10「通配等长结果与码表精确同档」。
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_wildcard: bool,
    /// 该候选来自**未启用的扩展词库**（影子层，`DisabledDictLayers`，reverse-mode spec §4.2）。
    /// 只由反查模式查询置位；显示序在同一精确档内把它排在已启用候选之后。
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub from_disabled_dict: bool,
    /// 前缀补全比**输入自身表达的音节数**多出几个音节（`0` = 音节数恰好对齐 / 非补全候选）。
    ///
    /// 「输入自身表达的音节数」= 完整音节数 + (有尾部残码 ? 1 : 0)，即引擎的 `started_syllables`；
    /// 不是输入在这条候选切分下占的音节数（`xia` 会因 `xi|an` 算成 2，见
    /// `wind_engine::pinyin::completion_syllable_cap`）。
    ///
    /// 必须是字段：显示序在协调器，而协调器不持有 `SyllableTrie`、算不出音节数。只服务显示序、
    /// 不参与引擎内部排序：引擎的 `sort_by` 紧跟 `truncate`，加进去就从降级变成销毁
    /// （同 `consumed_length`，见 `cmp_by_consumed`）。
    ///
    /// 生产方：拼音引擎 step4（系统词库前缀补全）/ step6（用户·临时词前缀补全）。
    /// `boundary == 0`（无边界信息）算 0，与全仓「无边界信息一律降级放行」一致。
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub completion_extra_syllables: u8,
    pub consumed_length: usize,
    /// 该候选 `code` 的**音节边界**（各音节起始字节位 bitmask），见
    /// `wind_dict::binformat::DictEntry::boundary`。`0` = 无边界信息 → 消费方降级回 DAG 猜切分。
    ///
    /// 来自词典真值（源数据 `ni hao` 的空格），供双拼按真实边界校验候选（`nihao` 双拼切
    /// ni|ha|o，与「你好」的 ni|hao 不符即拒绝）。
    ///
    /// 与 `code` 同进同出：任何换 `code` 的地方（如 `composite::merge_search`）必须一起换 boundary。
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub boundary: u64,
    /// 简繁 1对多变体候选的输出覆盖（如「出」的变体「齣」）。
    ///
    /// `Some(t)` = 显示与上屏直接用 `t`、绕过出口的 `maybe_convert`；`text` 仍是简体原字，
    /// 维持「内部状态一律简体」的不变量。协调器展开/消费，不推送 UI。
    #[serde(skip)]
    pub s2t_override: Option<String>,
    /// 上屏时实际写出的文本；`None` = 用 `text`。
    ///
    /// 目前只有词语联想用：候选栏显示整词「中国」，「中」已在屏上，只补「国」。
    /// 与 `s2t_override` 别互相顶替：那个绕过简繁转换，本字段只换上屏文本、简繁照常转。
    /// 只由 `handle_assoc::commit_assoc_at` 消费。
    #[serde(skip)]
    pub commit_override: Option<String>,
    pub source: CandidateSource,
    pub phrase_template: String,
    pub is_group: bool,
    pub is_group_member: bool,
    pub group_code: String,
    pub group_name: String,
    pub group_template: String,
    pub index: usize,
    pub has_shadow: bool,
    pub index_label: String,
    pub meta: CandidateMeta,
    /// 英文候选大小写投影之前的词库原文（`None` = 未经投影）。
    ///
    /// 词频键是 `(schema, code, text)`，读端 `apply_freq_rerank_in` 在投影之前、看到的是原文；
    /// 写端若存投影后的 `Hill` 就与读端 `hill` 永远对不上。故记账一律走 [`Candidate::freq_text`]。
    #[serde(skip)]
    pub case_source: Option<String>,
    /// 词库原文里的 `$` 模板 / `{..}` 插值展开之前的源文本（`None` = 未经展开）。
    ///
    /// 用户词 `rq` → `$Y年$M月$D日` 由 `finalize_candidates` 展开进 `text`；词频键取展开文本次日即
    /// 对不上，故记账走 [`Candidate::freq_text`] 取本字段。与 `case_source` 分开存：英文重套大小写
    /// 档位时会把 `text` 还原成 `case_source`，混在一起候选会显示成模板源码。
    #[serde(skip)]
    pub template_source: Option<String>,
    /// 候选稳定 id：跨会话、跨日期不变的身份标识，供 shadow 规则（置顶/移动）精准匹配。
    ///
    /// 格式 `phrase:{code}:{原始记录文本}`，由协调器短语装配（`build_candidates`）生成：求值型短语的
    /// `text` 逐日变化，以文本为键的规则次日即失配。空串 = 无稳定身份（静态候选按文本匹配即可）。
    pub id: String,
    /// 候选窗里显示的文本；空串 = 显示 `text`。上屏、词频、候选调整一律仍按 `text`。
    ///
    /// 目前唯一的生产方是码表逆切分的「次选只显示后段」（`SplitAltDisplay::Back`）：`hfkn` 的 ②
    /// 显示「困难」、上屏「很困难」，由显示出口 `cand_display_text` 按列表位置判定。
    /// 与 `commit_override` 方向相反、别互相顶替。不推送 UI。
    #[serde(skip)]
    pub display_text: String,
    pub actions: Vec<Action>,
}

impl Default for Candidate {
    fn default() -> Self {
        Self {
            text: String::new(),
            pinyin: String::new(),
            code: String::new(),
            merged_codes: Vec::new(),
            weight: 0,
            base_order: 0,
            natural_order: 0,
            comment: String::new(),
            is_common: false,
            user_rare: false,
            is_scope_filtered: false,
            is_emoji_suggestion: false,
            is_phrase: false,
            is_command: false,
            is_fuzzy: false,
            is_corrected: false,
            is_misread_recall: false,
            is_abbrev: false,
            is_fullpinyin_fallback: false,
            is_zero_weight_reading: false,
            is_prefix: false,
            is_partial: false,
            is_exact_code: false,
            is_sentence: false,
            is_sentence_demoted: false,
            sentence_rank: 0,
            is_synthesized: false,
            is_split_composed: false,
            is_draft: false,
            is_promoted_completion: false,
            is_direct_aux: false,
            is_wildcard: false,
            from_disabled_dict: false,
            completion_extra_syllables: 0,
            consumed_length: 0,
            boundary: 0,
            s2t_override: None,
            commit_override: None,
            source: CandidateSource::None,
            phrase_template: String::new(),
            is_group: false,
            is_group_member: false,
            group_code: String::new(),
            group_name: String::new(),
            group_template: String::new(),
            index: 0,
            has_shadow: false,
            index_label: String::new(),
            meta: CandidateMeta::default(),
            case_source: None,
            template_source: None,
            id: String::new(),
            display_text: String::new(),
            actions: Vec::new(),
        }
    }
}

impl Candidate {
    /// 低置信候选：经纠错（[`Self::is_corrected`]）或只因误读表召回（[`Self::is_misread_recall`]）。
    /// 不占首选、不计自动上屏、不否决上屏。
    pub fn is_low_confidence(&self) -> bool {
        self.is_corrected || self.is_misread_recall
    }

    /// 词频 / 候选调整记账用的文本：模板展开前、大小写投影前的词库原文（都没发生时即 `text`）。
    ///
    /// 存在理由见 [`Candidate::case_source`] 与 [`Candidate::template_source`]。
    pub fn freq_text(&self) -> &str {
        self.template_source
            .as_deref()
            .or(self.case_source.as_deref())
            .unwrap_or(&self.text)
    }

    /// 按 text 去重时调用：把被丢弃那条所占的码位并入本候选（语义见 [`Candidate::merged_codes`]）。
    ///
    /// 必须传整个被弃候选而非它的 `code`：去重是链式的（跨层合并 → 引擎 → 协调器），被弃者
    /// 可能已吸收过更早的码位。
    pub fn absorb_codes_from(&mut self, other: &Candidate) {
        // 跨来源一律不并：混输下 "wang" 既是五笔码又是拼音，`filter_smart` 按 `(source, code)`
        // 分组正是为隔离两者，跨来源并入会造出假的「该码位有常用字」而误滤码表生僻字。
        if self.source != other.source {
            return;
        }
        self.absorb_code(&other.code);
        for code in &other.merged_codes {
            self.absorb_code(code);
        }
    }

    /// 并入单个码位（与自身 `code` 相同或已记录则忽略）。
    ///
    /// 线性查重而非 `HashSet`：同一文本的码位只有个位数，而本函数在每次检索的去重循环里跑。
    pub fn absorb_code(&mut self, code: &str) {
        if code.is_empty() || code == self.code {
            return;
        }
        if !self.merged_codes.iter().any(|c| c == code) {
            self.merged_codes.push(code.to_string());
        }
    }
}

/// 候选**实际消费的输入长度**：`0` 是「引擎未标注 ⇒ 按整串算」的全仓约定（码表候选恒为 0）。
///
/// 不归一化就直接比较会把码表候选整体甩到最后，见 [`cmp_by_consumed`]。
pub fn effective_consumed(c: &Candidate, input_len: usize) -> usize {
    if c.consumed_length == 0 {
        input_len
    } else {
        c.consumed_length
    }
}

/// 同一文字既有经纠错（[`Candidate::is_corrected`]）又有非纠错的候选时，去掉纠错那条，
/// 非纠错那条留在原位。
pub fn drop_corrected_duplicates(cands: &mut Vec<Candidate>) {
    if !cands.iter().any(|c| c.is_corrected) {
        return;
    }
    let plain: std::collections::HashSet<String> = cands
        .iter()
        .filter(|c| !c.is_corrected)
        .map(|c| c.text.clone())
        .collect();
    cands.retain(|c| !c.is_corrected || !plain.contains(&c.text));
}

/// 纠错候选不占首选（设计 D6）：首条经纠错（或误读召回，见 [`Candidate::is_low_confidence`]）、
/// 且列表里有**完整**的普通候选时，把第一条这样的候选提到首位，其余相对顺序不变。
///
/// 「完整」= 消费整串、不是简拼 / 前缀补全 / 子短语。简拼不算：否则 `gogn` 切成 g+o+g+n
/// 出的简拼整句会让「工」永远拿不到首选。
pub fn protect_top_from_corrected(cands: &mut [Candidate], input_len: usize) {
    if !cands.first().is_some_and(Candidate::is_low_confidence) {
        return;
    }
    let complete = |c: &Candidate| {
        !c.is_low_confidence()
            && effective_consumed(c, input_len) >= input_len
            && !c.is_abbrev
            && !c.is_prefix
            && !c.is_partial
    };
    if let Some(i) = cands.iter().position(complete) {
        cands[..=i].rotate_right(1);
    }
}

/// 「消费输入更多者优先」比较，对齐 librime（候选容器以消费长度为 key、从最长遍历），
/// 先于词频、先于任何层级。
///
/// 必须与 [`cmp_match_layers`] 成对出现且恒在其之前：凡整体重排候选的地方两键要一起用，
/// 只有一处带上本键会让候选随词频记录有无而忽隐忽现。见 docs/architecture/candidate-sorting-rules.md 的「6.1」。
pub fn cmp_by_consumed(a: &Candidate, b: &Candidate, input_len: usize) -> std::cmp::Ordering {
    effective_consumed(b, input_len).cmp(&effective_consumed(a, input_len))
}

/// 候选「匹配层级」比较——`Exact >> 子短语 >> 前缀补全 >> 简拼 >> 全拼降级` 的**唯一真相**。
///
/// ⓪ 本方案编码的候选优先于**全拼降级**候选（双拼下打 `nihao`，双拼解读先于全拼解读）；
/// ① 全拼优先于简拼（输入 nh 时全拼命中先于简拼「你好」）；
/// ② 精确/子短语（**有效前缀层**为 false）优先于前缀补全（输入 si 时「四」先于补全「思考」）；
/// ③ 完整匹配优先于子短语（输入 baoan 时「保安」「报案」先于单字「报」「宝」）。
///
/// ⓪ 是唯一一个「来源差异」做成层级键的，判据是用户显式声明了主编码方案，不可照此给别的
/// 来源开口子（见 [`Candidate::is_fullpinyin_fallback`]）。
///
/// 有效前缀层 = `is_prefix && !is_promoted_completion`，提升后的补全在此等价于非补全。
/// `is_fuzzy` 刻意不参与：它是召回来源，惩罚由引擎施加在 weight 上（`FUZZY_WEIGHT_SCALE`）。
///
/// 引擎内部排序、协调器 `candidate_display_order`、词频重排 `rerank_pinyin_decay` 三处共用本函数，
/// 不要另写副本，各自的额外维度在其前后追加。
pub fn cmp_match_layers(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    // 有效前缀层：结构补全被提升后等价于非补全（落进精确/子短语层）。
    let eff_prefix = |c: &Candidate| c.is_prefix && !c.is_promoted_completion;
    // 全拼降级候选只有低置信的那部分（前缀补全 / 子短语）沉底；精确整词且消费整串的与双拼
    // 候选同层竞争。必须是首键（否则双拼简拼会沉得比降级通道还深）；判据用 `eff_prefix`
    // 而非裸 `is_prefix`，否则被提升的用户长词会被沉到截断线外。
    // 见 docs/architecture/candidate-sorting-rules.md 的「6.1」。
    let fp_demoted = |c: &Candidate| c.is_fullpinyin_fallback && (eff_prefix(c) || c.is_partial);
    // 简拼层：零权重读音的精确单字并入此层按 weight 竞争（见 `Candidate::is_zero_weight_reading`）。
    let abbrev_layer = |c: &Candidate| c.is_abbrev || c.is_zero_weight_reading;
    fp_demoted(a)
        .cmp(&fp_demoted(b))
        .then(abbrev_layer(a).cmp(&abbrev_layer(b)))
        .then(eff_prefix(a).cmp(&eff_prefix(b)))
        .then(a.is_partial.cmp(&b.is_partial))
}

/// 候选界面呈现序（协调器合并引擎候选与短语后无条件重排全部候选）。比较链：
/// 消费长度降序 → 草稿沉底 → 逆切分沉底 → [`cmp_match_layers`] → [`cmp_completion_extra`] →
/// [`cmp_exact_first`] → 未启用词库沉后 → 跨来源档位（仅 `mixed`）→ 权重降序（`ignore_weight` 时跳过）
/// → `base_order` 升序 → `natural_order` 升序。各键的依据见 docs/architecture/candidate-sorting-rules.md 的「6」。
///
/// - `is_exact_code` 不可少：本函数会推翻引擎序，不复刻该键码表简码字会被词组按权重压过。
/// - `base_order` 须在 `natural_order` 之前：后者是每库局部出现序，跨库直接比无意义。
/// - `ignore_weight`：`base_sort = "natural"` 时为 true，与引擎 `by_natural` 一致地忽略权重。
/// - `input`：取长度判「消费整串」（字节长度与 `consumed_length` 同域），取全串供 [`source_tier`]
///   判「码 == 输入」。「消费整串」不可省成 `!is_partial`：整句只解释部分输入时 `is_partial` 仍为 false。
/// - `mixed`：必须按引擎语境传入、不可恒 true。纯拼音下全体同源，档位会退化成「`is_common` 优先」，
///   把含生僻字的多字词降到全部常用单字之后。依赖 `mark_common` 已先跑过。
///
/// 放在本 crate 而非协调器：引擎侧评测（`pinyin_eval`）要报界面序，而引擎序不是界面序，
/// 两边共用一份比较键才不会漂移。
pub fn candidate_display_order(
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
    // 混输专属：跨来源档位（`source_tier` 是跨来源先后的唯一真相源）。纯拼音/纯码表下必须为空操作。
    let by_source_tier = if mixed {
        source_tier(a, input).cmp(&source_tier(b, input))
    } else {
        std::cmp::Ordering::Equal
    };
    // 消费长度是首要键，且必须与 `cmp_match_layers` 成对（见 `cmp_by_consumed`）。层级键原样
    // 复用 `cmp_match_layers`，别另写忽略 `is_promoted_completion` 的副本：布尔层级键等价于
    // 惩罚无穷大，会让引擎侧用 weight 表达的让位（step 6.5b）全部失效。
    let by_consumed = cmp_by_consumed(a, b, input_len);

    by_consumed
        // 草稿、逆切分候选依次沉底（草稿更沉）。两者都必须在 `cmp_match_layers` 之前：它们
        // `is_prefix = false`，用户词层的前缀补全 `is_prefix = true`，放在后面胜负已在 `eff_prefix`
        // 分出。按一个布尔分两区是合法全序、不改变既有次序。
        // 见 docs/architecture/candidate-sorting-rules.md 的「6.1」。
        .then(a.is_draft.cmp(&b.is_draft))
        .then(a.is_split_composed.cmp(&b.is_split_composed))
        .then_with(|| cmp_match_layers(a, b))
        // 音节数对齐者优先（`zaim` 先给 2 音节的「在吗/再买」，3 音节的「在美国」排其后）。
        // 置于层级之后、权重之前：层内分档，不跨层提拔。见 `cmp_completion_extra`。
        .then_with(|| cmp_completion_extra(a, b))
        .then_with(|| cmp_exact_first(a, b))
        // 全序：按布尔分两区，两条非未启用候选间恒 Equal ⇒ 不改任何既有次序（同 `is_draft` 论证）；
        // 放在 `cmp_exact_first` 之后 ⇒ 不跨等长 / 更长两档。
        .then(a.from_disabled_dict.cmp(&b.from_disabled_dict))
        .then(by_source_tier)
        .then(by_weight)
        .then(a.base_order.cmp(&b.base_order))
        .then(a.natural_order.cmp(&b.natural_order))
}

/// 「音节数对齐者优先」比较（[`Candidate::completion_extra_syllables`] 升序）。
///
/// 输入表达了几个音节就先给几个音节的候选，预测了更多未输入音节的补全排其后。位于
/// `candidate_display_order` 里 `cmp_match_layers` 之后、权重之前：层内分档，不跨层提拔。
///
/// 只在显示序施加一次，不要再写进 `freq_rerank` 的层级比较器：那里继承显示序，重复施加会让它
/// 变成词频也翻不过的硬约束（音节数对齐是先验，词频是实证）。权重折扣 `0.5^extra` 替代不了它，
/// 参考实现为何不需要它，以及严格逐级与两档的取舍，见 docs/architecture/candidate-sorting-rules.md 的「6.1」。
pub fn cmp_completion_extra(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    a.completion_extra_syllables
        .cmp(&b.completion_extra_syllables)
}

/// 候选「精确匹配档优先」比较（`is_exact_code` 降序）。
///
/// 与 `cmp_match_layers`（匹配质量层级）正交。码表引擎与协调器两处共用，引擎的精确档才不会被
/// 协调器的重排推翻。两处位置不同是有意的：
/// - 协调器 `candidate_display_order`：在 `cmp_match_layers` 之后，只在同层内生效、不跨层提拔；
/// - 码表引擎 `CodeTableEngine::convert`：作首要键、不叠 `cmp_match_layers`，否则会改变用户词
///   （`store_layer` 会设 `is_prefix`）与文件词库候选的既有相对序。
///
/// `wind_engine::freq_rerank::freq_tier` 的 `code == input` 是同概念的另一份判据（还承载词频语义），
/// 改任一处须核对另一处。
pub fn cmp_exact_first(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.is_exact_code.cmp(&a.is_exact_code)
}

/// 候选是否属于拼音精确档（[`source_tier`] 的档 1）：混输下该档整体先于码表前缀补全。
///
/// 判据是「完整、精确地解释了本次输入，且是常用字词」：
/// - 消费整串（`consumed_length` 为 0 即未标注，按整串算）。必须直接问 `consumed_length`，
///   不能拿 `!is_partial` 代替：整句只解释部分输入时 `is_partial` 仍为 false（`aaw` →「啊啊」）；
/// - `is_prefix`/`is_partial`/`is_abbrev` 全假，即音节精确对齐；
/// - `is_fuzzy` 假：这里是提档准入条件，不是层级键；
/// - `is_common` 真：单音节同音字动辄上百条，用检索范围而非固定条数把关。依赖协调器
///   `mark_common` 已在排序前无条件跑过（它不能放回 `apply_filter`，那里 Gb18030 档会提前返回）。
///
/// `input_len` 取字节长度，与 `consumed_length` 同域。
pub fn is_pinyin_exact_tier(c: &Candidate, input_len: usize) -> bool {
    c.source == CandidateSource::Pinyin
        && c.is_common
        && !c.is_prefix
        && !c.is_partial
        && !c.is_abbrev
        && !c.is_fuzzy
        && (c.consumed_length == 0 || c.consumed_length >= input_len)
}

/// 候选**来源档位**（数字越小越靠前）——跨来源先后的**唯一真相源**。
///
/// ```text
/// 0  码表精确全码（code == input）+ 精确码短语（is_phrase && is_exact_code）
/// 1  拼音精确档（is_pinyin_exact_tier：精确音节 + 常用字）  ← 先于码表前缀补全
/// 2  码表前缀补全
/// 3  前缀短语 + 其余来源
/// 4  拼音其余（前缀补全/子短语/简拼/模糊/生僻） + 英文
/// ```
///
/// 码表精确全码恒在拼音之上，词频重排只在同档内调整；纯拼音 / 纯码表下同源候选档位相同。
/// 档 1 是「五笔优先」的有意松动：短输入下码表前缀补全（要打满码才精确）的置信度低于完整拼音音节。
///
/// 档 0 内码表精确与精确码短语按权重裁决，这样开不开码表自动调频结果一致。前缀那一对刻意不合档：
/// 可比不等于该比，前缀短语是系统猜测、权重又普遍高于码表前缀补全，合档会压过多数补全候选
/// （回归测试 `input_flow.rs::prefix_group_marker_defers_*`）。
///
/// 调用方：协调器 `candidate_display_order`（混输）与 `freq_rerank::freq_tier`。`c.code == input` 与
/// [`Candidate::is_exact_code`] 是同一概念的两份判据，未合并是因为本档位还承载来源语义。
///
/// 只能在协调器侧调用：档 1 要求 `is_common`，而它只由协调器 `mark_common` 置位，在引擎里调用
/// 拼音精确候选会静默落到档 4。混输引擎的截断用自己的 `MixedEngine::truncation_tier`
/// （管「谁活下来」，本函数管「谁排前面」），不算并行实现。
pub fn source_tier(c: &Candidate, input: &str) -> u8 {
    use CandidateSource::*;
    if c.is_phrase {
        // 短语按完全匹配 vs 前缀匹配分档，勿因 is_phrase 一刀切抬到码表前缀补全之上：
        // 精确码短语（`is_exact_code`）进档 0 与码表精确同档比权重；前缀短语留档 3，否则打 `da`
        // 时 `date` 短语会压过码表前缀补全。
        return if c.is_exact_code { 0 } else { 3 };
    }
    if is_pinyin_exact_tier(c, input.len()) {
        return 1;
    }
    match c.source {
        CodeTable if c.code == input => 0, // 码表精确全码（如五笔 cang→駏）
        // 通配等长结果与码表精确同档（spec §10：非首位按通配键是明确意图）。只认
        // `is_wildcard`：非通配路径上 `is_exact_code` 与 `code == input` 同义，不另开口子。
        CodeTable if c.is_wildcard && c.is_exact_code => 0,
        CodeTable => 2, // 码表前缀补全
        Pinyin => 4,    // 拼音（非精确档：前缀补全/子短语/简拼/模糊/生僻）
        English => 4,
        // 其余来源（主要是未标注来源的 `None`）与前缀短语同档是沿袭而非设计；
        // 将来有来源需要明确定位应单独判，别继续挂在这里。
        _ => 3,
    }
}

/// 「拼音精确档先于码表前缀补全」二分比较（[`is_pinyin_exact_tier`] 降序）。
///
/// 显示序已改用 [`source_tier`]（其档 1 即本判据），本函数只剩测试在用。若要启用，只能用于混输：
/// 纯拼音下全体同源，它会退化成「`is_common` 优先」，把含生僻字的多字词降到全部常用单字之后。
pub fn cmp_pinyin_exact_first(
    a: &Candidate,
    b: &Candidate,
    input_len: usize,
) -> std::cmp::Ordering {
    is_pinyin_exact_tier(b, input_len).cmp(&is_pinyin_exact_tier(a, input_len))
}

/// 比较两个候选词的排序优先级（权重降序）
///
/// 与 Go 版本 `candidate.Better` 对齐。
pub fn better(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    // 层级：weight 降 → base_order 升（词库档位）→ natural_order 升（出现序）→ code → text。
    // base_order 默认 0 时该级为空操作，故不设 base_order 的路径（拼音/混输等）行为不变。
    a.weight
        .cmp(&b.weight)
        .reverse()
        .then(a.base_order.cmp(&b.base_order))
        .then(a.natural_order.cmp(&b.natural_order))
        .then(a.code.cmp(&b.code))
        .then(a.consumed_length.cmp(&b.consumed_length).reverse())
        .then(a.text.cmp(&b.text))
}

/// 比较两个候选词的自然排序优先级（精确匹配优先）
///
/// 与 Go 版本 `candidate.BetterNatural` 对齐。
pub fn better_natural(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    let a_exact = a.weight >= 0;
    let b_exact = b.weight >= 0;
    match (a_exact, b_exact) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a
            .natural_order
            .cmp(&b.natural_order)
            .then_with(|| better(a, b)),
    }
}

/// 比较两个候选词的**纯自然序**优先级（`base_sort = "natural"` 用）：**完全忽略权重**，
/// 只按 `natural_order`（词库出现序，含 base_order 层偏移）升序，再以 code/text 作稳定兜底。
///
/// 与 `better_natural` 的区别：后者精确匹配优先且以 `better`（权重）兜底；本函数不看权重，
/// 纯按设计者在词库文件里的排列顺序呈现（对齐用户"只按设计顺序"的诉求）。
pub fn by_natural(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    // 忽略权重：base_order 升（词库档位）→ natural_order 升（出现序）→ code → text。
    a.base_order
        .cmp(&b.base_order)
        .then(a.natural_order.cmp(&b.natural_order))
        .then(a.code.cmp(&b.code))
        .then(a.consumed_length.cmp(&b.consumed_length).reverse())
        .then(a.text.cmp(&b.text))
}

/// 排序候选词列表（权重降序）
pub fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(better);
}

/// 排序候选词列表（自然顺序，精确匹配优先）
pub fn sort_candidates_natural(candidates: &mut [Candidate]) {
    candidates.sort_by(better_natural);
}

#[cfg(test)]
#[path = "candidate_pinyin_exact_tier_tests.rs"]
mod pinyin_exact_tier_tests;

#[cfg(test)]
#[path = "candidate_match_layer_tests.rs"]
mod match_layer_tests;

#[cfg(test)]
#[path = "candidate_wildcard_tier_tests.rs"]
mod wildcard_tier_tests;

#[cfg(test)]
mod freq_text_tests {
    use super::*;

    fn cand(text: &str, case_source: Option<&str>, template_source: Option<&str>) -> Candidate {
        Candidate {
            text: text.into(),
            case_source: case_source.map(Into::into),
            template_source: template_source.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn plain_candidate_uses_text() {
        assert_eq!(cand("你好", None, None).freq_text(), "你好");
    }

    #[test]
    fn case_projected_candidate_uses_case_source() {
        assert_eq!(cand("Hill", Some("hill"), None).freq_text(), "hill");
    }

    /// 模板源文本最优先：它才是存储里那条记录，且逐日不变。
    #[test]
    fn template_source_wins_over_case_source() {
        let c = cand("2026年10月9日", Some("2026年10月9日"), Some("$Y年$M月$D日"));
        assert_eq!(c.freq_text(), "$Y年$M月$D日");
    }
}

#[cfg(test)]
#[path = "candidate_corrected_tests.rs"]
mod corrected_tests;
