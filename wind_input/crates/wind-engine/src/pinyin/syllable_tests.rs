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

/// 晋升校验：合法全拼（有 / 无边界）通过；击键混合码、边界切错的、非 ASCII 的拒。
#[test]
fn full_pinyin_code_check() {
    // bu|zhi|dao|ni|hao → 位 0/2/5/8/10
    assert!(is_full_pinyin_code("buzhidaonihao", 0b101_0010_0101));
    assert!(
        is_full_pinyin_code("buzhidaonihao", 0),
        "无边界时整码切得满即通过"
    );
    // 旧 ②b 的击键混合码：b|z|d|ni|hao（逐字母边界）与无边界两种形态都拒。
    assert!(!is_full_pinyin_code("bzdnihao", 0b10_1111));
    assert!(!is_full_pinyin_code("bzdnihao", 0));
    // 边界切错（bu|zh|idao…）：码本身合法，但记录自己的切分不是音节序列。
    assert!(!is_full_pinyin_code(
        "buzhidaonihao",
        0b101_0010_0101 | 0b1000
    ));
    assert!(!is_full_pinyin_code("", 0));
    assert!(!is_full_pinyin_code("不知道", 0));
}

/// 词库里真实出现、《现代汉语词典》/《通用规范汉字表》收录的稀有音节：
/// 嗲 dia、塞 sei（口语音 sēi）、𤭢 cei。缺了它们全拼打不出、含它们的临时词也晋升不了。
#[test]
fn rare_standard_syllables_are_recognized() {
    let trie = SyllableTrie::new();
    for syl in ["dia", "sei", "cei"] {
        assert!(trie.is_syllable(syl), "{syl} 应是标准音节");
    }
    // dia|sheng|dia|qi → 位 0/3/8/11
    assert!(is_full_pinyin_code("diashengdiaqi", 0b1001_0000_1001));
    // 边界为 0 时补表前也能按 di|a 切通，这条只防回归
    assert!(is_full_pinyin_code("diashengdiaqi", 0));
    assert!(is_full_pinyin_code("dia", 0b1));
    assert!(is_full_pinyin_code("sei", 0b1));
    assert!(is_full_pinyin_code("cei", 0));
}

/// 刻意**不**收的读音（取舍理由见 [`STANDARD_SYLLABLES`] 尾部注释）：
/// 台湾国语 / 方言音 / 非《汉语拼音方案》拼式。锁住决定，免得被按「词库里有」顺手补进来。
#[test]
fn nonstandard_readings_stay_out_of_table() {
    let trie = SyllableTrie::new();
    for syl in [
        "yai", "lvan", "nia", "pia", "fong", "eh", "wong", "fiao", "biang",
    ] {
        assert!(!trie.is_syllable(syl), "{syl} 不应进标准音节表");
    }
}
