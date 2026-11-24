use super::*;
use serde_json::json;

fn obj(v: Value) -> Obj {
    v.as_object().cloned().expect("测试夹具必须是对象")
}

#[test]
fn wildcard_star_and_question_mark_boundaries() {
    assert!(wildcard_match("", ""));
    assert!(!wildcard_match("", "a"));
    assert!(wildcard_match("*", ""));
    assert!(wildcard_match("*", "anything"));
    assert!(wildcard_match("**", "x"));
    assert!(!wildcard_match("?", ""));
    assert!(wildcard_match("?", "a"));
    assert!(!wildcard_match("?", "ab"));
    assert!(wildcard_match("a?c", "abc"));
    assert!(!wildcard_match("a?c", "ac"));
    assert!(wildcard_match("Chrome_WidgetWin_*", "Chrome_WidgetWin_1"));
    assert!(wildcard_match("Chrome_WidgetWin_*", "Chrome_WidgetWin_"));
    assert!(!wildcard_match("Chrome_WidgetWin_*", "Chrome_WidgetWin"));
    assert!(
        !wildcard_match("abc", "abcd"),
        "无通配时是整串匹配，不是前缀"
    );
    assert!(!wildcard_match("bcd", "abcd"), "也不是子串");
    assert!(wildcard_match("*b*", "abc"));
    assert!(wildcard_match("*a*b", "xaybzab"), "需要回溯");
    assert!(!wildcard_match("*a*b", "xaybzac"));
    assert!(wildcard_match("a*b*c", "a-b-b-c"));
    assert!(wildcard_match("*?", "x"));
    assert!(!wildcard_match("*??", "x"));
    assert!(wildcard_match("a*", "a"));
    assert!(!wildcard_match("a*", "ba"));
}

#[test]
fn wildcard_is_case_insensitive_and_unicode_aware_per_char() {
    assert!(wildcard_match("CHROME_widgetwin_*", "chrome_WidgetWin_0"));
    assert!(
        wildcard_match("ÉDITEUR", "éditeur"),
        "非 ASCII 也不区分大小写"
    );
    assert!(
        wildcard_match("?", "中"),
        "? 匹配一个字符（按 char，不是按字节）"
    );
    assert!(wildcard_match("新建?本文档*", "新建文本文档 - 记事本"));
    assert!(!wildcard_match("??", "中"));
    assert!(wildcard_match("*记事本", "无标题 - 记事本"));
}

#[test]
fn rule_id_defaults_process_to_any_trims_and_folds_case() {
    let id = rule_id("apps", &obj(json!({"class": " Chrome_WidgetWin_* "}))).unwrap();
    assert_eq!(id.process, "*", "不写 process = 所有进程");
    assert_eq!(id.class, "chrome_widgetwin_*");
    assert_eq!(id.title, "");
    assert!(id.has_window_condition());

    let a = rule_id(
        "apps",
        &obj(json!({"process": " AutoHotkey.EXE ", "title": "Foo"})),
    );
    let b = rule_id(
        "apps",
        &obj(json!({"process": "autohotkey.exe", "title": " foo "})),
    );
    assert_eq!(a, b, "身份 trim + 不区分大小写");
    assert_ne!(
        a,
        rule_id("apps", &obj(json!({"process": "autohotkey.exe"}))),
        "带窗口条件的与纯进程规则不是同一条"
    );
    let star = rule_id("apps", &obj(json!({"process": "*"}))).unwrap();
    assert_eq!(
        star,
        rule_id("apps", &obj(json!({"process": "*", "class": " "}))).unwrap()
    );
    assert!(!star.has_window_condition());
}

#[test]
fn rule_id_voids_rows_without_any_condition_or_with_wrong_types() {
    for bad in [
        json!({"auto_pair": true}),
        json!({"process": "  ", "class": "", "title": " "}),
        json!({"process": 1}),
        json!({"process": "a.exe", "class": 1}),
        json!({"class": ["X"]}),
        json!({"process": "a.exe", "title": true}),
    ] {
        assert_eq!(rule_id("apps", &obj(bad.clone())), None, "{bad}");
    }
}

#[test]
fn side_sections_have_no_window_conditions() {
    let id = rule_id(
        "initial_mode_scope",
        &obj(json!({"process": "Explorer.exe", "class": "X", "title": 1})),
    )
    .unwrap();
    assert_eq!(
        id,
        RuleId::process_only("explorer.exe"),
        "class / title 在附属段是未知键"
    );
    assert_eq!(rule_id("commit_newline", &obj(json!({"class": "X"}))), None);
}

#[test]
fn overlay_and_coalesce_use_the_rule_identity() {
    let base = vec![
        obj(json!({"process": "AutoHotkey.exe", "auto_pair": true})),
        obj(json!({"process": "AutoHotkey.exe", "title": "Foo*", "initial_mode": "english"})),
    ];
    let layer = vec![
        obj(json!({"process": "autohotkey.exe", "title": "FOO*", "caret_use_top": true})),
        obj(json!({"class": "Chrome_WidgetWin_*", "composition_placeholder": "zwsp"})),
    ];
    let out = overlay("apps", base, &layer);
    assert_eq!(out.len(), 3, "{out:?}");
    assert_eq!(
        out[0].get("caret_use_top"),
        None,
        "纯进程那条不受窗口规则影响"
    );
    assert_eq!(out[1]["initial_mode"], json!("english"));
    assert_eq!(out[1]["caret_use_top"], json!(true), "同身份逐字段叠加");
    assert_eq!(out[1]["title"], json!("Foo*"), "保留底层的写法");
    assert_eq!(out[2]["class"], json!("Chrome_WidgetWin_*"));

    let raw = parse_raw(
        "[[apps]]\nprocess = \"a.exe\"\nclass = \"X\"\nauto_pair = true\n\n[[apps]]\nprocess = \"a.exe\"\nauto_pair = false\n\n[[apps]]\nprocess = \"A.EXE\"\nclass = \" x \"\ncaret_use_top = true\n",
    )
    .unwrap();
    assert_eq!(
        raw.apps.len(),
        2,
        "同一段里同身份的多行合成一行: {:?}",
        raw.apps
    );
    assert_eq!(raw.apps[0]["auto_pair"], json!(true));
    assert_eq!(raw.apps[0]["caret_use_top"], json!(true));
}

#[test]
fn class_and_title_cannot_be_unset() {
    let base = obj(json!({"process": "a.exe", "class": "X", "auto_pair": true}));
    let layer =
        obj(json!({"process": "a.exe", "class": "X", "unset": ["class", "title", "auto_pair"]}));
    let out = compose(&base, &layer);
    assert_eq!(out["class"], json!("X"));
    assert_eq!(out["unset"], json!(["auto_pair"]));
}

#[test]
fn sanitize_reports_and_drops_rows_without_a_usable_identity() {
    use crate::app_compat::AppCompatRule;
    let rows = vec![
        obj(json!({"auto_pair": true})),
        obj(json!({"process": "a.exe", "class": 1, "auto_pair": true})),
        obj(json!({"class": "X", "auto_pair": true})),
    ];
    let (clean, report) = sanitize::<AppCompatRule>("apps", &rows);
    assert_eq!(
        clean.len(),
        1,
        "类型错的窗口条件整条作废，而不是去掉条件后放宽成整个进程: {clean:?}"
    );
    assert_eq!(clean[0]["class"], json!("X"));
    assert_eq!(report.len(), 2, "{report:?}");
}

#[test]
fn side_sections_keep_class_as_an_unknown_key() {
    use crate::app_compat::InitialModeScopeRule;
    let rows = vec![obj(
        json!({"process": "explorer.exe", "classes": ["A"], "class": "X"}),
    )];
    let (clean, report) = sanitize::<InitialModeScopeRule>("initial_mode_scope", &rows);
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(clean[0]["class"], json!("X"), "当未知键原样保留");
    assert!(!is_meta_key("initial_mode_scope", "class"));
    assert!(is_meta_key("apps", "class") && is_meta_key("apps", "title"));
}

#[test]
fn apply_edits_by_process_never_touches_a_window_rule() {
    use crate::app_compat::AppCompatRule;
    let sys = vec![obj(
        json!({"process": "a.exe", "class": "X", "caret_use_top": true}),
    )];
    let mut user = vec![obj(
        json!({"process": "a.exe", "class": "X", "auto_pair": true}),
    )];
    apply_edits::<AppCompatRule>(
        "apps",
        &sys,
        &mut user,
        "A.exe",
        &[FieldEdit::Set("caret_use_top".into(), json!(true))],
        true,
    );
    assert_eq!(user.len(), 2, "纯进程键新建一行: {user:?}");
    assert_eq!(user[0]["auto_pair"], json!(true));
    assert_eq!(
        user[1]["caret_use_top"],
        json!(true),
        "系统那条带窗口条件，不能把它当成同一条判冗余"
    );
    assert!(user[1].get("class").is_none());
}

#[test]
fn render_puts_window_conditions_right_after_process_and_roundtrips() {
    let raw = Raw {
        apps: vec![obj(json!({
            "auto_pair": true, "title": "Foo*", "comment": "c", "class": "X", "process": "a.exe"
        }))],
        ..Default::default()
    };
    let text = render_raw("", &raw).unwrap();
    let at = |k: &str| text.find(&format!("{k} =")).unwrap();
    assert!(
        at("process") < at("class") && at("class") < at("title") && at("title") < at("comment"),
        "{text}"
    );
    assert_eq!(parse_raw(&text).unwrap().apps, raw.apps);
}

/// 去平方之前的参考实现：逐行线性查找、每次比较都重算身份。建表版必须与它逐行相同。
fn reference_coalesce(section: &str, rows: Vec<Obj>) -> Vec<Obj> {
    let mut out: Vec<Obj> = Vec::new();
    for r in rows {
        let same = rule_id(section, &r).and_then(|id| {
            out.iter()
                .position(|o| rule_id(section, o).as_ref() == Some(&id))
        });
        match same {
            Some(i) => out[i] = compose(&out[i], &r),
            None => out.push(r),
        }
    }
    out
}

fn reference_overlay(section: &str, mut base: Vec<Obj>, layer: &[Obj]) -> Vec<Obj> {
    for row in layer {
        let Some(id) = rule_id(section, row) else {
            continue;
        };
        match base
            .iter()
            .position(|b| rule_id(section, b).as_ref() == Some(&id))
        {
            Some(i) => base[i] = compose(&base[i], row),
            None => base.push(compose(&Obj::new(), row)),
        }
    }
    base
}

/// 伪随机的一段行：身份从一个小池子里抽（大小写 / 空白不同的同一身份、窗口条件、作废行），
/// 夹着禁用 / 重新启用的行与对身份键的 unset；每行带一个按行号递增的值，叠加顺序一错结果就不同。
fn noisy_rows(seed: u64, n: usize) -> Vec<Obj> {
    let mut x = seed;
    let mut next = move || {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (x >> 33) as usize
    };
    let processes = [
        None,
        Some("a.exe"),
        Some("A.EXE"),
        Some(" a.exe "),
        Some("b.exe"),
        Some("*"),
    ];
    let classes = [None, Some("X"), Some(" x "), Some("Chrome_*")];
    let titles = [None, None, Some("T*")];
    (0..n)
        .map(|i| {
            let mut o = Obj::new();
            if next() % 13 == 0 {
                o.insert("process".into(), json!(1));
            } else if let Some(p) = processes[next() % processes.len()] {
                o.insert("process".into(), json!(p));
            }
            if let Some(c) = classes[next() % classes.len()] {
                o.insert("class".into(), json!(c));
            }
            if let Some(t) = titles[next() % titles.len()] {
                o.insert("title".into(), json!(t));
            }
            o.insert("caret_offset_x".into(), json!(i));
            match next() % 6 {
                0 => {
                    o.insert("unset".into(), json!(["auto_pair"]));
                }
                // 对身份键与 `disabled` 的 unset：身份键不可 unset（叠加时被过滤），`disabled` 可以。
                1 => {
                    o.insert(
                        "unset".into(),
                        json!(["process", "class", "title", "disabled", "auto_pair"]),
                    );
                }
                _ => {
                    o.insert("auto_pair".into(), json!(next() % 2 == 0));
                }
            }
            // 禁用行照样参与跨层叠加（`disabled` 是普通字段），可被后来的 `false` 重新启用。
            match next() % 5 {
                0 => {
                    o.insert("disabled".into(), json!(true));
                }
                1 => {
                    o.insert("disabled".into(), json!(false));
                }
                _ => {}
            }
            o
        })
        .collect()
}

/// 去平方（身份 → 下标建表）不改变任何叠加结果：行序仍是各身份**首次出现**的顺序，同身份
/// 后来的行逐字段叠上去；底层本身有重复身份时套给第一行（与线性 `position` 同一口径）。
#[test]
fn indexed_coalesce_and_overlay_match_the_linear_reference() {
    for seed in 0..40u64 {
        for section in ["apps", "commit_newline"] {
            let base = noisy_rows(seed, 60);
            let layer = noisy_rows(seed + 1000, 40);
            assert_eq!(
                coalesce(section, base.clone()),
                reference_coalesce(section, base.clone()),
                "coalesce seed={seed} section={section}"
            );
            // 未合并过的底层（含重复身份）与合并过的底层各比一次。
            assert_eq!(
                overlay(section, base.clone(), &layer),
                reference_overlay(section, base.clone(), &layer),
                "overlay(raw base) seed={seed} section={section}"
            );
            let merged = coalesce(section, base);
            assert_eq!(
                overlay(section, merged.clone(), &layer),
                reference_overlay(section, merged, &layer),
                "overlay(coalesced base) seed={seed} section={section}"
            );
        }
    }
}

/// 预折叠的匹配内核与公开的 `wildcard_match` 同一口径。
#[test]
fn folded_matcher_agrees_with_the_public_one() {
    let patterns = [
        "",
        "*",
        "?",
        "a*b",
        "*X?",
        "Ǆ*",
        "chrome_widgetwin_*",
        "**?",
    ];
    let texts = ["", "a", "ab", "aXb", "ǆz", "Chrome_WidgetWin_1", "xyz"];
    for p in patterns {
        for t in texts {
            let pf: Vec<char> = p.chars().map(fold_char).collect();
            let tf: Vec<char> = t.chars().map(fold_char).collect();
            assert_eq!(
                wildcard_match_folded(&pf, &tf),
                wildcard_match(p, t),
                "{p:?} vs {t:?}"
            );
        }
    }
}
