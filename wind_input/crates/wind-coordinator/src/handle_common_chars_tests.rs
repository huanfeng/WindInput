use super::{common_char_of, parse_common_chars_file, partition_for_export};

/// 导出分段：单字符进文本串，多码位单独成项。
///
/// ★ 关键在最后一组：`🇨` 与 `🇳` **各自都是单码位**，却不能拼进同一个文本串——
/// 拼了就合成一个国旗 `🇨🇳`，读回来少一条、多一条本不存在的。判据是往返自检而不是
/// 「数码位」，故不必列举「哪些单码位是危险的」。
#[test]
fn export_puts_plain_chars_in_text_and_sequences_in_array() {
    // 常见情况：全是单字符 ⇒ 数组段为空 ⇒ 导出文件里根本不会出现它。
    let (text, seq) = partition_for_export(&[
        "槮".to_string(),
        "鬱".to_string(),
        "ㄅ".to_string(),
        "⿰".to_string(),
    ]);
    assert_eq!(text, "槮鬱ㄅ⿰");
    assert!(seq.is_empty(), "没有多码位字符时不该产生数组段");

    // 多码位一律单列。
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let ball = "\u{26BD}\u{FE0F}";
    let (text, seq) = partition_for_export(&[
        "槮".to_string(),
        ball.to_string(),
        "鬱".to_string(),
        family.to_string(),
    ]);
    assert_eq!(text, "槮鬱");
    assert_eq!(seq, vec![ball.to_string(), family.to_string()]);

    // 单码位但会与相邻字符黏连的：必须挤出去。
    let (text, seq) = partition_for_export(&[
        "\u{1F1E8}".to_string(), // 🇨
        "\u{1F1F3}".to_string(), // 🇳
    ]);
    assert_eq!(text, "\u{1F1E8}", "第一个安全");
    assert_eq!(
        seq,
        vec!["\u{1F1F3}".to_string()],
        "第二个接上去会合成国旗，必须单列"
    );
}

#[test]
fn accepts_single_han_and_pua() {
    for s in ["我", "鬱", "\u{E831}", "\u{20000}"] {
        assert_eq!(common_char_of(s), Some(s), "{s} 应放行（键就是它自己）");
    }
}

/// 多码位的一个图形也算一个字符——用户报的 `⚽️`，以及 ZWJ 序列、国旗、keycap。
///
/// 键是**整簇**：只登记组合，单个成员不受牵连（见 wind-candidate 的
/// `multi_code_point_cluster_override_takes_effect`）。
#[test]
fn accepts_multi_code_point_graphemes() {
    for (s, what) in [
        ("\u{26BD}\u{FE0F}", "变体选择符 ⚽️"),
        ("\u{1F44D}\u{1F3FB}", "肤色修饰符"),
        (
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "ZWJ 家庭序列",
        ),
        ("\u{1F1E8}\u{1F1F3}", "国旗"),
        ("\u{0031}\u{FE0F}\u{20E3}", "keycap"),
    ] {
        assert_eq!(common_char_of(s), Some(s), "{what} 应放行");
    }
}

#[test]
fn rejects_phrases() {
    // 「常用」是字级属性，词组没有——给词组存覆盖，读端逐字判定时永远看不到它。
    assert_eq!(common_char_of("我们"), None);
    assert_eq!(common_char_of(""), None);
}

/// 非汉字字符**现在一律放行**（issue #83：词库管理全范围放开）。
///
/// 取代旧的 `rejects_out_of_scope_chars`——那条钉的是相反的行为，理由是读端会忽略
/// 域外覆盖、放行等于存死记录。读端改成覆盖优先后那个理由不再成立，两条断言必然互斥。
/// 用户点名要能关掉的字根、间架结构符、注音、假名，全都在这一批里。
#[test]
fn accepts_non_han_chars() {
    for s in ["、", "，", "①", "℃", "あ", "ㄅ", "⿰", "😀", "A", "7"] {
        assert_eq!(common_char_of(s), Some(s), "{s} 应放行");
    }
}

/// 空白与控制字符仍然拒绝——数据卫生，不是作用域判断：它们不会作为候选出现，
/// 登记进去只会在列表里显示成一行空白，用户既看不出是什么也点不掉。
#[test]
fn rejects_blank_and_control_chars() {
    for s in [" ", "\t", "\u{3000}", "\u{0}"] {
        assert_eq!(common_char_of(s), None, "{s:?} 不该放行");
    }
}

/// 导入文件的行尾不挑食。
///
/// 这个函数靠「首个有效行」判 TOML 还是 JSONL——孤立 `\r` 会让整份文件算成一行，
/// **格式在第一步就判错**。而注释里说得明白，用户手上这份是从备份包里掏出来的，
/// 经过解压与编辑器，行尾不由我们决定。
///
/// ⚠️ 样本在代码里构造，不要改用仓库 fixture（git core.autocrlf 会按平台改行尾）。
#[test]
fn import_file_line_endings_do_not_change_the_result() {
    let jsonl = "{\"ch\":\"的\",\"common\":true}\n{\"ch\":\"兲\",\"common\":false}\n";
    let toml = "# 我的常用字调整\nwind_common_chars = 1\ncommon = \"的一\"\nrare = \"兲\"\n";
    for lf in [jsonl, toml] {
        let want = parse_common_chars_file(lf).expect("LF 版应能解析");
        for (tag, text) in [
            ("CRLF", lf.replace('\n', "\r\n")),
            ("CR", lf.replace('\n', "\r")),
        ] {
            let got = parse_common_chars_file(&text)
                .unwrap_or_else(|e| panic!("{tag}: 解析失败 {e}（样本 {lf:?}）"));
            assert_eq!(got.entries, want.entries, "{tag}: 条目与 LF 版不同");
            assert_eq!(got.skipped, want.skipped, "{tag}: 跳过行与 LF 版不同");
        }
    }
}
