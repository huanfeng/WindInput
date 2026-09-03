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
    /// 联想候选：**上屏之后**按刚上屏的内容给出的下一批候选，没有编码。
    ///
    /// 它与其余来源的区别不在「哪本词库」而在「有没有输入」——正因如此，凡是以
    /// 「输入码」为 key 的加工（词频记账、自动造词、码表调序）都必须跳过它，
    /// 判据就是这个来源值。对齐 [`CandidateSource::Phrase`] 的既有先例：那个也是
    /// 「有文本无码位」故恒不记词频。
    #[serde(rename = "assoc")]
    Assoc,
}

/// 候选词元数据
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateMeta {
    pub lexicon_name: String,
    pub is_user_dict: bool,
    pub is_temp_dict: bool,
    pub raw_weight: i32,
    pub freq_boost: i32,
    /// 当前 `weight` 由**哪个词库层**贡献。
    ///
    /// 跨层同词合并时权重取各启用层的最大值（`composite::merge_search`），而
    /// `code`/`natural_order`/`base_order` 仍属首个出现层——于是合并结果是「混血」的，
    /// 光看一个权重数字无从判断它到底来自哪本词库。本字段随 weight 一起被继承，
    /// 供悬停调试段标出来源（`权 5000 ←ext`）。
    ///
    /// 存在理由是**排查成本**：曾有一轮「扩展词库权重更高却没生效」的排查卡在这里——
    /// 调试段只显示一个最终数字，分不清是「合并没取到 max」还是「取到了但被别的排序
    /// 维度盖过」，两种根因在界面上长得一模一样。
    ///
    /// `Arc<str>` 而非 `String`：本字段在每条候选上都要填，而候选在按键热路径上成百上千地
    /// 造；层名是固定的几个短串，克隆一次原子计数远比一次堆分配便宜。
    /// `None` = 该候选不来自词库层（短语 / 命令 / 联想等），调试段不显示箭头。
    #[serde(skip)]
    pub weight_layer: Option<std::sync::Arc<str>>,
    /// 短语来源归属：`is_phrase` 候选时有意义，true=系统短语，false=用户短语。
    /// 仅供悬停调试提示区分来源（`wind_phrase::PhraseHit::is_system` 透传而来）。
    #[serde(default)]
    pub is_system_phrase: bool,
    /// 用户词/临时词在 store 里的**真实存储码**（`user_words`/`temp_words` 的 key 中段）。
    ///
    /// 存在理由是同文合并会造出**嵌合候选**：`pinyin/mod.rs` 第 6 步把 store 层候选并入
    /// 已有同文候选时，`code`/`boundary` 刻意保留已有那条的（换过去会把系统词典的真值
    /// 边界抹成未知），只把来源标记盖上去。于是候选的 `code` 是系统词/整句的码，而
    /// `is_user_dict`/`is_temp_dict` 指向 store —— 右键删除拿 `code` 拼 key 就删了个空。
    /// `redb` 的 `remove` 对不存在的 key 静默成功，表现为**点多少次都无作用**。
    ///
    /// 排序/边界一律不读本字段，它只服务「按 key 回写 store」的反向操作（删除）。
    /// `None` = 该候选不来自 store 层，或本就是新增分支（此时 `code` 即存储码）。
    #[serde(skip)]
    pub store_code: Option<std::sync::Arc<str>>,
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
    /// 本次检索中该文本**还占用过的其它码位**（同文本去重时，被丢弃那条的 `code` 并入此处）。
    ///
    /// **存在理由＝去重会破坏「检索范围」过滤的分组**。`filter_smart` 按 `(source, code)` 分组，
    /// 组内有常用词则滤掉非常用词；而同一个字命中多个码位时只留一条，其余码位随之消失，
    /// 于是「某码位下有常用字」这个事实在过滤之前就丢了——过滤结果因此**不单调**：
    /// 打 `siv` 时「档」以简码命中（`code="siv"`），它在 `sivg` 的那条被去重丢弃，`sivg` 组只剩
    /// 生僻的「桜」成了孤儿码而放行；打全 `sivg` 时「档」「桜」同组，「桜」才被滤掉。
    /// **同一个字，打得越全反而越不出**。
    ///
    /// 本字段把被吃掉的码位留在幸存候选身上，使分组统计能还原词库真相。只有常用候选的
    /// `merged_codes` 会被消费（非常用候选不遮蔽任何人），但累积是无条件的——`is_common`
    /// 由协调器在更下游填，去重发生时还不知道谁常用。
    ///
    /// ⚠️ 新增按 text 去重的地方，若其结果会流向 `filter_candidates`，**必须**调用
    /// [`Candidate::absorb_codes_from`] 并入，否则该处又会重新引入上述不单调
    /// （编译器抓不到，现象只在特定码位组合下出现）。当前四处：跨词库层合并 `composite`、
    /// 码表引擎 `convert` 的精确/前缀两循环、混输 `sort_dedup_truncate`、协调器 `build_candidates`。
    ///
    /// **反过来，这三类去重刻意不接**（并入会造出假的同码关系）：
    /// - 跨**方案**合并（快捷输入 `handle_mode` 按 mix members 汇总）——不同方案的码不同域，
    ///   而它们的 `source` 可能同为 `CodeTable`，守卫拦不住；该路径也不经过检索范围过滤；
    /// - 现造候选（临英大小写变形等）`code` 恒空，无码位可言；
    /// - 拼音模糊变体查询用的是 `LookupHit` 而非本类型，且同属一个 code 域。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_codes: Vec<String>,
    pub weight: i32,
    /// 词库**层级基序档位**（`[[dictionaries]].base_order`，默认 0）。排序时作为**独立层级**：
    /// weight 之后、natural_order 之前（见 `better`/`by_natural`）。小整数即可把整库排到另一库
    /// 前/后，与 natural_order 大小无关（不同于把偏移加进 natural_order 的旧做法）。
    pub base_order: i32,
    pub natural_order: i32,
    pub comment: String,
    pub is_common: bool,
    /// 用户**显式**把这个字标成了生僻（词库管理 / 候选右键），不是出厂字表里就没有它。
    ///
    /// ## 为什么不能只看 `is_common == false`
    ///
    /// 两者强弱不同：出厂没收录只是「字表里查不到」，而这一位是用户亲口说的「我不要它」。
    /// 智能档的「孤儿码位」规则（同码位一个常用字都没有时全部放行）本意是别让人打不出字，
    /// 但对用户亲手降级的字，那条保底会把它原样放回**而且还在第一位**——用户看到的是
    /// 「设了完全没反应」。故这一位让它**不吃那条保底**（见 `filter_smart`）。
    ///
    /// 滤掉不等于打不出：被滤的候选进 `FilterOutcome::filtered`，末页再按一次翻页键即可
    /// 放宽调出（`is_scope_filtered` 那条通路）。
    pub user_rare: bool,
    /// 该候选**按当前检索范围本应被滤掉**，是因用户按翻页键**临时放宽**才留在列表里
    /// （设计见 `docs/design/smart-filter-scope-relax.md`）。
    ///
    /// **语义严格限定为这一个客观事实，不编码任何排序或显示决策**。三个消费者各自决定行为：
    /// - `decide_auto_commit`：计数时**跳过**——今天的全码自动上屏，一部分正是靠智能过滤滤掉了
    ///   同码生僻字（见该函数注释与 `recheck_auto_commit_unique_after_filter`）。放宽把它们放回来
    ///   而不排除，会让一批原本满码即上屏的字退化成要按空格；
    /// - 排序：追加在列表**末尾**，原有候选顺序纹丝不动（首选位置恒定，盲打安全）；
    /// - 词频重排：**排除**在外，沉底是硬约束（否则误选一次就被 used-first 顶到常用字前，
    ///   而码表侧 used-first 不衰减）；
    /// - UI：加 `input.scope_relax.prefix` 前缀标注（默认 `·`）。
    ///
    /// ⚠️ **不要把「末尾项」写死进这个字段的语义**。它表达的是「本应被滤」这个客观事实，
    /// 沉底是**消费端（排序）**的决定——本设计的呈现方式就改过一次（曾按真实顺序插入），
    /// 两者混在一起会让改呈现必须连字段语义一起推翻。`is_prefix` 被静态短语借作「非精确层」、
    /// `is_fuzzy` 被简拼借作「沉底标记」都是同类前科——来源属性与排序决策必须分开。
    #[serde(default)]
    pub is_scope_filtered: bool,
    pub is_phrase: bool,
    pub is_command: bool,
    /// 是否来自模糊音变体命中（非原拼音精确匹配）。
    ///
    /// **这是「召回来源」标记，不是层级**（对齐 Go `CandidateFeatures.IsFuzzy`：
    /// 「MatchType 决定基础分层，IsFuzzy 施加额外惩罚」，二者正交可组合 ——
    /// 模糊命中同样可以是音节完全对齐的精确匹配）。故本字段**不参与**
    /// `cmp_match_layers`：模糊候选与精确候选同层竞争，惩罚由引擎在 weight 上
    /// 施加（见 `pinyin::FUZZY_WEIGHT_SCALE`）。
    ///
    /// **曾经是层级键，已废除**：此前它是 `cmp_match_layers` 的首要键，等价于
    /// 「惩罚 = ∞」——真实词典下打 `si` 时「是」被 230 条非模糊候选（多为码更长的
    /// 前缀补全「思考」「似乎」）压到第 231 位，而生产候选上限是 50~300，模糊音
    /// 因此在三条路径（拼音 / 混输 / 临拼）上全部等价于未实现。
    pub is_fuzzy: bool,
    /// 是否为**简拼**（声母缩写）候选，如 `nh`→「你好」。排序时整体降到全拼候选之后。
    ///
    /// **为何不复用 `is_fuzzy`**：用户词简拼路径此前正是借它沉底（`mod.rs` step6 硬置
    /// `is_fuzzy = true`），但简拼与模糊音是两种无关的召回方式——`is_fuzzy` 退出层级键后
    /// 那条借用会连带把简拼一起放上来。同 `is_prefix` 被静态短语借作「非精确层」标记的
    /// 前科，一个字段承担两种含义，复用即耦合两件无关的事。
    ///
    /// **两条简拼路径此前标志不一致**：系统词库简拼（step5，查 wdat `AbbrevSection`）走
    /// `is_prefix=true`，用户/临时词简拼（step6，现算声母比对）走 `is_fuzzy=true`。
    /// 本字段统一承接后者；前者的 `is_prefix` 维持不动（改它会动到前缀补全层的既有序）。
    #[serde(default)]
    pub is_abbrev: bool,
    /// 是否为**全拼降级**候选：双拼方案下把击键串当全拼解释所得（`nihao` → 「你好」）。
    ///
    /// 服务「多人共用一台机器」——主力用户打双拼，偶尔来的人只会全拼。开关见
    /// `schema.pinyin.shuangpin.allow_full_pinyin`；关闭时本字段全域恒 `false`。
    ///
    /// ## 这是**来源标记**，不是层级键
    ///
    /// 本字段只回答「这条候选来自全拼降级支路吗」。**沉不沉底是另一回事**，由
    /// [`cmp_match_layers`] 叠加置信度判定（高置信＝精确整词且消费整串 → 不沉底；
    /// 前缀补全 / 子短语 → 沉底）。二者拆开与 [[is_prefix]]（结构事实）和
    /// [[is_promoted_completion]]（排序决策）的关系同构。
    ///
    /// ⚠️ **曾经二者合一，是错的**：首版把「是否沉底」直接编码进本字段，于是高置信候选
    /// 不带标记 —— 而下面三处豁免认的正是这个标记，结果 preedit 跟随和边界校验豁免对精确
    /// 整词齐齐失灵。字段承担两种含义，改一个就会碰坏另一个。
    ///
    /// 「双拼优先」也不靠本字段兜底，而是由**同文去重**保证：同一个词双拼也解释得出时
    /// （consumed 相等），保留双拼那条（见 `recall_full_pinyin` 的 push）。
    ///
    /// ## ⚠️ 与 `is_abbrev` 同构：三处豁免缺一不可
    ///
    /// 本字段标记的候选，其 `code` 是**词的全拼码**、与双拼击键**不同域**，故双拼引擎里
    /// 三处按「击键即双拼」立论的逻辑必须一并豁免（`pinyin/mod.rs`）：
    /// ① `sp_boundary_mask` 真值边界校验（不豁免 ⇒ **整批候选被静默滤光**）；
    /// ② `consumed_length` 的 `map_consumed_length` 回映射（本流消费数本就是击键域，
    ///    再换算一次即错位）；
    /// ③ preedit 的 `build_raw_preedit`（按双拼音节边界切，首选是全拼候选时自相矛盾）。
    ///
    /// 三处的现成写法照抄 `is_abbrev` 即可——它们是同一类东西。
    ///
    /// 非双拼方案 / 开关关闭时恒 `false`，故对码表、混输、纯全拼三条路径零回归。
    /// 引擎内部用于排序，不推送 UI。
    #[serde(skip)]
    pub is_fullpinyin_fallback: bool,
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
    /// **谁置位**（新增候选来源时须对照，漏标 = 被压到精确候选之下，且编译器抓不到）：
    /// - 码表引擎 `CodeTableEngine::convert`：按 `code == input` 置位，覆盖文件词库/用户词/临时词
    ///   （它们都经 `dm.search`/`dm.search_prefix` 返回）及薄封装的英文引擎；
    /// - 混输 overflow 分支：以**完整输入**重新归一（其码表半边是按前 N 码查的）；
    /// - 协调器精确码短语（`phrases.lookup`）：按定义即精确匹配；
    /// - 协调器引导键导航候选（`$CC`/`$SS`/`$AA` 前缀命中）：**按既有设计恒置顶**而非因编码相等，
    ///   用户正是按引导键为了看到它们。这是本字段唯一的「非 code==input」成员，故字段语义取
    ///   「精确**档**」而非字面的「编码相等」。
    /// - 拼音引擎不置位：混输下码表精确恒先于拼音（与 `freq_rerank::freq_tier` 的档位设计一致）；
    ///   纯拼音模式全体为 `false`，本键退化为无操作。
    ///
    /// **为何不复用 `is_prefix`**：该字段已被自定义短语借作「非精确层」标记（协调器
    /// `build_candidates` 中前缀短语恒 `is_prefix=true`）。若给码表前缀候选也标 `is_prefix`，
    /// 短语会与码表词组落进同层——一个字段承担两种含义，复用即耦合两件无关的事。
    /// （当时短语还带 `PHRASE_WEIGHT_BASE`=40M，落进同层就会整体浮到词组之上；那个常量已删除，
    /// 但「一个字段两种含义」这条理由与权重无关，依然成立。）
    ///
    /// **为何需要独立层级而非靠权重**：词组权重来自词频、单字权重来自字频，两套量纲不可比。
    /// 「新的」(usrq, 47487) 纯按权重会压过简码「新」(usr, 11777)，把简码字挤到第三位——
    /// 跨类别比 weight，比的其实是类别。
    #[serde(default)]
    pub is_exact_code: bool,
    /// 是否为**引擎合成的整句解**（Viterbi 多词拼接，或超长词典整词的等价整句分）。
    ///
    /// 语义 = "这是引擎对整串输入的最优解读"，词频重排（`freq_rerank`）据此把它连同
    /// `is_phrase` 一起锚定在顶部，不因用户词频而下沉。
    ///
    /// 此前该判定靠 `weight >= 20_000_000` 的数值阈值实现，把"来源语义"编码进了权重数值，
    /// 导致两类问题：① 任何因别的原因被提权到 20M 以上的候选都会被误锚定，永久失去词频
    /// 学习能力；② 不相关的提权功能（如 `BARE_INITIAL_SINGLE_CHAR_BOOST`）必须小心避让
    /// 这条阈值线。改用显式标记后，权重只表达"多重要"，来源语义由本字段表达。
    #[serde(default)]
    pub is_sentence: bool,
    /// 整句解**已让位于精确整词**（降级，非销毁）。
    ///
    /// 触发条件：Viterbi 合成出的整句**不是**词典词条，而候选中存在覆盖同一段输入的
    /// 严格精确整词（系统词库或用户/临时层，非模糊命中）。此时整句仍是一条可选候选，
    /// 只是不再霸占首位 —— 代价从「选不到」降为「多按一次」。
    ///
    /// **为什么不直接清 `is_sentence`**：该标记的语义是「引擎对整串输入的最优解读」，
    /// 是**来源**属性；降级是**排序**决策。两者混在一个布尔里，日后任何新增的
    /// `is_sentence` 消费方都会连带继承排序语义。目前 `is_sentence` 的唯一生产消费点是
    /// `freq_rerank` 的顶部锚定，正是本字段要豁免的那一条。
    ///
    /// **为什么不复用 `is_exact_code`**：拼音引擎按约定全体不置位该字段
    /// （见其文档「拼音引擎不置位」一条），混输下码表精确档恒先于拼音依赖这个约定；
    /// 在拼音侧置位会让拼音候选整体越过码表候选，伤及共用比较器的另外两个引擎。
    ///
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_sentence_demoted: bool,
    /// 该整句是引擎**新合成**的解读，词库里没有以它为整体的词条。
    ///
    /// ## 为什么不能用 [[is_sentence]] 代替
    ///
    /// `is_sentence` 有两个来源：① 引擎新建的整句候选（Viterbi 多节点拼出，词库无此词条）；
    /// ② 整句与词典候选**同文合并**时给那条已有词典候选补的标记
    /// （`existing.is_sentence = true`，见 `pinyin/mod.rs` step 2 / 2b / 6.2 三处，
    /// 注释「同文合并后它就是整句解本身，须继承整句身份」）。
    ///
    /// 对排序而言两者等价——都该被 `freq_rerank` 锚在顶部，所以 `is_sentence` 合并它们是对的。
    /// 但对**自动造词**是致命的：打 `nihao` 选系统词库里的「你好」时 ② 同样为真，
    /// 照 `is_sentence` 放行就会把系统词一条条抄进临时词库，每次上屏多一次 redb 写事务，
    /// 配了 `promote_count` 的用户还会把大量系统词「晋升」进用户词库。
    ///
    /// 本字段只由 ① 置位，故语义恰好是造词要问的那句话：**这个词词库里有没有**。
    ///
    /// ## 为什么不改成协调器侧查词库
    ///
    /// 查系统词库要走 `word_codes_in` → `reverse_index_for`，而那份反查索引**最多缓存两份**
    /// （本次方案 + 全局主码表）。造词查拼音方案、悬停查主码表，两者会交替把对方挤掉，
    /// 每次重建是十万词级几十毫秒 —— 直接落在按键路径上。引擎侧本就知道这个事实，
    /// 透出一个 bool 是零运行时代价的做法。
    ///
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_synthesized: bool,
    /// 前缀补全**已被提升进完整匹配层**（排序决策，与 `is_prefix` 表达的「码更长」结构事实正交）。
    ///
    /// `is_prefix=true` 表达的是结构事实——候选码严格长于输入（补全词）；而「该不该沉到
    /// 非精确层」是**排序决策**。二者曾被塞进 `is_prefix` 一个布尔里：拼音残码上浮
    /// （`meiy→没有`）与用户长词上浮都靠**给真·补全词硬标 `is_prefix=false`** 实现，使
    /// 该字段名不符实（一条「码更长」的候选却 `is_prefix=false`）。
    ///
    /// 现按 [[is_sentence]] / `is_sentence_demoted` 的先例拆分：`is_prefix` 恒表结构事实，
    /// 本字段承接排序提升。`cmp_match_layers` 计算「有效前缀层」= `is_prefix && !本字段`，
    /// 提升后的补全在层级比较中等价于非补全（落进 Exact/子短语层，再按权重排）。
    ///
    /// 生产方：拼音引擎 step4（系统词残码上浮）/ step6（用户·临时词长词上浮）。
    /// 引擎内部用，不推送 UI。
    #[serde(skip)]
    pub is_promoted_completion: bool,
    /// 前缀补全比**输入自身表达的音节数**多出几个音节（`0` = 音节数恰好对齐 / 非补全候选）。
    ///
    /// 「输入自身表达的音节数」= 完整音节数 + (有尾部残码 ? 1 : 0)，即 `pinyin` 引擎里的
    /// `started_syllables`。⚠️ **不是**「输入在这条候选的切分下占了几个音节」——后者对
    /// `xia` 会因为存在 `xi|an`（西安）的切分而算成 2，整批放行词组，见
    /// `wind_engine::pinyin::completion_syllable_cap` 的文档。
    ///
    /// ## 为什么必须是字段而不是排序闭包里的局部量
    ///
    /// 协调器算不出音节数（它只有击键串，不持有 `SyllableTrie`），而**显示序在协调器**。
    /// 同款教训见 `is_promoted_completion` 与整句等效权重：任何只活在引擎闭包里的排序信息，
    /// 都会被协调器的重排静默推翻。
    ///
    /// ## 只服务显示序，**不参与引擎内部排序**
    ///
    /// 引擎的 `sort_by` 紧跟 `truncate`，排序键在那里同时决定**去留**；把本字段加进去等于
    /// 「音节数超出即被截断」——那是销毁，而本字段的语义是降级。分工与 `consumed_length`
    /// 完全一致（见 `cmp_by_consumed`），两者都只在协调器侧生效。
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
    /// 来自词典真值（rime 源数据 `ni hao` 的空格），供双拼按真实边界校验候选：
    /// 输入 nihao(5键) 双拼解释为 ni|ha|o，而「你好」的 boundary 是 ni|hao，二者不符即拒绝。
    ///
    /// **与 `code` 同进同出**：`composite::merge_search` 同 text 去重时 code 取高优先层、
    /// 换最短码时也换 code，boundary 必须跟着一起换，否则会配出「A 层的 code + B 层的 boundary」。
    ///
    /// 引擎内部用，不推送 UI（`serde(skip)`，省 IPC 带宽）。
    #[serde(skip)]
    pub boundary: u64,
    /// 简繁 1对多**变体候选**的输出覆盖（如「出」的变体「齣」）。
    ///
    /// `Some(t)` = 本候选是协调器在简繁开启时展开出的变体：显示与上屏**直接用 `t`**，
    /// 绕过出口处的 `maybe_s2t`；`text` 仍保持简体原字——词频学习、词库反查、shadow/
    /// 词频重排的按 text 匹配全部落在简体域，维持「内部状态一律简体」的不变量。
    ///
    /// 协调器展开/消费，不推送 UI（UI 收到的 CandidateItem.text 已是覆盖后的显示文本）。
    #[serde(skip)]
    pub s2t_override: Option<String>,
    /// **上屏时实际写出的文本**；`None` = 就用 `text`（绝大多数候选）。
    ///
    /// 目前唯一的用户是**词语联想**：候选栏显示整词「中国」（用户才看得懂自己在选什么），
    /// 而「中」已经在屏幕上了，真正要补出去的只有「国」。
    ///
    /// ⚠️ 与紧邻上方的 `s2t_override` **不是一回事**，别互相顶替：那个是「这条候选的简繁
    /// 变体形态」，会绕过出口的 `maybe_s2t`；本字段只换上屏文本，简繁转换照常发生。
    /// 拿 `s2t_override` 装联想后缀，会让开着简繁的用户补出来的那半截不转换。
    ///
    /// 只由联想的提交路径（`handle_assoc::commit_assoc_at`）消费——常规选词路径不读它，
    /// 因为常规候选恒为 `None`，读了也是白读。
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
    /// 候选**稳定 id**：跨会话、跨日期不变的身份标识，供 shadow 规则（置顶/移动）精准匹配。
    ///
    /// 格式 `phrase:{code}:{原始记录文本}`（对齐 Go `dict.phraseCandID`）。生产方是协调器的
    /// 短语装配（`build_candidates`）——`text` 是模板求值结果、逐日变化，`phrase_template`
    /// 才是 store 里的原始记录。空串 = 该候选无稳定身份（码表/拼音等静态候选，其 `text`
    /// 本身就稳定，shadow 按文本匹配即可）。
    ///
    /// **为什么必须存在**：`date`/`time` 这类求值型短语的显示文本每天/每秒都变，shadow 规则
    /// 若以文本为键，写入次日即失配——用户看到的是「候选调整昨天设了，今天被还原」，
    /// 且失效的旧规则会逐日在 redb 里堆积。匹配契约见 [[ShadowPinRule]]。
    pub id: String,
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
            is_phrase: false,
            is_command: false,
            is_fuzzy: false,
            is_abbrev: false,
            is_fullpinyin_fallback: false,
            is_prefix: false,
            is_partial: false,
            is_exact_code: false,
            is_sentence: false,
            is_sentence_demoted: false,
            is_synthesized: false,
            is_promoted_completion: false,
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
            id: String::new(),
            display_text: String::new(),
            actions: Vec::new(),
        }
    }
}

impl Candidate {
    /// 按 text 去重时调用：把**被丢弃那条**所占的码位并入本候选（保留者吸收被弃者）。
    ///
    /// 语义见 [`Candidate::merged_codes`]。**必须传整个被弃候选而非它的 `code`**：被弃者自身
    /// 可能已经吸收过更早的同文本条目，只取 `code` 会让那些码位在第二次去重时静默丢失
    /// （去重是链式的：跨层合并 → 引擎层 → 协调器，同一个字可被连续吃掉三次）。
    pub fn absorb_codes_from(&mut self, other: &Candidate) {
        // ⚠️ **跨来源一律不并**。码表码与拼音码是两套编码体系，同一字符串在两边含义不同
        // （混输下 "wang" 既是五笔码又是拼音）——`filter_smart` 正是按 `(source, code)` 分组
        // 来隔离它们的。跨来源并入会给码表凭空造出一个「拼音码位有常用字」的假事实，
        // 反过来误滤同码的码表生僻字，与本字段要修的 bug 恰好对称。
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
    /// 线性查重而非 `HashSet`：同一文本的命中码位实测个位数（一个字在码表里的简码 + 全码），
    /// 而本函数在每次检索的去重循环里跑，`HashSet` 的分配开销反而占大头。
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

/// 「消费输入更多者优先」比较——对齐 librime：其候选容器
/// `DictEntryCollector = map<size_t, DictEntryIterator>` 以「消费的输入长度」为 key，
/// `phrase_->rbegin()` 从最长开始遍历 ⇒ 消费更多输入者恒优先，**先于词频、先于任何层级**。
///
/// ## ⚠️ 必须与 [`cmp_match_layers`] 成对出现，且恒在其**之前**
///
/// 本键与层级键分居两处、只有一处带上本键时，会出现「同一批候选两种次序」——而下游那处
/// 又只在某个条件下才跑，于是**功能表现为「时灵时不灵」**。实际发生过：
/// 协调器 `candidate_display_order` 以本键开头，而最后一道整体排序
/// `freq_rerank::rerank_positional` 的层级比较器只有 `cmp_match_layers`；后者**仅在本次输入
/// 有词频记录时才被调用**（`recs.is_empty()` 直接 return）。真机现象是：
/// 「冰冻三尺非一日之寒」打到 `bingdongsanchi` 首次能出（第 1 位），**一旦上过屏进了词频表
/// 就再也出不来**（掉到第 24 位），从词频表删掉又恢复。
///
/// 根因是该候选在整音节边界上拿不到残码上浮（`is_promoted_completion=false`）⇒ 落进
/// `cmp_match_layers` 的前缀补全层 ⇒ 被 `bing` 的几十个同音单字整层压住；而它消费了整串、
/// 本该由本键把它顶在最前。**词频记录本身一格都没提升它**（`promote_prefix=single` 判假），
/// 记录的唯一作用是让那道用错键的重排被触发。
///
/// ⇒ 凡是会整体重排候选的地方，本键与层级键要一起用、顺序一致。
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
/// ⓪ 的位置与理由见 [[Candidate::is_fullpinyin_fallback]]（它是唯一一个「来源差异」却做成
/// 层级键的字段，判据是「用户显式声明了主编码方案」，不可照此给别的来源开口子）。
///
/// **有效前缀层** = `is_prefix && !is_promoted_completion`：`is_prefix` 表结构事实（码更长），
/// `is_promoted_completion` 表「已被提升进完整匹配层」的排序决策（拼音残码上浮 / 用户长词
/// 上浮）。二者正交，见 [[is_promoted_completion]] 字段文档。提升后的补全在此等价于非补全。
///
/// **`is_fuzzy` 刻意不在此参与**：模糊音是「召回来源」而非「匹配质量层级」——通过 zh↔z 命中的
/// 词同样可以音节完全对齐。把它做成层级键等价于「惩罚 = ∞」，真实词典下会把模糊候选压到
/// 200 名开外（远超 50~300 的生产候选上限），使模糊音整体失效。惩罚改由引擎在 weight 上
/// 施加，见 `wind_engine::pinyin::FUZZY_WEIGHT_SCALE`；这也是 Go 版 `ranker.go` 的原始设计
/// （`IsFuzzy` 只 `score -= 100`，与音节对齐 `+500`、用户词 `+300` 同量纲）。
///
/// 该层级此前在三处各写了一遍——引擎内部排序、协调器 `candidate_display_order`、
/// 词频重排 `rerank_pinyin_decay`——三份必须手工保持同步，漏改任何一处都不会编译报错，
/// 只会让候选顺序在某条路径上静默发散。三处现统一调用本函数，各自的额外维度
/// （权重/`base_order`/衰减分/整句锚定）在其前后自行追加。
pub fn cmp_match_layers(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    // 有效前缀层：结构补全被提升后等价于非补全（落进精确/子短语层）。
    let eff_prefix = |c: &Candidate| c.is_prefix && !c.is_promoted_completion;
    // 全拼降级候选**只有低置信的那部分**沉底。
    //
    // `is_fullpinyin_fallback` 本身是**来源标记**（「这条来自全拼降级支路」），单独拿它当
    // 层级键会把精确整词一起沉掉 —— 真机 `zaijian` 的正解「再见」当时被整层压到第 8 位，
    // 沉底沉掉的不是噪音而是答案本身。故判据叠加置信度：
    // - **高置信**（精确整词 + 消费整串，如 `zaijian`→「再见」）→ 不沉底，与双拼候选同层
    //   竞争，由协调器的「消费长度优先」把它排到那些只吃 2 键的同音单字之前；
    // - **低置信**（`is_prefix` 前缀补全＝预测尚未输入的音节、`is_partial` 子短语＝只解释
    //   了开头一截）→ 沉底。它们与双拼候选高度同质，放出来只挤占版面。
    //
    // 「双拼优先」不靠这个键兜底，而是由**同文去重**保证（consumed 相同则保留双拼那条，
    // 见 `recall_full_pinyin` 的 push）—— 两件事各归各位。
    //
    // ⚠️ 必须是**首键**：挂在 `is_abbrev` 之后会让双拼简拼候选反而沉得更深（简拼在第一键
    // 就输了），而简拼是双拼用户自己在用的输入方式，理应高于给外人预备的降级通道。
    //
    // **零回归**：非双拼 / 开关关闭时全域 `is_fullpinyin_fallback == false`，首键恒相等，
    // 比较直接落到原来的三键，与改动前逐字节等价。
    // ⚠️ 判据用 `eff_prefix` 而非裸 `is_prefix` —— 与下一行**同口径**。
    //
    // 本键问的是「这条是不是低置信的预测」，而 `is_promoted_completion` 的定义正是
    // 「引擎已判定它置信度足够高、主动提升进完整匹配层」（残码上浮 / 用户长词上浮，各自
    // 还带着 `COMPLETION_FAR_WEIGHT_FLOOR` 或 `max_extra_syllables` 的门槛）。两者在回答
    // 同一个问题，用结构真值回答就等于把上面刚建立的语义在这里丢掉。
    //
    // 真机现场：双拼开「允许全拼输入」后，用全拼打用户词库里的长词
    // （`qingfengshurufa` → 11 音节的「清风输入法内测问题反馈」）**完全打不出来**。
    // 双拼主路径把这串当双拼码切、根本命中不到该词，降级支路是它唯一的产出通道；
    // 而它在那里带 `is_prefix=true` ⇒ 被本键首位沉底 ⇒ 落在 595/604 位，协调器传的
    // limit 恒为 300 ⇒ 被 `truncate` 丢弃。给它补上上浮判据也无用：`is_promoted_completion`
    // 在这一行根本不被看，位次只从 603 挪到 595，出不了沉底组。
    let fp_demoted = |c: &Candidate| c.is_fullpinyin_fallback && (eff_prefix(c) || c.is_partial);
    fp_demoted(a)
        .cmp(&fp_demoted(b))
        .then(a.is_abbrev.cmp(&b.is_abbrev))
        .then(eff_prefix(a).cmp(&eff_prefix(b)))
        .then(a.is_partial.cmp(&b.is_partial))
}

/// 「音节数对齐者优先」比较（[`Candidate::completion_extra_syllables`] 升序）。
///
/// 输入表达了几个音节，就先给几个音节的候选；预测了更多未输入音节的补全排在其后。
/// 位置：`candidate_display_order` 里 `cmp_match_layers` **之后**、权重之前 —— 层内分档，
/// 不跨层提拔。
///
/// ## ⚠️ 只在显示序施加一次，**不要**再写进 `freq_rerank` 的层级比较器
///
/// 那里的 `base_pos` 就是显示序的下标，档位已随之传入；重复施加会把它从「默认序」升级成
/// **词频也翻不过的硬约束**。实测代价：`jisuanjik` 下选过 30 次的「计算机科学」(extra=1)
/// 再也压不过残码整句「计算机看」(extra=0)。
///
/// ★ 判据：音节数对齐是**先验**（用户还没表态时的合理猜测），词频是**实证**（他已经选过
/// 30 次）。实证该能推翻先验。真正不容词频跨越的是 [`cmp_match_layers`] 那种结构性质量
/// 差异（模糊命中 vs 精确匹配）。
///
/// ## 为什么权重折扣替代不了它
///
/// 引擎侧本已有 `COMPLETION_WEIGHT_DISCOUNT`（每多一个音节权重打对折，`0.5^extra`），
/// 但**折扣量级压不过词频跨度**：真实词库实测 `zaij` 下 3 音节的「再加上」(w 21112，折后
/// 5278) 稳压 2 音节的「再加」(2922，折后 1461)、「再见」(1419)；`zaim` 下「在美国」(3699)
/// 压过「再买」(819)。0.5 在对数域只有 0.69，而中文词频跨 5 个数量级。
/// 同款现象见 `wind_engine::pinyin` 里模糊音「折扣对付不了跨数量级的同音词频」那条。
///
/// ## 参考实现为什么用 0.5 就够
///
/// 因为**它们的音节图从结构上不产生这种候选**，0.5 只需区分「补全的最后一个音节」一档：
/// - librime（`algo/syllabifier.cc:190`）的 completion 只在 `[farthest, input.length())`
///   这一条边上展开 ⇒ 音节图的音节位数恒 = 完整音节 + (有残码?1:0)，而 `Table::Query`
///   严格沿图逐音节 `Advance` ⇒ 「在美国」需要第 3 个音节位，图上没有，**匹配不到**。
///   其词级预测（`predict_word` / `extra_code`）只对 >3 音节（`kIndexCodeMaxLength`）的长词
///   尾部生效，用户词典侧还有 `kNumSyllablesToPredictWord = 4` 的硬门槛。
/// - fcitx5/libime（`pinyin/pinyindictionary.cpp:519`）的超长词匹配要求
///   `max(minimumLongWordLength=3, LongWordLengthLimit) + 1 <= path.size()`，
///   而 `LongWordLengthLimit` 默认 **4** ⇒ 2 音节输入下压根不匹配超长词。
///
/// 我们把 0.5 借来当 `extra ∈ 0..4` 的通用惩罚，超出了它被标定的范围；补上本档位即可。
///
/// ## 逐级 vs 两档
///
/// 现为**严格逐级**（extra 升序）。若真机嫌 `beijingd` 下「北京大学」(extra=1) 掉得太深，
/// 收成 `min(extra, 1)` 两档是一行改动 —— 但收之前先确认不是 `max_extra_syllables`
/// 那个召回旋钮该调。
pub fn cmp_completion_extra(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    a.completion_extra_syllables
        .cmp(&b.completion_extra_syllables)
}

/// 候选「精确匹配档优先」比较（`is_exact_code` 降序）。
///
/// 与 `cmp_match_layers` 分设：后者表达「匹配质量层级」（模糊/前缀/子短语），本函数表达
/// 「是否属于精确档」，两者正交。
///
/// **必须两处共用**：码表引擎排完序后，协调器合并短语时还会用 `candidate_display_order`
/// 无条件重排全部候选。若只在引擎内排好而不落到 `Candidate::is_exact_code` 字段上，下游重排
/// 无从得知谁是精确匹配，只能按纯权重重来，引擎的结果被静默推翻——本函数即为修此断层而抽出。
///
/// **两处调用位置不同，是有意为之**：
/// - 协调器 `candidate_display_order`：置于 `cmp_match_layers` **之后**、权重之前——精确优先
///   只在同一匹配层内生效，不跨层提拔（`is_prefix=true` 的静态短语前缀枚举仍留在下层）。
/// - 码表引擎 `CodeTableEngine::convert`：作**顶层首要键**，不叠 `cmp_match_layers`。因其基础
///   排序器 `better`/`by_natural` 本就不含匹配层级，贸然引入会改变用户词（`store_layer` 会设
///   `is_prefix`）与文件词库候选的既有相对序，超出本键的职责范围。
///
/// **另有一份同概念判据**：`wind_engine::freq_rerank::freq_tier` 的 `code == input`（码表档位）。
/// 二者在纯码表路径结论一致；未合并是因为 `freq_tier` 只在开启自动调频时参与，且其档位划分
/// 还承载词频语义。改动任一处时须同步核对另一处。
pub fn cmp_exact_first(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.is_exact_code.cmp(&a.is_exact_code)
}

/// 候选是否属于**拼音精确档**：混输下该档整体先于码表前缀补全（见 [`cmp_pinyin_exact_first`]）。
///
/// 判据是「这条拼音候选完整、精确地解释了本次输入，且是常用字词」：
/// - **消费整串**（`consumed_length >= input_len`，0 = 引擎未标注，按整串算，与全仓约定一致）；
/// - `is_prefix`/`is_partial`/`is_abbrev` 全假 = 既非前缀补全、也非子短语、也非简拼，即音节精确对齐；
/// - `is_fuzzy` 假 = 非模糊音变体。★ 注意这里**不是**把 `is_fuzzy` 当层级键用（它明确不是层级，
///   见其字段文档），而是作为**提档准入**条件：模糊命中意味着用户没精确打对，不该越过码表词；
/// - `is_common` 真 = 落在检索范围（常用字表）内。★ 这一条是本判据的关键：拼音单音节的同音字
///   动辄上百条（`xu` 有 329 条，含权重 0 的生僻字 𬣙/馘/谞），若不限常用字，整片生僻字都会
///   越过码表前缀补全，反过来把码表候选挤出候选配额。用 `is_common` 而非固定条数上限，是让
///   「提多少条」跟着用户的检索范围设置走。
///
/// ⚠️⚠️ **「消费整串」必须直接问 `consumed_length`，不能拿 `!is_partial` 代替**（首版即栽于此）：
/// 真机打 `aaw`（本意 `aawt`→工作）时首选变成拼音「啊啊」。那是 Viterbi 整句（词条 `啊啊 a a`），
/// `code` 取 `completed`="aa"、`consumed_length=2`，只解释了 3 键中的 2 键（`w` 是残码）——
/// **可它的 `is_partial` 是 false**：整句走 `insert(0)` 不经 `pinyin/mod.rs` 的 `push_hit` 闭包
/// （那里才算 `is_partial`），同文合并时还会主动 `existing.is_partial = false`。
/// ★ `is_partial` 的语义是「这不是子短语」，**不是**「消费了整串」——两者在残码场景下分叉：
/// 整句「啊啊」既不是子短语、也没消费整串。两个条件都保留（`is_partial=true` 而
/// `consumed_length` 恰好未标注为 0 时，靠 `!is_partial` 兜住）。
///
/// ⚠️ **依赖 `is_common` 已被置位**：协调器 `mark_common` 必须在排序**之前**无条件跑过一遍。
/// 该置位历史上写在 `apply_filter` 内部，且 `FilterMode::Gb18030` 时提前 return 完全不置位 ——
/// 若沿用那个位置，本判据在 Gb18030 下会因 `is_common` 恒假而**整体失效且无任何痕迹**。
/// 判定（无副作用）与过滤（按模式裁剪）因此拆开：判定无条件、过滤仍留在原步骤。
///
/// `input_len` 取**字节长度**，与 `consumed_length` 同域（输入缓冲恒为 ASCII 码字符）。
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
/// 五笔优先的硬约束：码表精确全码恒在拼音之上，词频重排只在同档内调整。
/// 纯拼音 / 纯码表模式下同源候选档位相同，退化为按词频排序。
///
/// ★ 档 1 是「五笔优先」的一处**有意松动**：码表**精确**仍恒先于拼音（档 0 < 档 1），但码表
/// **前缀补全**要让位于拼音精确匹配。理由是短输入下二者置信度恰好反相关——`xu` 的 124 条码表
/// 前缀补全全都要打满 4 码才精确，而拼音 `xu` 已是完整音节。
///
/// ## 档 0 内「码表精确 vs 精确码短语」由权重裁决
///
/// 二者曾分居档 0/档 1（短语恒在后）。合并的直接动因是**消除一处开关依赖的不一致**：
/// `PHRASE_WEIGHT_BASE`(40M) 删除后，纯码表下协调器已按权重比二者，而本函数（经
/// `freq_rerank::freq_tier`）在**开启码表自动调频**时是首要键、整体压过协调器显示序——
/// 于是同一个输入，开调频码表精确恒赢、关调频按权重比，两种结果。
///
/// ⚠️ **只合并了「精确」这一对，前缀那一对刻意不合**（前缀短语仍在档 3、码表前缀补全在档 2）。
/// 归一化让两边量纲可比了，但**可比 ≠ 该比**：档位表达的不只是量纲，还有置信度。
/// 精确码短语是「用户打全了码、明确要它」，前缀短语是「只打了前缀、系统猜他可能要」。
/// 且前缀短语权重（新建默认 1800、系统短语 800~2000）普遍高于码表前缀补全（五笔主库
/// median 941），合档会让它压过多数补全候选——那正是历史上用户报过的
/// 「系统/用户短语前缀匹配时优先级偏高、压普通编码/候选」（回归测试见
/// `input_flow.rs::prefix_group_marker_defers_*`）。
///
/// ## 为什么放在 `wind-candidate`
///
/// 本函数原名 `freq_rerank::freq_tier`，是「跨来源档位」这个语义的**第二份**实现——
/// 协调器 `candidate_display_order` 的 `cmp_exact_first` + `cmp_pinyin_exact_first`
/// 是第一份，混输引擎的权重加成是第三份。搬到这里是收敛的第一步：让判据只有一个定义，
/// 调用方各自决定**在哪一级用它**。第三份（混输加成）已随
/// `docs/design/mixed-source-tier-quota.md` 拆除，引擎侧改用自己的**截断**档位
/// （`MixedEngine::truncation_tier`，只管「谁活下来」，与本函数的「谁排前面」职责不重叠）。
///
/// ⚠ `c.code == input` 与 [`Candidate::is_exact_code`]（见 [`cmp_exact_first`]）是同一概念的
/// 两份判据，纯码表路径结论一致。未合并是因为本档位还承载来源语义（前缀短语单独占档、
/// 按来源分 Pinyin/English 档）。
///
/// ## ⚠️⚠️ 只能在**协调器侧**调用，引擎内部用不了
///
/// 拼音精确档（档 1）的判据 [`is_pinyin_exact_tier`] 要求 `c.is_common`，而 `is_common` **只在协调器
/// `mark_common` 置位**（`handle_candidate.rs`），引擎产出的候选该字段恒为 `false`（Default）。
/// 于是在引擎里调用本函数，拼音精确候选会静默落到档 4 —— **不报错、不 panic，只是档位悄悄错**。
///
/// 这条约束的后果：混输引擎内部的排序（`sort_dedup_truncate`，决定**截断时谁进得来**）
/// 不能改用本函数。它现在有自己的 `MixedEngine::truncation_tier`——**刻意是另一套**，
/// 因为两者回答的不是同一个问题：本函数管「谁排前面」（含 `is_common` 这种展示层概念），
/// 引擎那套管「谁活下来」（只需来源与精确性，不需要 `is_common`）。职责不重叠，
/// 故不构成红线③所说的并行实现。
pub fn source_tier(c: &Candidate, input: &str) -> u8 {
    use CandidateSource::*;
    if c.is_phrase {
        // 短语按「完全匹配 vs 前缀匹配」分档，勿因 is_phrase 一刀切抬到码表前缀补全之上：
        // - 精确码短语（`lookup`，码==输入的完全匹配 → `is_exact_code=true`）进档 0，
        //   与码表精确候选**同档比权重**（理由见函数文档「档 0 内…由权重裁决」）；
        // - 前缀短语（`lookup_prefix` 命中 → `is_exact_code=false`）留档 3、在码表前缀补全之后。
        //   否则混输/拼音下打 `da` 会让 `date` 短语只因 is_phrase 就压过码表前缀补全（如 矼）。
        //
        // ⚠️ 前缀短语曾与码表前缀补全同档、靠 weight 分先后，那从来不是一次真正的比较：
        // 混输引擎给码表前缀补全 `+PARTIAL_MATCH_BOOST`(500K)，而前缀短语拿原始权重
        // （当时的 `PHRASE_WEIGHT_BASE`=40M 只加给精确码短语），于是码表恒赢。加成拆除后
        // 就地拆成两档，把既有次序写成规则——**这条分档保留**，不随精确那一对合并，
        // 理由见函数文档（可比 ≠ 该比）。
        return if c.is_exact_code { 0 } else { 3 };
    }
    if is_pinyin_exact_tier(c, input.len()) {
        return 1;
    }
    match c.source {
        CodeTable if c.code == input => 0, // 码表精确全码（如五笔 cang→駏）
        CodeTable => 2,                    // 码表前缀补全
        Pinyin => 4,                       // 拼音（非精确档：前缀补全/子短语/简拼/模糊/生僻）
        English => 4,
        // 其余来源（主要是 `CandidateSource::None`，即引擎未标注来源的候选）。
        // 与前缀短语同档是**沿袭**而非设计——二者都属「说不清置信度」，放在码表补全之后、
        // 拼音其余之前。若将来有来源需要明确定位，应单独判而不是继续挂在这里。
        _ => 3,
    }
}

/// 「拼音精确档先于码表前缀补全」比较（**仅混输**，见 `candidate_display_order` 的 `mixed` 参数）。
///
/// **要解决什么**：混输打 `xu`，首选是码表精确全码「弱」（`xu` 是二简码），但拼音的「需」
/// （`code==xu` 的精确匹配、该音节最高频字）此前排在**第 98 位**（实测，报告者 `per_page=5`
/// ⇒ 第 20 页）——被码表 `xu*` 的 124 条前缀补全整体压住。
/// 根因是「精确 vs 前缀」这个维度混输只承认码表那一半：
/// 码表精确 +1e7、码表前缀补全 +`PARTIAL_MATCH_BOOST`(500K)，而拼音**不分精确与补全**统一
/// `÷PINYIN_TIER_SCALE`(100)。拼音侧的 `is_prefix`/`is_partial` 信息一直是齐全的，只是在
/// `normalize_pinyin` 那一步被抹平了。
///
/// **为何是层级键而不是权重加成**：给拼音精确档加一个介于 500K 与 1e7 之间的常量同样能排对，
/// 但那是「把类别编码进权重」的老范式（红线②）。更要紧的是拼音词库最高权重是「的」
/// **15,378,475**，任何「不先归一就加 boost」的写法都会让它越过码表精确档 1e7 ——
/// 打 `de` 首选从五笔字变成「的」。层级键没有这个量纲陷阱。
///
/// **位置**：置于 `cmp_exact_first` **之后**、`by_weight` 之前。于是三档顺序为
/// 「码表精确/精确码短语 → 拼音精确 → 码表前缀补全」，档内仍按权重竞争。
///
/// ⚠️ **只在混输生效**：纯拼音下全体候选都是 `Pinyin` 来源，本键会退化成「`is_common` 优先」，
/// 把含生僻字的多字词（`is_string_common` 要求整串每字都常用）硬降到全部常用单字之后 ——
/// 那是明显回归。纯码表下没有拼音候选，本键是空操作。
///
/// ⚠️ **`freq_rerank::freq_tier` 是同概念的第二份判据**（同一个 `is_pinyin_exact_tier`，档位 1）。
/// 开自动调频时它是首要键、整体压过本比较链，两处改一处须核对另一处（红线③）。
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
mod pinyin_exact_tier_tests {
    use super::*;
    use std::cmp::Ordering;

    /// 混输里「拼音精确档」的典型成员：`xu`→「需」（精确音节、常用字）。
    fn pinyin_exact() -> Candidate {
        Candidate {
            text: "需".into(),
            code: "xu".into(),
            weight: 6999,
            is_common: true,
            source: CandidateSource::Pinyin,
            ..Default::default()
        }
    }

    /// 码表前缀补全：`xu` 输入下的 `xuaj`→「弹幕」（码更长，故非精确）。
    fn codetable_prefix() -> Candidate {
        Candidate {
            text: "弹幕".into(),
            code: "xuaj".into(),
            weight: 1554,
            is_common: true,
            source: CandidateSource::CodeTable,
            ..Default::default()
        }
    }

    /// `xu` 的字节长度（本组用例的输入）。
    const XU_LEN: usize = 2;

    #[test]
    fn pinyin_exact_common_char_is_in_tier() {
        assert!(is_pinyin_exact_tier(&pinyin_exact(), XU_LEN));
    }

    /// ★★ 真机 `aaw` 现场（本判据首版栽在这里）：Viterbi 整句「啊啊」只消费 2/3 键，
    /// 但 `is_partial` **是 false**（整句走 `insert(0)` 不经算 `is_partial` 的闭包，同文合并时
    /// 还会主动置 false）。故「消费整串」必须直接问 `consumed_length`，不能拿 `!is_partial` 代替。
    #[test]
    fn partial_sentence_is_excluded_even_though_not_marked_partial() {
        let aa = Candidate {
            text: "啊啊".into(),
            code: "aa".into(),
            weight: 516,
            is_common: true,
            is_partial: false, // ← 整句路径就是这么置的，正是本用例的要害
            consumed_length: 2,
            source: CandidateSource::Pinyin,
            ..Default::default()
        };
        assert!(
            !is_pinyin_exact_tier(&aa, 3),
            "只消费 2/3 键的整句不得进精确档（`aaw` 时它会抢走首位）"
        );
        // 同一条候选，输入恰好是 `aa`（消费整串）时才够格。
        assert!(is_pinyin_exact_tier(&aa, 2), "消费整串时应进档");
    }

    /// `consumed_length == 0` = 引擎未标注 ⇒ 按整串算（与全仓约定一致，如 `apply_freq_rerank`
    /// 的 `consumes_all`、`clear_blocked_by_candidates`）。
    #[test]
    fn unmarked_consumption_counts_as_whole_input() {
        let c = Candidate {
            consumed_length: 0,
            ..pinyin_exact()
        };
        assert!(is_pinyin_exact_tier(&c, 5));
    }

    /// ★ 生僻字不进档 —— 这是「不设条数上限、改用检索范围把关」的落点。
    /// `xu` 有 329 条同音字，若生僻字一并提档，整片生僻字都会越过码表前缀补全。
    #[test]
    fn rare_char_is_excluded_from_tier() {
        let rare = Candidate {
            is_common: false,
            ..pinyin_exact()
        };
        assert!(
            !is_pinyin_exact_tier(&rare, XU_LEN),
            "非常用字不得进拼音精确档"
        );
    }

    /// 四类「非精确」召回一律不进档（各自单独翻转，避免一个漏判被别的条件掩盖）。
    #[test]
    fn non_exact_recalls_are_excluded_from_tier() {
        for (label, c) in [
            (
                "前缀补全",
                Candidate {
                    is_prefix: true,
                    ..pinyin_exact()
                },
            ),
            (
                "子短语",
                Candidate {
                    is_partial: true,
                    ..pinyin_exact()
                },
            ),
            (
                "简拼",
                Candidate {
                    is_abbrev: true,
                    ..pinyin_exact()
                },
            ),
            (
                "模糊音",
                Candidate {
                    is_fuzzy: true,
                    ..pinyin_exact()
                },
            ),
        ] {
            assert!(
                !is_pinyin_exact_tier(&c, XU_LEN),
                "{label}候选不得进拼音精确档"
            );
        }
    }

    /// 码表候选永不进本档（本档是拼音专属；码表的分层靠 `is_exact_code`）。
    #[test]
    fn codetable_never_enters_pinyin_tier() {
        assert!(!is_pinyin_exact_tier(&codetable_prefix(), XU_LEN));
        let ct_exact = Candidate {
            code: "xu".into(),
            is_exact_code: true,
            ..codetable_prefix()
        };
        assert!(!is_pinyin_exact_tier(&ct_exact, XU_LEN));
    }

    /// 核心断言：拼音精确档先于码表前缀补全，**且与权重高低无关**。
    /// 现场是混输打 `xu`——码表前缀补全带 `PARTIAL_MATCH_BOOST`(500K) 而拼音 `÷100` 只剩 69，
    /// 纯按权重永远输；层级键让它翻过来。
    #[test]
    fn pinyin_exact_outranks_codetable_prefix_regardless_of_weight() {
        let py = pinyin_exact(); // 混输里实际权重 6999/100 = 69
        let ct = Candidate {
            weight: 509_999, // 码表前缀补全的权重上限（9999 + 500K）
            ..codetable_prefix()
        };
        assert_eq!(cmp_pinyin_exact_first(&py, &ct, XU_LEN), Ordering::Less);
        assert_eq!(cmp_pinyin_exact_first(&ct, &py, XU_LEN), Ordering::Greater);
    }

    /// 反向锁：同档内本键不表态（交给后续权重级决），否则会掩盖档内的词频序。
    #[test]
    fn same_tier_is_undecided() {
        assert_eq!(
            cmp_pinyin_exact_first(&pinyin_exact(), &pinyin_exact(), XU_LEN),
            Ordering::Equal
        );
        assert_eq!(
            cmp_pinyin_exact_first(&codetable_prefix(), &codetable_prefix(), XU_LEN),
            Ordering::Equal
        );
    }
}

#[cfg(test)]
mod match_layer_tests {
    use super::*;
    use std::cmp::Ordering;

    fn cand(is_prefix: bool, is_partial: bool, is_promoted: bool) -> Candidate {
        Candidate {
            is_prefix,
            is_partial,
            is_promoted_completion: is_promoted,
            ..Default::default()
        }
    }

    /// `is_promoted_completion` 让「码更长的补全」在层级比较中等价于非补全（有效前缀层为 false）。
    #[test]
    fn promoted_completion_ranks_in_exact_layer() {
        let exact = cand(false, false, false); // 精确
        let plain_prefix = cand(true, false, false); // 普通前缀补全（沉底层）
        let promoted = cand(true, false, true); // 提升后的前缀补全

        // 普通补全排在精确之后。
        assert_eq!(cmp_match_layers(&exact, &plain_prefix), Ordering::Less);
        // 提升后的补全与精确同层（层级比较相等，交由后续权重决出）。
        assert_eq!(cmp_match_layers(&exact, &promoted), Ordering::Equal);
        // 提升后的补全排在普通补全之前。
        assert_eq!(cmp_match_layers(&promoted, &plain_prefix), Ordering::Less);
    }

    /// 提升只影响前缀层，不越过子短语维度：提升补全(is_partial=false)仍优先于子短语(is_partial=true)。
    #[test]
    fn promoted_completion_still_above_subphrase() {
        let promoted = cand(true, false, true);
        let subphrase = cand(false, true, false);
        assert_eq!(cmp_match_layers(&promoted, &subphrase), Ordering::Less);
    }

    /// **`is_fuzzy` 不得参与层级比较**（本次改动的核心不变量）。
    ///
    /// 它是「召回来源」而非「匹配质量」：把它做成层级键等价于「惩罚 = ∞」，真实词库下会把
    /// 模糊候选压到 200 名开外（`si` 下「是」第 231 位），而生产候选上限仅 50~300 ——
    /// 模糊音因此在拼音 / 混输 / 临拼三条路径上全部等价于未实现。惩罚改由引擎在 weight 上
    /// 施加（`wind_engine::pinyin::FUZZY_WEIGHT_SCALE`）。
    ///
    /// 谁把 `is_fuzzy` 加回 `cmp_match_layers`，这条就会挂。
    #[test]
    fn fuzzy_is_not_a_layer() {
        let exact = Candidate::default();
        let fuzzy = Candidate {
            is_fuzzy: true,
            ..Default::default()
        };
        assert_eq!(
            cmp_match_layers(&exact, &fuzzy),
            Ordering::Equal,
            "模糊命中须与精确命中同层，由权重而非层级决出先后"
        );

        // 与其它层级维度组合时，也只由那些维度说了算。
        let fuzzy_exact = Candidate {
            is_fuzzy: true,
            ..cand(false, false, false)
        };
        let plain_prefix = cand(true, false, false);
        assert_eq!(
            cmp_match_layers(&fuzzy_exact, &plain_prefix),
            Ordering::Less,
            "模糊的精确匹配仍应优先于非模糊的前缀补全"
        );
    }

    /// 简拼整体沉到全拼之后。此前该语义由用户词简拼硬置 `is_fuzzy=true` 借位实现，
    /// `is_fuzzy` 退出层级键后改由本字段承接——行为须与从前一致。
    #[test]
    fn abbrev_ranks_below_full_spelling() {
        let full = Candidate::default();
        let abbrev = Candidate {
            is_abbrev: true,
            ..Default::default()
        };
        assert_eq!(cmp_match_layers(&full, &abbrev), Ordering::Less);
        assert_eq!(cmp_match_layers(&abbrev, &full), Ordering::Greater);
    }

    /// 层级维度的优先级顺序：简拼 > 有效前缀层 > 子短语。
    /// 前者为真即整体沉底，不因后者更优而被拉回。
    #[test]
    fn abbrev_outranks_other_layer_dimensions() {
        // 简拼但结构上是「精确匹配」；对手是普通前缀补全（更差的结构）。
        let abbrev_exact = Candidate {
            is_abbrev: true,
            ..cand(false, false, false)
        };
        let plain_prefix = cand(true, false, false);
        assert_eq!(
            cmp_match_layers(&abbrev_exact, &plain_prefix),
            Ordering::Greater,
            "简拼是首要键，即便其结构更优也须沉在前缀补全之后"
        );
    }
}
