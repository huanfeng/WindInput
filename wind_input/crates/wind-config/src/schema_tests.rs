use super::*;

/// 方案文件里残留的注释模板键不生效，只进降级提示；方案其余部分照常解析。
#[test]
fn schema_file_template_degrades() {
    let s: Schema =
        toml::from_str("[candidate]\nlayout = \"vertical\"\ncomment_template_vertical = \"x\"\n")
            .expect("方案应解析成功");
    let b = s.behavior();
    assert_eq!(b.candidate_layout, crate::config::LayoutIntent::Vertical);
    assert_eq!(b.degraded_items.len(), 1, "{:?}", b.degraded_items);
    assert!(b.degraded_items[0].contains("候选注释"));

    let s: Schema = toml::from_str("[overlay]\ncomment_template_horizontal = \"\"\n").unwrap();
    assert_eq!(s.behavior().degraded_items.len(), 1);

    assert!(Schema::default().behavior().degraded_items.is_empty());
}

/// 通用方案级 UI 覆盖：`[candidate]` 里不属于强类型字段的键被收为补丁，
/// 登记在册且值合法的进 `SchemaBehavior`，其余进降级清单；强类型字段不被截走。
#[test]
fn candidate_generic_keys_become_patch() {
    let s: Schema = toml::from_str(
        "[candidate]\nlayout = \"vertical\"\nper_page = 9\nno_such_key = 1\nmin_rows = 3\n",
    )
    .expect("方案应解析成功");
    let b = s.behavior();
    assert_eq!(b.candidate_layout, crate::config::LayoutIntent::Vertical);
    assert_eq!(
        b.candidate_ui.len(),
        1,
        "只有 per_page 在白名单内：{:?}",
        b.candidate_ui
    );
    assert_eq!(b.candidate_ui["per_page"].as_integer(), Some(9));
    // 未登记（no_such_key）与未开放（min_rows 首期不在白名单）都要报给用户，不能静默。
    assert_eq!(b.degraded_items.len(), 2, "{:?}", b.degraded_items);
    assert!(b.degraded_items.iter().any(|i| i.contains("no_such_key")));
    assert!(b.degraded_items.iter().any(|i| i.contains("min_rows")));
}

/// 值类型或值域不对的补丁项整条丢弃（不能让错值盖掉好的全局值），并进降级清单。
#[test]
fn candidate_generic_bad_value_is_dropped() {
    for bad in ["\"nine\"", "0", "-3", "1.5"] {
        let s: Schema = toml::from_str(&format!("[candidate]\nper_page = {bad}\n")).unwrap();
        let b = s.behavior();
        assert!(b.candidate_ui.is_empty(), "{bad} 不该生效");
        assert_eq!(b.degraded_items.len(), 1, "{bad}: {:?}", b.degraded_items);
        assert!(b.degraded_items[0].contains("per_page"));
    }
}

/// 没写补丁的方案：补丁为空、降级清单为空。
#[test]
fn candidate_generic_absent_is_empty() {
    let b = Schema::default().behavior();
    assert!(b.candidate_ui.is_empty());
    assert!(b.degraded_items.is_empty());
}

#[test]
fn shuangpin_is_supported() {
    let mut s = Schema::default();
    s.engine.engine_type = "pinyin".into();
    s.engine.pinyin.scheme = "shuangpin".into();
    assert!(s.is_supported());
}

#[test]
fn pinyin_spec_ignores_removed_fields() {
    let toml_str = r#"
scheme = "shuangpin"
show_code_hint = true
fuzzy = { enabled = true, zh_z = true }
[shuangpin]
layout = "xiaohe"
"#;
    let spec: PinyinSpec = toml::from_str(toml_str).unwrap();
    assert_eq!(spec.scheme, "shuangpin");
    assert_eq!(spec.shuangpin.layout, "xiaohe");
}

/// 截断的两个方向都要成立：该截的截、不该截的别动。
///
/// 只测截短会放过「恒返回首字符」（`En` 只显示 `E`）；只测不动会放过「按字符数取 2」
/// （双汉字整个放行、渲染端缩字号到认不出）。两个方向缺一不可。
#[test]
fn icon_label_truncates_by_display_width() {
    assert_eq!(icon_label_trunc("英"), "英", "一个汉字宽 2，恰好满格");
    assert_eq!(icon_label_trunc("En"), "En", "两个拉丁字符合计宽 2，不许动");
    assert_eq!(icon_label_trunc("English"), "En", "超宽截到前两个拉丁字符");
    assert_eq!(
        icon_label_trunc("符号"),
        "符",
        "双汉字宽 4 → 截回首字（#85）"
    );
    assert_eq!(icon_label_trunc("符号表"), "符", "更长也一样只留首字");
    assert_eq!(icon_label_trunc("Ｅｎ"), "Ｅ", "全角字母按 CJK 宽度算");
    assert_eq!(icon_label_trunc("A符"), "A", "混排：A 占 1，再加汉字就超");
}

/// 首尾空白必须吃掉：设置页输入框里带出来的空格若原样落进标签，
/// 会挤掉本就只有 16px 的绘制宽度，且用户在界面上看不出多了个空格。
#[test]
fn icon_label_trims_whitespace() {
    assert_eq!(icon_label_trunc("  En  "), "En");
    assert_eq!(icon_label_trunc("   "), "", "全空白视同未配置");
    assert_eq!(icon_label_trunc(""), "");
}

/// 回落方向：空 → fallback，非空 → 不许被 fallback 顶掉。
///
/// 后半条防「永远返回 fallback」的实现（用户怎么配都不生效）。
#[test]
fn icon_label_or_falls_back_only_when_empty() {
    assert_eq!(icon_label_or("", "英"), "英");
    assert_eq!(icon_label_or("   ", "英"), "英");
    assert_eq!(icon_label_or("En", "英"), "En", "配了就用配的");
    assert_eq!(icon_label_or("English", "英"), "En", "截断后仍是配的");
}

/// 截断以 Unicode 标量值为单位推进，不是 UTF-16 code unit、也不是字节。
///
/// 这约束 C++ 侧缓冲：宽度上限 2 即最多 2 个标量值，emoji 记 1 宽却占 2 个 wchar，
/// 最坏 4 wchar + NUL，`_inputTypeLabel` 须 ≥ 5（现为 8）。同时钉住「emoji 记 1 宽」，
/// 改了要重算缓冲最坏值。
#[test]
fn icon_label_limit_counts_scalar_values() {
    let two_emoji = icon_label_trunc("🙂🙃🙂");
    assert_eq!(two_emoji.chars().count(), 2, "按标量值计数");
    assert_eq!(
        two_emoji.encode_utf16().count(),
        4,
        "两个标量值可占四个 wchar——C++ 侧缓冲按这个上限取容量"
    );
}

// ── 方案级自定义标点表 ──────────────────────────────────────────────

/// 三态必须由同一个字段区分开：不写 = 跟随全局、空表 = 一条都不要、
/// 非空表 = 整表替换。`None` 与 `Some({})` 混为一谈就表达不了「本方案不要自定义标点」。
#[test]
fn schema_punct_custom_mappings_three_states() {
    let parse = |s: &str| toml::from_str::<Schema>(s).expect("方案应解析成功").punct;

    assert_eq!(
        parse("[punct]\nmode = \"chinese\"").custom_mappings,
        None,
        "段里不写 custom_mappings ⇒ 跟随全局"
    );
    assert_eq!(
        parse("[punct]\n[punct.custom_mappings]").custom_mappings,
        Some(Default::default()),
        "空表 ⇒ 本方案一条自定义都不要（与「跟随」必须可区分）"
    );

    let filled = parse("[punct]\n[punct.custom_mappings]\n\".\" = [\"。\", \"．\", \"。\", \".\"]");
    let table = filled.custom_mappings.expect("非空表应是 Some");
    assert_eq!(
        table.get("."),
        Some(&vec![
            "。".to_string(),
            "．".to_string(),
            "。".to_string(),
            ".".to_string()
        ]),
        "列序契约与全局表一致：[中半, 英全, 中全, 英半]"
    );
}

/// `custom_mappings` 在 override 合并里整体替换而非深合并：深合并下用户删不掉方案作者写的行。
/// 断言「作者的行还在不在」，因为「我写的行生效没有」在两种策略下都通过。
#[test]
fn custom_mappings_override_replaces_whole_table() {
    let mut base = toml::from_str::<toml::Value>(
        "[punct.custom_mappings]\n\
             \".\" = [\"。\"]\n\
             \",\" = [\"，\"]\n",
    )
    .unwrap();
    // 用户在设置页只留下了 `.` 那一行（删掉了作者写的 `,`）。
    let over = toml::from_str::<toml::Value>("[punct.custom_mappings]\n\".\" = [\"·\"]\n").unwrap();
    merge_toml(&mut base, over);

    let table = base["punct"]["custom_mappings"].as_table().unwrap();
    assert_eq!(
        table.get(".").and_then(|v| v[0].as_str()),
        Some("·"),
        "用户改的那行要生效"
    );
    assert!(
        !table.contains_key(","),
        "★ 作者写的 `,` 必须随整表替换被删掉——深合并时它会残留，\
             表现为「设置页删了、保存重开又回来」"
    );
}

/// 反向对照：同一份 override，换成别的键名就该走深合并（作者的行保留）。
/// 防止误写成「所有 table 都整体替换」，那会毁掉 `[engine]` 等段的合并语义。
#[test]
fn non_custom_mappings_tables_still_deep_merge() {
    let mut base =
        toml::from_str::<toml::Value>("[engine]\ntype = \"codetable\"\nfoo = 1\n").unwrap();
    let over = toml::from_str::<toml::Value>("[engine]\nfoo = 2\n").unwrap();
    merge_toml(&mut base, over);

    assert_eq!(base["engine"]["foo"].as_integer(), Some(2), "覆盖的键生效");
    assert_eq!(
        base["engine"]["type"].as_str(),
        Some("codetable"),
        "未提及的键必须保留——这才是深合并"
    );
}

/// 连续辅助码的两个内部项（`continuous`、`preedit_separator`）：不写 = `None`，序列化时
/// 不冒出来（否则设置端 getConfig → saveConfig 的 diff 会把默认值写进 override）。
#[test]
fn aux_internal_keys_absent_are_none_and_not_serialized() {
    let s: Schema = toml::from_str("[engine.aux_code]\ndirect = true\n").unwrap();
    assert_eq!(s.engine.aux_code.continuous, None);
    assert_eq!(s.engine.aux_code.preedit_separator, None);
    let out = toml::to_string(&s).unwrap();
    assert!(!out.contains("continuous"), "{out}");
    assert!(!out.contains("preedit_separator"), "{out}");
}

/// 写了就原样解析、原样序列化；校验（分隔符白名单）在引擎侧，这里不改值——空串与
/// 不合格的值都要原样留着，用户改回来之前不能被配置层悄悄抹掉。
#[test]
fn aux_internal_keys_parse_and_roundtrip() {
    for sep in ["", "·", " ", "x"] {
        let src = format!(
            "[engine.aux_code]\ndirect = true\ncontinuous = false\npreedit_separator = {sep:?}\n"
        );
        let s: Schema = toml::from_str(&src).unwrap();
        assert_eq!(s.engine.aux_code.continuous, Some(false));
        assert_eq!(s.engine.aux_code.preedit_separator.as_deref(), Some(sep));
        let back: Schema = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.engine.aux_code.continuous, Some(false));
        assert_eq!(back.engine.aux_code.preedit_separator.as_deref(), Some(sep));
    }
}

/// override 层只写内部项：与方案文件深合并，作者的 `files` / `direct` 保留；
/// 全局折叠（`AuxCodeGlobal::resolved`）不受内部项影响。
#[test]
fn aux_internal_keys_merge_from_override() {
    let mut base = toml::from_str::<toml::Value>(
        "[engine.aux_code]\nfiles = [\"aux_code/t.txt\"]\ndirect = true\n",
    )
    .unwrap();
    let over = toml::from_str::<toml::Value>(
        "[engine.aux_code]\ncontinuous = false\npreedit_separator = \"\"\n",
    )
    .unwrap();
    merge_toml(&mut base, over);
    let s: Schema = base.try_into().unwrap();
    let aux = &s.engine.aux_code;
    assert_eq!(aux.files, vec!["aux_code/t.txt".to_string()]);
    assert_eq!(aux.direct, Some(true));
    assert_eq!(aux.continuous, Some(false));
    assert_eq!(aux.preedit_separator.as_deref(), Some(""));
    let r = crate::config::AuxCodeGlobal {
        enabled: true,
        ..Default::default()
    }
    .resolved(Some(aux));
    assert!(r.enabled && r.direct, "逃生口不改 direct 本身：{r:?}");
}
