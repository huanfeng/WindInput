//! 音节 Trie（~400 个合法拼音音节）
//!
//! 与 Go 版本 `wind_input/internal/engine/pinyin/syllable_trie.go` 对齐。
//! 使用 HashMap Trie 实现高效音节边界检测。

use std::collections::HashMap;

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
    /// ⚠️ **整表替换，不是累加**：`with_fuzzy` 是链式 builder，可以被调第二次。若只累加，
    /// 前一次配置里那些「后来被关掉的组」的拼写会残留在图上，表现为「模糊音某组关了却还
    /// 生效」——而且只在链式调用两次时才复现，极难查。
    pub fn load_fuzzy_spellings(&mut self, spellings: &[String]) {
        self.fuzzy_root = TrieNode::default();
        self.has_fuzzy = !spellings.is_empty();
        for s in spellings {
            Self::insert(&mut self.fuzzy_root, s);
        }
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

/// 标准普通话音节全集（约 410 个，封闭集）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinyin::fuzzy::{FuzzyConfig, fuzzy_spellings};

    fn fuzzy_trie() -> SyllableTrie {
        let mut trie = SyllableTrie::new();
        trie.load_fuzzy_spellings(&fuzzy_spellings(&FuzzyConfig {
            in_ing: true,
            ..Default::default()
        }));
        trie
    }

    #[test]
    fn plain_trie_cannot_match_invalid_spelling() {
        let trie = SyllableTrie::new();
        assert_eq!(trie.match_at("tinzhi", 0), vec!["ti".to_string()]);
    }

    /// 注册模糊拼写后，`tin` 成为切分层的一条边 —— 这正是模糊音能被执行的前提。
    #[test]
    fn fuzzy_layer_adds_edge_for_invalid_spelling() {
        let trie = fuzzy_trie();
        assert_eq!(
            trie.match_at("tinzhi", 0),
            vec!["tin".to_string(), "ti".to_string()],
            "须最长优先，且两层结果合并后仍按长度降序"
        );
    }

    /// 重复加载须**整表替换**：关掉某组后，它的拼写不得残留。
    #[test]
    fn reloading_fuzzy_spellings_replaces_instead_of_accumulating() {
        let mut trie = fuzzy_trie(); // in_ing
        assert_eq!(trie.match_at("tinzhi", 0).len(), 2, "先确认 tin 已注册");

        trie.load_fuzzy_spellings(&fuzzy_spellings(&FuzzyConfig {
            zh_z: true,
            ..Default::default()
        }));
        assert_eq!(
            trie.match_at("tinzhi", 0),
            vec!["ti".to_string()],
            "换成 zh_z 后 in_ing 的 tin 不得残留"
        );
        // `zuan`/`zu` 是标准音节，`zuang`/`zua` 来自 zh_z 模糊表（zhuang/zhua 的对端）。
        assert_eq!(
            trie.match_at("zuangzhi", 0),
            vec![
                "zuang".to_string(),
                "zuan".to_string(),
                "zua".to_string(),
                "zu".to_string()
            ],
            "新组须生效"
        );

        // 全关 ⇒ 退回纯标准音节表。
        trie.load_fuzzy_spellings(&fuzzy_spellings(&FuzzyConfig::default()));
        assert_eq!(trie.match_at("tinzhi", 0), vec!["ti".to_string()]);
        assert_eq!(
            trie.match_at("zuangzhi", 0),
            vec!["zuan".to_string(), "zu".to_string()],
            "全关后模糊层须整体消失，只剩标准音节"
        );
    }

    /// 真值判据不受模糊层影响 —— 双拼校验、造词读音归属都靠它们。
    #[test]
    fn fuzzy_layer_does_not_leak_into_truth_predicates() {
        let trie = fuzzy_trie();
        assert!(!trie.is_syllable("tin"), "模糊拼写不是合法音节");
        assert!(trie.is_syllable("ting"));
        assert_eq!(
            trie.match_at_strict("tinzhi", 0),
            vec!["ti".to_string()],
            "strict 版须与未注册模糊层时完全一致"
        );
    }
}
