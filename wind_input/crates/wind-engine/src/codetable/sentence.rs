//! 码表整句：把一长串码切成多个编码单元并组句。
//!
//! 设计文档：`docs/design/codetable-sentence-input.md`。
//!
//! # 与拼音整句的关系
//!
//! **解码器完全复用**——[`ViterbiDecoder`] 收的是「按结束位置分桶的字节跨度节点」
//! ([`WordNode`])，不含任何拼音语义。本模块只负责产出那批节点，Viterbi 一行都不用改。
//!
//! 不复用的是两样东西：
//!
//! 1. **切分图**。拼音有 417 个合法音节构成的先验（`SyllableTrie` + `SegGraph`），
//!    码表没有——任何位置切开都「合法」。故这里直接枚举跨度 `(p, q)`，
//!    `q - p ∈ [1, max_code_length]`，剪枝全部交给词典命中与简码策略。
//!    （原设计想给 `SegGraph` 加一个 `from_fixed_len` 构造器，实现时发现用不上：
//!    `SegGraph` 的价值在 `reachable` 剪枝与歧义接缝惩罚，码表两者都没有——
//!    全位置可达、没有「同一段又能拆成两段」的音节学含义。）
//!
//! 2. **打分函数**。见 [`score_code_node`]：不给单字虚词优待，理由在那里。
//!
//! # 为什么需要一张索引
//!
//! 极点五笔词库的 `weight` **是层级带，不是词频**：一简 9999 / 二简 9950 / 三简 9000 /
//! 全码均值 851。直接拿它当整句分，`ln(9999) − ln(851) ≈ 2.46` 的落差会让任何输入
//! 都被切成一串简码字。[`ShortCodeIndex`] 同时解决两件事：判定「这条是不是简码」，
//! 以及给简码条目换上**它自己全码位置的权重**（那才是真实词频量纲）。

use crate::pinyin::grammar::Grammar;
use crate::pinyin::viterbi::{ViterbiDecoder, ViterbiResult, WordNode};
use std::collections::HashMap;
use std::sync::Arc;
use wind_candidate::Candidate;
use wind_dict::DictManager;
use wind_dict::cached::CachedDict;

/// 词频归一化基准——**标定系数，非精确的词库总权重**（同 `pinyin::lattice::DICT_TOTAL`
/// 的性质，理由见那里的长注释）。
///
/// 取值来自本仓极点五笔词库的实测：4 码带 82469 条 × 均值 851 ≈ 7.0e7。
/// 它与 [`WORD_PENALTY`] 共同构成「每词固定罚」这**一个**旋钮
/// （`Σ ln(f/T) = Σ ln(f) − n·ln T`），换词库导致的漂移只等价于该罚值的微调，
/// 故不必随词库精确浮动。
///
/// ⚠️ 与拼音侧的 `DICT_TOTAL`(2.4e8) **不同域**，两者不可互换：码表整句用的是
/// 「全码带权重」（120~7999），拼音用的是词库原始词频（百万级）。
const CODE_DICT_TOTAL: f64 = 70_000_000.0;

/// 走**拼音词频**时的归一化基准，取拼音侧 `pinyin::lattice::DICT_TOTAL` 的同一个值。
///
/// ⚠️ **两个 TOTAL 必须分开，不能混用**：`weight_of` 有两条来源——拼音真实词频（百万级）
/// 与码表自身权重（百位级）。同一个基准套两套量纲，等于给其中一条平白加/减一个常数罚
/// （`Σ ln(f/T) = Σ ln(f) − n·ln T`，n 是边数 ⇒ 影响的是「切几段」的判断，不是平移）。
const PINYIN_DICT_TOTAL: f64 = 242_154_693.0;

/// 每条边的固定惩罚。语义与量纲同 `pinyin::lattice::WORD_PENALTY`：
/// Viterbi 的路径分是各节点之和，「每节点减 W」等价于「按边数罚 k·W」——
/// 把一个词打碎成几个高频字不再免费。
///
/// **这是压制简码切碎的主力**，而非 [`SHORT_CODE_PENALTY`]：一条 4 码词边对上
/// 四条 1 码简码边，后者要多付 3 份本罚 + 3 份单字罚，量级远大于任何词频差。
const WORD_PENALTY: f64 = 3.0;

/// 「这个权重是不是真实词频」的分界线。
///
/// # 极点五笔词库按**码长**发权重带，不按频率
///
/// | 区间 | 含义 |
/// |---|---|
/// | 9999 / 9950 / 9000 | 一简 / 二简 / 三简 —— 简码带，纯粹的展示序 |
/// | 8000–8999 | `protected_codes` 保护带 = 上游键位约定（四叠码等） |
/// | 120–7999 | 主库普通带 —— **只有这一段是真实 unigram 词频折算的** |
/// | 1–119 | 扩展库带 |
///
/// 分界取 8000（= `gen_dict` 的 `regular_weight_max=8999` 之上那一档的起点），
/// 不是魔法数：它就是 `gen_dict` 分带契约里普通带的上沿。
///
/// # 为什么必须挡
///
/// [`ShortCodeIndex`] 把简码换成「它全码位置的权重」，但**全码位置的权重也可能是带**：
/// 「八」的最长码 `wty` 是 3 码，拿的是 9000 简码带；「人」的最长码 `wwww` 是四叠保护带
/// 8010。于是「八」以 9000 压过「人」的 8010，`khlgw` 解成「中国八」。
///
/// # ⚠️ 这只是止血，不是修复
///
/// 落回中性值等于承认「这个字的真实频率，这份词库里没有」——它在一简位是 9999、
/// 在四叠位是键位约定，两处都不是词频。真正的修法是设计文档 §6.2 的方案 ②：
/// `gen_dict` 给词库**增列真实词频**（它本来就有 unigram 数据源，只是折算后被带覆盖了）。
/// 在那之前，同带内的字之间只能靠词典返回序裁决。
const SENTENCE_TRUSTED_WEIGHT_MAX: i32 = 7999;

/// 带内权重的中性替身：普通带（120–7999）的实测均值。
///
/// 取均值而非上沿/下沿：它要与**普通带里的真实词频**放在一起比较，落在中间才不会
/// 系统性地压过或压不过它们。
const NEUTRAL_WEIGHT: i32 = 851;

/// 把「带」换成中性值，普通带原样放行。见 [`SENTENCE_TRUSTED_WEIGHT_MAX`]。
fn trusted_weight(raw: i32) -> i32 {
    if raw > SENTENCE_TRUSTED_WEIGHT_MAX {
        NEUTRAL_WEIGHT
    } else {
        raw
    }
}

// ⛔ **这里没有单字惩罚 / 多字词加成**（拼音侧的 `SINGLE_CHAR_PENALTY` /
// `BASE_CONTENT_WORD_BONUS`），理由见 `score_code_node`。这不是遗漏，
// 是实测推翻了照搬——首版照搬后 `ggll` 上「一」(2831) 输给「来来回回」(1298)、
// `trnt` 上「我」(3371) 输给「特性」(1812)。

/// 简码边的对数惩罚（**串中**位置）。
///
/// # 它罚的是什么
///
/// 不是「这个字冷门」——简码条目的整句权重已经由 [`ShortCodeIndex`] 换成了它全码位置
/// 的真实词频，冷热已经如实表达。本罚针对的是**结构性的不确定性**：简码是「用户本可以
/// 打得更长却停在这里」，在一串连打的中间出现，比全码单元更可能是切分算法自作多情。
///
/// # 取值
///
/// 2.0（概率域 ≈ ×0.135），初值，**待实测标定**。标定方式见设计文档 §8：拿真实句子的
/// 码串做参数扫描，而不是拿词库条目回测（那恒能正确切分，是假绿）。
///
/// ⚠️ 改这个值之前先确认问题真的出在它身上：多数「切碎」的直觉问题其实由
/// [`WORD_PENALTY`] 与 [`SINGLE_CHAR_PENALTY`] 决定，本罚只是精调。
const SHORT_CODE_PENALTY: f64 = 2.0;

/// 简码边的对数惩罚（**串尾**位置）。
///
/// 串尾恒是「用户还没打完的那个编码」——此刻它长度不足是常态，不是投机拆分。
/// 故轻罚而非重罚，取 `ln 2`（概率域 ×0.5），与拼音侧 `PARTIAL_FINAL_PENALTY`
/// 对残码位的处置同一量级、同一理由。
const SHORT_CODE_TAIL_PENALTY: f64 = std::f64::consts::LN_2;

/// 每条边保留几条同码词条。对齐 librime `table_translator` 的 `max_homographs`
/// （默认 1，墨奇五笔整句方案配 2）。
///
/// 边上不放全部候选：排在后面的同码词几乎不可能赢下整句路径，却会让节点数与
/// Viterbi 的边数一起膨胀。
///
/// ⚠️ **取 8 而不是 librime 的 1~2**：`dm.search(code, N)` 是按**码表 weight** 降序取前 N，
/// 而码表 weight 是层级带、与真实词频无关（见 [`SentenceFreq`]）。取前 2 会在「码表序」
/// 里就把真实高频的那条筛掉，后面用真实词频打分也救不回来——**筛选与排序不在同一个
/// 量纲上时，筛选口必须放宽**。8 是「够覆盖一个 4 码位上的常见词条」的经验值。
const MAX_HOMOGRAPHS: usize = 8;

/// 词典组词表收几个词：拼音词库里按词频取前 N 个多字词。
///
/// 组词解决的是「这个词码表里没有」——五笔词库只收了约 8 万条，句子里大量的词只能逐字打，
/// 各字按单字词频各算各的，切分就很容易出错（逐字最短码打法首选仅 7.7%）。
///
/// 规模是实测扫出来的（3000 句 Tatoeba，「组词 + 语法」首选，词组最短码 / 逐字最短码）：
///
/// | 规模 | 首选 | 内存（紧凑表） |
/// |---|---|---|
/// | 3 万 | 84.0% / 77.2% | ~0.6 MB |
/// | 10 万 | 88.1% / 86.4% | ~2 MB |
/// | **20 万** | **89.9% / 89.2%** | ~4 MB |
/// | 全量 60 万 | 90.9% / 89.9% | ~12 MB |
///
/// 20 万之后边际收益不到 1 个点，内存却要翻三倍。
const COMPOSE_LEXICON_SIZE: usize = 200_000;

/// 组词最多跨几个编码单元。
const COMPOSE_MAX_UNITS: usize = 4;

/// 组词词表只收这么长以内的词：更长的词拼音库里多是成语俗语，跨 4 个单元也凑不出来。
const COMPOSE_MAX_CHARS: usize = 6;

/// 组词时每个跨度只拿分最高的几条参与拼接——同一跨度上排在后面的同码字几乎不可能
/// 组出正解，全拿会让组合数按 `MAX_HOMOGRAPHS^单元数` 膨胀。
const COMPOSE_PER_SPAN: usize = 3;

/// 码表整句用的语法模型文件（`<schemas>/pinyin/grammar/` 下，随安装包分发）。
///
/// 与拼音的 `schema.pinyin.grammar.model` **不共用配置**：拼音桌面端出厂不挂模型，码表整句
/// 实测挂上收益大得多（词组最短码首选 56.8% → 79.5%）——两边的取舍不同，不能互相牵连。
pub const GRAMMAR_MODEL: &str = "wanxiang-mini.gram";

/// 整句候选的权重基数。
///
/// 取值只需满足「高于码表任何词条权重」——本仓词库上限是简码带的 9999。
/// 给到 1e6 留足余量，同时**远低于**拼音侧 `SENTENCE_WEIGHT_BASE`(3e7)：
/// 两者不在同一个候选列表里比较，各自只需压过自己那一路的常规候选。
///
/// ⚠️ 顶部锚定**不靠这个数值**，靠 `Candidate::is_sentence` 标记
/// （`freq_rerank` 按标记判定，见该字段文档「此前该判定靠 weight >= 20_000_000 的
/// 数值阈值实现」的历史注记）。这里的权重只表达「在引擎内部排序时排前面」。
pub const SENTENCE_WEIGHT_BASE: i32 = 1_000_000;

/// 整句词频表：`text → 真实词频`，取自**拼音词库**。
///
/// # 为什么必须外借一份词频
///
/// 码表词库的 `weight` 是按**码长**发的层级带（一简 9999 / 二简 9950 / 三简 9000 /
/// 全码均值 851），不是词频。[`ShortCodeIndex`] 把简码换成「它全码位置的权重」只解决了
/// 一半——**全码位置的权重也可能是带**：3 码条目里有 1604 条其实是全码（字根不足 4 个
/// 的字），它们跟三级简码一起拿了 9000 带。实测后果：「个」(`whj`=9000 ⇒ 被当带) 输给
/// 「修」(`whte`=2135 普通带)，低频字压过极高频字。
///
/// 拼音词库（`rime_frost` + `cn_dicts`）里存的是**真实语料词频**，且与码表词库共享
/// 「文本」这一个键。实测覆盖率：码表 83640 个 text 里 66392 个能查到（79.4%），
/// 其中多字词 94.0%、单字 35.7%。
///
/// # 查不到怎么办
///
/// 单字覆盖率低是因为码表收了大量生僻字（拼音库的字表只有 8105 + 41448 扩展）。
/// **查不到本身就等于「罕见」**，故兜底给 [`OOV_FREQ`] 而不是中性值——把生僻字按中位数
/// 计价会让它们在整句里与常用字平起平坐。
///
/// # 只留交集
///
/// 拼音库 60 万条 text 全收进内存要 20~30 MB，而码表整句只可能用到码表里有的那些。
/// 构建时先扫码表建 text 集合，再扫拼音库只留交集 ⇒ 6.6 万条、约 3 MB。
#[derive(Debug, Default)]
pub struct SentenceFreq {
    freq: HashMap<String, i32>,
    /// 词典组词表，见 [`COMPOSE_LEXICON_SIZE`]。
    compose: ComposeLexicon,
    /// 用户词 / 临时词查不到词频时的下限，见 [`Self::user_floor`]。
    user_floor: i32,
}

/// 紧凑的 `text → 词频` 表：所有文本拼进一个 `String`，索引按文本排序后二分查找。
///
/// 不用 `HashMap<String, i32>`：20 万条下那要 20 万次堆分配、约 10 MB；这里两次分配、约 4 MB。
/// 表建好后只读，用不上哈希表的增删。
#[derive(Debug, Default)]
pub struct ComposeLexicon {
    blob: String,
    /// `(起点, 长度, 词频)`，按 `blob[起点..起点+长度]` 的字节序升序。
    index: Vec<(u32, u32, i32)>,
}

impl ComposeLexicon {
    /// `words` 可含重复文本（同词多读音），取词频最大的一条。
    fn from_words(mut words: Vec<(String, i32)>) -> Self {
        words.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        words.dedup_by(|later, kept| later.0 == kept.0);
        let mut blob = String::with_capacity(words.iter().map(|w| w.0.len()).sum());
        let mut index = Vec::with_capacity(words.len());
        for (t, w) in words {
            index.push((blob.len() as u32, t.len() as u32, w));
            blob.push_str(&t);
        }
        Self { blob, index }
    }

    fn text(&self, e: &(u32, u32, i32)) -> &str {
        &self.blob[e.0 as usize..(e.0 + e.1) as usize]
    }

    pub fn get(&self, text: &str) -> Option<i32> {
        self.index
            .binary_search_by(|e| self.text(e).cmp(text))
            .ok()
            .map(|i| self.index[i].2)
    }

    /// 有没有词以 `prefix` 开头（含等于）。组词 DFS 据此剪枝：拼到一半已不是任何词的前缀，
    /// 再往下拼也组不出词。按字节序排好的表里，以 `prefix` 开头的词恰好落在
    /// `prefix` 的插入点上。
    fn has_prefix(&self, prefix: &str) -> bool {
        let i = self.index.partition_point(|e| self.text(e) < prefix);
        self.index
            .get(i)
            .is_some_and(|e| self.text(e).starts_with(prefix))
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    fn heap_bytes(&self) -> usize {
        self.blob.capacity() + self.index.capacity() * size_of::<(u32, u32, i32)>()
    }
}

/// 拼音库查不到的 text 的兜底词频。
///
/// 取 1（而非中性值）：码表收的生僻字在真实语料里本就罕见，按中位数计价等于让
/// 「兲」和「的」在整句里平起平坐。`score_code_node` 对 `w > 0` 走 `ln(w/T)`，
/// 1 会得到很低的分，正是想要的。
const OOV_FREQ: i32 = 1;

impl SentenceFreq {
    /// 从拼音词库构建，只保留 `codetable` 里出现过的 text。
    ///
    /// ⚠️ O(码表 + 拼音库) 的全表扫描（合计约 70 万条），**只在首次整句解码时一次**。
    pub fn build(codetable: &DictManager, pinyin_dict: &CachedDict) -> Self {
        let mut wanted: std::collections::HashSet<String> = std::collections::HashSet::new();
        codetable.for_each_entry(&mut |_code, text, _w| {
            wanted.insert(text.to_string());
        });
        let mut freq: HashMap<String, i32> = HashMap::new();
        pinyin_dict.for_each_entry(&mut |_code, text, w| {
            if w <= 0 || !wanted.contains(text) {
                return;
            }
            // 同一个词在多个读音上各有一条，取 max：整句只关心「这个词有多常用」，
            // 不关心它这次是按哪个读音收录的。
            let slot = freq.entry(text.to_string()).or_insert(0);
            if w > *slot {
                *slot = w;
            }
        });
        let (compose, user_floor) = Self::build_compose(pinyin_dict, &freq);
        tracing::debug!(
            codetable_texts = wanted.len(),
            matched = freq.len(),
            compose = compose.len(),
            user_floor,
            "码表整句：词频表构建完成"
        );
        Self {
            freq,
            compose,
            user_floor,
        }
    }

    /// 组词表：拼音库里词频前 [`COMPOSE_LEXICON_SIZE`] 的多字词。返回 `(表, 入表门槛词频)`。
    ///
    /// 两遍扫描：先只收词频找出门槛，再收门槛以上的文本。一遍做完得先把 60 万条文本全部
    /// 装进内存再排序，峰值多出二三十 MB（虽然在后台线程上，也是实打实的高水位）。
    ///
    /// 码表收了的多字词（`codetable_freq` 里那份）无论排第几都一并收进来：逐字打出来也该认得，
    /// 且组词表要能回答「是不是某个词的前缀」（剪枝用），词就不能分散在两张表里。
    fn build_compose(
        pinyin_dict: &CachedDict,
        codetable_freq: &HashMap<String, i32>,
    ) -> (ComposeLexicon, i32) {
        let eligible =
            |text: &str, w: i32| w > 0 && (2..=COMPOSE_MAX_CHARS).contains(&text.chars().count());
        let mut freqs: Vec<i32> = Vec::new();
        pinyin_dict.for_each_entry(&mut |_code, text, w| {
            if eligible(text, w) {
                freqs.push(w);
            }
        });
        if freqs.is_empty() {
            return (ComposeLexicon::default(), OOV_FREQ);
        }
        // 同词多读音会重复计数，门槛因此略偏高、实收略少于 N——无伤大雅，不值得为它去重。
        let nth = COMPOSE_LEXICON_SIZE.min(freqs.len()) - 1;
        let threshold = *freqs.select_nth_unstable_by(nth, |a, b| b.cmp(a)).1;
        drop(freqs);
        let mut words: Vec<(String, i32)> = Vec::new();
        pinyin_dict.for_each_entry(&mut |_code, text, w| {
            if eligible(text, w) && w >= threshold {
                words.push((text.to_string(), w));
            }
        });
        words.extend(
            codetable_freq
                .iter()
                .filter(|(t, w)| eligible(t, **w))
                .map(|(t, w)| (t.clone(), *w)),
        );
        (ComposeLexicon::from_words(words), threshold)
    }

    /// 该文本的整句词频；查不到给 [`OOV_FREQ`]。
    pub fn get(&self, text: &str) -> i32 {
        self.freq.get(text).copied().unwrap_or(OOV_FREQ)
    }

    /// 词典组词用：`text` 是不是词、词频多少。组词表里没有时再查码表词的那份词频
    /// （码表收了、但不在拼音前 20 万里的词，逐字打出来也该认得）。
    fn compose_freq(&self, text: &str) -> Option<i32> {
        self.compose.get(text)
    }

    /// 用户词 / 临时词在拼音库查不到时的词频下限：取组词表的入表门槛。
    ///
    /// 查不到词频的用户词多是人名、新词——正是用户特意造的。按 [`OOV_FREQ`] 计价会让它
    /// 在整句里几乎永远输给逐字拆开（t134 拼音侧同一个病）。门槛的含义是「至少和组词表里
    /// 最冷的那个词一样常用」：用户造过它，这个待遇不算高估；再往上就是在替用户猜词频了。
    pub fn user_floor(&self) -> i32 {
        self.user_floor
    }

    pub fn len(&self) -> usize {
        self.freq.len()
    }

    pub fn is_empty(&self) -> bool {
        self.freq.is_empty()
    }
}

/// 简码索引：回答「这条词条是不是简码」+「它的整句权重该取多少」。
///
/// # 判据
///
/// ```text
/// is_short_code(code, text) := len(code) < max{ len(c) : c 是 text 的任一编码 }
/// ```
///
/// 五笔的简码定义就是全码的前 N 码，故这个判据**不需要 text→code 反查索引**
/// （同 `docs/design/codetable-short-code-yields-full.md` §2 论证过的性质）。
///
/// # 规模
///
/// 本仓极点五笔词库实测：一简 50 条**全部**是简码、二简 654/655 是简码、
/// 三简 3748/5352 是简码——合计 4452 条进表。剩下的 1604 条 3 码条目是**全码**
/// （「皮 hci」「线 xgt」这类字根不足 4 个的字），它们必须照常参与整句，
/// 这正是「整句只认 4 码单元」那条捷径走不通的原因。
#[derive(Debug, Default)]
pub struct ShortCodeIndex {
    /// `(code, text)` → 该 text 全码条目的权重。**只收录简码条目**，
    /// 故查不到即「不是简码」，无需再存一份布尔。
    short: HashMap<(String, String), i32>,
}

impl ShortCodeIndex {
    /// 扫全表构建。**O(全表 × 2 遍)，只在首次整句解码时调用一次**
    /// （由 [`CodeSentenceDecoder`] 的 `OnceLock` 守着）。
    ///
    /// 两遍而非一遍：一遍需要把全部条目缓存下来才能回头判「谁比谁长」，
    /// 8.8 万条的 `Vec<(String,String,i32)>` 比第二次遍历贵得多。
    pub fn build(dm: &DictManager) -> Self {
        // 第一遍：每个 text 的最长码长 + 该长度上的最高权重。
        let mut longest: HashMap<String, (usize, i32)> = HashMap::new();
        dm.for_each_entry(&mut |code, text, weight| {
            let len = code.chars().count();
            longest
                .entry(text.to_string())
                .and_modify(|slot| {
                    match len.cmp(&slot.0) {
                        std::cmp::Ordering::Greater => *slot = (len, weight),
                        // 同为最长码时取权重更高的那条：容错码/异体打法下同一 text 可有
                        // 多条等长全码，取 max 与词典查询「同码取首选」的方向一致。
                        std::cmp::Ordering::Equal => slot.1 = slot.1.max(weight),
                        std::cmp::Ordering::Less => {}
                    }
                })
                .or_insert((len, weight));
        });

        // 第二遍：挑出「码短于该 text 最长码」的条目，记下全码权重。
        let mut short = HashMap::new();
        dm.for_each_entry(&mut |code, text, _weight| {
            let len = code.chars().count();
            if let Some(&(max_len, full_weight)) = longest.get(text)
                && len < max_len
            {
                short.insert((code.to_string(), text.to_string()), full_weight);
            }
        });

        tracing::debug!(
            entries = longest.len(),
            short = short.len(),
            "码表整句：简码索引构建完成"
        );
        Self { short }
    }

    /// 该条目的整句权重与「是不是简码」。
    ///
    /// 非简码走 `raw`——全码带（120~7999）本就是真实词频折算的量纲。
    /// （25 个 `protected_codes` 保护带条目 8000+ 是上游键位约定而非词频，
    /// 数量太少不值得单独处理，留作已知偏差。）
    pub fn resolve(&self, code: &str, text: &str, raw: i32) -> (i32, bool) {
        match self.short.get(&(code.to_string(), text.to_string())) {
            Some(&full) => (full, true),
            None => (raw, false),
        }
    }

    /// 表内简码条目数（测试与诊断用）。
    pub fn len(&self) -> usize {
        self.short.len()
    }

    pub fn is_empty(&self) -> bool {
        self.short.is_empty()
    }
}

/// 节点对数概率打分：**纯词频 + 每边固定罚**，没有任何按词性/字数的调整。
///
/// 拼音侧 `score_node` 的三项调整逐条都不适用，且照搬会实测出错：
///
/// # ① 多字词加成（`BASE_CONTENT_WORD_BONUS`）——没有补偿对象
///
/// 拼音里 n 字词占 **n 个音节**，加成补偿的是「unigram 独立性假设对路径上每个词
/// 各扣一次 `ln(TOTAL)`」的过度惩罚：一个 3 字整词只扣一次，拆成 3 个字要扣三次。
///
/// 码表没有这回事——**4 码词组与 4 码单字占同一条边**，都只扣一次。过度惩罚不存在，
/// 加成便退化成「凡是词组一律白送 3.0×√字数」。实测后果（真实词库探针）：
/// `ggll` 上「一」(w=2831) 输给「来来回回」(w=1298)，`trnt` 上「我」(w=3371) 输给
/// 「特性」(w=1812) —— 两条都是**同码同跨度**的比较，本该纯按词频。
///
/// # ② 单字惩罚（`SINGLE_CHAR_PENALTY`）——同理
///
/// 它在拼音里罚的是「把词拆成单字」，而跨度相同就无所谓拆不拆。跨度不同的比较
/// 已经由 [`WORD_PENALTY`]（每边固定罚）承担。
///
/// # ③ 单字虚词优待（`FUNCTION_WORD_BONUS`）——会精准帮倒忙
///
/// 五笔一简 25 键对应的字里「是 / 在 / 要 / 不 / 了 / 有 / 我」全都在拼音侧的虚词表内，
/// 该优待合计 5.0（加成 2.0 + 豁免每词罚 3.0）恰好把**最危险的那批简码字**整体提权，
/// 正是本模块要压制的东西。
///
/// 三条合起来是同一句话：**同一条加成，在两套编码结构里前提不同**。这与拼音侧
/// `score_node_partial_final` 为残码位去掉虚词优待是同一条判据的又一次应用。
fn score_code_node(weight: i32, total: f64) -> f64 {
    // w ≤ 0 等价于「频次 0.5」，同 librime 用 DBL_EPSILON 让存疑条目排不上去的思路。
    let log_prob = if weight > 0 {
        (weight as f64 / total).ln()
    } else {
        (0.5 / total).ln()
    };
    log_prob - WORD_PENALTY
}

/// 一条整句解。
#[derive(Debug, Clone)]
pub struct SentenceResult {
    /// 拼接后的整句文本。
    pub text: String,
    /// 路径上的各个词（供 preedit 切分显示与调试）。
    pub words: Vec<String>,
    /// 路径总对数概率。
    pub log_prob: f64,
    /// 各编码单元的起始位 bitmask（相对输入串起点）。供 preedit 显示切分形态。
    pub boundary: u64,
}

/// 组合区切分显示用的分隔符。
///
/// 与拼音的音节分隔显示同一个字符：用户不必为码表另学一套符号。
pub const SPLIT_SEPARATOR: char = '\'';

impl SentenceResult {
    /// 把输入码按整句实际采用的切分插入分隔符：`aawtaawt` → `aawt'aawt`。
    ///
    /// 数据源是 [`Self::boundary`]（Viterbi 回溯出来的**实际**路径），不是重新按词长猜
    /// —— 同一串码可有多种切法，整句是按哪条拼出来的只有解码器知道。这与拼音侧
    /// `ViterbiResult::boundary` 存在的理由完全相同（那里记过一次「谎报切分」的教训）。
    ///
    pub fn split_code(&self, input: &str) -> String {
        if self.boundary == 0 || input.len() > 64 {
            return String::new();
        }
        let mut out = String::with_capacity(input.len() + self.words.len());
        for (i, ch) in input.char_indices() {
            if ch == SPLIT_SEPARATOR {
                // 用户自己敲的分隔符：原样留下，不再叠一个。
                out.push(ch);
                continue;
            }
            // bit i = 「第 i 个字节是某个编码单元的起点」。位置 0 与紧跟在用户分隔符
            // 之后的那个起点都不额外加符号（前者是串首，后者已经有分隔符了）。
            let after_sep = i > 0 && input.as_bytes()[i - 1] == SPLIT_SEPARATOR as u8;
            if i > 0 && !after_sep && (self.boundary >> i) & 1 == 1 {
                out.push(SPLIT_SEPARATOR);
            }
            out.push(ch);
        }
        out
    }
}

/// 码表整句解码器：持简码索引 + Viterbi。
///
/// 两张表都是懒构建（`OnceLock`）——只有真正用到整句的方案才付代价，且同一解码器只付一次。
/// 但「懒」只解决**要不要付**，不解决**在哪条线程上付**：见 [`LazyTables`]。
pub struct CodeSentenceDecoder {
    max_code_length: usize,
    /// 两张懒表连同它们的来源。**单独装在 `Arc` 里**，为的是 [`Self::prewarm`] 能把它
    /// 整份交给后台线程去填——解码器本身挂在引擎上，不便跨线程共享。
    tables: std::sync::Arc<LazyTables>,
    viterbi: ViterbiDecoder,
}

/// 首次整句解码要付的两笔一次性开销，连同它们的来源。
///
/// # ⚠️ 懒加载会把开销挪到按键线程上
///
/// 本机实测（`codetable_sentence_latency_probe`，页缓存已热）：
///
/// | 项 | 耗时 |
/// |---|---|
/// | [`ShortCodeIndex::build`]：码表全表扫描 | ~90 ms |
/// | 打开 82 MB `rime_frost.merged.wdat` + 全表扫建词频 | ~570 ms |
/// | **首次解码合计** | **~660 ms** |
/// | 其后每次解码 | < 0.2 ms |
///
/// 这 0.66 秒原本恰好落在**用户敲下第 5 个码**的那一刻（整句的触发闸门就是「超过码长且
/// 无精确匹配」），且冷页缓存下只会更久、`merged.wdat` 过期还要现场重建（数秒）。
/// 所以 [`CodeSentenceDecoder::prewarm`] 在引擎构建完成时就把这两张表推给后台线程去填。
///
/// 预热**不改变任何取值**，只改变谁来等：`OnceLock::get_or_init` 保证两条线程抢到同一份
/// 结果。极端情况（预热还没跑完用户就打到了超码长）按键线程仍会在 `get_or_init` 上等，
/// 但那是与「不预热」持平的最坏情况，不会更差。
struct LazyTables {
    index: std::sync::OnceLock<ShortCodeIndex>,
    /// 语法模型来源。`None` = 不挂（权重为 0 或未配）。
    grammar_source: Option<GrammarSource>,
    /// 挂了语法模型的解码器。**内层 `None` 表示「试过了但没成」**（模型文件缺失等），同 `freq`。
    grammar: std::sync::OnceLock<Option<ViterbiDecoder>>,
    /// 整句词频的来源。`None` = 没配 ⇒ 退回码表自身权重那套
    /// （见 [`CodeSentenceDecoder::weight_of`]），功能仍可用、只是没那么准。
    freq_source: Option<FreqSource>,
    /// 词频表。**内层 `Option` 表示「试过了但没成」**——加载失败要记住，
    /// 否则每次解码都会重试一遍那个必然失败的加载。
    freq: std::sync::OnceLock<Option<SentenceFreq>>,
}

impl LazyTables {
    fn new() -> Self {
        Self {
            index: std::sync::OnceLock::new(),
            grammar_source: None,
            grammar: std::sync::OnceLock::new(),
            freq_source: None,
            freq: std::sync::OnceLock::new(),
        }
    }

    /// 取（必要时打开）挂语法模型的解码器。没配、或模型打不开时返回 `None`。
    fn grammar_viterbi(&self) -> Option<&ViterbiDecoder> {
        let src = self.grammar_source.as_ref()?;
        self.grammar
            .get_or_init(|| match src {
                GrammarSource::Ready(g) => Some(ViterbiDecoder::with_grammar(Arc::clone(g))),
                GrammarSource::File { path, weight } => {
                    let cfg = crate::pinyin::octagram::OctagramConfig {
                        weight: *weight,
                        ..Default::default()
                    };
                    match crate::pinyin::octagram::OctagramGrammar::open(path, cfg) {
                        Ok(g) => Some(ViterbiDecoder::with_grammar(Arc::new(g))),
                        Err(e) => {
                            // 模型不随所有发行形态分发（定制版可能删掉），缺了就退回无模型。
                            tracing::warn!(
                                "码表整句：语法模型不可用（{}），按无模型解码：{e:#}",
                                path.display()
                            );
                            None
                        }
                    }
                }
            })
            .as_ref()
    }

    /// 取（必要时构建）整句词频表。没有来源、或来源加载失败时返回 `None`。
    fn freq_table(&self, dm: &DictManager) -> Option<&SentenceFreq> {
        let src = self.freq_source.as_ref()?;
        self.freq
            .get_or_init(|| match src {
                FreqSource::Dict(d) => Some(SentenceFreq::build(dm, d)),
                FreqSource::SchemasDir(dir) => {
                    let d = crate::manager::EngineManager::load_sentence_freq_dict(dir)?;
                    Some(SentenceFreq::build(dm, &d))
                }
            })
            .as_ref()
    }

    /// 取（必要时构建）简码索引。
    fn index(&self, dm: &DictManager) -> &ShortCodeIndex {
        self.index.get_or_init(|| ShortCodeIndex::build(dm))
    }
}

/// 整句词频的来源。
///
/// 两个变体的差别只在**什么时候把词库读进来**：
///
/// - [`Self::SchemasDir`]（生产路径）：只存目录，词库到首次整句解码时才加载。
///   ★ 这一层懒**不能省**：混输方案的主引擎同样走码表分支构建，但它的整句永远不会被
///   调用到（`MixedEngine::convert` 超码长直接走 `convert_overflow`，不经 primary），
///   构建期就加载等于白读一份 60 万条的词库；纯五笔用户若从没用过拼音方案，那次加载
///   还要现场构建 `merged.wdat`，直接体现为「切到五笔卡一下」。
/// - [`Self::Dict`]（测试路径）：已经拿在手上的词库，直接用。
enum FreqSource {
    SchemasDir(std::path::PathBuf),
    Dict(std::sync::Arc<CachedDict>),
}

/// 语法模型的来源：生产路径给文件（预热线程里打开），测试直接给实例。
enum GrammarSource {
    File {
        path: std::path::PathBuf,
        weight: f64,
    },
    Ready(Arc<dyn Grammar>),
}

impl CodeSentenceDecoder {
    /// 诊断：两张懒表里已建好的那几张的自报大小。不触发构建。
    pub(crate) fn memory_parts(&self) -> Vec<crate::engine::MemPart> {
        let mut v = Vec::new();
        if let Some(i) = self.tables.index.get() {
            let mut bytes = i.short.capacity() * (size_of::<((String, String), i32)>() + 1);
            for (c, t) in i.short.keys() {
                bytes += c.capacity() + t.capacity();
            }
            v.push(crate::engine::MemPart {
                name: "sentence/ShortCodeIndex".into(),
                bytes,
                allocs: 1 + i.short.len() * 2,
            });
        }
        if let Some(Some(f)) = self.tables.freq.get() {
            let mut bytes = f.freq.capacity() * (size_of::<(String, i32)>() + 1);
            for k in f.freq.keys() {
                bytes += k.capacity();
            }
            v.push(crate::engine::MemPart {
                name: "sentence/SentenceFreq".into(),
                bytes,
                allocs: 1 + f.freq.len(),
            });
            v.push(crate::engine::MemPart {
                name: "sentence/ComposeLexicon".into(),
                bytes: f.compose.heap_bytes(),
                allocs: 2,
            });
        }
        v
    }

    pub fn new(max_code_length: usize) -> Self {
        Self {
            max_code_length: max_code_length.max(1),
            tables: std::sync::Arc::new(LazyTables::new()),
            viterbi: ViterbiDecoder::new(),
        }
    }

    /// 配置数据来源用。只能在 [`Self::prewarm`] 之前调——预热线程拿走一份 `Arc` 后
    /// 这里就拿不到独占引用了，那是构建顺序写错了，直接 panic 比静默丢配置好查。
    fn tables_mut(&mut self) -> &mut LazyTables {
        Arc::get_mut(&mut self.tables).expect("整句解码器的数据来源须在 prewarm 之前配置")
    }

    /// 注入**数据目录**作为词频来源：词库到预热线程（或首次整句解码）时才读。生产路径走这个。
    pub fn with_schemas_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.tables_mut().freq_source = Some(FreqSource::SchemasDir(dir));
        self
    }

    /// 注入**已加载的**拼音词库作为词频来源。测试与探针走这个。
    pub fn with_pinyin_dict(mut self, dict: std::sync::Arc<CachedDict>) -> Self {
        self.tables_mut().freq_source = Some(FreqSource::Dict(dict));
        self
    }

    /// 挂语法模型文件（预热线程里打开）。`weight <= 0` 等于不挂。生产路径走这个。
    pub fn with_grammar_file(mut self, path: std::path::PathBuf, weight: f64) -> Self {
        self.tables_mut().grammar_source =
            (weight > 0.0).then_some(GrammarSource::File { path, weight });
        self
    }

    /// 挂已就绪的语法模型。测试与探针走这个。
    pub fn with_grammar(mut self, grammar: Arc<dyn Grammar>) -> Self {
        self.tables_mut().grammar_source = Some(GrammarSource::Ready(grammar));
        self
    }

    /// 把两张懒表推给后台线程去填，使按键线程不必为首次整句解码等上数百毫秒。
    ///
    /// 由引擎在构建完成时调用（`CodeTableEngine::prewarm_sentence`）。开销与判据见
    /// [`LazyTables`] 上的表——**只搬运，不改变任何取值**。
    pub fn prewarm(&self, dm: std::sync::Arc<DictManager>) {
        let tables = std::sync::Arc::clone(&self.tables);
        let spawned = std::thread::Builder::new()
            .name("codetable-sentence-warm".into())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                tables.index(&dm);
                let with_freq = tables.freq_table(&dm).is_some();
                let with_grammar = tables.grammar_viterbi().is_some();
                tracing::info!(
                    ms = t0.elapsed().as_millis(),
                    with_freq,
                    with_grammar,
                    "码表整句：后台预热完成"
                );
            });
        if let Err(e) = spawned {
            // 不是致命错误：懒表仍会在首次解码时现场构建，只是那一下会卡。
            tracing::warn!("码表整句：预热线程启动失败（{e}），退回首次解码时现场构建");
        }
    }

    /// 取（必要时构建）整句词频表。没有来源、或来源加载失败时返回 `None`。
    fn freq_table(&self, dm: &DictManager) -> Option<&SentenceFreq> {
        self.tables.freq_table(dm)
    }

    /// 一条词典命中的整句权重。
    ///
    /// **两条来源，优先拼音词频**：
    ///
    /// 1. 有拼音词库 ⇒ 直接查 `text` 的真实词频（查不到给 `OOV_FREQ`，等于「罕见」）。
    ///    此时**完全不看码表 weight**——它是层级带，掺进来只会污染。
    /// 2. 没有 ⇒ 退回原方案：简码换成全码位置的权重（[`ShortCodeIndex`]），
    ///    再用 [`trusted_weight`] 把「带」落回中性值。功能可用，但 §11.4 记的那些
    ///    错例会回来。
    ///
    /// 用户词 / 临时词在拼音库查不到时不按 [`OOV_FREQ`] 计价，而是给
    /// [`SentenceFreq::user_floor`]，理由见那里。
    fn weight_of(&self, dm: &DictManager, code: &str, hit: &Candidate) -> (i32, f64, bool) {
        let (text, raw) = (hit.text.as_str(), hit.weight);
        // 「是不是简码」两条路都要问：惩罚与词频是两件事。
        let is_short = self.index(dm).resolve(code, text, raw).1;
        match self.freq_table(dm) {
            Some(f) => {
                let mut w = f.get(text);
                if w == OOV_FREQ && (hit.meta.is_user_dict || hit.meta.is_temp_dict) {
                    w = f.user_floor();
                }
                (w, PINYIN_DICT_TOTAL, is_short)
            }
            None => {
                let (w, _) = self.index(dm).resolve(code, text, raw);
                (trusted_weight(w), CODE_DICT_TOTAL, is_short)
            }
        }
    }

    /// 内部用：取（必要时构建）简码索引。
    fn index(&self, dm: &DictManager) -> &ShortCodeIndex {
        self.short_code_index(dm)
    }

    /// 取（必要时构建）简码索引。
    pub fn short_code_index(&self, dm: &DictManager) -> &ShortCodeIndex {
        self.tables.index(dm)
    }

    /// 建词图：逐跨度 `(p, q)` 查词典。
    ///
    /// 跨度数 = `len × max_code_length`（≤ 4n），每跨度一次精确查询。
    /// 20 码输入 ⇒ ≤ 80 次，与拼音整句同量级。
    fn build_nodes(&self, input: &str, dm: &DictManager) -> Vec<Vec<WordNode>> {
        let bytes = input.len();
        let mut nodes: Vec<Vec<WordNode>> = vec![Vec::new(); bytes + 1];
        let compose = self.freq_table(dm).filter(|f| !f.compose.is_empty());
        // 组词的原料：每条编码单元边连同它的简码罚（组出的词要继承这份罚，见 `add_composed_nodes`）。
        let mut units: Vec<Unit> = Vec::new();

        // 码恒为 ASCII，故字节位置即字符位置，可直接切片。
        for p in 0..bytes {
            for q in (p + 1)..=(p + self.max_code_length).min(bytes) {
                let code = &input[p..q];
                for hit in dm.search(code, MAX_HOMOGRAPHS) {
                    let (weight, total, is_short) = self.weight_of(dm, code, &hit);
                    // 串尾轻罚、串中重罚——见两个常量的文档。
                    let penalty = match (is_short, q == bytes) {
                        (false, _) => 0.0,
                        (true, true) => SHORT_CODE_TAIL_PENALTY,
                        (true, false) => SHORT_CODE_PENALTY,
                    };
                    let log_prob = score_code_node(weight, total) - penalty;
                    if compose.is_some() {
                        units.push(Unit {
                            start: p,
                            end: q,
                            text: hit.text.clone(),
                            log_prob,
                            penalty,
                        });
                    }
                    nodes[q].push(WordNode {
                        start: p,
                        end: q,
                        word: hit.text,
                        // 码表没有音节，一个编码单元就是一个「段」，故只置起始位。
                        // 它随 Viterbi 回溯累加成整句的切分 mask，供 preedit 显示。
                        syl_mask: 1,
                        log_prob,
                        canon: None,
                    });
                }
            }
        }
        if let Some(f) = compose {
            add_composed_nodes(&mut nodes, &units, f);
        }
        nodes
    }

    /// 解码。返回 `None` 表示**没有覆盖整串的解**或解只有一个词。
    ///
    /// # 为什么必须覆盖整串
    ///
    /// [`ViterbiDecoder::decode`] 在 `dp[input_len]` 不可达时会「从最远可达位置回溯」
    /// 并返回一条**部分路径**，而 `log_prob` 字段仍是 `dp[input_len]`（此时为 `-inf`）。
    /// 判据因此是 `log_prob.is_finite()`，不是 `words.is_empty()`。
    ///
    /// 只收整串解，换来的是**整句候选恒消费整串**⇒ `consumed_length` 可以留 0，
    /// 不打破全仓「码表候选 `consumed_length` 恒 0」的约定（设计文档 §7.1 列的四处
    /// 依赖点因此一处都不用动）。分段上屏留到后续阶段。
    ///
    /// # 为什么排除单词解
    ///
    /// 对齐 librime `Poet::MakeSentenceWithStrategy` 的
    /// `if (start_pos == 0 && end_pos == total_length) continue;`——整串本身若在码表里
    /// 有词，那条候选由精确/前缀路径产出，整句不重复产，否则同一个词会以两种身份
    /// 进候选列表再被去重逻辑合并。**例外**是词典组词出的单词解，见 [`Self::accepts`]。
    pub fn decode(&self, input: &str, dm: &DictManager) -> Option<SentenceResult> {
        self.decode_nbest(input, dm, 1).into_iter().next()
    }

    /// 至多 `k` 条整句解，按名次排好、已按文本去重。`k <= 1` 等价于 [`Self::decode`]。
    ///
    /// # 挂了语法模型时怎么出备选
    ///
    /// [`ViterbiDecoder::decode_nbest`] 在有语法模型时只给一条（理由见那里：beam 按末词分组，
    /// 前几条往往只差最后一个词）。故名次 1 取**挂模型**的解，名次 2 起取**不挂模型**的
    /// N-best 补位、与名次 1 同文的跳过。实测这样拼出来的前 3 / 前 5 与纯无模型 N-best
    /// 几乎一样高，首选却是挂模型的那个（词组最短码：前 3 86.1% vs 76.1%）。
    ///
    /// # 手动分隔符
    ///
    /// 输入含分隔符时只出一条：各段独立解码，段间的 N-best 组合是乘积级的，而用户敲分隔符
    /// 本就是在消歧——此时再给备选意义不大。
    pub fn decode_nbest(&self, input: &str, dm: &DictManager, k: usize) -> Vec<SentenceResult> {
        if input.is_empty() || !input.is_ascii() {
            return Vec::new();
        }
        if input.contains(SPLIT_SEPARATOR) {
            return self.decode_with_separators(input, dm).into_iter().collect();
        }
        let k = k.max(1);
        let len = input.len();
        let nodes = self.build_nodes(input, dm);
        let mut out: Vec<SentenceResult> = Vec::new();
        let take = |r: ViterbiResult, out: &mut Vec<SentenceResult>| {
            if !self.accepts(&r, len) {
                return;
            }
            let text = r.words.concat();
            if out.iter().any(|o| o.text == text) {
                return;
            }
            out.push(SentenceResult {
                text,
                words: r.words,
                log_prob: r.log_prob,
                boundary: if len <= 64 { r.boundary | 1 } else { 0 },
            });
        };
        // 最优解若是「一个码表词条覆盖整串」，整串就是个词，不该再拿次优解冒充整句。
        let whole_word = |r: &ViterbiResult| {
            r.log_prob.is_finite() && r.words.len() == 1 && len <= self.max_code_length
        };
        if let Some(g) = self.tables.grammar_viterbi() {
            let r = g.decode(&nodes, len);
            if whole_word(&r) {
                return Vec::new();
            }
            take(r, &mut out);
        }
        if out.len() < k {
            // 语法解已占了名次 1 时多要一条：它可能与无模型的某条同文（解码器已按文本去重，
            // 但管不到语法解）。没有语法解时恰好要 k 条——k = 1 就走 Viterbi 的单路径 DP，
            // 与改动前逐位相同的那条零回归路径。
            let want = if out.is_empty() { k } else { k + 1 };
            for (i, r) in self
                .viterbi
                .decode_nbest(&nodes, len, want)
                .into_iter()
                .enumerate()
            {
                if i == 0 && out.is_empty() && whole_word(&r) {
                    return Vec::new();
                }
                take(r, &mut out);
                if out.len() >= k {
                    break;
                }
            }
        }
        out.truncate(k);
        out
    }

    /// 这条解能不能当整句：覆盖整串，且不是「一个码表词条就覆盖了整串」。
    ///
    /// 单词解只在**超码长**时放行——码表词条的码不超过满码长，超码长还能一个词覆盖整串的，
    /// 必是词典组词边（`hcigaawt` → 「皮衣工作」之外，逐字打出的「喃喃自语」）。它在精确路径
    /// 上产不出来，排除它就是把这条候选整个弄丢。
    fn accepts(&self, r: &ViterbiResult, len: usize) -> bool {
        r.log_prob.is_finite()
            && !r.words.is_empty()
            && (r.words.len() >= 2 || len > self.max_code_length)
    }

    /// 含手动分隔符的输入：各段独立解码后拼接。
    fn decode_with_separators(&self, input: &str, dm: &DictManager) -> Option<SentenceResult> {
        let mut words: Vec<String> = Vec::new();
        let mut log_prob = 0.0f64;
        let mut boundary = 0u64;
        let expressible = input.len() <= 64;

        // 手动分隔符把输入切成若干**硬边界**段，各段独立解码后拼起来。
        let mut offset = 0usize;
        // 最长一段的码长：只有一个词时据此套 `accepts` 的超码长放行（组词出的单词解）。
        let mut longest_seg = 0usize;
        for seg in input.split(SPLIT_SEPARATOR) {
            if seg.is_empty() {
                // 连续分隔符 / 首尾分隔符：跳过。用户多敲一下不该让整句失败。
                offset += 1;
                continue;
            }
            let r = self.decode_segment(seg, dm)?;
            longest_seg = longest_seg.max(seg.len());
            if expressible {
                // 段内 boundary 相对段起点，左移到全串空间；段起点本身也是一个切分点。
                boundary |= r.boundary << offset;
                boundary |= 1 << offset;
            }
            words.extend(r.words);
            log_prob += r.log_prob;
            offset += seg.len() + 1; // +1 = 被 split 吃掉的那个分隔符
        }

        // ★ **「至少两个词」是整体判据，不是段级判据**：`aawt'aawt` 每段恰好一个词，
        //   正是用户敲分隔符时最典型的形态。段级判会把它整个否掉。
        //   单词解与无分隔符时同一条规矩（见 `accepts`）：`hcigaawt'` 多敲的尾部分隔符
        //   不该让组词出的单词整句消失。
        if words.is_empty() || (words.len() < 2 && longest_seg <= self.max_code_length) {
            return None;
        }
        Some(SentenceResult {
            text: words.concat(),
            words,
            log_prob,
            boundary: if expressible { boundary } else { 0 },
        })
    }

    /// 解一个**不含分隔符**的段。返回 `None` = 没有覆盖该段的解。
    ///
    /// # 为什么判据是 `log_prob.is_finite()`
    ///
    /// [`ViterbiDecoder::decode`] 在 `dp[len]` 不可达时会「从最远可达位置回溯」并返回一条
    /// **部分路径**，而 `log_prob` 仍是 `dp[len]`（此时 `-inf`）。只看 `words.is_empty()`
    /// 会把部分解误判为有解。
    fn decode_segment(&self, seg: &str, dm: &DictManager) -> Option<ViterbiResult> {
        let nodes = self.build_nodes(seg, dm);
        let viterbi = self.tables.grammar_viterbi().unwrap_or(&self.viterbi);
        let r = viterbi.decode(&nodes, seg.len());
        r.log_prob.is_finite().then_some(r)
    }
}

/// 一条编码单元边（组词的原料）。
struct Unit {
    start: usize,
    end: usize,
    text: String,
    log_prob: f64,
    /// 这条边吃的简码罚（0 = 全码）。
    penalty: f64,
}

/// 词典组词：2~[`COMPOSE_MAX_UNITS`] 个相邻编码单元拼出的文本若是词，补一条跨这些单元的词边。
///
/// # 为什么需要
///
/// 码表整句的边只来自码表词条，码表没收的词只能逐字成边，各字按**单字**词频各算各的——
/// 解码器看不出「这几个字连起来是个词」，于是常被别的切法压过。实测（3000 句）：
///
/// | 打法 | 无组词 | 有组词 |
/// |---|---|---|
/// | 逐字最短码 | 7.7% | 70.4% |
/// | 逐字全码 | 83.5% | 98.6% |
/// | 词组最短码 | 56.8% | 75.2% |
///
/// # 怎么打分
///
/// 组出的词按它**自己的**词频打分（同一个 `score_code_node`，同一份每边罚），再扣掉各组成
/// 单元的简码罚之和：简码罚表达的是「这个字母可能本该属于别的码」，它不会因为拼出了词就消失。
///
/// 组成单元的边原样保留：组词是多给解码器一个选项，不替它做决定。
fn add_composed_nodes(nodes: &mut [Vec<WordNode>], units: &[Unit], freq: &SentenceFreq) {
    let bytes = nodes.len() - 1;
    // 每个跨度只留分最高的 COMPOSE_PER_SPAN 条，按起点分桶。
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by(|&a, &b| units[b].log_prob.total_cmp(&units[a].log_prob));
    let mut span_taken: HashMap<(usize, usize), usize> = HashMap::new();
    let mut by_start: Vec<Vec<usize>> = vec![Vec::new(); bytes + 1];
    for i in order {
        let n = span_taken
            .entry((units[i].start, units[i].end))
            .or_insert(0);
        if *n < COMPOSE_PER_SPAN {
            *n += 1;
            by_start[units[i].start].push(i);
        }
    }

    struct Partial {
        end: usize,
        text: String,
        penalty: f64,
        units: usize,
        /// 各单元起点相对组词起点的 bitmask（组合区按单元切分显示用）。
        mask: u64,
    }
    let mut stack: Vec<Partial> = Vec::new();
    for start in 0..bytes {
        stack.extend(by_start[start].iter().map(|&i| Partial {
            end: units[i].end,
            text: units[i].text.clone(),
            penalty: units[i].penalty,
            units: 1,
            mask: 1,
        }));
        while let Some(p) = stack.pop() {
            if p.units >= 2
                && let Some(f) = freq.compose_freq(&p.text)
            {
                let log_prob = score_code_node(f, PINYIN_DICT_TOTAL) - p.penalty;
                let node = WordNode {
                    start,
                    end: p.end,
                    word: p.text.clone(),
                    syl_mask: p.mask,
                    log_prob,
                    canon: None,
                };
                // 同跨度同文可由不同单元组合拼出（`甲|乙丙` 与 `甲乙|丙`），简码罚与切法都可能
                // 不同：留分最高的那条。码表词条本身的边同样参与比较。
                match nodes[p.end]
                    .iter_mut()
                    .find(|w| w.start == start && w.word == p.text)
                {
                    Some(w) if w.log_prob < log_prob => *w = node,
                    Some(_) => {}
                    None => nodes[p.end].push(node),
                }
            }
            if p.units >= COMPOSE_MAX_UNITS
                || p.text.chars().count() >= COMPOSE_MAX_CHARS
                || p.end >= bytes
            {
                continue;
            }
            let rel = p.end - start;
            for &i in &by_start[p.end] {
                let u = &units[i];
                let text = format!("{}{}", p.text, u.text);
                if !freq.compose.has_prefix(&text) {
                    continue;
                }
                stack.push(Partial {
                    end: u.end,
                    text,
                    penalty: p.penalty + u.penalty,
                    units: p.units + 1,
                    mask: if rel < 64 {
                        p.mask | (1 << rel)
                    } else {
                        p.mask
                    },
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_candidate::Candidate;
    use wind_dict::layer::{DictLayer, LayerType};

    /// 最小内存词库层：直接持 `(code, text, weight)` 列表。
    ///
    /// 不用真实 wdat：本模块的判据全在「码长关系 + 权重量纲」上，
    /// 手写词条才能把一简/二简/全码的**结构**摆到台面上，
    /// 真实词库反而会把判据淹没在 8.8 万条噪声里（且 worktree 未必有 build_dev/data）。
    struct MemLayer(Vec<(String, String, i32)>);

    impl MemLayer {
        fn new(entries: &[(&str, &str, i32)]) -> Self {
            Self(
                entries
                    .iter()
                    .map(|(c, t, w)| (c.to_string(), t.to_string(), *w))
                    .collect(),
            )
        }
    }

    impl DictLayer for MemLayer {
        fn name(&self) -> &str {
            "mem"
        }
        fn layer_type(&self) -> LayerType {
            LayerType::System
        }
        fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
            let mut v: Vec<Candidate> = self
                .0
                .iter()
                .filter(|(c, _, _)| c == code)
                .map(|(c, t, w)| Candidate {
                    text: t.clone(),
                    code: c.clone(),
                    weight: *w,
                    ..Default::default()
                })
                .collect();
            v.sort_by_key(|c| std::cmp::Reverse(c.weight));
            v.truncate(limit);
            v
        }
        fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
            let mut v: Vec<Candidate> = self
                .0
                .iter()
                .filter(|(c, _, _)| c.starts_with(prefix) && c != prefix)
                .map(|(c, t, w)| Candidate {
                    text: t.clone(),
                    code: c.clone(),
                    weight: *w,
                    ..Default::default()
                })
                .collect();
            v.sort_by_key(|c| std::cmp::Reverse(c.weight));
            v.truncate(limit);
            v
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            for (c, t, w) in &self.0 {
                f(c, t, *w);
            }
        }
    }

    /// 五笔结构的缩微模型：一简 / 二简 / 三简 / 3 码全码 / 4 码全码 / 4 码词组俱全，
    /// 且**权重照抄极点词库的层级带**（简码 9999/9950/9000，全码带百位）。
    fn dm() -> DictManager {
        let dm = DictManager::new();
        dm.register_layer(Box::new(MemLayer::new(&[
            // 一简（皆为简码：各自都有更长的码）
            ("g", "一", 9999),
            ("a", "工", 9999),
            ("d", "在", 9999),
            ("w", "人", 9999),
            // 二简（简码：各自都配了 4 码全码，否则它们自己就是全码——
            //        这正是「二简 655 条里有 1 条其实是全码」的那种情形）
            ("aa", "式", 9950),
            ("wt", "何", 9950),
            // 三简（简码：有 4 码全码）
            ("aaa", "工", 9000),
            // 3 码全码（无更长码 —— 「整句只认 4 码」走不通的那 1604 条的代表）
            ("hci", "皮", 1200),
            // 4 码全码单字
            ("aaaa", "工", 800),
            ("ggll", "一", 700),
            ("dhfh", "在", 650),
            ("wtgf", "人", 600),
            ("aagg", "式", 500),
            ("wtwt", "何", 450),
            // 4 码词组
            ("aawt", "工作", 1241),
            ("dhwt", "在人", 90),
            ("hcig", "皮衣", 400),
        ])));
        dm
    }

    /// 分隔符的字符串形式（测试里拼串用）。
    const SEP: &str = "'";

    fn decoder() -> CodeSentenceDecoder {
        CodeSentenceDecoder::new(4)
    }

    #[test]
    fn short_code_index_marks_only_short_entries() {
        let dm = dm();
        let idx = ShortCodeIndex::build(&dm);

        // 一简「工 a」是简码，整句权重换成它 4 码全码 aaaa 的 800。
        assert_eq!(idx.resolve("a", "工", 9999), (800, true));
        // 三简「工 aaa」同样是简码。
        assert_eq!(idx.resolve("aaa", "工", 9000), (800, true));
        // 4 码全码不是简码，权重原样。
        assert_eq!(idx.resolve("aaaa", "工", 800), (800, false));
        // ★ 3 码全码「皮 hci」**不是**简码——它没有更长的码。
        //   这条是「整句只认 4 码单元」那条捷径走不通的判据现场。
        assert_eq!(idx.resolve("hci", "皮", 1200), (1200, false));
        // 词组不是简码。
        assert_eq!(idx.resolve("aawt", "工作", 1241), (1241, false));

        // 二简也要被认出来（它们各自配了 4 码全码）。
        assert_eq!(idx.resolve("aa", "式", 9950), (500, true));

        // 进表的恰是简码那几条：a/g/d/w 四个一简 + aa/wt 两个二简 + aaa 一个三简。
        //
        // ⚠️ 这个数字是**夹具结构的函数**，不是随手写的常量：改夹具必须回头核对。
        // 首次写成 7 时夹具里的二简还没配全码 —— 它们那时其实是「二简位上的全码字」，
        // 索引不收是对的，错的是断言。数字对不上时先问夹具对不对。
        assert_eq!(idx.len(), 7, "简码条目数");
    }

    #[test]
    fn sentence_prefers_word_over_short_code_chain() {
        // `aawt` 整串就是「工作」一个词 ⇒ 按约定不产整句（交给精确匹配路径）。
        // 这里验证的是**更长的串**里，词边能赢过简码链：
        // `aawtaawt` 应解成「工作工作」而不是「工工何…」之类的简码碎片。
        let dm = dm();
        let r = decoder().decode("aawtaawt", &dm).expect("应有整串解");
        assert_eq!(r.text, "工作工作");
        assert_eq!(r.words, vec!["工作", "工作"]);
    }

    #[test]
    fn sentence_uses_three_code_full_entry() {
        // 3 码全码「皮」必须能参与整句：`hciaawt` → 皮 + 工作。
        // 若把「不足 4 码一律当简码抑制」，这里会解不出或解错。
        let dm = dm();
        let r = decoder().decode("hciaawt", &dm).expect("应有整串解");
        assert_eq!(r.text, "皮工作");
    }

    #[test]
    fn single_word_solution_is_rejected() {
        // 整串恰是一个词 ⇒ 不产整句（对齐 librime 排除「单个词覆盖全串」）。
        let dm = dm();
        assert!(decoder().decode("aawt", &dm).is_none());
    }

    #[test]
    fn unreachable_input_yields_none() {
        // 尾部 `z` 无任何词条 ⇒ 没有覆盖整串的路径 ⇒ 不产整句。
        //
        // ⚠️ 这条测的是 `log_prob.is_finite()` 而非 `words.is_empty()`：
        // decode 在 dp[len] 不可达时会「从最远可达位置回溯」并返回一条**部分路径**，
        // 只看 words 非空会误判为有解。
        let dm = dm();
        assert!(decoder().decode("aawtz", &dm).is_none());
    }

    #[test]
    fn short_code_still_usable_when_no_full_code_path() {
        // 简码不是被禁掉、只是被罚：没有别的解法时它照样成句。
        // `ghci` = 一(g，一简) + 皮(hci，3 码全码)，`ghci` 整串与 `ghc` 都无词条。
        let dm = dm();
        let r = decoder().decode("ghci", &dm).expect("应有整串解");
        assert_eq!(r.text, "一皮");
    }

    #[test]
    fn tail_short_code_is_penalized_less_than_middle() {
        // 同一条简码边，串尾比串中便宜——这是「用户还没打完」的表达。
        // 用打分函数直接对账，避免依赖某个具体输入串的解码结果。
        let mid = score_code_node(700, CODE_DICT_TOTAL) - SHORT_CODE_PENALTY;
        let tail = score_code_node(700, CODE_DICT_TOTAL) - SHORT_CODE_TAIL_PENALTY;
        assert!(tail > mid, "串尾简码应罚得更轻");
    }

    #[test]
    fn same_span_prefers_frequency_not_word_length() {
        // ★ 同跨度下**纯按词频**，不因「是词组」而偏好 —— 这条锁住的是实测推翻拼音
        //   多字词加成的那次改动（见 `score_code_node` 文档 ①）。
        //
        //   现场就是真实词库里的 `ggll`：单字「一」与四字词「来来回回」同码同跨度，
        //   「一」词频更高。加回 `+3.0×√字数` 的加成后「来来回回」会赢，本测试会红。
        let dm = DictManager::new();
        dm.register_layer(Box::new(MemLayer::new(&[
            ("g", "一", 9999),
            ("ggll", "一", 700),
            ("ggll", "来来回回", 600),
            ("hci", "皮", 1200),
        ])));
        let r = decoder().decode("ggllhci", &dm).expect("应有整串解");
        assert_eq!(
            r.words,
            vec!["一", "皮"],
            "同跨度应按词频选「一」，不得因字数偏好「来来回回」"
        );
    }

    #[test]
    fn pinyin_freq_overrides_codetable_weight_bands() {
        // ★★ 词频来源的核心行为：接上拼音词库后，**完全不看码表 weight**。
        //
        //   夹具让同一条边（`wh`）上两个字对抗，两套权重给出**相反**的结论：
        //   - 码表 weight：「丁」9950（简码带）vs「个」100（普通带）⇒ 丁赢
        //   - 真实词频：  「个」215733       vs「丁」1000        ⇒ 个赢
        //
        //   必须同码同跨度才构成竞争 —— 首版拿「个 wh」对「修 wht」，跨度不同、
        //   压根不在一条边上，测的其实是「哪条路径走得通」。
        let entries: &[(&str, &str, i32)] =
            &[("wh", "丁", 9950), ("wh", "个", 100), ("hci", "皮", 1200)];
        let dm = DictManager::new();
        dm.register_layer(Box::new(MemLayer::new(entries)));

        // 「拼音词库」：只需是一份带真实词频的 (code,text,weight) 表，码是什么无所谓
        //  —— `SentenceFreq` 按 **text** 查，不碰码。
        let py = || {
            CachedDict::Memory({
                let mut d = wind_dict::codetable::CodetableDict::empty();
                d.merge_single("ge".into(), "个".into(), 215_733, 0);
                d.merge_single("ding".into(), "丁".into(), 1_000, 1);
                d.merge_single("pi".into(), "皮".into(), 5_000, 2);
                d
            })
        };

        let with_freq = decoder().with_pinyin_dict(std::sync::Arc::new(py()));
        let r = with_freq.decode("whhci", &dm).expect("应有整串解");
        assert_eq!(r.words, vec!["个", "皮"], "接上真实词频后应按词频选「个」");

        // 对照：退化路径（无拼音词库）被权重带带偏，选「丁」。
        // 这条**不是**在锁定错误行为，而是标明「没有词频来源时准确率确实会掉」，
        // 免得日后有人以为退化路径同样可靠。
        let r2 = decoder().decode("whhci", &dm).expect("应有整串解");
        assert_eq!(
            r2.words,
            vec!["丁", "皮"],
            "退化路径按码表权重带走 —— 这是已知的准确率损失"
        );
    }

    #[test]
    fn sentence_freq_falls_back_to_oov_for_missing_text() {
        // 拼音库查不到的字给 OOV_FREQ（=罕见），不是中性值：码表收了大量生僻字，
        // 按中位数计价会让它们在整句里与常用字平起平坐。
        let dm = DictManager::new();
        dm.register_layer(Box::new(MemLayer::new(&[("qq", "兲", 9950)])));
        let py = CachedDict::Memory({
            let mut d = wind_dict::codetable::CodetableDict::empty();
            d.merge_single("de".into(), "的".into(), 1_000_000, 0);
            d
        });
        let f = SentenceFreq::build(&dm, &py);
        assert_eq!(f.get("兲"), OOV_FREQ, "码表有、拼音库无 ⇒ 兜底为罕见");
        // ★ 只留交集：拼音库里的「的」不在码表里，不该进表（内存 20MB→3MB 的由来）。
        assert_eq!(f.len(), 0, "只保留码表里出现过的 text");
    }

    #[test]
    fn split_code_marks_engine_chosen_boundaries() {
        // 切分串必须来自 Viterbi **实际走的**那条路径（boundary），不是按词长重猜——
        // 同一串码可有多种切法，谎报切分正是拼音侧记过一次的教训。
        let dm = dm();
        let r = decoder().decode("aawtaawt", &dm).expect("应有整串解");
        assert_eq!(r.split_code("aawtaawt"), "aawt@aawt".replace('@', SEP));
    }

    #[test]
    fn manual_separator_splits_into_segments() {
        // ★ 手动分隔符：每段独立解码。`aa` + `wt` 各是一个二简字，
        //   **段内各只有一个词** —— 首版把「至少两个词」判在段级，这种最典型的
        //   分隔符用法会被整个否掉。判据必须是整体词数。
        let dm = dm();
        let input = format!("aa{SEP}wt");
        let r = decoder().decode(&input, &dm).expect("分段应能解出");
        assert_eq!(r.words, vec!["式", "何"]);
        // 用户自己敲的分隔符原样留下，不再叠一个。
        assert_eq!(r.split_code(&input), input);
    }

    #[test]
    fn manual_separator_overrides_greedy_split() {
        // 分隔符的**用途**：把本该被贪心切走的串掰开。
        // `aawt` 整串是词「工作」，加了分隔符后强制读成两个二简字。
        let dm = dm();
        let joined = decoder().decode("aawtaawt", &dm).expect("应有解");
        assert_eq!(joined.words, vec!["工作", "工作"]);

        let input = format!("aa{SEP}wt{SEP}aa{SEP}wt");
        let split = decoder().decode(&input, &dm).expect("应有解");
        assert_eq!(
            split.words,
            vec!["式", "何", "式", "何"],
            "分隔符是硬边界，不得跨段成词"
        );
    }

    #[test]
    fn redundant_separators_are_tolerated() {
        // 首尾/连续分隔符是用户多敲的，不该让整句失败。
        let dm = dm();
        let input = format!("{SEP}aa{SEP}{SEP}wt{SEP}");
        let r = decoder().decode(&input, &dm).expect("冗余分隔符应被容忍");
        assert_eq!(r.words, vec!["式", "何"]);
    }

    #[test]
    fn separator_segment_without_solution_fails_whole() {
        // 某一段解不出 ⇒ 整体无解。分隔符是用户的明确表态，不能悄悄跨段重切。
        let dm = dm();
        let input = format!("aa{SEP}zz");
        assert!(decoder().decode(&input, &dm).is_none());
    }

    /// 拼音词库替身：`(text, 词频)`，码无所谓（`SentenceFreq` 按 text 查）。
    fn pinyin_dict(entries: &[(&str, i32)]) -> std::sync::Arc<CachedDict> {
        let mut d = wind_dict::codetable::CodetableDict::empty();
        for (i, (t, w)) in entries.iter().enumerate() {
            d.merge_single(format!("py{i}"), t.to_string(), *w, i as _);
        }
        std::sync::Arc::new(CachedDict::Memory(d))
    }

    /// 组词夹具：五个单字，全是全码（都没有更长的码）。
    ///
    /// `aabbcc` 有两种切法：`甲|乙|丙`（3 条边）与 `丁|戊`（2 条边）。单字词频相同，
    /// 每边罚让 2 条边的那种赢——除非解码器知道「甲乙」是个词。
    fn compose_dm() -> DictManager {
        let dm = DictManager::new();
        dm.register_layer(Box::new(MemLayer::new(&[
            ("aa", "甲", 500),
            ("bb", "乙", 500),
            ("cc", "丙", 500),
            ("aab", "丁", 500),
            ("bcc", "戊", 500),
        ])));
        dm
    }

    const CHARS: [(&str, i32); 5] = [
        ("甲", 10_000),
        ("乙", 10_000),
        ("丙", 10_000),
        ("丁", 10_000),
        ("戊", 10_000),
    ];

    #[test]
    fn composes_lexicon_word_across_char_units() {
        // ★ 词典组词的核心行为：码表里没有「甲乙」，逐字打出来也要认得它是个词。
        let dm = compose_dm();
        let with_word = [&CHARS[..], &[("甲乙", 500_000)]].concat();
        let r = decoder()
            .with_pinyin_dict(pinyin_dict(&with_word))
            .decode("aabbcc", &dm)
            .expect("应有整串解");
        assert_eq!(r.words, vec!["甲乙", "丙"]);
        // 组出的词在组合区仍按**编码单元**切分：用户打的是三个码，不是两个。
        assert_eq!(r.split_code("aabbcc"), "aa@bb@cc".replace('@', SEP));

        // 对照：拼音库里没有这个词 ⇒ 不组词，每边罚让 2 条边的切法赢。
        // 没有这条，「无条件把相邻单字拼起来」的实现同样能让上面变绿。
        let r = decoder()
            .with_pinyin_dict(pinyin_dict(&CHARS))
            .decode("aabbcc", &dm)
            .expect("应有整串解");
        assert_eq!(r.words, vec!["丁", "戊"]);
    }

    #[test]
    fn composed_word_may_cover_whole_input() {
        // 超码长时一个词覆盖整串也要放行：它只能是组词边，精确路径产不出来，
        // 照「排除单词解」的老规矩就是把这条候选整个弄丢（逐字打「喃喃自语」的实测现场）。
        let dm = compose_dm();
        let with_word = [&CHARS[..], &[("甲乙丙", 500_000)]].concat();
        let r = decoder()
            .with_pinyin_dict(pinyin_dict(&with_word))
            .decode("aabbcc", &dm)
            .expect("应有整串解");
        assert_eq!(r.words, vec!["甲乙丙"]);
    }

    #[test]
    fn trailing_separator_keeps_composed_single_word() {
        // 无分隔符能出的组词单词解，多敲一个尾部分隔符（冗余，本该被容忍）不能让它消失。
        let dm = compose_dm();
        let with_word = [&CHARS[..], &[("甲乙丙", 500_000)]].concat();
        let dec = decoder().with_pinyin_dict(pinyin_dict(&with_word));
        let input = format!("aabbcc{SEP}");
        let r = dec.decode(&input, &dm).expect("尾部分隔符不应让整句消失");
        assert_eq!(r.words, vec!["甲乙丙"]);
        // 对照：码长内的单词解仍按老规矩排除（那是码表词条，精确路径会出）。
        assert!(dec.decode(&format!("aawt{SEP}"), &self::dm()).is_none());
    }

    #[test]
    fn missing_grammar_file_falls_back_to_plain_decoding() {
        // 定制版可能删掉模型文件：打不开就按无模型解码，结果与不挂模型逐条相同。
        let dm = dm();
        let plain = decoder().decode_nbest("aawtaawt", &dm, 3);
        let missing = decoder()
            .with_grammar_file("/nonexistent/wanxiang-mini.gram".into(), 0.5)
            .decode_nbest("aawtaawt", &dm, 3);
        let texts = |v: &[SentenceResult]| v.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&missing), texts(&plain));
    }

    /// 只出用户词的层：candidate 带 `is_user_dict`，与生产 `StoreUserLayer` 经 composite 后的形态一致。
    struct UserLayer(MemLayer);

    impl DictLayer for UserLayer {
        fn name(&self) -> &str {
            "user"
        }
        fn layer_type(&self) -> LayerType {
            LayerType::User
        }
        fn search(&self, code: &str, limit: usize) -> Vec<Candidate> {
            let mut v = self.0.search(code, limit);
            for c in &mut v {
                c.meta.is_user_dict = true;
            }
            v
        }
        fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<Candidate> {
            self.0.search_prefix(prefix, limit)
        }
        fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
            self.0.for_each_entry(f)
        }
    }

    #[test]
    fn user_word_missing_from_lexicon_gets_floor_frequency() {
        // 用户造的词（人名、新词）多半不在拼音库里。按「罕见」计价，它在整句里永远输给
        // 逐字拆开；给它组词表的入表门槛作下限，才能赢回来。
        //
        // 竞争者：`xxyy` 也能切成两个高频字「子」+「丑」。
        let entries: &[(&str, &str, i32)] =
            &[("xx", "子", 500), ("yy", "丑", 500), ("zz", "寅", 500)];
        let py = pinyin_dict(&[
            ("子", 100_000),
            ("丑", 100_000),
            ("寅", 100_000),
            // 组词表里唯一的词 ⇒ 入表门槛 = 它的词频。
            ("别的词", 50_000),
        ]);
        let run = |user: bool| {
            let dm = DictManager::new();
            dm.register_layer(Box::new(MemLayer::new(entries)));
            let word = MemLayer::new(&[("xxyy", "盖伦", 100)]);
            if user {
                dm.register_layer(Box::new(UserLayer(word)));
            } else {
                dm.register_layer(Box::new(word));
            }
            decoder()
                .with_pinyin_dict(py.clone())
                .decode("xxyyzz", &dm)
                .expect("应有整串解")
                .words
        };
        assert_eq!(run(true), vec!["盖伦", "寅"], "用户词应拿到词频下限");
        // 对照：同一条词条不是用户词 ⇒ 仍按罕见计价，输给逐字拆开。
        assert_eq!(run(false), vec!["子", "丑", "寅"]);
    }

    #[test]
    fn nbest_returns_distinct_ranked_alternatives() {
        let dm = dm();
        let rs = decoder().decode_nbest("aawtaawt", &dm, 3);
        assert!(rs.len() >= 2, "应有备选，实得 {}", rs.len());
        assert_eq!(rs[0].text, "工作工作", "名次 1 与单条解码一致");
        let mut texts: Vec<&str> = rs.iter().map(|r| r.text.as_str()).collect();
        texts.dedup();
        assert_eq!(texts.len(), rs.len(), "按文本去重");
        assert!(
            rs.windows(2).all(|w| w[0].log_prob >= w[1].log_prob),
            "按分数降序"
        );
        assert!(decoder().decode_nbest("aawtaawt", &dm, 1).len() == 1);
    }

    /// 把「工作」罚到出局的语法模型。
    struct HateGongzuo;

    impl Grammar for HateGongzuo {
        fn query(&self, _context: &str, word: &str, _is_rear: bool) -> f64 {
            if word == "工作" { -100.0 } else { 0.0 }
        }
    }

    #[test]
    fn grammar_picks_rank_one_and_plain_nbest_fills_the_rest() {
        // 挂模型时 Viterbi 的 N-best 只给一条，备选由无模型解码补位。
        let dm = dm();
        let plain = decoder().decode("aawtaawt", &dm).expect("应有解").text;
        assert_eq!(plain, "工作工作");
        let rs = decoder()
            .with_grammar(std::sync::Arc::new(HateGongzuo))
            .decode_nbest("aawtaawt", &dm, 3);
        assert_ne!(rs[0].text, plain, "名次 1 应来自挂模型的解码");
        assert_eq!(
            rs.get(1).map(|r| r.text.as_str()),
            Some(plain.as_str()),
            "名次 2 起是无模型的最优解"
        );
    }

    #[test]
    fn empty_and_non_ascii_input_is_rejected() {
        let dm = dm();
        assert!(decoder().decode("", &dm).is_none());
        assert!(decoder().decode("工作", &dm).is_none());
    }
}
