use super::*;

/// `parse` 与 `as_str` 必须严格互逆——候选 id 靠这对函数往返，
/// 一侧改了名字另一侧没改，右键就会认不出这条候选（且没有任何报错）。
#[test]
fn kind_parse_and_as_str_roundtrip() {
    for &k in FormatKind::ALL {
        assert_eq!(FormatKind::parse(k.as_str()), Some(k));
    }
    assert!(FormatKind::parse("weather").is_none());
}

/// `ALL` 是「全部类别」的单一真相源，它自己漏一项就会让所有依赖它的穷尽性测试
/// 一起失效（而且全都照绿）。故这里独立钉住两件事：条数，以及与 `group_order` 同序。
#[test]
fn all_kinds_is_exhaustive_and_ordered() {
    assert_eq!(FormatKind::ALL.len(), 5, "新增类别必须同步 ALL");
    let orders: Vec<u8> = FormatKind::ALL.iter().map(|k| k.group_order()).collect();
    let mut sorted = orders.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(orders, sorted, "ALL 必须按 group_order 升序且无重复");
}

#[test]
fn builtin_covers_every_kind() {
    let t = FormatTable::builtin();
    for &k in FormatKind::ALL {
        assert!(
            t.entries_of(k).next().is_some(),
            "内置表缺 kind={}",
            k.as_str()
        );
    }
}

#[test]
fn builtin_templates_all_validate() {
    // 内置表自身必须过校验——写错变量名会静默丢条目
    for e in FormatTable::builtin().entries() {
        assert!(
            validate_format_text(e.kind, &e.text).is_ok(),
            "内置条目 {} 校验失败: {:?}",
            e.id,
            validate_format_text(e.kind, &e.text)
        );
    }
}

#[test]
fn parse_orders_by_position_not_file_order() {
    let t = FormatTable::parse(
        r#"
[[formats]]
id = "b"
kind = "date"
text = "$Y"
position = 2

[[formats]]
id = "a"
kind = "date"
text = "$M"
position = 1
"#,
    )
    .unwrap();
    let ids: Vec<&str> = t
        .entries_of(FormatKind::Date)
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(ids, vec!["a", "b"]);
}

#[test]
fn missing_position_falls_back_to_file_order() {
    let t = FormatTable::parse(
        r#"
[[formats]]
id = "first"
kind = "date"
text = "$Y"

[[formats]]
id = "second"
kind = "date"
text = "$M"
"#,
    )
    .unwrap();
    let ids: Vec<&str> = t
        .entries_of(FormatKind::Date)
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(ids, vec!["first", "second"]);
}

/// 单条非法只剔除该条，其余存活——否则用户改错一个变量名，整个快捷输入退回出厂。
#[test]
fn one_bad_entry_does_not_kill_the_rest() {
    let t = FormatTable::parse(
        r#"
[[formats]]
id = "good"
kind = "date"
text = "$Y年"

[[formats]]
id = "unknown_var"
kind = "date"
text = "$NOPE"

[[formats]]
id = "unknown_kind"
kind = "weather"
text = "$Y"

[[formats]]
id = "wrong_kind_var"
kind = "calc"
text = "$Y"

[[formats]]
id = "expr"
kind = "date"
text = "{month(cn='true')}"

[[formats]]
id = "mixed_syntax"
kind = "date"
text = "${Y}年{month()}月"
"#,
    )
    .unwrap();
    let ids: Vec<&str> = t.entries().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["good", "expr"],
        "合法的变量模板与表达式模板都留下，其余剔除"
    );
}

/// 表达式模板的函数名/参数由 cmdbar 在求值期校验，加载期不认识它们——
/// 但混用两套语法必须在加载期就拒绝：变量路径不认识 `{month()}`，
/// 放行的话会把它当字面量上屏。
#[test]
fn expression_entries_are_accepted_but_mixing_is_not() {
    let t = FormatTable::parse(
        r#"
[[formats]]
id = "expr.unknown_func"
kind = "date"
text = "{no_such_func()}"
"#,
    )
    .unwrap();
    assert_eq!(t.entries().len(), 1, "未知函数留到求值期报，加载期放行");
    assert!(t.entries()[0].is_expression());

    assert!(validate_format_text(FormatKind::Date, "${Y}年{month()}月").is_err());
    assert!(validate_format_text(FormatKind::Date, "$Y年{month()}月").is_err());
}

/// 漏掉外层花括号（如 `amt(zheng='false')`）必须在保存时就报错：它恰好是一条合法的
/// 纯字面模板，`expand` 原样返回，不拦的话候选出的就是这段源码本身。
#[test]
fn function_without_braces_is_rejected_with_a_pointed_hint() {
    let err = validate_format_text(FormatKind::Number, "amt(zheng='false')").unwrap_err();
    assert!(
        err.contains("{amt(zheng='false')}"),
        "提示要把正确写法回显出来，实际: {err}"
    );
    // 反向对照：包上花括号后照常通过——拦的是漏括号，不是这个函数
    assert!(validate_format_text(FormatKind::Number, "{amt(zheng='false')}").is_ok());
}

/// 不含括号的纯字面模板同样拒绝：一条与所输内容无关、恒定出同一段文字的「格式」
/// 在快捷输入里没有用途（那是短语的活）。`$$` 转义后也是字面量，同样不放行。
#[test]
fn literal_only_templates_are_rejected() {
    assert!(validate_format_text(FormatKind::Number, "人民币").is_err());
    assert!(validate_format_text(FormatKind::Date, "$$Y").is_err());
    // 反向对照：只要有一个真变量就放行，字面量可以照常混在里面
    assert!(validate_format_text(FormatKind::Date, "农历$LMD").is_ok());
    assert!(validate_format_text(FormatKind::Date, "$$$Y").is_ok());
}

#[test]
fn duplicate_id_within_kind_is_dropped() {
    let t = FormatTable::parse(
        r#"
[[formats]]
id = "dup"
kind = "date"
text = "$Y"

[[formats]]
id = "dup"
kind = "date"
text = "$M"
"#,
    )
    .unwrap();
    assert_eq!(t.entries().len(), 1);
    assert_eq!(t.entries()[0].text, "$Y", "保留先出现的那条");
}

#[test]
fn same_id_in_different_kinds_is_allowed() {
    let t = FormatTable::parse(
        r#"
[[formats]]
id = "cn"
kind = "date"
text = "$Y"

[[formats]]
id = "cn"
kind = "calc"
text = "$RESULT"
"#,
    )
    .unwrap();
    assert_eq!(t.entries().len(), 2);
}

#[test]
fn syntax_error_is_reported_to_caller() {
    assert!(FormatTable::parse("[[formats]]\nid = ").is_err());
}

// ───────── 用户调整（FormatAdjust）─────────

fn date_ids(t: &FormatTable, a: &FormatAdjust) -> Vec<String> {
    t.entries_of_adjusted(FormatKind::Date, a)
        .iter()
        .map(|e| e.id.clone())
        .collect()
}

// ───────── 设置页口径（entries_of_view）─────────

/// 设置页里启用项的顺序必须与候选完全一致，否则「设置页点上移、实际候选没动」且两边都不报错。
/// 故直接拿 `entries_of_adjusted`（候选那条路径）的输出做基准，而非另写一份期望顺序。
#[test]
fn view_enabled_order_equals_candidate_order() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.iso".into(), 0), ("date.lunar".into(), 2)],
        disabled: vec!["date.basic".into()],
        added: Vec::new(),
    };
    let candidate: Vec<&str> = t
        .entries_of_adjusted(FormatKind::Date, &a)
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    let view_enabled: Vec<&str> = t
        .entries_of_view(FormatKind::Date, &a)
        .iter()
        .filter(|v| v.enabled)
        .map(|v| v.entry.id.as_str())
        .collect();
    assert_eq!(view_enabled, candidate);
}

/// 停用项留在列表里（这正是设置页存在的理由），且条数一条不少。
#[test]
fn view_keeps_disabled_entries_listed() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        disabled: vec!["date.cn".into(), "date.basic".into()],
        ..Default::default()
    };
    let view = t.entries_of_view(FormatKind::Date, &a);
    assert_eq!(
        view.len(),
        t.entries_of(FormatKind::Date).count(),
        "候选里少了两条，管理列表里一条不少"
    );
    assert_eq!(view.iter().filter(|v| !v.enabled).count(), 2);
}

/// 停用一行不该让它换位置，否则同一行会在停用/启用之间来回跳。对每一条都验，
/// 因为首条、末条、中间条走锚点算法的不同分支（锚点为 None / 锚点在末尾 / 一般情形）。
#[test]
fn disabling_a_row_does_not_move_it() {
    let t = FormatTable::builtin();
    for &kind in FormatKind::ALL {
        let base: Vec<String> = t
            .entries_of_view(kind, &FormatAdjust::default())
            .iter()
            .map(|v| v.entry.id.clone())
            .collect();
        for (i, id) in base.iter().enumerate() {
            let a = FormatAdjust {
                disabled: vec![id.clone()],
                ..Default::default()
            };
            let after: Vec<String> = t
                .entries_of_view(kind, &a)
                .iter()
                .map(|v| v.entry.id.clone())
                .collect();
            assert_eq!(
                after,
                base,
                "停用 {id}（kind={}, 第 {i} 行）后整列顺序不该有任何变化",
                kind.as_str()
            );
        }
    }
}

/// 先调序、再停用同一条，位置也不该变：锚点须参照「假设全部启用」的顺序，
/// 否则被移到首位的条目一停用就跳回出厂位置。
#[test]
fn disabling_a_reordered_row_keeps_its_new_position() {
    let t = FormatTable::builtin();
    for &kind in FormatKind::ALL {
        let ids: Vec<String> = t.entries_of(kind).map(|e| e.id.clone()).collect();
        if ids.len() < 3 {
            continue; // calc 只有两条，移动空间不足以体现差别
        }
        // 把末条移到每一个可能的位置，各验一遍「停用前后视图不变」。
        for target in 0..ids.len() {
            let moved = vec![(ids[ids.len() - 1].clone(), target)];
            let before: Vec<String> = t
                .entries_of_view(
                    kind,
                    &FormatAdjust {
                        moved: moved.clone(),
                        disabled: Vec::new(),
                        added: Vec::new(),
                    },
                )
                .iter()
                .map(|v| v.entry.id.clone())
                .collect();
            let after: Vec<String> = t
                .entries_of_view(
                    kind,
                    &FormatAdjust {
                        moved: moved.clone(),
                        disabled: vec![ids[ids.len() - 1].clone()],
                        added: Vec::new(),
                    },
                )
                .iter()
                .map(|v| v.entry.id.clone())
                .collect();
            assert_eq!(
                after,
                before,
                "kind={} 把 {} 移到第 {target} 位后再停用，顺序不该变",
                kind.as_str(),
                ids[ids.len() - 1]
            );
        }
    }
}

/// 连续多条停用也各自留在原位（含首条——它的锚点是 `None`）。
#[test]
fn several_disabled_rows_each_stay_put() {
    let t = FormatTable::builtin();
    let base: Vec<String> = t
        .entries_of(FormatKind::Date)
        .map(|e| e.id.clone())
        .collect();
    // 首条 + 相邻两条 + 末条：覆盖锚点为 None、同锚点多条、锚点在末尾三种情形。
    let a = FormatAdjust {
        disabled: vec![
            base[0].clone(),
            base[2].clone(),
            base[3].clone(),
            base[base.len() - 1].clone(),
        ],
        ..Default::default()
    };
    let after: Vec<String> = t
        .entries_of_view(FormatKind::Date, &a)
        .iter()
        .map(|v| v.entry.id.clone())
        .collect();
    assert_eq!(after, base, "四条同时停用，顺序仍与出厂一致");
}

/// 「调序 + 停用」混合时启用项的相对顺序仍须与候选完全一致——「不剔除就套 `moved`
/// 下标」会在这里算错（见 `entries_of_view`）。
#[test]
fn enabled_order_still_matches_candidates_with_disabled_in_between() {
    let t = FormatTable::builtin();
    let base: Vec<String> = t
        .entries_of(FormatKind::Date)
        .map(|e| e.id.clone())
        .collect();
    // 停用首条 + 把末条移到下标 1：直接套下标会得到与候选不同的启用序。
    let a = FormatAdjust {
        moved: vec![(base[base.len() - 1].clone(), 1)],
        disabled: vec![base[0].clone()],
        added: Vec::new(),
    };
    let candidate: Vec<&str> = t
        .entries_of_adjusted(FormatKind::Date, &a)
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    let view_enabled: Vec<&str> = t
        .entries_of_view(FormatKind::Date, &a)
        .iter()
        .filter(|v| v.enabled)
        .map(|v| v.entry.id.as_str())
        .collect();
    assert_eq!(view_enabled, candidate);
}

/// `adjusted` 标记决定设置页「恢复此条」能不能点：移动过、停用过都算，
/// 没碰过的条目为 false。
#[test]
fn view_marks_adjusted_entries() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.lunar".into(), 0)],
        disabled: vec!["date.basic".into()],
        added: Vec::new(),
    };
    let view = t.entries_of_view(FormatKind::Date, &a);
    let flag = |id: &str| {
        view.iter()
            .find(|v| v.entry.id == id)
            .map(|v| v.adjusted)
            .unwrap()
    };
    assert!(flag("date.lunar"), "移动过");
    assert!(flag("date.basic"), "停用过");
    assert!(!flag("date.cn"), "没碰过的不该显示成已调整");
}

/// 空调整下，视图 == 基表原序、全部启用、全部未调整。
#[test]
fn view_without_adjust_is_base_table() {
    let t = FormatTable::builtin();
    // 视图借用 adjust（用户条目住在里面），故不能传临时值
    let none = FormatAdjust::default();
    for &k in FormatKind::ALL {
        let view = t.entries_of_view(k, &none);
        let base: Vec<&str> = t.entries_of(k).map(|e| e.id.as_str()).collect();
        let got: Vec<&str> = view.iter().map(|v| v.entry.id.as_str()).collect();
        assert_eq!(got, base, "kind={}", k.as_str());
        assert!(view.iter().all(|v| v.enabled && !v.adjusted));
    }
}

/// 空调整 == 基表原序。
#[test]
fn empty_adjust_is_identity() {
    let t = FormatTable::builtin();
    let base: Vec<String> = t
        .entries_of(FormatKind::Date)
        .map(|e| e.id.clone())
        .collect();
    assert_eq!(date_ids(&t, &FormatAdjust::default()), base);
}

#[test]
fn disabled_entry_is_dropped() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        disabled: vec!["date.basic".into()],
        ..Default::default()
    };
    let ids = date_ids(&t, &a);
    assert!(!ids.contains(&"date.basic".to_string()));
    assert!(ids.contains(&"date.cn".to_string()), "其余不受影响");
}

#[test]
fn move_to_front() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.lunar".into(), 0)],
        ..Default::default()
    };
    assert_eq!(date_ids(&t, &a)[0], "date.lunar");
}

/// 逆序遍历 = LIFO：后写入的规则（index 0）优先级最高，两条都想占 0 号位时最新的赢。
/// 搞反的话「刚调的那条被上一次的调整顶掉」。
#[test]
fn newest_move_wins() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        // index 0 = 最新
        moved: vec![("date.iso".into(), 0), ("date.lunar".into(), 0)],
        ..Default::default()
    };
    assert_eq!(date_ids(&t, &a)[0], "date.iso", "最新的规则赢");
}

/// 移到中间位置（上移/下移用的就是这条路径）。
#[test]
fn move_to_middle() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.lunar".into(), 2)],
        ..Default::default()
    };
    assert_eq!(date_ids(&t, &a)[2], "date.lunar");
}

/// 越界下标不 panic，钳到末尾——条目可能因停用或用户改文件而变少。
#[test]
fn out_of_range_position_clamps() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.cn".into(), 999)],
        ..Default::default()
    };
    let ids = date_ids(&t, &a);
    assert_eq!(ids.last().unwrap(), "date.cn");
    assert_eq!(ids.len(), t.entries_of(FormatKind::Date).count());
}

/// 孤儿规则（基表里没有该 id，如用户整份覆盖格式表删了某条）静默忽略，不影响其余条目。
#[test]
fn orphan_rule_is_ignored() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.nonexistent".into(), 0), ("date.lunar".into(), 0)],
        disabled: vec!["date.alsogone".into()],
        added: Vec::new(),
    };
    let ids = date_ids(&t, &a);
    assert_eq!(ids[0], "date.lunar");
    assert_eq!(ids.len(), t.entries_of(FormatKind::Date).count());
}

/// 停用 + 移动叠加：先剔除再重排，下标按剔除后的列表算。
#[test]
fn disable_and_move_compose() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.slash".into(), 0)],
        disabled: vec!["date.cn".into()],
        added: Vec::new(),
    };
    let ids = date_ids(&t, &a);
    assert_eq!(ids[0], "date.slash");
    assert!(!ids.contains(&"date.cn".to_string()));
    assert_eq!(ids.len(), t.entries_of(FormatKind::Date).count() - 1);
}

/// 被停用的条目即使有移动规则也不复活。
#[test]
fn disabled_entry_stays_out_even_if_moved() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("date.basic".into(), 0)],
        disabled: vec!["date.basic".into()],
        added: Vec::new(),
    };
    assert!(!date_ids(&t, &a).contains(&"date.basic".to_string()));
}

/// 调整只作用于本类，不串类。
#[test]
fn adjust_does_not_leak_across_kinds() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        moved: vec![("number.amount".into(), 0)],
        disabled: vec!["calc.result".into()],
        added: Vec::new(),
    };
    // date 类不含这些 id，故完全不受影响
    let base: Vec<String> = t
        .entries_of(FormatKind::Date)
        .map(|e| e.id.clone())
        .collect();
    assert_eq!(date_ids(&t, &a), base);
}

#[test]
fn load_without_path_falls_back_to_builtin() {
    assert_eq!(FormatTable::load(None), FormatTable::builtin());
}

#[test]
fn load_of_empty_file_falls_back_to_builtin() {
    // 空表（合法 TOML 但零条目）必须兜底，否则快捷输入整个哑掉
    let dir = std::env::temp_dir().join("wind_quick_fmt_empty_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("system.quick.toml");
    std::fs::write(&p, "# 空表\n").unwrap();
    assert_eq!(FormatTable::load(Some(&p)), FormatTable::builtin());
    let _ = std::fs::remove_file(&p);
}

#[test]
fn load_of_broken_file_falls_back_to_builtin() {
    let dir = std::env::temp_dir().join("wind_quick_fmt_broken_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("system.quick.toml");
    std::fs::write(&p, "[[formats]]\nid = ").unwrap();
    assert_eq!(FormatTable::load(Some(&p)), FormatTable::builtin());
    let _ = std::fs::remove_file(&p);
}

// ───────── 用户自定义条目（P2）─────────

fn user_entry(id: &str, kind: FormatKind, text: &str, position: i32) -> FormatEntry {
    FormatEntry {
        id: id.into(),
        kind,
        text: text.into(),
        position,
    }
}

/// 新加的条目落在本类末尾：出厂条目的位置不因为用户加了东西而变。
#[test]
fn user_entries_come_after_factory_ones() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        added: vec![
            user_entry("date.u1", FormatKind::Date, "$Y/$M/$D", 0),
            user_entry("date.u2", FormatKind::Date, "$D.$M.$Y", 1),
        ],
        ..Default::default()
    };
    let ids: Vec<&str> = t
        .entries_of_adjusted(FormatKind::Date, &a)
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    let base: Vec<&str> = t
        .entries_of(FormatKind::Date)
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(&ids[..base.len()], &base[..], "出厂条目原序不动");
    assert_eq!(&ids[base.len()..], &["date.u1", "date.u2"]);
}

/// 用户条目只出现在它自己的类别里——`added` 是一张跨类别的表，按 kind 过滤。
#[test]
fn user_entries_do_not_leak_across_kinds() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        added: vec![user_entry("number.u1", FormatKind::Number, "$N", 0)],
        ..Default::default()
    };
    for &k in FormatKind::ALL {
        let has = t
            .entries_of_adjusted(k, &a)
            .iter()
            .any(|e| e.id == "number.u1");
        assert_eq!(has, k == FormatKind::Number, "kind={}", k.as_str());
    }
}

/// 用户条目与出厂条目共用同一套调序/停用机制，不另开一条路径。
#[test]
fn user_entry_obeys_move_and_disable() {
    let t = FormatTable::builtin();
    let added = vec![user_entry("date.u1", FormatKind::Date, "$Y/$M/$D", 0)];

    let moved = FormatAdjust {
        moved: vec![("date.u1".into(), 0)],
        added: added.clone(),
        ..Default::default()
    };
    assert_eq!(
        t.entries_of_adjusted(FormatKind::Date, &moved)[0].id,
        "date.u1",
        "能移到首位"
    );

    let off = FormatAdjust {
        disabled: vec!["date.u1".into()],
        added: added.clone(),
        ..Default::default()
    };
    assert!(
        !t.entries_of_adjusted(FormatKind::Date, &off)
            .iter()
            .any(|e| e.id == "date.u1"),
        "停用后不出候选"
    );
}

/// 停用保位对用户条目同样成立：`entries_of_view` 的「假设全部启用」那趟必须带上 `added`，
/// 漏了的话用户条目找不到锚点、只能垫到末尾。
#[test]
fn disabled_user_entry_keeps_its_moved_position() {
    let t = FormatTable::builtin();
    let added = vec![user_entry("date.u1", FormatKind::Date, "$Y/$M/$D", 0)];
    let a = FormatAdjust {
        moved: vec![("date.u1".into(), 0)],
        disabled: vec!["date.u1".into()],
        added,
    };
    let view = t.entries_of_view(FormatKind::Date, &a);
    assert_eq!(view[0].entry.id, "date.u1", "★ 停用后仍在首位");
    assert!(!view[0].enabled);
    assert!(view[0].user);
}

#[test]
fn view_marks_user_entries() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        added: vec![user_entry("date.u1", FormatKind::Date, "$Y/$M/$D", 0)],
        ..Default::default()
    };
    let view = t.entries_of_view(FormatKind::Date, &a);
    for v in &view {
        assert_eq!(
            v.user,
            v.entry.id == "date.u1",
            "只有 date.u1 是用户条目，实际 id={}",
            v.entry.id
        );
    }
}

/// id 生成取「最大序号 + 1」：换成「条数 + 1」的话删掉中间一条后新条目会撞上仍存在的 id。
#[test]
fn next_user_id_survives_a_deletion_in_the_middle() {
    let mut a = FormatAdjust {
        added: vec![
            user_entry("date.u1", FormatKind::Date, "$Y", 0),
            user_entry("date.u2", FormatKind::Date, "$M", 1),
            user_entry("date.u3", FormatKind::Date, "$D", 2),
        ],
        ..Default::default()
    };
    assert_eq!(next_user_format_id(FormatKind::Date, &a), "date.u4");
    a.added.retain(|e| e.id != "date.u2"); // 删中间那条
    assert_eq!(
        next_user_format_id(FormatKind::Date, &a),
        "date.u4",
        "★ 不能回退成 date.u3——那个 id 还在用"
    );
}

/// 序号按类别各自计数（`added` 是跨类别的一张表）。
#[test]
fn next_user_id_counts_per_kind() {
    let a = FormatAdjust {
        added: vec![
            user_entry("date.u1", FormatKind::Date, "$Y", 0),
            user_entry("date.u2", FormatKind::Date, "$M", 1),
        ],
        ..Default::default()
    };
    assert_eq!(next_user_format_id(FormatKind::Date, &a), "date.u3");
    assert_eq!(next_user_format_id(FormatKind::Number, &a), "number.u1");
}

/// 提示清单里的每个名字都必须真的被 `supports_var` 接受——否则设置页照着提示写出的
/// 模板会在保存时被拒，而提示本身看不出错。
#[test]
fn var_hints_are_all_actually_supported() {
    for &k in FormatKind::ALL {
        for (name, desc) in k.var_hints() {
            assert!(
                k.supports_var(name),
                "kind={} 的提示里有不被支持的变量 ${name}",
                k.as_str()
            );
            assert!(!desc.is_empty(), "${name} 缺说明");
        }
        assert!(
            !k.var_hints().is_empty(),
            "kind={} 没有任何提示",
            k.as_str()
        );
    }
}

/// 每类的变量条数写死。加变量时这条会红——那正是它的用途：提醒同步
/// `var_hints`（设置页提示）与文档站的变量表，那两处没有编译期约束。
#[test]
fn var_hint_counts_are_pinned() {
    let n = |k: FormatKind| k.var_hints().len();
    assert_eq!(n(FormatKind::Date), 17, "公历 10 + 农历 7");
    assert_eq!(n(FormatKind::MonthDay), 17, "与 date 同一套");
    assert_eq!(n(FormatKind::YearMonth), 7, "无 $D 系列、无农历");
    assert_eq!(n(FormatKind::Number), 6);
    assert_eq!(n(FormatKind::Calc), 3);
}

/// 农历变量给 date/month_day 而不给 year_month：这条分工在提示清单里也要成立，
/// 否则用户照提示给年月写 `$LMD`，保存时才被拒。
#[test]
fn year_month_hints_offer_no_lunar_vars() {
    let names: Vec<&str> = FormatKind::YearMonth
        .var_hints()
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert!(
        !names.iter().any(|n| crate::lunar::is_var(n)),
        "年月的提示里不该有农历变量: {names:?}"
    );
    assert!(
        FormatKind::Date
            .var_hints()
            .iter()
            .any(|(n, _)| crate::lunar::is_var(n)),
        "完整日期应当提供农历变量"
    );
}

#[test]
fn user_id_shape_is_recognized() {
    assert!(is_user_format_id("date.u1"));
    assert!(is_user_format_id("month_day.u42"));
    assert!(!is_user_format_id("date.upper"), "u 后面不是数字");
    assert!(!is_user_format_id("date.cn"));
    assert!(!is_user_format_id("u1"), "没有 kind 前缀");
    assert!(!is_user_format_id("date.u"), "缺序号");
    assert!(!is_user_format_id(""));
}

/// 加载期守门：出厂表/手写文件占用 `.u数字` 形态时跳过该条，否则撞上后行为取决于遍历顺序且无报错。
#[test]
fn parse_rejects_reserved_user_id_shape() {
    let t = FormatTable::parse(
        r#"
[[formats]]
id = "date.u1"
kind = "date"
text = "$Y"

[[formats]]
id = "date.mine"
kind = "date"
text = "$M"
"#,
    )
    .unwrap();
    let ids: Vec<&str> = t.entries().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["date.mine"], "保留形态的那条被剔除");
}

#[test]
fn duplicate_text_spots_factory_and_user_entries() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        added: vec![user_entry("date.u1", FormatKind::Date, "$Y/$M/$D", 0)],
        ..Default::default()
    };
    assert_eq!(
        t.duplicate_text_of(FormatKind::Date, "$Y年$M月$D日", &a, None)
            .as_deref(),
        Some("date.cn"),
        "撞出厂条目"
    );
    assert_eq!(
        t.duplicate_text_of(FormatKind::Date, "$Y/$M/$D", &a, None)
            .as_deref(),
        Some("date.u1"),
        "撞用户条目"
    );
    assert!(
        t.duplicate_text_of(FormatKind::Date, "$YY年", &a, None)
            .is_none(),
        "没撞上"
    );
    // 同一串模板在别的类别里不算撞车（各类独立一张表）
    assert!(
        t.duplicate_text_of(FormatKind::Number, "$Y/$M/$D", &a, None)
            .is_none()
    );
}

/// 改写一条时要把自己排除掉，否则「只改了个错别字、模板没动」会被判成撞自己。
#[test]
fn duplicate_text_can_exclude_the_entry_being_edited() {
    let t = FormatTable::builtin();
    let a = FormatAdjust {
        added: vec![user_entry("date.u1", FormatKind::Date, "$Y/$M/$D", 0)],
        ..Default::default()
    };
    assert!(
        t.duplicate_text_of(FormatKind::Date, "$Y/$M/$D", &a, Some("date.u1"))
            .is_none()
    );
    assert_eq!(
        t.duplicate_text_of(FormatKind::Date, "$Y年$M月$D日", &a, Some("date.u1"))
            .as_deref(),
        Some("date.cn"),
        "排除自己不影响撞别人"
    );
}
