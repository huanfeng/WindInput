use super::*;

fn doc(text: &str) -> CharsetDoc {
    parse_doc(text).expect("解析")
}

#[test]
fn parses_head_and_body() {
    let d = doc("---\nkey: mine\nname: 我的\norder: 5\ndefault: rare\n...\n★\n⌘\n-☯\n");
    assert_eq!(d.def.key, "mine");
    assert_eq!(d.def.display_name(), "我的");
    assert_eq!(d.def.order_or_default(), 5);
    assert_eq!(d.def.default, Some(Commonality::Rare));
    assert_eq!(d.added, vec!["★", "⌘"]);
    assert_eq!(d.removed, vec!["☯"]);
}

/// 只调字段、不动列表的覆盖文件没有 `...`，整份都是 meta 头——用户层最常见的形态。
#[test]
fn a_head_only_file_is_valid() {
    let d = doc("key: emoji\ndefault: common\n");
    assert_eq!(d.def.key, "emoji");
    assert!(d.added.is_empty() && d.removed.is_empty());
}

#[test]
fn body_skips_comments_and_blanks() {
    let d = doc("key: k\n...\n# 注释\n\n★\n");
    assert_eq!(d.added, vec!["★"]);
}

#[test]
fn missing_key_is_rejected() {
    assert!(parse_doc("name: 没有 key\n").is_err());
}

/// 拼错字段名要报错而不是静默吞掉，否则得到一个「配了没反应」且毫无线索的类。
#[test]
fn unknown_field_is_rejected() {
    assert!(parse_doc("key: k\ndefualt: rare\n").is_err());
}

/// 字段级合并：用户层只写 `default`，出厂的 `ranges` 与列表必须留下。
/// 整文件覆盖会让这个类当场变成空集，且用户从此脱离出厂更新。
#[test]
fn overlay_keeps_what_it_did_not_mention() {
    let mut m = BTreeMap::new();
    apply_doc(
        &mut m,
        doc("key: emoji\nname: 表情\nranges: ['U+1F300-U+1FAFF']\ndefault: rare\n...\n😀\n😁\n"),
    );
    apply_doc(&mut m, doc("key: emoji\ndefault: common\n"));

    let e = &m["emoji"];
    assert_eq!(e.def.default, Some(Commonality::Common), "改过的字段生效");
    assert_eq!(
        e.def.ranges.as_deref(),
        Some(&["U+1F300-U+1FAFF".to_string()][..]),
        "★ 没提到的 ranges 必须留着"
    );
    assert_eq!(e.members.len(), 2, "★ 没提到的列表必须留着");
    assert_eq!(e.def.name.as_deref(), Some("表情"));
}

#[test]
fn overlay_list_is_incremental_by_default() {
    let mut m = BTreeMap::new();
    apply_doc(&mut m, doc("key: k\n...\n😀\n😁\n"));
    apply_doc(&mut m, doc("key: k\n...\n★\n-😀\n"));

    let e = &m["k"];
    assert!(e.members.contains("★") && e.members.contains("😁"));
    assert!(!e.members.contains("😀"), "被 - 前缀移除");
    assert!(
        e.removed.contains("😀"),
        "★ 移除记录要留着——它还要作用于 file: 引用的外部字表"
    );
}

/// `replace: true` 是「我要接管整份字表」的显式声明，从此不跟随出厂更新。
#[test]
fn replace_wipes_the_lower_layers_list() {
    let mut m = BTreeMap::new();
    apply_doc(&mut m, doc("key: k\nname: 出厂\n...\n😀\n😁\n"));
    apply_doc(&mut m, doc("key: k\nreplace: true\n...\n★\n"));

    let e = &m["k"];
    assert_eq!(e.members.iter().cloned().collect::<Vec<_>>(), vec!["★"]);
    assert_eq!(
        e.def.name.as_deref(),
        Some("出厂"),
        "replace 只管列表，不动 meta"
    );
}

/// 先移除、后重新加入 ⇒ 该条重新生效，且不再留在排除集里
/// （否则 `file:` 引用的那份外部字表里的同一条会被误删）。
#[test]
fn re_adding_a_removed_entry_clears_the_removal() {
    let mut m = BTreeMap::new();
    apply_doc(&mut m, doc("key: k\n...\n-☯\n"));
    apply_doc(&mut m, doc("key: k\n...\n☯\n"));
    let e = &m["k"];
    assert!(e.members.contains("☯"));
    assert!(!e.removed.contains("☯"));
}

/// 合并宏漏写字段是静默的，用「全字段非 None 的 overlay 应整体生效」把它变成会红的测试。
#[test]
fn merge_covers_every_field() {
    let full = CharsetDef {
        key: "k".into(),
        name: Some("n".into()),
        ranges: Some(vec!["U+1-U+2".into()]),
        file: Some("f.txt".into()),
        scope: Some(ScopeKind::Han),
        default: Some(Commonality::Rare),
        outside: Some(Commonality::Common),
        order: Some(7),
        no_freq: Some(true),
        in_rare: Some(true),
        enabled: Some(false),
        replace: Some(true),
    };
    let mut base = CharsetDef {
        key: "k".into(),
        ..Default::default()
    };
    base.merge_from(full.clone());
    assert_eq!(base, full, "有字段没被 merge_from 覆盖到");
}

#[test]
fn parses_range_forms() {
    assert_eq!(parse_range("U+4E00-U+9FFF"), Ok((0x4E00, 0x9FFF)));
    assert_eq!(parse_range("U+1F600"), Ok((0x1F600, 0x1F600)), "单点");
    assert_eq!(
        parse_range("  u+20 - u+7f  "),
        Ok((0x20, 0x7F)),
        "空白与小写"
    );
}

#[test]
fn rejects_bad_ranges() {
    for bad in ["4E00-9FFF", "U+9FFF-U+4E00", "U+ZZZZ", "U+110000", "U+"] {
        assert!(parse_range(bad).is_err(), "{bad} 应被拒绝");
    }
}

/// 一条写错不得连累同类的其余段——否则一个笔误会静默抹掉整类字符。
#[test]
fn one_bad_range_does_not_drop_the_rest() {
    let raw = vec![
        "U+2600-U+26FF".to_string(),
        "垃圾".to_string(),
        "U+1F300".to_string(),
    ];
    assert_eq!(
        parse_ranges("t", &raw),
        vec![(0x2600, 0x26FF), (0x1F300, 0x1F300)]
    );
}

#[test]
fn scans_only_yaml_and_in_a_stable_order() {
    let dir = std::env::temp_dir().join(format!("charsets_scan_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.yaml"), "key: b\n").unwrap();
    std::fs::write(dir.join("a.yaml"), "key: a\n").unwrap();
    std::fs::write(dir.join("note.md"), "not a charset").unwrap();
    std::fs::write(dir.join("c.yaml.bak"), "key: nope\n").unwrap();

    let docs = load_layer(&dir);
    let keys: Vec<&str> = docs.iter().map(|d| d.def.key.as_str()).collect();
    assert_eq!(keys, vec!["a", "b"], "只扫 .yaml，且按文件名定序");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 一个写坏的文件不该让整层失效。
#[test]
fn a_broken_file_does_not_kill_the_layer() {
    let dir = std::env::temp_dir().join(format!("charsets_broken_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ok.yaml"), "key: ok\n").unwrap();
    std::fs::write(dir.join("bad.yaml"), "key: [不是字符串\n").unwrap();

    let docs = load_layer(&dir);
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].def.key, "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── 列表体的两级切分 ───────────────────────────────────────────────────

fn members_of(body: &str) -> Vec<String> {
    let doc = parse_doc(&format!("---\nkey: t\n...\n{body}")).unwrap();
    doc.added
}

/// 一行写了多个字：切开，而不是当成一个永远匹配不上的成员。
///
/// 用户可能照 `common_chars.txt` 的习惯连写，当成一个成员的话那一行静默全不生效。
#[test]
fn a_line_with_several_chars_is_split_rather_than_taken_whole() {
    assert_eq!(members_of("的一是"), vec!["的", "一", "是"]);
    // 空白也能分隔，两种写法可以混用。
    assert_eq!(members_of("的 一是\n不"), vec!["的", "一", "是", "不"]);
}

/// 多码位簇整体算一个成员，不会被拆散。
///
/// 拆散的话配置里写 `⚽️` 匹配不上候选里那个簇。
#[test]
fn a_multi_code_point_cluster_stays_whole() {
    assert_eq!(members_of("⚽\u{FE0F}"), vec!["⚽\u{FE0F}"]);
    assert_eq!(
        members_of("👨\u{200D}👩\u{200D}👧"),
        vec!["👨\u{200D}👩\u{200D}👧"]
    );
    assert_eq!(members_of("1\u{FE0F}\u{20E3}"), vec!["1\u{FE0F}\u{20E3}"]);
}

/// 空白分隔存在的唯一理由：相邻的区域指示符会合成国旗。
///
/// `🇨🇳` 连写是一面旗（一个成员），两个区域指示符字母只能用空格分开。
#[test]
fn adjacent_regional_indicators_need_a_space_to_stay_apart() {
    assert_eq!(members_of("🇨🇳").len(), 1, "连写合成国旗，是一个成员");
    assert_eq!(members_of("🇨 🇳").len(), 2, "要分开就得用空格");
}

/// `-` 前缀作用于整个词。
#[test]
fn the_remove_prefix_covers_the_whole_word() {
    let doc = parse_doc("---\nkey: t\n...\n-的一是 好\n").unwrap();
    assert_eq!(doc.removed, vec!["的", "一", "是"]);
    assert_eq!(doc.added, vec!["好"]);
}

// ── 用户层：渲染 / 编辑视图 / diff ─────────────────────────────────────

fn doc_of(key: &str) -> CharsetDoc {
    CharsetDoc {
        def: CharsetDef {
            key: key.to_string(),
            ..Default::default()
        },
        added: Vec::new(),
        removed: Vec::new(),
    }
}

/// 渲染只写用户改过的字段，不带出厂值。
///
/// 写全量会把当前的 `ranges` / `order` 固化进用户层，出厂以后的更新这个用户永远拿不到。
#[test]
fn rendering_writes_only_what_the_user_changed() {
    let mut doc = doc_of("emoji");
    doc.def.default = Some(Commonality::Rare);
    let text = render_doc(&doc).unwrap();
    assert!(text.contains("key: emoji"));
    assert!(text.contains("default: rare"));
    for absent in [
        "ranges", "order", "file", "scope", "outside", "no_freq", "in_rare",
    ] {
        assert!(
            !text.contains(&format!("\n{absent}:")),
            "没改过的 {absent} 不该被写进用户层（会固化出厂值）"
        );
    }
    assert!(!text.contains("\n...\n"), "没有成员增删就不该有列表体");
}

/// 渲染出来的东西读得回来——写端与读端用的是同一套语法。
#[test]
fn a_rendered_doc_round_trips() {
    let mut doc = doc_of("mine");
    doc.def.name = Some("我的类".into());
    doc.def.order = Some(7);
    doc.def.no_freq = Some(true);
    doc.added = vec!["★".into(), "☆".into()];
    doc.removed = vec!["☀".into()];
    let back = parse_doc(&render_doc(&doc).unwrap()).unwrap();
    assert_eq!(back, doc);
}

/// 渲染出来的文本带头注释，说明「只含改过的字段」——备份包里用户会直接看到它。
#[test]
fn the_rendered_text_explains_it_is_sparse() {
    let mut doc = doc_of("emoji");
    doc.def.order = Some(1);
    let text = render_doc(&doc).unwrap();
    assert!(text.starts_with('#'), "开头得是注释");
    assert!(text.contains("只含你改过的字段"));
    assert_eq!(
        text.lines().next(),
        Some(STORED_DOC_BANNER),
        "第一行是说明行"
    );
    assert!(
        STORED_DOC_BANNER.starts_with("# 清风输入法 · ")
            && STORED_DOC_BANNER.ends_with("。可手动编辑。")
    );
}

/// 危险的 key 在入口就被拒，不做「安全化」。
///
/// 安全化会让两个不同的 key 落到同一个文件上，后写的静默覆盖先写的。
///
/// `a/b` 与 `a\b` 在所有平台上都必须被拒（`std::path::is_separator` 在 Linux 不认 `\`）；
/// 挂了要改守卫，不是删这两条。
#[test]
fn dangerous_keys_are_rejected_not_sanitized() {
    for bad in [
        "../escape",
        "a/b",
        "a\\b",
        "C:stream",
        ".",
        "..",
        "",
        " lead",
        "trail ",
        "wi*ld",
    ] {
        assert!(!is_valid_key(bad), "「{bad}」不该被当作合法 key");
    }
    for good in ["emoji", "common_han", "表情符号", "my-set_1"] {
        assert!(is_valid_key(good), "「{good}」该是合法 key");
    }
    assert!(
        render_doc(&doc_of("../escape")).is_err(),
        "非法 key 必须渲染失败"
    );
}

/// 库里的文本按条解析：坏的一条跳过、库键与文本 key 不一致的跳过，其余照常。
#[test]
fn stored_docs_skip_the_broken_and_the_mismatched() {
    let docs = parse_stored_docs([
        ("emoji", "key: emoji\ndefault: rare\n"),
        ("broken", "key: [\n"),
        ("mismatch", "key: something_else\n"),
        ("mine", "key: mine\n...\n★\n"),
    ]);
    let keys: Vec<&str> = docs.iter().map(|d| d.def.key.as_str()).collect();
    assert_eq!(keys, ["emoji", "mine"]);
    assert_eq!(docs[1].added, vec!["★".to_string()]);
}

/// 行尾不挑食：字符类文件无论 LF / CRLF / 孤立 CR，解析结果一致。
///
/// 孤立 `\r` 会让整份文件算成一行、找不到 `...` 分隔行，一个类都出不来。归一化放在
/// `split_head_body`（`parse_doc` / `parse_docs` 的共同下游）。
///
/// 样本在代码里构造，不要改用仓库 fixture（git core.autocrlf 会按平台改行尾）。
#[test]
fn line_endings_do_not_change_the_result() {
    // 带 `...` 分隔行：head 是 YAML、body 是逐行成员，两边都依赖分行
    let lf = "key: 测试类\nranges:\n  - U+4E00-U+4E05\n...\n甲\n乙\n";
    let want = parse_doc(lf).expect("LF 版应能解析");
    // 精确比，不能用 contains——孤立 \r 会把整份挤成一行，key 变成一长串，
    // 那串里照样「包含」类名，contains 判不出来
    assert_eq!(want.def.key, "测试类", "样本本身要能解析出干净的 key");
    for (tag, text) in [
        ("CRLF", lf.replace('\n', "\r\n")),
        ("CR", lf.replace('\n', "\r")),
    ] {
        let got = parse_doc(&text).unwrap_or_else(|e| panic!("{tag}: 解析失败 {e}"));
        assert_eq!(got.def.key, want.def.key, "{tag}: key 与 LF 版不同");
        assert_eq!(
            (&got.added, &got.removed),
            (&want.added, &want.removed),
            "{tag}: 成员与 LF 版不同"
        );
    }
}
