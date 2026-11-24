//! 码表整句：把一长串码切成多个编码单元并组句。
//!
//! 设计文档：`docs/design/codetable-sentence-input.md`（取值依据见 §11、§12）。
//!
//! 解码器完全复用 [`ViterbiDecoder`]：它收的是按结束位置分桶的字节跨度节点（[`WordNode`]），
//! 不含拼音语义。不复用的是切分图（码表任何位置切开都合法，直接枚举 `q - p ∈ [1, max_code_length]`
//! 的跨度，剪枝交给词典命中与简码策略）和打分函数（见 [`score_code_node`]）。
//!
//! 码表词库的 `weight` 是按码长发的层级带而不是词频，直接当整句分会让任何输入都被切成一串
//! 简码字。故整句词频优先取拼音词库（[`SentenceFreq`]），退路是 [`ShortCodeIndex`]。

use crate::pinyin::grammar::Grammar;
use crate::pinyin::viterbi::{ViterbiDecoder, ViterbiResult, WordNode};
use std::collections::HashMap;
use std::sync::Arc;
use wind_candidate::Candidate;
use wind_dict::DictManager;
use wind_dict::cached::CachedDict;

/// 词频归一化基准，是标定系数而非精确的词库总权重（同 `pinyin::lattice::DICT_TOTAL` 的性质）。
///
/// 取值为极点五笔 4 码带条数 × 均值。它与 [`WORD_PENALTY`] 共同构成「每词固定罚」这一个旋钮
/// （`Σ ln(f/T) = Σ ln(f) − n·ln T`），换词库的漂移只等价于该罚值微调。与拼音侧 `DICT_TOTAL`
/// 不同域，不可互换。
const CODE_DICT_TOTAL: f64 = 70_000_000.0;

/// 走拼音词频时的归一化基准，取拼音侧 `pinyin::lattice::DICT_TOTAL` 的同一个值。
///
/// 两个 TOTAL 必须分开：`weight_of` 的两条来源量纲不同，同一基准套两套量纲等于给其中一条按
/// 边数加罚，影响的是「切几段」的判断。
const PINYIN_DICT_TOTAL: f64 = 242_154_693.0;

/// 每条边的固定惩罚。语义与量纲同 `pinyin::lattice::WORD_PENALTY`：「每节点减 W」等价于
/// 「按边数罚 k·W」，把一个词打碎成几个高频字不再免费。这是压制简码切碎的主力，
/// 而非 [`SHORT_CODE_PENALTY`]。
const WORD_PENALTY: f64 = 3.0;

/// 「这个权重是不是真实词频」的分界线。
///
/// 极点五笔词库按码长发权重带：9999 / 9950 / 9000 是一简 / 二简 / 三简，8000–8999 是
/// `protected_codes` 保护带，只有 120–7999 是真实词频折算的。分界即 `gen_dict` 普通带的上沿。
///
/// [`ShortCodeIndex`] 换来的全码位置权重也可能是带（「八」最长码 3 码拿 9000，「人」最长码
/// 四叠拿 8010），不挡就会 `khlgw` 解成「中国八」。落回中性值只是止血，根治见设计文档 §6.2。
const SENTENCE_TRUSTED_WEIGHT_MAX: i32 = 7999;

/// 带内权重的中性替身：普通带（120–7999）的实测均值，落在中间才不会系统性压过或压不过真实词频。
const NEUTRAL_WEIGHT: i32 = 851;

/// 把「带」换成中性值，普通带原样放行。见 [`SENTENCE_TRUSTED_WEIGHT_MAX`]。
fn trusted_weight(raw: i32) -> i32 {
    if raw > SENTENCE_TRUSTED_WEIGHT_MAX {
        NEUTRAL_WEIGHT
    } else {
        raw
    }
}

// 这里刻意没有单字惩罚 / 多字词加成（拼音侧的 `SINGLE_CHAR_PENALTY` / `BASE_CONTENT_WORD_BONUS`），
// 理由见 `score_code_node`。

/// 简码边的对数惩罚（串中位置）。
///
/// 冷热已由整句权重表达，本罚针对结构性不确定：简码在连打中间出现，更可能是切分算法自作多情。
/// 2.0 是初值，待按设计文档 §8 用真实句子码串扫参标定。多数「切碎」问题其实由 [`WORD_PENALTY`]
/// 决定，本罚只是精调。
const SHORT_CODE_PENALTY: f64 = 2.0;

/// 简码边的对数惩罚（串尾位置）。串尾恒是还没打完的编码，长度不足是常态，故轻罚 `ln 2`，
/// 与拼音侧 `PARTIAL_FINAL_PENALTY` 同量级、同理由。
const SHORT_CODE_TAIL_PENALTY: f64 = std::f64::consts::LN_2;

/// 每条边保留几条同码词条（对应 librime `table_translator` 的 `max_homographs`）。
///
/// 取 8 而不是 librime 的 1~2：`dm.search(code, N)` 按码表 weight（层级带）取前 N，而打分用真实
/// 词频。筛选与排序不在同一个量纲上时筛选口必须放宽，否则真实高频的那条在筛选时就丢了。
const MAX_HOMOGRAPHS: usize = 8;

/// 词典组词表收几个词：拼音词库里按词频取前 N 个多字词。
///
/// 组词解决「这个词码表里没有，只能逐字打」。20 万之后边际收益不到 1 个点、内存却翻三倍，
/// 扫参表见设计文档 §12.4。
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
/// 与拼音的 `schema.pinyin.grammar.model` 不共用配置：拼音桌面端出厂不挂，码表整句挂上收益大得多。
pub const GRAMMAR_MODEL: &str = "wanxiang-mini.gram";

/// 整句候选的权重基数。只需高于码表任何词条权重（上限 9999），且远低于拼音侧
/// `SENTENCE_WEIGHT_BASE`(3e7)，两者不在同一列表里比较。
///
/// 顶部锚定不靠这个数值，靠 `Candidate::is_sentence` 标记；这里只表达引擎内部排序时排前面。
pub const SENTENCE_WEIGHT_BASE: i32 = 1_000_000;

/// 整句词频表：`text → 真实词频`，取自拼音词库。
///
/// 码表 weight 是层级带，连 [`ShortCodeIndex`] 换来的全码位置权重也可能是带（3 码全码字拿
/// 9000），会让低频字压过极高频字。拼音词库存的是真实语料词频，且与码表共享「文本」这个键。
///
/// 查不到即「罕见」，兜底 [`OOV_FREQ`] 而不是中性值。构建时只留与码表 text 的交集，免得
/// 60 万条全进内存。覆盖率数据见设计文档 §11.9。
#[derive(Debug, Default)]
pub struct SentenceFreq {
    freq: HashMap<String, i32>,
    /// 词典组词表，见 [`COMPOSE_LEXICON_SIZE`]。
    compose: ComposeLexicon,
    /// 用户词 / 临时词查不到词频时的下限，见 [`Self::user_floor`]。
    user_floor: i32,
}

/// 紧凑的 `text → 词频` 表：所有文本拼进一个 `String`，索引按文本排序后二分查找。
/// 比 `HashMap<String, i32>` 省掉每条一次堆分配；表建好后只读。
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

/// 拼音库查不到的 text 的兜底词频。取 1 而非中性值：码表收的生僻字在真实语料里本就罕见，
/// `ln(w/T)` 得到很低的分正是想要的。
const OOV_FREQ: i32 = 1;

impl SentenceFreq {
    /// 从拼音词库构建，只保留 `codetable` 里出现过的 text。
    ///
    /// ⚠️ O(码表 + 拼音库) 的全表扫描（合计约 70 万条），只在首次整句解码时一次。
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
    /// 两遍扫描（先找门槛、再收文本），免得把 60 万条文本全装进内存排序。码表收了的多字词无论
    /// 排第几都一并收进来：组词表要能回答「是不是某个词的前缀」（剪枝用），词不能分散在两张表里。
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
    /// 这类词多是用户特意造的人名、新词，按 [`OOV_FREQ`] 计价会几乎永远输给逐字拆开。门槛的含义
    /// 是「至少和组词表里最冷的那个词一样常用」，再往上就是在替用户猜词频了。
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
/// ```text
/// is_short_code(code, text) := len(code) < max{ len(c) : c 是 text 的任一编码 }
/// ```
///
/// 五笔的简码就是全码的前 N 码，故不需要 text→code 反查索引。三码条目里有相当一部分是全码
/// （「皮 hci」这类字根不足 4 个的字），必须照常参与整句，「整句只认 4 码单元」走不通。
#[derive(Debug, Default)]
pub struct ShortCodeIndex {
    /// `(code, text)` → 该 text 全码条目的权重。只收录简码条目，
    /// 故查不到即「不是简码」，无需再存一份布尔。
    short: HashMap<(String, String), i32>,
}

impl ShortCodeIndex {
    /// 扫全表构建，O(全表 × 2 遍)，只在首次整句解码时调用一次（`OnceLock` 守着）。
    /// 两遍而非一遍：一遍需要把全部条目缓存下来才能回头判「谁比谁长」，那比再遍历一次贵。
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

    /// 该条目的整句权重与「是不是简码」。非简码走 `raw`（全码带本就是词频折算的量纲）；
    /// 少数 `protected_codes` 保护带条目是键位约定而非词频，留作已知偏差。
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

/// 节点对数概率打分：纯词频 + 每边固定罚，没有任何按词性/字数的调整。
///
/// 拼音侧 `score_node` 的三项调整都不适用，照搬会出错：
/// - 多字词加成补偿的是「n 字词占 n 个音节、拆开要多扣 n 次 `ln(TOTAL)`」，码表 4 码词组与
///   4 码单字占同一条边，加成退化成白送（`ggll` 上「一」输给「来来回回」）；
/// - 单字惩罚同理，跨度不同的比较已由 [`WORD_PENALTY`] 承担；
/// - 单字虚词优待会把五笔一简里「是 / 在 / 不 / 了…」这批最危险的简码字整体提权。
///
/// 同一条加成在两套编码结构里前提不同，与拼音侧 `score_node_partial_final` 同一条判据。
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
    /// 数据源是 [`Self::boundary`]（Viterbi 回溯的实际路径），不能按词长重猜：同一串码可有多种
    /// 切法，只有解码器知道用的是哪条（同拼音侧 `ViterbiResult::boundary`）。
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

/// 码表整句解码器：持简码索引 + Viterbi。两张表懒构建（`OnceLock`），只有用到整句的方案才付
/// 代价；在哪条线程上付见 [`LazyTables`]。
pub struct CodeSentenceDecoder {
    max_code_length: usize,
    /// 两张懒表连同它们的来源。单独装在 `Arc` 里，为的是 [`Self::prewarm`] 能把它
    /// 整份交给后台线程去填——解码器本身挂在引擎上，不便跨线程共享。
    tables: std::sync::Arc<LazyTables>,
    viterbi: ViterbiDecoder,
}

/// 首次整句解码要付的两笔一次性开销（码表全表扫描建简码索引、打开拼音 merged.wdat 全表扫建
/// 词频），合计数百毫秒，其后每次解码不到 0.2 ms。
///
/// 懒加载会把这笔开销挪到用户敲下超码长那一键上，故 [`CodeSentenceDecoder::prewarm`] 在引擎构建
/// 完成时把两张表推给后台线程。预热不改变任何取值，`OnceLock::get_or_init` 保证两条线程拿到
/// 同一份；预热未完成时按键线程仍会等，与不预热持平。实测见设计文档 §11.14。
struct LazyTables {
    index: std::sync::OnceLock<ShortCodeIndex>,
    /// 语法模型来源。`None` = 不挂（权重为 0 或未配）。
    grammar_source: Option<GrammarSource>,
    /// 挂了语法模型的解码器。内层 `None` 表示「试过了但没成」（模型文件缺失等），同 `freq`。
    grammar: std::sync::OnceLock<Option<ViterbiDecoder>>,
    /// 整句词频的来源。`None` = 没配 ⇒ 退回码表自身权重那套
    /// （见 [`CodeSentenceDecoder::weight_of`]），功能仍可用、只是没那么准。
    freq_source: Option<FreqSource>,
    /// 词频表。内层 `Option` 表示「试过了但没成」——加载失败要记住，
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
/// - [`Self::SchemasDir`]（生产路径）：只存目录，词库到首次整句解码时才加载。这一层懒不能省：
///   混输方案的主引擎也走码表分支构建，但它的整句永远不会被调用到，构建期加载就是白读，
///   没用过拼音方案的用户还要现场构建 `merged.wdat`。
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

/// 词库词条在整句 / 逆切分里的文本。`None` = 不进（特殊语法）；`Some(None)` = 原文；
/// `Some(Some(t))` = 纯文本直通，用上屏文本 `t`。两处共用，判据不能漂移。
pub(crate) fn sentence_text(text: &str) -> Option<Option<String>> {
    if !text.contains('$') && !text.contains('{') {
        return Some(None);
    }
    wind_cmdbar::plain_type_text(text).map(Some)
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

    /// 注入数据目录作为词频来源：词库到预热线程（或首次整句解码）时才读。生产路径走这个。
    pub fn with_schemas_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.tables_mut().freq_source = Some(FreqSource::SchemasDir(dir));
        self
    }

    /// 注入已加载的拼音词库作为词频来源。测试与探针走这个。
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
    /// 由 `CodeTableEngine::prewarm_sentence` 在构建完成时调用，只搬运、不改变取值。
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

    /// 一条词典命中的整句权重。两条来源，优先拼音词频：
    ///
    /// 1. 有拼音词库 ⇒ 查 `text` 的真实词频（查不到给 `OOV_FREQ`），完全不看码表 weight；
    /// 2. 没有 ⇒ 简码换成全码位置的权重（[`ShortCodeIndex`]），再用 [`trusted_weight`] 把「带」
    ///    落回中性值，功能可用但精度回落。
    ///
    /// 用户词 / 临时词查不到时给 [`SentenceFreq::user_floor`]。
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

    /// 建词图：逐跨度 `(p, q)` 查词典。跨度数 ≤ `len × max_code_length`，每跨度一次精确查询，
    /// 与拼音整句同量级。
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
                for mut hit in dm.search(code, MAX_HOMOGRAPHS) {
                    // 特殊语法词条整句里不能求值也不能执行动作，拼进去就是把源码当文本。例外：
                    // 纯文本直通 `$CC("标签", type("字"))` 解析期已定，按「字」当普通词条用；其余剔除，
                    // 剔光的段视同无码。打分仍按词库原文（词频/简码索引以原文为键），打完分再换文本。
                    let Some(plain) = sentence_text(&hit.text) else {
                        continue;
                    };
                    let (weight, total, is_short) = self.weight_of(dm, code, &hit);
                    if let Some(t) = plain {
                        hit.text = t;
                    }
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
                        aux: 0,
                    });
                }
            }
        }
        if let Some(f) = compose {
            add_composed_nodes(&mut nodes, &units, f);
        }
        nodes
    }

    /// 解码。返回 `None` 表示没有覆盖整串的解或解只有一个词。
    ///
    /// 只收整串解：整句候选因此恒消费整串，`consumed_length` 可留 0，不打破「码表候选
    /// `consumed_length` 恒 0」的约定（设计文档 §7.1）。排除单词解对齐 librime
    /// `Poet::MakeSentenceWithStrategy`：整串在码表里有词时由精确/前缀路径产出，整句不重复产。
    /// 例外是词典组词出的单词解，见 [`Self::accepts`]。
    pub fn decode(&self, input: &str, dm: &DictManager) -> Option<SentenceResult> {
        self.decode_nbest(input, dm, 1).into_iter().next()
    }

    /// 至多 `k` 条整句解，按名次排好、已按文本去重。`k <= 1` 等价于 [`Self::decode`]。
    ///
    /// 挂语法模型时 [`ViterbiDecoder::decode_nbest`] 只给一条，故名次 1 取挂模型的解，名次 2 起由
    /// 不挂模型的 N-best 补位（与名次 1 同文的跳过）。输入含分隔符时只出一条：段间组合是乘积级的，
    /// 且用户敲分隔符本就是在消歧。
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
    /// 单词解只在超码长时放行：码表词条的码不超过满码长，超码长的单词解必是词典组词边（逐字打出的
    /// 「喃喃自语」），它在精确路径上产不出来。
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

        // 手动分隔符把输入切成若干硬边界段，各段独立解码后拼起来。
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

        // 「至少两个词」是整体判据，不是段级判据：`aawt'aawt` 每段恰好一个词，段级判会整个否掉。
        // 单词解与无分隔符时同一条规矩（见 `accepts`），多敲的尾部分隔符不该让它消失。
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

    /// 解一个不含分隔符的段。返回 `None` = 没有覆盖该段的解。
    ///
    /// 判据是 `log_prob.is_finite()` 而不是 `words.is_empty()`：[`ViterbiDecoder::decode`] 在终点
    /// 不可达时会从最远可达位置回溯出一条部分路径，`log_prob` 此时为 `-inf`。
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
/// 码表没收的词只能逐字成边、各按单字词频计价，解码器看不出几个字连起来是个词（效果见设计文档
/// §12.2）。组出的词按它自己的词频打分（同一个 `score_code_node`、同一份每边罚），再扣掉各组成
/// 单元的简码罚之和：简码罚不会因为拼出了词就消失。组成单元的边原样保留，只多给解码器一个选项。
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
                    aux: 0,
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
#[path = "sentence_tests.rs"]
mod tests;
