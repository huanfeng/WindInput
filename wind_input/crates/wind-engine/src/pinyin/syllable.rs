//! 音节 Trie（414 个合法拼音音节）
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/syllable_trie.go` 对齐。
//! 使用 HashMap Trie 实现高效音节边界检测。

use std::collections::{HashMap, HashSet};

/// 音节 Trie 节点
#[derive(Default)]
struct TrieNode {
    children: HashMap<u8, TrieNode>,
    is_end: bool,
}

/// 音节 Trie
pub struct SyllableTrie {
    root: TrieNode,
    /// 模糊拼写层：**只参与切分**（[`Self::match_at`]），不参与
    /// [`Self::is_syllable`]/[`Self::is_prefix`] 这两条**真值判据**。
    ///
    /// 分成两棵树而非合并进 `root`，是因为两者回答的是不同的问题：
    /// 「这段码能不能作为一条边走通」（切分，可以宽容）与「这段码是不是一个音节」
    /// （真值，必须严格）。后者被双拼真值校验、造词边界推导、简拼判据复用，
    /// 把 `tin` 混进去会让它们把用户的错音当成真读音。
    /// 见 [`fuzzy::fuzzy_spellings`](super::fuzzy::fuzzy_spellings)。
    fuzzy_root: TrieNode,
    /// `fuzzy_root` 是否非空。切分是按键热路径，全关时据此跳过第二棵树的遍历。
    has_fuzzy: bool,
    /// `fuzzy_root` 里哪些是换位纠错拼写（[`Self::set_correction_spellings`]）。
    correction: HashSet<String>,
}

impl Default for SyllableTrie {
    fn default() -> Self {
        Self::new()
    }
}

impl SyllableTrie {
    pub fn new() -> Self {
        let mut trie = Self {
            root: TrieNode::default(),
            fuzzy_root: TrieNode::default(),
            has_fuzzy: false,
            correction: HashSet::new(),
        };
        trie.load_standard_syllables();
        trie
    }

    fn load_standard_syllables(&mut self) {
        for syl in STANDARD_SYLLABLES {
            Self::insert(&mut self.root, syl);
        }
    }

    /// 注册模糊拼写（[`fuzzy::fuzzy_spellings`](super::fuzzy::fuzzy_spellings) 的产物）。
    ///
    /// 只影响 [`Self::match_at`]，即「切分时这段码算不算一条边」；
    /// `is_syllable`/`is_prefix` 保持严格，见 [`Self::fuzzy_root`] 的说明。
    /// 由 `PinyinEngine::with_fuzzy` 在构造期调用一次，不进按键热路径。
    /// 整表替换，不是累加：`with_fuzzy` 可被链式调用两次，累加会让已关掉的组残留生效。
    pub fn load_fuzzy_spellings(&mut self, spellings: &[String]) {
        self.fuzzy_root = TrieNode::default();
        self.has_fuzzy = !spellings.is_empty();
        for s in spellings {
            Self::insert(&mut self.fuzzy_root, s);
        }
    }

    /// 标出 `fuzzy_root` 里的换位纠错拼写（它们须已随 [`Self::load_fuzzy_spellings`] 注册）。
    /// 整表替换。纠错边只给词图与变体展开用，主切分（[`super::dag::Dag::maximum_match`]）
    /// 不走它，见那里的说明。
    pub fn set_correction_spellings<'a>(&mut self, spellings: impl IntoIterator<Item = &'a str>) {
        self.correction = spellings.into_iter().map(str::to_string).collect();
    }

    /// `s` 是不是换位纠错拼写。
    pub fn is_correction_spelling(&self, s: &str) -> bool {
        !self.correction.is_empty() && self.correction.contains(s)
    }

    fn insert(root: &mut TrieNode, syl: &str) {
        let mut node = root;
        for byte in syl.bytes() {
            node = node.children.entry(byte).or_default();
        }
        node.is_end = true;
    }

    /// 在 `root` 上收集从 `pos` 起的所有完整匹配，按结束位置**升序**追加到 `out`。
    fn collect_from(root: &TrieNode, input: &str, pos: usize, out: &mut Vec<String>) {
        let bytes = input.as_bytes();
        let mut node = root;
        for i in pos..bytes.len() {
            match node.children.get(&bytes[i]) {
                Some(child) => {
                    node = child;
                    if node.is_end {
                        out.push(input[pos..=i].to_string());
                    }
                }
                None => break,
            }
        }
    }
}

/// 标准普通话音节全集（414 个，封闭集）。
/// 供 SyllableTrie 构建，以及造词时遍历查词典反推单字读音（generate::CharPinyinIndex）。
pub const STANDARD_SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi",
    "bian", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can", "cang", "cao", "ce",
    "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong",
    "chou", "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci", "cong", "cou",
    "cu", "cuan", "cui", "cun", "cuo", "da", "dai", "dan", "dang", "dao", "de", "dei", "den",
    "deng", "di", "dian", "diao", "die", "ding", "diu", "dong", "dou", "du", "duan", "dui", "dun",
    "duo", "e", "ei", "en", "eng", "er", "fa", "fan", "fang", "fei", "fen", "feng", "fo", "fou",
    "fu", "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu",
    "gua", "guai", "guan", "guang", "gui", "gun", "guo", "ha", "hai", "han", "hang", "hao", "he",
    "hei", "hen", "heng", "hong", "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo",
    "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan",
    "jue", "jun", "ka", "kai", "kan", "kang", "kao", "ke", "ken", "keng", "kong", "kou", "ku",
    "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo", "la", "lai", "lan", "lang", "lao", "le",
    "lei", "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling", "liu", "lo", "long",
    "lou", "lu", "luan", "lun", "luo", "lv", "lve", "ma", "mai", "man", "mang", "mao", "me", "mei",
    "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou", "mu", "na",
    "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni", "nian", "niang", "niao", "nie",
    "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nuo", "nv", "nve", "o", "ou", "pa", "pai",
    "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie", "pin", "ping", "po",
    "pou", "pu", "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu",
    "quan", "que", "qun", "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru",
    "ruan", "rui", "run", "ruo", "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha",
    "shai", "shan", "shang", "shao", "she", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai",
    "shuan", "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun",
    "suo", "ta", "tai", "tan", "tang", "tao", "te", "teng", "ti", "tian", "tiao", "tie", "ting",
    "tong", "tou", "tu", "tuan", "tui", "tun", "tuo", "wa", "wai", "wan", "wang", "wei", "wen",
    "weng", "wo", "wu", "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu",
    "xu", "xuan", "xue", "xun", "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yong",
    "you", "yu", "yuan", "yue", "yun", "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen",
    "zeng", "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhen", "zheng", "zhi", "zhong", "zhou",
    "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo", "zi", "zong", "zou", "zu",
    "zuan", "zui", "zun", "zuo",
    // 与 Go syllable_trie.go / shuangpin.validPinyinSyllables 对齐补全的稀有音节
    // （双拼转换真值依赖：紫光 ik→shei、ziguang 等；以及 kei/tei/zhei/nun/rua/yo）。
    "kei", "tei", "zhei", "shei", "nun", "rua", "yo",
    // 词库（rime cn_dicts）里真实出现、且《现代汉语词典》/《通用规范汉字表》收录的稀有音节：
    // 嗲 dia、塞 sei（口语音 sēi）、𤭢 cei。缺了它们，切分器把 `dia` 错切成 `di|a`；
    // `sei`/`cei` 切不动 ⇒ 这些字打不出，含它们的临时词过不了晋升校验。
    // `sei`/`cei` 前面不可能有以 s/c 收尾的音节，`dia` 的新切法只与 `di|a…` 竞争；补表前后
    // 对拍词库里全部 13552 条含 `dia` 的词，全拼界面首选一条未变。
    // 代价已由「零权重读音让位」处理：双拼里解码到这三个音节的两键组合（小鹤 `dx`/`sw`/`cw`、
    // 搜狗 `dw`/`sz`/`cz` 等）以前解不出音节、落到简拼（东西/所谓/成为…），补表后首选一度是
    // 嗲/塞/𤭢。现在 𤭢(cei)/塞(sei) 这类词库权重 0 的读音并入简拼层、让位给简拼词
    // （`Candidate::is_zero_weight_reading`，nun/zhei 同理）；嗲(dia=182) 有权重，`dx` 仍首选嗲
    // ——小鹤 dx 就是 dia，这是方案定义本身。
    //
    // 刻意不收（词库 41448 / 8105 里有、但不是普通话规范音节）：
    // - `yai`(崖)：台湾国语读音，普通话读 yá；`lvan`(娈孪挛)：普通话读 luán，lüan 不是
    //   普通话音节，且补了会破坏 `spelling.rs` 的 ü 归一化穷尽性前提；
    // - `nia`(娘)、`pia`(啪)、`fong`(甮)、`wong`(𥥈)、`fiao`(覅)：方言 / 象声，《现汉》不作
    //   普通话音节（fiao 仅标〈方〉）；
    // - `eh`(诶)：rime 对 ê 的转写，不是《汉语拼音方案》拼式；
    // - `biang`(𰻝)：不在《通用规范汉字表》。
    // 另有代价：补 `eh`/`wong`/`fong`/`pia` 后，`eh`→「恶化」、`wong`→「我能够」这类
    // 简拼 / 残码打法整屏只剩一个生僻字。
    "dia", "sei", "cei",
];

impl SyllableTrie {
    /// 在指定位置匹配所有可能的音节（最长优先）。
    ///
    /// **含模糊拼写层**（若已 [`Self::load_fuzzy_spellings`]）：切分必须能走通用户的错音串，
    /// 否则模糊音在切分阶段就断了，后面「逐音节展开变体」的逻辑一次都轮不到执行
    /// （`tinzhi` 只切出 `ti`，`nzhi` 全成残码）。
    pub fn match_at(&self, input: &str, pos: usize) -> Vec<String> {
        let mut matches = self.match_at_strict(input, pos);
        if self.has_fuzzy {
            let before = matches.len();
            Self::collect_from(&self.fuzzy_root, input, pos, &mut matches);
            if matches.len() > before {
                // 两层互斥（`fuzzy_spellings` 只收非合法音节），故不会有重复项，
                // 但拼接后不再有序 —— 重排回最长优先。
                matches.sort_by_key(|s| std::cmp::Reverse(s.len()));
            }
        }
        matches
    }

    /// 只按标准音节表匹配，**不含**模糊拼写层。
    ///
    /// 供**真值推导**路径使用（造词边界 `generate::boundary_by_char_count`）：那里要回答
    /// 「这串码的真实音节切分是什么」，把用户的错音当成边会推出错误的读音归属。
    pub fn match_at_strict(&self, input: &str, pos: usize) -> Vec<String> {
        let mut matches = Vec::new();
        Self::collect_from(&self.root, input, pos, &mut matches);
        matches.reverse(); // 最长优先
        matches
    }

    /// 检查是否为合法音节。
    ///
    /// **刻意不含模糊拼写层**：这是真值判据（双拼真值校验、造词读音归属都靠它），
    /// `is_syllable("tin")` 恒为 `false`。要问「切分时走不走得通」请用 [`Self::match_at`]。
    pub fn is_syllable(&self, s: &str) -> bool {
        let mut node = &self.root;
        for byte in s.bytes() {
            match node.children.get(&byte) {
                Some(child) => node = child,
                None => return false,
            }
        }
        node.is_end
    }

    /// 检查是否为合法音节的前缀。**刻意不含模糊拼写层**，同 [`Self::is_syllable`]。
    pub fn is_prefix(&self, s: &str) -> bool {
        let mut node = &self.root;
        for byte in s.bytes() {
            match node.children.get(&byte) {
                Some(child) => node = child,
                None => return false,
            }
        }
        true
    }
}

/// 拼音词条码 `code` 能否切回合法全拼音节序列——临时词自动晋升进用户库前的校验
/// （协调层 `maybe_promote_temp`），挡住按击键混合码造出的记录（`bzdnihao`），
/// 免得它晋升成用户库里的永久坏码。判据：
/// - 有边界：按记录自己的边界切（[`super::mixed_abbrev::syllables_from_boundary`]，与简拼、
///   混合判据同一个切法），每段都须是标准音节（[`SyllableTrie::is_syllable`]，不含模糊拼写层）；
/// - 边界为 0（无信息：手输码、v1 遗留、拼接超 64 字节或任一段缺边界时造词置 0）或码长超 64
///   （边界表达不下）：退而问「整码能否被标准音节表切满」（[`super::dag::Dag::build_strict`]）。
///   合法全拼的无边界记录照常通过，`bzdnihao` 这类从 `b` 起就切不动的照样被拒。
///
/// 含分隔符 `'`、大写字母或非 ASCII 的码一律按「非全拼」处理（不做规范化）：造词写入的码
/// 恒为小写全拼，出现这些字符说明来源本就不是规范码。
///
/// 判据就是 [`STANDARD_SYLLABLES`]：词库里的非规范读音（`yai`/`lvan`/`nia`/`pia`/`eh` 等，
/// 取舍见该表尾注）不在表内，含它们的词不会自动晋升——与它们本就打不出来一致。
pub fn is_full_pinyin_code(code: &str, boundary: u64) -> bool {
    static TRIE: std::sync::LazyLock<SyllableTrie> = std::sync::LazyLock::new(SyllableTrie::new);
    if code.is_empty() || !code.is_ascii() {
        return false;
    }
    if boundary == 0 || code.len() > 64 {
        return super::dag::Dag::build_strict(code, &TRIE)
            .unmatched_tail()
            .is_empty();
    }
    super::mixed_abbrev::syllables_from_boundary(code, boundary)
        .is_some_and(|syls| syls.iter().all(|s| TRIE.is_syllable(s)))
}

#[cfg(test)]
#[path = "syllable_tests.rs"]
mod tests;
