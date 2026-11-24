use super::*;

const RIME_SAMPLE: &str = "# Rime dictionary\n---\nname: luna_pinyin\nversion: \"0.9\"\nsort: by_weight\n...\n\n你好\tni hao\t100\n世界\tshi jie\t50\n";

#[test]
fn detect_winddict_rime_tsv_unknown() {
    let wd = "# c\nwind_dict:\n  version: 1\n\n--- !words\na\t工\t1\n";
    assert_eq!(detect_dict_format(wd), DictFormat::WindDict);
    assert_eq!(detect_dict_format(RIME_SAMPLE), DictFormat::Rime);
    assert_eq!(detect_dict_format("nihao\t你好\t10\n"), DictFormat::Tsv);
    assert_eq!(
        detect_dict_format("只有词\n没有制表符\n"),
        DictFormat::Unknown
    );
    // BOM 不影响探测
    assert_eq!(
        detect_dict_format("\u{feff}wind_dict:\n  version: 1\n\n--- !words\n"),
        DictFormat::WindDict
    );
}

/// ⚠️ **断言方向已反转**（原为「拼音码去空格连写」，测试名亦为 `..._space_join`）。
///
/// rime 源里 `你好\tni hao\t100` 的空格是词库作者标注的**音节真值**。旧的
/// `normalize_code` 直接把它删掉，导入的词一律 boundary=0 —— 信息拿在手上、用完即弃。
/// 现保留为单空格，落库时由 `wdict::split_spaced_code` 拆成 flat key + 边界
/// （key 仍是扁平的 `nihao`，见 docs/design/pinyin-code-domains.md §2.2）。
#[test]
fn rime_default_columns_keeps_syllable_spaces() {
    let (rows, skipped) = parse_words_rime(RIME_SAMPLE, CodePolicy::PINYIN).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(rows.len(), 2);
    // 默认列序 text 在前；音节空格保留，供落库端拆出边界
    assert_eq!(rows[0].text, "你好");
    assert_eq!(rows[0].code, "ni hao");
    assert_eq!(rows[0].weight, 100);
    assert_eq!(rows[1].code, "shi jie");
    // 落库端拆分后 key 仍是扁平码，边界随之得到
    assert_eq!(
        crate::wdict::split_spaced_code(&rows[0].code),
        ("nihao".to_string(), 0b101)
    );
}

/// 空白折叠：多空格/制表列内空白归一为单个空格；码表码（无空格）幂等。
/// 两条导入路径都须反转义词条文本，且与 `escape_field` 往返自洽
/// （导出 → 再导入应还原原文，这是备份还原的基本契约）。
#[test]
fn import_unescapes_text_on_both_paths() {
    let rime = "---\nname: t\n...\n甲\\n乙\tjy\t10\nC:\\Users\tcu\t20\n";
    let (rows, _) = parse_words_rime(rime, CodePolicy::PINYIN).unwrap();
    assert_eq!(rows[0].text, "甲\n乙", "Rime 路径须反转义 \\n");
    assert_eq!(
        rows[1].text, "C:\\Users",
        "未知转义序列原样保留，路径类词条不被改写"
    );

    let tsv = "jy\t甲\\n乙\t10\ncu\tC:\\Users\t20\n";
    let (rows, _) = parse_words_tsv(tsv, CodePolicy::PINYIN).unwrap();
    assert_eq!(rows[0].text, "甲\n乙", "TSV 路径须反转义 \\n");
    assert_eq!(rows[1].text, "C:\\Users");
}

/// 命令栏语法条目列序写反时逐行纠正（t172）：`$SS(...)` 不可能是编码。
/// 两条导入路径都得认，否则同一行在「加载」与「导入」下结局不同。
#[test]
fn import_swaps_cmdbar_entry_written_in_wrong_column() {
    let ss = r#"$SS("括号", "【】", "（）")"#;
    let rime = format!("---\nname: t\n...\n甲\tjy\nuu\t{ss}\n");
    let (rows, skipped) = parse_words_rime(&rime, CodePolicy::CODETABLE).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!((rows[1].code.as_str(), rows[1].text.as_str()), ("uu", ss));

    let tsv = format!("jy\t甲\n{ss}\tuu\n");
    let (rows, skipped) = parse_words_tsv(&tsv, CodePolicy::CODETABLE).unwrap();
    assert_eq!(skipped, 0, "列序写反的 $SS 行不该被当非法行丢掉");
    assert_eq!((rows[1].code.as_str(), rows[1].text.as_str()), ("uu", ss));
}

/// 反例：编码列只含 `{`（无 marker）不算列序写反，两条路径都不得对调。
#[test]
fn import_does_not_swap_on_bare_brace_code() {
    let rime = "---\nname: t\n...\n甲\t{a}\n";
    let (rows, _) = parse_words_rime(rime, CodePolicy::CODETABLE).unwrap();
    assert!(
        rows.iter().all(|r| r.text != "{a}"),
        "不该把 {{a}} 当文本: {rows:?}"
    );

    let tsv = "{a}\t甲\n";
    let (rows, _) = parse_words_tsv(tsv, CodePolicy::CODETABLE).unwrap();
    assert!(
        rows.iter().all(|r| r.text != "{a}"),
        "不该把 {{a}} 当文本: {rows:?}"
    );
}

/// 反转义必须发生在 trim **之后**：转义序列在 trim 阶段是可见字符，剥不掉；
/// 而围绕字段的裸空格是排版噪声，照剥不误。
#[test]
fn import_trims_bare_space_but_keeps_escaped_whitespace() {
    let tsv = "jy\t  甲乙  \t10\nbd\t丙\\t丁\t20\n";
    let (rows, _) = parse_words_tsv(tsv, CodePolicy::PINYIN).unwrap();
    assert_eq!(rows[0].text, "甲乙", "字段两侧的裸空格按既有语义剥除");
    assert_eq!(
        rows[1].text, "丙\t丁",
        "转义制表符须存活——它在 trim 阶段还不是空白"
    );
}

#[test]
fn normalize_code_folds_whitespace_and_is_idempotent_for_flat() {
    let p = CodePolicy::PINYIN;
    assert_eq!(normalize_code("ni  hao", p), "ni hao");
    assert_eq!(normalize_code("  ni hao  ", p), "ni hao");
    assert_eq!(normalize_code("abcd", p), "abcd");
}

/// ★ 撇号是**音节真值**，不是噪声：Rime 的 `ni'hao` 与 `ni hao` 表达同一件事。
/// 归一成空格后落库端才拆得出边界；留着它则会进 flat key，那条词永远查不到。
#[test]
fn pinyin_policy_upgrades_apostrophe_to_boundary() {
    let p = CodePolicy::PINYIN;
    assert_eq!(normalize_code("ni'hao", p), "ni hao");
    assert_eq!(normalize_code("xi'an'ning", p), "xi an ning");
    // 连写与混用都归一到单空格
    assert_eq!(normalize_code("ni''hao", p), "ni hao");
    assert_eq!(normalize_code("ni' hao", p), "ni hao");
    // 落库端据此拆出边界——这正是本条归一化的目的
    assert_eq!(
        crate::wdict::split_spaced_code(&normalize_code("ni'hao", p)),
        ("nihao".to_string(), 0b101)
    );
}

/// ★ 大写码落库后在设置页看得见却永远打不出来：查询侧判据全链路是
/// `is_ascii_lowercase`，而用户词表是裸字节前缀匹配、不做大小写归一。
#[test]
fn pinyin_policy_lowercases() {
    assert_eq!(normalize_code("NiHao", CodePolicy::PINYIN), "nihao");
    assert_eq!(normalize_code("NI HAO", CodePolicy::PINYIN), "ni hao");
}

/// ⚠️ 码表策略**不做任何改写**——快符码里就有 `@`，五笔方案也可能自定义字符集。
/// 与本策略引入之前逐字节一致。
#[test]
fn codetable_policy_leaves_code_untouched() {
    let p = CodePolicy::CODETABLE;
    assert_eq!(normalize_code("ni'hao", p), "ni'hao");
    assert_eq!(normalize_code("NiHao", p), "NiHao");
    assert_eq!(normalize_code("@ab", p), "@ab");
    // 空白折叠是两类策略共有的（列内排版噪声与引擎无关）
    assert_eq!(normalize_code("  a  b ", p), "a b");
}

#[test]
fn rime_explicit_columns_reorder() {
    let s = "---\nname: t\ncolumns:\n  - code\n  - text\n  - weight\n...\nnihao\t你好\t7\n";
    let (rows, _) = parse_words_rime(s, CodePolicy::PINYIN).unwrap();
    assert_eq!(rows[0].code, "nihao");
    assert_eq!(rows[0].text, "你好");
    assert_eq!(rows[0].weight, 7);
}

#[test]
fn rime_skips_incomplete_and_comment_lines() {
    let s = "---\nname: t\n...\n# 注释行\n\n只有词没有码\n好\tni hao\n";
    let (rows, skipped) = parse_words_rime(s, CodePolicy::PINYIN).unwrap();
    // 缺 code 的行跳过;缺 weight 列回退 0
    assert_eq!(rows.len(), 1);
    assert_eq!(skipped, 1);
    assert_eq!(rows[0].weight, 0);
}

#[test]
fn rime_missing_separator_is_error() {
    assert!(parse_words_rime("name: t\n你好\tni hao\t1\n", CodePolicy::PINYIN).is_err());
}

#[test]
fn rime_float_weight_truncates() {
    let s = "---\nname: t\n...\n你好\tni hao\t520.9\n";
    let (rows, _) = parse_words_rime(s, CodePolicy::PINYIN).unwrap();
    assert_eq!(rows[0].weight, 520, "浮点权重截断取整");
}

#[test]
fn tsv_basic_and_optional_weight() {
    let s = "# 注释\nnihao\t你好\t10\nshijie\t世界\n";
    let (rows, skipped) = parse_words_tsv(s, CodePolicy::PINYIN).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].weight, 10);
    assert_eq!(rows[1].weight, 0, "缺权重列回退 0");
}

#[test]
fn tsv_skips_bad_lines() {
    let s = "单列无制表符x\n你好\tnihao\t1\nab\t好\t2\n";
    let (rows, skipped) = parse_words_tsv(s, CodePolicy::PINYIN).unwrap();
    // 第 1 行列数不足;第 2 行 code 为汉字(列序颠倒防护)→ 均跳过
    assert_eq!(rows.len(), 1);
    assert_eq!(skipped, 2);
    assert_eq!(rows[0].code, "ab");
}

#[test]
fn auto_dispatch_matches_detection() {
    let (fmt, rows, _) = parse_words_auto(RIME_SAMPLE, CodePolicy::PINYIN).unwrap();
    assert_eq!(fmt, DictFormat::Rime);
    assert_eq!(rows.len(), 2);
    let (fmt, rows, _) = parse_words_auto("a\t工\t5\n", CodePolicy::PINYIN).unwrap();
    assert_eq!(fmt, DictFormat::Tsv);
    assert_eq!(rows[0].code, "a");
    assert!(parse_words_auto("既无头也无制表符\n", CodePolicy::PINYIN).is_err());
}
