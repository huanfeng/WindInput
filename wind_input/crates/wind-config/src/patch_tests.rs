use super::*;

/// 默认配置序列化成的值树，充当测试里的「当前生效配置」。
fn default_tree() -> toml::Value {
    toml::Value::try_from(Config::default()).expect("serialize default config")
}

fn preview_text(text: &str) -> Vec<PatchEntry> {
    let fragment = parse_fragment(text).expect("片段应能解析");
    preview(&fragment, &default_tree())
}

#[test]
fn parse_failure_is_whole_fragment_error() {
    assert!(parse_fragment("= not toml =").is_err());
    assert!(parse_fragment("[unclosed\n").is_err());
}

/// 降级闸：Map 条目的合并种子来自生效配置，坏段处那张表是出厂值 ⇒ 必须拒绝应用，
/// 否则「加一条映射」会把用户已有的整表抹掉（永久、无痕）。标量键则不受牵连。
#[test]
fn degraded_map_seed_is_rejected_but_scalars_still_apply() {
    let mut entries = preview_text(
        "[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n\n[ui.candidate]\nper_page = 9\n",
    );
    assert!(
        entries.iter().all(|e| e.error.is_none()),
        "前置：这份片段本身合法，后面的 error 只能来自降级闸"
    );

    // `input.punct` 整段降级 ⇒ `custom_mappings` 的当前表是出厂空表。
    let deg = ConfigDegradation {
        sections: vec!["input.punct".into()],
        total_fallback: false,
        unparsable: Vec::new(),
    };
    mark_degraded_seeds(&mut entries, &deg);

    let map_entry = entries
        .iter()
        .find(|e| e.key == "input.punct.custom_mappings")
        .expect("Map 条目应在");
    assert!(
        map_entry.error.is_some(),
        "★ 种子不可信的 Map 条目必须报错——放行它就是把用户已有的映射整表抹掉"
    );

    // 标量键不受牵连：它的落盘值是片段里的显式新值，与种子无关。
    // 连它一起拦会让降级期间「导入一份只改几个开关的配置」整个堵死。
    let scalar = entries
        .iter()
        .find(|e| e.key == "ui.candidate.per_page")
        .expect("标量条目应在");
    assert!(scalar.error.is_none(), "标量键不该被降级闸拦下");

    // 对照：降级发生在别处时，同一个 Map 条目必须放行（否则闸恒真、上面那条是假绿）。
    let mut entries2 = preview_text("[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n");
    mark_degraded_seeds(
        &mut entries2,
        &ConfigDegradation {
            sections: vec!["ui.font".into()],
            total_fallback: false,
            unparsable: Vec::new(),
        },
    );
    assert!(
        entries2[0].error.is_none(),
        "无关段降级不得牵连 Map 条目，实得 {:?}",
        entries2[0].error
    );

    // 整份回落 ⇒ 一切种子不可信。
    let mut entries3 = preview_text("[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n");
    mark_degraded_seeds(
        &mut entries3,
        &ConfigDegradation {
            sections: Vec::new(),
            total_fallback: true,
            unparsable: Vec::new(),
        },
    );
    assert!(entries3[0].error.is_some(), "整份回落时必须拦下");
}

/// 已有 error 的条目不被降级闸改写：先报的那个错才是作者要看的根因，
/// 覆盖掉等于把「你这个键写错了」换成「系统降级了」，排查方向被带偏。
#[test]
fn existing_error_is_not_overwritten() {
    let mut entries = preview_text("[input.punct.custom_mappings]\n\"'1\" = 42\n");
    let first = entries[0].error.clone();
    assert!(first.is_some(), "前置：值类型不合法本就该报错");
    mark_degraded_seeds(
        &mut entries,
        &ConfigDegradation {
            sections: vec!["input.punct".into()],
            total_fallback: false,
            unparsable: Vec::new(),
        },
    );
    assert_eq!(entries[0].error, first, "原有 error 必须原样保留");
}

/// Map 键（custom_mappings）逐条目产出：`key` 恒为父 Map 键，条目名走 `map_entry`，
/// 值是条目自身的值（不是整表）。
#[test]
fn map_key_flattens_per_entry() {
    let entries =
        preview_text("[input.punct.custom_mappings]\n\"'1\" = [\"①\"]\n\"'2\" = [\"②\"]\n");
    assert_eq!(entries.len(), 2, "两个条目应产出两条");
    for e in &entries {
        assert_eq!(e.key, "input.punct.custom_mappings", "key 恒为父 Map 键");
        assert!(e.error.is_none(), "{:?}", e.error);
    }
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e.map_entry.as_deref().expect("Map 条目须带 map_entry"))
        .collect();
    assert_eq!(names, vec!["'1", "'2"]);
    assert_eq!(
        entries[0].next,
        toml::Value::Array(vec![toml::Value::String("①".into())]),
        "next 是条目自身的值,不是整表"
    );
}

/// 条目名可以含 `.`（`custom_mappings` 的源字符就是标点）。条目名并进点分键
/// 就再也拆不回来,故必须由 `map_entry` 独立承载。
#[test]
fn map_entry_name_with_dot_survives() {
    let entries = preview_text("[input.punct.custom_mappings]\n\".\" = [\"。\"]\n");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "input.punct.custom_mappings");
    assert_eq!(entries[0].map_entry.as_deref(), Some("."));
    assert!(entries[0].error.is_none(), "{:?}", entries[0].error);
}

/// 条目值类型错 → 错误归到该条目（不是整表报错,也不牵连同表的合法条目）。
#[test]
fn map_entry_type_error_is_attributed_to_that_entry() {
    let entries = preview_text("[keys.key_actions]\nbacktick = \"english\"\nf4 = 5\n");
    assert_eq!(entries.len(), 2);
    let ok = entries
        .iter()
        .find(|e| e.map_entry.as_deref() == Some("backtick"))
        .unwrap();
    assert!(ok.error.is_none(), "合法条目不应被牵连: {:?}", ok.error);
    let bad = entries
        .iter()
        .find(|e| e.map_entry.as_deref() == Some("f4"))
        .unwrap();
    let err = bad.error.as_deref().expect("整数条目值应被拒绝");
    assert!(err.contains("类型或取值不合法"), "{err}");
}

/// 空条目名报错而不是当正常条目:没有任何按键名/方案 id/源字符会是空串,
/// 放行只会写进一条永远匹配不上、也删不掉的死条目。
#[test]
fn empty_map_entry_name_is_rejected() {
    let entries = preview_text("[keys.key_actions]\n\"\" = \"english\"\n");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].map_entry.as_deref(), Some(""));
    assert_eq!(entries[0].error.as_deref(), Some("条目名不能为空"));
}

/// Map 键下的空表 = no-op（不产出条目）。「空表 = 清空」是脚枪,片段没有删条目的语义。
#[test]
fn empty_map_table_is_noop() {
    assert!(preview_text("[keys.key_actions]\n").is_empty());
    assert!(preview_text("keys.key_actions = {}\n").is_empty());
}

/// Map 键给了非表值 → 落回单条,按 Map 类型报「类型应为 table」。
#[test]
fn map_key_with_non_table_value_reports_type_error() {
    let entries = preview_text("keys.key_actions = 5\n");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].map_entry.is_none());
    let err = entries[0].error.as_deref().expect("整数应被拒绝");
    assert!(err.contains("类型应为 table"), "{err}");
}

/// Map 条目的 current 取自当前表内的同名条目：缺席 = None（新增）,在场 = 原值。
#[test]
fn map_entry_current_comes_from_existing_table() {
    let mut tree = default_tree();
    if let toml::Value::Table(t) = &mut tree {
        let mut m = toml::map::Map::new();
        m.insert("backtick".into(), toml::Value::String("english".into()));
        crate::config::set_nested(t, &["keys", "key_actions"], toml::Value::Table(m));
    }
    let fragment =
        parse_fragment("[keys.key_actions]\nbacktick = \"半角\"\nf4 = \"english\"\n").unwrap();
    let entries = preview(&fragment, &tree);
    let old = entries
        .iter()
        .find(|e| e.map_entry.as_deref() == Some("backtick"))
        .unwrap();
    assert_eq!(
        old.current.as_ref().and_then(|v| v.as_str()),
        Some("english"),
        "已有条目应报当前值"
    );
    let new = entries
        .iter()
        .find(|e| e.map_entry.as_deref() == Some("f4"))
        .unwrap();
    assert!(new.current.is_none(), "缺席条目 = 新增,无当前值");
}

// ── writes()：落盘键值折算 ──

/// Map 合并：当前表既有条目保留,同名条目被片段覆盖,父键只产出一条。
#[test]
fn writes_merges_map_keeping_existing_entries() {
    let mut tree = default_tree();
    if let toml::Value::Table(t) = &mut tree {
        let mut m = toml::map::Map::new();
        m.insert("backtick".into(), toml::Value::String("english".into()));
        m.insert("f2".into(), toml::Value::String("半角".into()));
        crate::config::set_nested(t, &["keys", "key_actions"], toml::Value::Table(m));
    }
    let fragment =
        parse_fragment("[keys.key_actions]\nbacktick = \"全角\"\nf4 = \"english\"\n").unwrap();
    let entries = preview(&fragment, &tree);
    let w = writes(&entries, &tree);
    assert_eq!(w.len(), 1, "同一 Map 父键只落一条,实际: {w:?}");
    assert_eq!(w[0].0, "keys.key_actions");
    let t = w[0].1.as_table().expect("Map 落盘值须是整表");
    assert_eq!(
        t.get("f2").and_then(|v| v.as_str()),
        Some("半角"),
        "未提及的条目保留"
    );
    assert_eq!(
        t.get("backtick").and_then(|v| v.as_str()),
        Some("全角"),
        "同名条目覆盖"
    );
    assert_eq!(
        t.get("f4").and_then(|v| v.as_str()),
        Some("english"),
        "新条目并入"
    );
    assert_eq!(t.len(), 3);
}

/// 当前树里该 Map 键缺席（或是空表）时,合并结果 = 片段条目本身。
#[test]
fn writes_seeds_empty_map_from_fragment_only() {
    let tree = default_tree();
    let fragment = parse_fragment("[keys.key_actions]\nf4 = \"english\"\n").unwrap();
    let entries = preview(&fragment, &tree);
    let w = writes(&entries, &tree);
    assert_eq!(w.len(), 1);
    let t = w[0].1.as_table().unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!(t.get("f4").and_then(|v| v.as_str()), Some("english"));
}

/// 标量与多个 Map 键混合：标量原样一条,每个 Map 父键各合并成一条,顺序 = 首次出现顺序。
#[test]
fn writes_groups_scalars_and_multiple_maps() {
    let tree = default_tree();
    let fragment = parse_fragment(
        "ui.candidate.per_page = 9\n\
             [keys.key_actions]\nf4 = \"english\"\nf5 = \"半角\"\n\
             [keys.session_actions]\nf6 = \"english\"\n",
    )
    .unwrap();
    let entries = preview(&fragment, &tree);
    assert_eq!(entries.len(), 4, "条目仍逐条,实际: {entries:?}");
    let w = writes(&entries, &tree);
    let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "keys.key_actions",
            "keys.session_actions",
            "ui.candidate.per_page"
        ],
        "每个 Map 父键一条 + 标量一条（顺序 = TOML 表遍历序）"
    );
    let ka = w
        .iter()
        .find(|(k, _)| k == "keys.key_actions")
        .unwrap()
        .1
        .as_table()
        .unwrap();
    assert_eq!(ka.len(), 2, "同一 Map 的两个条目并进同一张表");
    let per_page = w
        .iter()
        .find(|(k, _)| k == "ui.candidate.per_page")
        .unwrap();
    assert_eq!(per_page.1.as_integer(), Some(9), "标量原样落盘");
}

/// StructList 键（mix_modes）同理：数组整体是一个值，元素不展开。
#[test]
fn struct_list_stops_flatten() {
    let entries = preview_text("[[schema.mix_modes]]\nid = \"m1\"\nname = \"测试\"\n");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "schema.mix_modes");
    assert!(entries[0].error.is_none(), "{:?}", entries[0].error);
}

#[test]
fn unknown_key_reported_per_entry() {
    let entries = preview_text("[input.foo]\nbar = 1\n");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "input.foo.bar");
    assert_eq!(entries[0].error.as_deref(), Some("未知配置键"));
    assert!(entries[0].current.is_none(), "未知键无当前值");
}

/// 旧的模式级注释模板键已迁入 `[ui.comment]`，片段里再写按未知键报。
#[test]
fn retired_mode_comment_template_is_unknown() {
    let entries = preview_text("[input.temp_english]\ncomment_template_vertical = \"${dict}\"\n");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].error.as_deref(), Some("未知配置键"));
}

#[test]
fn registered_key_rejects_type_mismatch_and_enum_out_of_range() {
    let entries = preview_text("[ui.candidate]\nper_page = \"seven\"\nlayout = \"diagonal\"\n");
    assert_eq!(entries.len(), 2);
    let per_page = entries
        .iter()
        .find(|e| e.key == "ui.candidate.per_page")
        .unwrap();
    assert!(per_page.error.as_deref().unwrap().contains("类型应为"));
    let layout = entries
        .iter()
        .find(|e| e.key == "ui.candidate.layout")
        .unwrap();
    assert!(layout.error.as_deref().unwrap().contains("不在允许集合"));
}

/// diff 正确性：current 取自传入的当前值树，next 取自片段。
#[test]
fn diff_reports_current_and_next() {
    let entries = preview_text("[ui.candidate]\nper_page = 9\n");
    assert_eq!(entries.len(), 1);
    let default_per_page =
        crate::config::get_nested(&default_tree(), &["ui", "candidate", "per_page"]).cloned();
    assert_eq!(entries[0].current, default_per_page);
    assert_eq!(entries[0].next.as_integer(), Some(9));
    assert!(entries[0].error.is_none());
}

/// 点分键与嵌套表两种写法产出完全相同的条目集（TOML 解析层已归一）。
#[test]
fn dotted_and_nested_forms_are_equivalent() {
    let dotted = preview_text("ui.candidate.per_page = 9\ninput.auto_pair.chinese = false\n");
    let nested = preview_text("[ui.candidate]\nper_page = 9\n[input.auto_pair]\nchinese = false\n");
    let mut a: Vec<(String, toml::Value)> = dotted.into_iter().map(|e| (e.key, e.next)).collect();
    let mut b: Vec<(String, toml::Value)> = nested.into_iter().map(|e| (e.key, e.next)).collect();
    a.sort_by(|x, y| x.0.cmp(&y.0));
    b.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(a, b);
}

/// 未知路径上的空表不产出条目（无叶子可报），空片段同理。
#[test]
fn empty_fragment_and_empty_unknown_table_yield_no_entries() {
    assert!(preview_text("").is_empty());
    assert!(preview_text("[input.foo]\n").is_empty());
}

// ── `[package]` 保留段与说明元信息 ──

fn info_of(text: &str) -> Option<PatchInfo> {
    let fragment = parse_fragment(text).expect("片段应能解析");
    extract_info(&fragment).expect("说明元信息应合法")
}

fn info_err(text: &str) -> String {
    let fragment = parse_fragment(text).expect("片段应能解析");
    extract_info(&fragment).expect_err("说明元信息应被拒绝")
}

/// 保留段整段跳过：不产出配置条目,也不报「未知配置键」。同片段里的真配置键照常展平。
#[test]
fn reserved_section_is_skipped_by_flatten() {
    let entries = preview_text(
        "[package]\ntitle = \"演示\"\ndescription = \"说明\"\n\
             [ui.candidate]\nper_page = 9\n",
    );
    assert_eq!(entries.len(), 1, "只应剩真配置键,实际: {entries:?}");
    assert_eq!(entries[0].key, "ui.candidate.per_page");
    assert!(entries[0].error.is_none(), "{:?}", entries[0].error);
}

/// 只有 `[package]` 段的片段 → entries 为空,「片段为空」的既有语义不变
/// （applyPatch 视作成功 no-op）。
#[test]
fn fragment_with_only_reserved_section_has_no_entries() {
    assert!(preview_text("[package]\ntitle = \"只有说明\"\n").is_empty());
    // 段内子键叫得跟配置域一模一样也照样跳过（`package.ui` 不是 `ui`）。
    assert!(preview_text("[package.ui]\ncandidate = 9\n").is_empty());
}

/// title / description 正常提取,description 可多行。
#[test]
fn extract_info_reads_title_and_multiline_description() {
    let info =
        info_of("[package]\ntitle = \"快符方案\"\ndescription = \"\"\"\n第一行\n第二行\n\"\"\"\n")
            .expect("两字段都写了应返回 Some");
    assert_eq!(info.title.as_deref(), Some("快符方案"));
    assert_eq!(
        info.description.as_deref(),
        Some("第一行\n第二行"),
        "多行说明保留内部换行,首尾空白 trim"
    );
}

/// 段不存在 / 两字段都缺省 → None（RPC 响应里就没有 info 字段）。
#[test]
fn extract_info_absent_yields_none() {
    assert!(info_of("ui.candidate.per_page = 9\n").is_none());
    assert!(info_of("[package]\n").is_none());
    assert!(
        info_of("[package]\nkind = \"schema_text\"\n").is_none(),
        "只有未知子键 = 没写说明"
    );
}

/// 段内未知子键一律忽略（向前兼容）：`kind` / `format_version` 是信封与包的字段,
/// 出现在同一段里不该报错,也不该影响 title/description 的提取。
#[test]
fn extract_info_ignores_unknown_subkeys() {
    let info = info_of(
        "[package]\nformat_version = 2\nkind = \"schema_text\"\n\
             future_field = { nested = true }\ntitle = \"带未知子键\"\n",
    )
    .expect("有 title 就该返回 Some");
    assert_eq!(info.title.as_deref(), Some("带未知子键"));
    assert!(info.description.is_none());
}

/// title 必须是单行:内部含换行即拒。归一后的 `\r\n` 同样算换行——归一是为了统一判据,
/// 不是为了放行。
#[test]
fn title_with_inner_newline_is_rejected() {
    for text in [
        "[package]\ntitle = \"第一行\\n第二行\"\n",
        "[package]\ntitle = \"第一行\\r\\n第二行\"\n",
    ] {
        let err = info_err(text);
        assert!(err.contains("package.title"), "错误须点名字段: {err}");
        assert!(err.contains("换行"), "{err}");
    }
}

/// 首尾换行放行:TOML 多行字符串是写 title 的合法语法,尾随换行是该语法的固有产物,
/// 不是分发者的错误。「不含换行」挡的是「title 得是单行」,不是「字节里不许出现 \n」。
#[test]
fn title_with_surrounding_newline_is_accepted() {
    let info = info_of("[package]\ntitle = \"\"\"\n快符方案\n\"\"\"\n")
        .expect("TOML 多行字符串写的单行标题应放行");
    assert_eq!(info.title.as_deref(), Some("快符方案"));
    let info = info_of("[package]\ntitle = \"尾随换行\\r\\n\"\n").unwrap();
    assert_eq!(info.title.as_deref(), Some("尾随换行"));
}

/// 限额按字符不是字节：200 个 CJK 字符（600 字节）的 title 合法,201 个即拒。
/// description 同理 4000/4001。
#[test]
fn length_limit_counts_chars_not_bytes() {
    let ok_title = "字".repeat(INFO_TITLE_MAX_CHARS);
    assert_eq!(ok_title.len(), INFO_TITLE_MAX_CHARS * 3, "CJK 每字 3 字节");
    let info = info_of(&format!("[package]\ntitle = \"{ok_title}\"\n")).unwrap();
    assert_eq!(info.title.as_deref(), Some(ok_title.as_str()));

    let long_title = "字".repeat(INFO_TITLE_MAX_CHARS + 1);
    let err = info_err(&format!("[package]\ntitle = \"{long_title}\"\n"));
    assert!(err.contains("package.title"), "{err}");
    assert!(
        err.contains(&(INFO_TITLE_MAX_CHARS + 1).to_string())
            && err.contains(&INFO_TITLE_MAX_CHARS.to_string()),
        "错误须写明实际长度与上限: {err}"
    );

    let ok_desc = "说".repeat(INFO_DESCRIPTION_MAX_CHARS);
    assert!(info_of(&format!("[package]\ndescription = \"{ok_desc}\"\n")).is_some());
    let long_desc = "说".repeat(INFO_DESCRIPTION_MAX_CHARS + 1);
    let err = info_err(&format!("[package]\ndescription = \"{long_desc}\"\n"));
    assert!(err.contains("package.description"), "{err}");
}

/// C0 控制字符与 DEL 拒绝;`\t` 放行;`\r\n` 与孤立 `\r` 归一为 `\n`。
#[test]
fn control_chars_rejected_tab_allowed_crlf_normalized() {
    for bad in ["\\u0000", "\\u0007", "\\u001B", "\\u007F"] {
        let err = info_err(&format!("[package]\ndescription = \"说明{bad}\"\n"));
        assert!(err.contains("package.description"), "{err}");
        assert!(err.contains("控制字符"), "{err}");
    }
    let info = info_of("[package]\ntitle = \"制表\\t符\"\n").expect("\\t 应放行");
    assert_eq!(info.title.as_deref(), Some("制表\t符"));

    let info = info_of("[package]\ndescription = \"甲\\r\\n乙\\r丙\"\n").unwrap();
    assert_eq!(
        info.description.as_deref(),
        Some("甲\n乙\n丙"),
        "\\r\\n 与孤立 \\r 都归一为 \\n"
    );
}

/// 类型不对 → 报错,不静默忽略（忽略就成了「写了没生效」）。段本身不是表同理。
#[test]
fn wrong_type_is_rejected() {
    let err = info_err("[package]\ntitle = 5\n");
    assert!(err.contains("package.title"), "{err}");
    assert!(err.contains("字符串"), "{err}");
    let err = info_err("[package]\ndescription = [\"a\"]\n");
    assert!(err.contains("package.description"), "{err}");
    let err = info_err("package = 5\n");
    assert!(err.contains("package"), "{err}");
    assert!(
        preview_text("package = 5\n").is_empty(),
        "写坏的保留段也不该冒充配置键"
    );
}

/// 全空白视同没写（trim 后为空 → None）,不是「一个空标题」。
/// 全角空格 U+3000 同样算空白（Rust `trim()` 认它）——中文分发者最容易误打的正是它。
#[test]
fn all_whitespace_is_treated_as_absent() {
    assert!(info_of("[package]\ntitle = \"   \"\n").is_none());
    assert!(
        info_of("[package]\ntitle = \"\u{3000}\u{3000}\"\n").is_none(),
        "全角空格也是空白,视同没写"
    );
    assert_eq!(
        info_of("[package]\ntitle = \"\u{3000}有标题\u{3000}\"\n")
            .unwrap()
            .title
            .as_deref(),
        Some("有标题"),
        "全角空格同样被 trim 掉"
    );
    assert!(info_of("[package]\ndescription = \"\\n\\n  \"\n").is_none());
    let info = info_of("[package]\ntitle = \"  有标题  \"\ndescription = \"  \"\n").unwrap();
    assert_eq!(info.title.as_deref(), Some("有标题"), "首尾空白 trim");
    assert!(info.description.is_none(), "全空白的说明视同没写");
}

/// [`sanitize_info_text`] 是三种载体共用的入口,直接调用时的语义与经 extract_info
/// 走一遍一致（wind-transfer 复用的正是这条路）。
#[test]
fn sanitize_info_text_is_reusable_directly() {
    assert_eq!(
        sanitize_title("  标题\r\n\r\n").unwrap(),
        "标题",
        "首尾换行 trim 掉即可,只有内部换行才算多行"
    );
    assert!(sanitize_title("甲\r\n乙").unwrap_err().contains("换行"));
    assert_eq!(sanitize_description("甲\r\n乙").unwrap(), "甲\n乙");
    assert_eq!(sanitize_description("   ").unwrap(), "", "空串 = 没写");
    let err = sanitize_info_text("自定字段", "a\u{1}b", 10, true).unwrap_err();
    assert!(
        err.starts_with("自定字段"),
        "错误须点名调用方给的字段名: {err}"
    );
}

// ── ALLOWED_UNREGISTERED_KEYS 守门：名单不许腐烂 ──

/// (a) 名单键必须不在 REGISTRY——日后登记了就该从名单删除，否则同一个键有两条校验路径。
#[test]
fn allowlist_keys_stay_out_of_registry() {
    for key in ALLOWED_UNREGISTERED_KEYS {
        assert!(
            !config_schema::is_known_key(key),
            "{key} 已进 REGISTRY，应从 ALLOWED_UNREGISTERED_KEYS 移除"
        );
    }
}

/// (b) 名单键必须真实存在于 `Config`：它不 deny 未知字段，只有回读同值才能证明字段存在。
/// 样例值按全员 `Option<String>` 取字符串，加入其他类型的键需单独给样例。
#[test]
fn allowlist_keys_deserialize_and_round_trip() {
    for key in ALLOWED_UNREGISTERED_KEYS {
        let mut base = toml::Value::try_from(Config::default()).unwrap();
        let path: Vec<&str> = key.split('.').collect();
        if let toml::Value::Table(t) = &mut base {
            crate::config::set_nested(t, &path, toml::Value::String("样例".into()));
        }
        let cfg: Config = base
            .try_into()
            .unwrap_or_else(|e| panic!("{key} 应能反序列化: {e}"));
        let back = toml::Value::try_from(cfg).unwrap();
        assert_eq!(
            crate::config::get_nested(&back, &path).and_then(|v| v.as_str()),
            Some("样例"),
            "{key} 写入后未能原值读回——名单里可能是不存在的键"
        );
    }
}

// ── 风险提示（RISKY_KEYS）────────────────────────────────────

/// 危险键在预览里必须带提示，且照常可应用（提示不是阻断）。
#[test]
fn risky_key_is_flagged_but_still_applicable() {
    let frag: toml::Value = toml::from_str(
        r#"
            [[ui.toolbar.buttons]]
            id = "x"
            label = "符"
            action = 'proc.run("evil.exe")'
            "#,
    )
    .unwrap();
    let cur = toml::Value::try_from(Config::default()).unwrap();
    let entries = preview(&frag, &cur);
    let e = entries
        .iter()
        .find(|e| e.key == "ui.toolbar.buttons")
        .expect("应产出该键的条目");
    assert!(e.error.is_none(), "合法片段不该报错：{:?}", e.error);
    let w = e.warning.as_deref().expect("危险键必须带提示");
    assert!(w.contains("启动程序"), "提示要说清后果，实际：{w}");
}

/// 寻常键不带提示——否则提示遍地都是，等于没有提示。
#[test]
fn ordinary_key_has_no_warning() {
    let frag: toml::Value = toml::from_str("[ui.candidate]\nper_page = 9\n").unwrap();
    let cur = toml::Value::try_from(Config::default()).unwrap();
    let entries = preview(&frag, &cur);
    assert!(entries.iter().all(|e| e.warning.is_none()));
}

/// 写错了的危险键照样提示：用户多半会改对再导入一次，那时提示就该已经说过。
#[test]
fn risky_key_warns_even_when_invalid() {
    // StructList 键给了标量 → 类型不合法。
    let frag: toml::Value = toml::from_str(r#"ui.toolbar.buttons = 5"#).unwrap();
    let cur = toml::Value::try_from(Config::default()).unwrap();
    let entries = preview(&frag, &cur);
    let e = entries
        .iter()
        .find(|e| e.key == "ui.toolbar.buttons")
        .expect("应产出该键的条目");
    assert!(e.error.is_some(), "标量给 StructList 键应报类型错");
    assert!(e.warning.is_some(), "写错了也要提示");
}

/// 名单里的每个键都必须是真的登记键——否则那条提示永远不会触发，
/// 而「提示没出现」没有任何信号。同 ALLOWED_UNREGISTERED_KEYS 的守门思路。
#[test]
fn risky_keys_are_all_registered() {
    for (key, msg) in RISKY_KEYS {
        assert!(
            config_schema::is_known_key(key),
            "{key} 不在 REGISTRY 里，这条风险提示永远不会触发"
        );
        assert!(!msg.trim().is_empty(), "{key} 的提示语不能为空");
    }
}
