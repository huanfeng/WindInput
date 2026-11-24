use super::*;

fn trie() -> SyllableTrie {
    SyllableTrie::new()
}

fn keys(input: &str) -> Vec<String> {
    let mut k: Vec<String> = mixed_patterns(input, &trie())
        .iter()
        .map(|p| p.key().to_string())
        .collect();
    k.sort();
    k.dedup();
    k
}

/// 声母在前：`nhao` 的主解释是 [n][hao]，投影键退化为纯简拼 `nh`——正是 AbbrevSection 里已有的键。
#[test]
fn initial_first_projects_to_plain_abbrev_key() {
    let pats = mixed_patterns("nhao", &trie());
    let main = pats
        .iter()
        .find(|p| p.len() == 2)
        .expect("nhao 应有 [n][hao] 这条 2 段解释");
    assert_eq!(main.key(), "nh");
    assert!(main.matches(&["ni", "hao"]), "ni|hao 应命中");
    assert!(main.matches(&["na", "hao"]), "声母段只约束首字母");
    assert!(!main.matches(&["ni", "hai"]), "音节段要求全等");
    assert!(!main.matches(&["ni", "hao", "ma"]), "音节数必须相等");
}

/// 全拼在前：`nih` 在 `is_abbreviation` 那里连门都进不去（`i` 不是任何音节首字母），
/// 但作为模式它完全成立 —— 文档 §2 说的「两种形态卡在不同环节」在这里被同一套表示统一。
#[test]
fn syllable_first_is_expressible() {
    let pats = mixed_patterns("nih", &trie());
    let main = pats
        .iter()
        .find(|p| p.matches(&["ni", "hao"]))
        .expect("nih 应能匹配 ni|hao");
    assert_eq!(main.key(), "nh", "投影键同样退化为纯简拼");
    assert!(!main.matches(&["ni", "ao"]), "第二段须以 h 开头");
}

/// 全声母串不归本模块 —— 那是纯简拼，由 `AbbrevMatcher` + step5 处理，
/// 在这里产出只会与之重复召回。
#[test]
fn pure_initial_form_is_excluded() {
    assert!(keys("nh").is_empty());
    assert!(keys("dblg").is_empty());
}

/// 完整全拼串照样有混合解释（`nihao` = ni + h + ao），本模块不负责挡它，挡它的是引擎 step 5b
/// 的 `mixed_covered` 短路。那个短路是正确性依赖：去掉它，常见全拼输入会静默多出一批 is_abbrev 噪音候选。
#[test]
fn full_pinyin_still_has_mixed_readings_caller_must_short_circuit() {
    assert!(
        !keys("nihao").is_empty(),
        "ni|h|ao 是合法混合式；排除它是调用方的职责，不是枚举器的"
    );
    assert!(!keys("xian").is_empty(), "xi|a|n 同理");
}

/// 判据①失败的那批串（`woain`）在这里是合法混合式 —— 恰好就是用户想要的「我爱你」。
#[test]
fn partial_syllable_tail_is_mixed() {
    let pats = mixed_patterns("woain", &trie());
    assert!(
        pats.iter().any(|p| p.matches(&["wo", "ai", "ni"])),
        "wo|ai|n 应能匹配 wo|ai|ni: {:?}",
        pats.iter().map(|p| p.key()).collect::<Vec<_>>()
    );
}

/// 单字母音节（`a`/`e`/`o`）不重复产出「音节段 + 声母段」两条同长边。
/// 声母段的约束是音节段的超集，留两条只是把同一批词查两遍。
#[test]
fn single_letter_syllable_does_not_duplicate_initial_edge() {
    let pats = mixed_patterns("hao", &trie());
    // h + ao / ha + o(声母) —— 后者的 o 只能是声母段，不该同时再出一条音节段 [o]
    let with_o_tail: Vec<_> = pats.iter().filter(|p| p.len() == 2).collect();
    let dup = with_o_tail
        .iter()
        .filter(|p| p.matches(&["ha", "o"]))
        .count();
    assert!(dup <= 1, "「ha|o」不应有两条等价模式: {with_o_tail:?}");
}

/// 非法输入一律不解释（含大写/数字/分隔符——分隔符是全拼的硬边界，与简拼无关）。
#[test]
fn rejects_non_lowercase_and_extremes() {
    assert!(keys("NHao").is_empty());
    assert!(keys("n2ao").is_empty());
    assert!(keys("ni'h").is_empty());
    assert!(keys("n").is_empty(), "单字母不构成混合式");
    assert!(
        keys(&"nhao".repeat(5)).is_empty(),
        "超长串不做混合解释（20 字节 > 上限）"
    );
}

/// 模式数有硬上限，且枚举不得随长度爆炸 —— 这是热路径上的成本闸门。
#[test]
fn pattern_count_is_bounded() {
    for input in [
        "zhongguorenm",
        "nhaoshijien",
        "wdjdxzgr",
        "aeiouaeiou",
        // 卷舌串：双字母声母加了一档边，分支最密的形状要一并看住。
        "chengshizhong",
        "shanghaishizh",
        "zhchshzh",
        "zhzhzhzh",
    ] {
        let pats = mixed_patterns(input, &trie());
        assert!(
            pats.len() <= MAX_PATTERNS,
            "{input}: {} 条超过上限",
            pats.len()
        );
        assert!(pats.iter().all(|p| p.len() <= MAX_SEGMENTS));
    }
}

/// 新增一个维度不得挤掉旧解释。
///
/// `Retroflex` 边排在 `Initial` 边之前，同一位置上新分支先展开，深层位置会把 `MAX_PATTERNS`
/// 的额度提前用光，`chengshizhong` 这类串会丢掉老写法的解释（用户感知为老写法打不出词）。
#[test]
fn retroflex_edges_do_not_evict_pre_existing_interpretations() {
    let k = keys("chengshizhong");
    for expected in ["chesz", "cheszh", "chegsz"] {
        assert!(
            k.contains(&expected.to_string()),
            "丢了改动前就有的解释 {expected}：{k:?}"
        );
    }
}

/// 投影键至少 2 位：`Retroflex` 吃 2 个击键却只投影 1 个字母，单段模式会绕过
/// `MIN_ABBREV_STROKE`（「单字母不构成简拼，退到 1 只会拖出一堆高频单字」）。
#[test]
fn single_retroflex_segment_does_not_yield_one_letter_key() {
    assert!(keys("zh").is_empty(), "zh 单独一段投影成键 `z`，不该产出");
    assert!(keys("ch").is_empty());
    assert!(keys("sh").is_empty());
    // 两段起才算数。
    assert_eq!(keys("zhy"), vec!["zy".to_string()]);
}

/// 双字母声母的模式与投影键。
#[test]
fn retroflex_patterns_project_to_existing_keys() {
    assert!(keys("zhge").contains(&"zg".to_string()), "zh|ge → zg");
    assert!(
        keys("baichx").contains(&"bcx".to_string()),
        "bai|ch|x → bcx"
    );
    // 老解释并存：zhge 同时还有 z|h|ge。
    assert!(
        keys("zhge").contains(&"zhg".to_string()),
        "z|h|ge → zhg 仍在"
    );
}

/// 段语义：`Retroflex` 要求音节以两个字母开头，比 `Initial` 严。
#[test]
fn retroflex_segment_is_stricter_than_initial() {
    assert!(AbbrevSeg::Retroflex('z').matches_exact("zhe"));
    assert!(!AbbrevSeg::Retroflex('z').matches_exact("ze"));
    assert!(!AbbrevSeg::Retroflex('z').matches_exact("z"));
    assert!(AbbrevSeg::Initial('z').matches_exact("ze"), "单字母仍宽松");
}

/// preedit 渲染：两种解释的显示都要在。
///
/// 单遍「双字母优先」贪心会让第二行返回 `None`（第一步吃掉 2 字节、第二个音节没键可分），
/// 组合区退回无分隔符的 `zh` —— 而 `z'h'ge` 这种显示正是双字母声母缺失时用户能看到的
/// 唯一线索，不能因为新增一种解释就把另一种的显示弄丢。
#[test]
fn preedit_renders_both_retroflex_and_single_letter_splits() {
    assert_eq!(
        render_keystroke_preedit("zhge", &["zhe", "ge"]),
        Some(("zh'ge".into(), 4)),
        "zh 是一个声母段"
    );
    assert_eq!(
        render_keystroke_preedit("zh", &["zhong", "hua"]),
        Some(("z'h".into(), 2)),
        "老写法：z 和 h 各是一段"
    );
    assert_eq!(
        render_keystroke_preedit("baichx", &["bai", "cheng", "xian"]),
        Some(("bai'ch'x".into(), 6))
    );
    // 不变量：去掉 ' 恰好还原击键串的已消费部分。
    for (raw, syls) in [
        ("zhge", &["zhe", "ge"][..]),
        ("zh", &["zhong", "hua"][..]),
        ("baichx", &["bai", "cheng", "xian"][..]),
    ] {
        let (rendered, used) = render_keystroke_preedit(raw, syls).expect("应渲染得出");
        assert_eq!(rendered.replace('\'', ""), raw[..used]);
    }
}

/// boundary 切分与 `abbrev_of_code` 同源：每段首字母拼起来必须等于纯简拼串。
#[test]
fn boundary_split_agrees_with_abbrev_projection() {
    // 「西安宁」xi|an|ning：位 0/2/4
    let syls = syllables_from_boundary("xianning", 0b10101).expect("有边界");
    assert_eq!(syls, vec!["xi", "an", "ning"]);
    let abbrev: String = syls.iter().filter_map(|s| s.chars().next()).collect();
    assert_eq!(abbrev, "xan", "与 abbrev_of_code 的投影必须一致");

    assert_eq!(
        syllables_from_boundary("nihao", 0b101),
        Some(vec!["ni", "hao"])
    );
    assert_eq!(syllables_from_boundary("xian", 0b1), Some(vec!["xian"]));
}

/// 无边界信息 = 判据不存在 → 不参与混合简拼（不是放行）。
#[test]
fn missing_boundary_yields_no_syllables() {
    assert_eq!(syllables_from_boundary("nihao", 0), None);
    // bit0 未置位：第一个音节不从 0 开始，坏数据，不猜
    assert_eq!(syllables_from_boundary("nihao", 0b100), None);
    assert_eq!(syllables_from_boundary("", 0b1), None);
}
