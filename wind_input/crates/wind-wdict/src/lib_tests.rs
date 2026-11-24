use super::*;

/// 空格表示 ↔ 存储表示的往返。多音节必须无损，否则备份还原仍会丢边界。
#[test]
fn spaced_code_roundtrip() {
    // ni|hao → 起始字节位 {0,2}
    assert_eq!(join_code_by_boundary("nihao", 0b101), "ni hao");
    assert_eq!(split_spaced_code("ni hao"), ("nihao".into(), 0b101));
    // xi|an|ning → {0,2,4}
    assert_eq!(join_code_by_boundary("xianning", 0b10101), "xi an ning");
    assert_eq!(
        split_spaced_code("xi an ning"),
        ("xianning".into(), 0b10101)
    );
    // 变长音节 zhuang|ni → {0,6}
    assert_eq!(join_code_by_boundary("zhuangni", 0b1000001), "zhuang ni");
    assert_eq!(
        split_spaced_code("zhuang ni"),
        ("zhuangni".into(), 0b1000001)
    );
}

/// 无空格一律解释为「无边界信息」——五笔码、旧版导出的扁平拼音码走的都是这条路，
/// 结果与本次改动前完全等价（boundary=0 → 消费方降级回 DAG）。
#[test]
fn flat_code_means_unknown_boundary() {
    assert_eq!(split_spaced_code("abcd"), ("abcd".into(), 0)); // 五笔
    assert_eq!(split_spaced_code("nihao"), ("nihao".into(), 0)); // 旧版导出
    assert_eq!(split_spaced_code(""), ("".into(), 0));
    // 反向：无边界 / 单音节都不加空格（没有可插入的内部边界）
    assert_eq!(join_code_by_boundary("nihao", 0), "nihao");
    assert_eq!(join_code_by_boundary("ni", 0b1), "ni");
}

/// ⚠️ 单音节 `0b1` 不可逆（文档已声明）：join 后无空格，split 回来是 0。
/// 锁住这个**已知且可接受**的损失，避免日后被当成 bug 顺手「修」成 0b1 ——
/// 那会让五笔码也被判成单音节，语义反而错了。
#[test]
fn single_syllable_boundary_is_lossy_by_design() {
    let joined = join_code_by_boundary("ni", 0b1);
    assert_eq!(split_spaced_code(&joined).1, 0, "单音节边界不经文本往返");
}

/// 超过 64 字节的拼接：bitmask 装不下 → 整体降级为 0，不给半截错误边界。
#[test]
fn overlong_code_degrades_to_zero() {
    let spaced = ["zhuang"; 12].join(" "); // 12*6=72B
    let (flat, mask) = split_spaced_code(&spaced);
    assert_eq!(flat.len(), 72);
    assert_eq!(mask, 0, "超长码整体降级");
}

#[test]
fn phrases_wdict_roundtrip() {
    let rows = vec![
        PhraseIo {
            code: "bj".into(),
            text: "北京".into(),
            weight: 1000,
            position: 0,
            enabled: true,
        },
        PhraseIo {
            code: "ml".into(),
            text: "多行\n第二行\t带制表".into(),
            weight: 500,
            position: 2,
            enabled: false,
        },
    ];
    let s = export_phrases_wdict(&rows, "2026-07-02T00:00:00+08:00");
    assert!(s.contains("wind_dict:"));
    assert!(s.contains("--- !phrases"));
    let (parsed, skipped) = parse_phrases_wdict(&s).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(parsed, rows, "导出→解析应无损往返（含换行/制表）");
}

#[test]
fn parse_rejects_bad_version() {
    let bad = "wind_dict:\n  version: 2\n  sections:\n    phrases:\n      columns: [code, text, weight, position, enabled]\n\n--- !phrases\nx\t词\t1\t0\t1\n";
    assert!(parse_phrases_wdict(bad).is_err(), "version!=1 应拒绝");
}

#[test]
fn parse_tolerates_bad_lines() {
    let s = "wind_dict:\n  version: 1\n  sections:\n    phrases:\n      columns: [code, text, weight, position, enabled]\n\n--- !phrases\nok\t好\t1000\t0\t1\nbadline_no_tabs\nkw\t坏权重\tNaN\t0\t1\n";
    let (rows, skipped) = parse_phrases_wdict(s).unwrap();
    // 第 2 行列数不足跳过；第 3 行权重非数字 → 回退 0，仍收（不算跳过）
    assert_eq!(rows.len(), 2);
    assert_eq!(skipped, 1);
    assert_eq!(rows[0].code, "ok");
    assert_eq!(rows[1].weight, 0, "非法数字回退 0");
}

#[test]
fn escape_roundtrip() {
    for s in ["普通", "含\t制表", "含\n换行", "反斜杠\\结尾", "混\\t\n合"] {
        assert_eq!(unescape_field(&escape_field(s)), s, "往返应还原: {s:?}");
    }
}
#[test]
fn escape_only_special() {
    assert_eq!(escape_field("abc"), "abc");
    assert_eq!(escape_field("a\tb"), r"a\tb");
    assert_eq!(escape_field("a\nb"), r"a\nb");
    assert_eq!(escape_field(r"a\b"), r"a\\b");
}
/// 命令栏语法条目的反斜杠**零层数变化**：这是「文档写 `\\`、照做就对」的全部依据。
///
/// 两层各吃一个是本次修复前的实际行为——用户按文档写两个，落库剩一个，lexer 再把
/// `\n` 解成换行，路径静默坏掉且不报错。
#[test]
fn cmdbar_text_keeps_backslashes() {
    let src = r#"$CC("[打开]", open("D:\\notes\\temp"))"#;
    assert_eq!(escape_text_field(src), src, "出口不得再转义反斜杠");
    assert_eq!(unescape_text_field(src), src, "入口不得再还原反斜杠");
    // `\\n`（字面反斜杠 + n）不可被拆成「反斜杠 + 换行」
    let lit = r#"$CC("x", type("a\\nb"))"#;
    assert_eq!(unescape_text_field(lit), lit);
}

/// 普通词条不受影响：仍按原表转义，`C:\Users` 那类未知序列原样保留。
#[test]
fn plain_text_still_escaped() {
    assert_eq!(escape_text_field(r"C:\Users"), r"C:\\Users");
    assert_eq!(unescape_text_field(r"C:\\Users"), r"C:\Users");
    assert_eq!(unescape_text_field("甲\\n乙"), "甲\n乙");
}

/// 分隔符保护不可省：短语编辑框是多行输入，命令源码里的真换行若原样写进 TSV
/// 会把一条记录切成两行。**文件形态往返自洽**是这里的验收标准。
#[test]
fn cmdbar_text_protects_separators() {
    let stored = "$CC(\"打开\",\n\topen(\"https://x\"))";
    let file = escape_text_field(stored);
    assert!(
        !file.contains('\n') && !file.contains('\t'),
        "真换行/制表必须折成转义序列，否则 TSV 行列结构损坏: {file:?}"
    );
    assert_eq!(unescape_text_field(&file), stored, "文件形态往返应还原");
    assert_eq!(escape_text_field(&unescape_text_field(&file)), file);
}

#[test]
fn bool_format_parse() {
    assert_eq!(format_bool(true), "1");
    assert_eq!(format_bool(false), "0");
    assert!(parse_bool("1"));
    assert!(!parse_bool("0"));
    assert!(!parse_bool(""));
}

#[test]
fn words_wdict_roundtrip() {
    let rows = vec![
        WordIo {
            code: "a".into(),
            text: "工".into(),
            weight: 100,
            count: 42,
            boundary: None,
        },
        WordIo {
            code: "ml".into(),
            text: "多行\n带\t制表".into(),
            weight: 0,
            count: 0,
            boundary: None,
        },
    ];
    let s = export_words_wdict(&rows, "2026-07-11T00:00:00+08:00");
    assert!(s.contains("wind_dict:"));
    assert!(s.contains("--- !words"));
    assert!(s.contains("count"), "列头应含 count");
    let (parsed, skipped) = parse_words_wdict(&s).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(parsed, rows, "导出→解析应无损往返(含换行/制表/count)");
}

#[test]
fn words_wdict_reads_legacy_3col() {
    // 老文件：words 段只声明 3 列、每行 3 字段（无 count）→ count 回退 0，不跳过。
    let legacy = "wind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight]\n\n--- !words\na\t工\t100\n";
    let (rows, skipped) = parse_words_wdict(legacy).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].count, 0, "缺 count 列回退 0");
    assert_eq!(rows[0].weight, 100);
}

#[test]
fn words_parse_rejects_bad_version() {
    let bad = "wind_dict:\n  version: 2\n\n--- !words\na\t工\t1\n";
    assert!(parse_words_wdict(bad).is_err(), "version!=1 应拒绝");
}

#[test]
fn words_parse_tolerates_bad_lines() {
    let s = "wind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight, count]\n\n--- !words\nok\t好\t10\t0\nbadline\nkw\t坏权重\tNaN\t0\n";
    let (rows, skipped) = parse_words_wdict(s).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(skipped, 1, "单字段行跳过");
    assert_eq!(rows[1].weight, 0, "非法数字回退 0");
}

#[test]
fn dict_wdict_roundtrip_words_and_shadow() {
    let words = vec![WordIo {
        code: "a".into(),
        text: "工".into(),
        weight: 100,
        count: 7,
        boundary: None,
    }];
    let shadow = vec![
        ShadowActionIo {
            action: "pin".into(),
            code: "xhwp".into(),
            word: "[小鹤网盘]".into(),
            position: 0,
            cand_id: None,
        },
        ShadowActionIo {
            action: "pin".into(),
            code: "aaaa".into(),
            word: "日期".into(),
            position: 1,
            cand_id: Some("phrase:aaaa:date".into()),
        },
        ShadowActionIo {
            action: "del".into(),
            code: "j".into(),
            word: "见".into(),
            position: 0,
            cand_id: None,
        },
    ];
    let s = export_dict_wdict(&words, &shadow, "2026-07-14T00:00:00+08:00");
    assert!(s.contains("--- !words"));
    assert!(s.contains("--- !shadow"));

    let (pw, sk1) = parse_words_wdict(&s).unwrap();
    assert_eq!(sk1, 0);
    assert_eq!(pw, words, "words 段往返无损（含 count）");

    let (ps, sk2) = parse_shadow_wdict(&s).unwrap();
    assert_eq!(sk2, 0);
    // del 行 position 归 0（导出留空、解析回退 0）
    let expected: Vec<ShadowActionIo> = shadow
        .iter()
        .cloned()
        .map(|mut r| {
            if r.action == "del" {
                r.position = 0;
            }
            r
        })
        .collect();
    assert_eq!(ps, expected, "shadow 段往返无损（含 cand_id/del）");
}

#[test]
fn parse_shadow_skips_unknown_action() {
    let s = "wind_dict:\n  version: 1\n  sections:\n    shadow:\n      columns: [action, code, word, position, cand_id]\n\n--- !shadow\npin\txhwp\t网盘\t0\t\nbogus\tj\t见\t\t\ndel\tj\t见\t\t\n";
    let (rows, skipped) = parse_shadow_wdict(s).unwrap();
    assert_eq!(rows.len(), 2, "pin + del 收，未知 action 跳过");
    assert_eq!(skipped, 1);
}

#[test]
fn parse_shadow_missing_section_is_empty() {
    // 只有 words 段的老文件：解析 shadow 段返回空，不报错。
    let s = "wind_dict:\n  version: 1\n\n--- !words\na\t工\t1\t0\n";
    let (rows, skipped) = parse_shadow_wdict(s).unwrap();
    assert!(rows.is_empty());
    assert_eq!(skipped, 0);
}
