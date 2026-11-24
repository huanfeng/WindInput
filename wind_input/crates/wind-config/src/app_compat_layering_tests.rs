use super::*;
use crate::compat_overlay::parse_raw;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("wind_compat_lay_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn write(dir: &std::path::Path, text: &str) {
    std::fs::write(dir.join(COMPAT_FILE_NAME), text).unwrap();
}
fn read(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join(COMPAT_FILE_NAME)).unwrap_or_default()
}
fn raw(text: &str) -> Raw {
    parse_raw(text).expect("测试夹具必须是合法 TOML")
}
fn load(sys: &str, user: &str, tag: &str) -> AppCompat {
    let (d, u) = (tmp(&format!("{tag}_d")), tmp(&format!("{tag}_u")));
    write(&d, sys);
    write(&u, user);
    let c = AppCompat::load_layered(Some(&d), None, Some(&u));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&u);
    c
}

const WECHAT: &str =
    "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = true\nstale_probe_guard = true\n";

/// 用户规则只写一项 first_show_mode 时，出厂的 caret_use_top / stale_probe_guard 不得丢失
/// （整条覆盖会全丢）。
#[test]
fn a_sparse_user_rule_keeps_every_other_system_field() {
    let c = load(
        WECHAT,
        "[[apps]]\nprocess = \"weixin.exe\"\nfirst_show_mode = \"instant\"\n",
        "wechat",
    );
    let r = c.get_rule("Weixin.exe").unwrap();
    assert!(r.caret_use_top, "出厂的 caret_use_top 不能丢");
    assert!(r.stale_probe_guard, "出厂的 stale_probe_guard 不能丢");
    assert_eq!(r.first_show_mode, Some(FirstShowMode::Instant));
}

#[test]
fn explicit_false_in_the_user_layer_turns_off_a_system_switch() {
    let c = load(
        WECHAT,
        "[[apps]]\nprocess = \"Weixin.exe\"\ncaret_use_top = false\n",
        "off",
    );
    let r = c.get_rule("Weixin.exe").unwrap();
    assert!(!r.caret_use_top, "显式 false 必须能关掉出厂打开的开关");
    assert!(r.stale_probe_guard, "没提到的字段照旧继承");
}

/// `multiline_commit` 与其它字段同样逐字段叠加：用户层写 commit 压过出厂 paste，
/// unset 回到默认，都不动微信的其它出厂字段。
#[test]
fn multiline_commit_overlays_and_unsets_per_field() {
    let sys = format!("{WECHAT}multiline_commit = \"paste\"\n");
    let inherit = load(
        &sys,
        "[[apps]]\nprocess = \"weixin.exe\"\nfirst_show_mode = \"wait\"\n",
        "ml_inherit",
    );
    let r = inherit.get_rule("Weixin.exe").unwrap();
    assert_eq!(
        r.multiline_commit,
        Some(MultilineCommit::Paste),
        "不写即继承"
    );

    let off = load(
        &sys,
        "[[apps]]\nprocess = \"Weixin.exe\"\nmultiline_commit = \"commit\"\n",
        "ml_off",
    );
    let r = off.get_rule("Weixin.exe").unwrap();
    assert_eq!(r.multiline_commit, Some(MultilineCommit::Commit));
    assert!(r.caret_use_top, "没提到的字段照旧继承");

    let unset = load(
        &sys,
        "[[apps]]\nprocess = \"Weixin.exe\"\nunset = [\"multiline_commit\"]\n",
        "ml_unset",
    );
    let r = unset.get_rule("Weixin.exe").unwrap();
    assert_eq!(r.multiline_commit, None, "unset 回到默认（commit）");
    assert!(r.stale_probe_guard);
}

#[test]
fn unset_returns_a_system_field_to_follow_global() {
    let c = load(
        "[[apps]]\nprocess = \"EXCEL.EXE\"\nfirst_show_mode = \"wait\"\nauto_pair = false\n",
        "[[apps]]\nprocess = \"excel.exe\"\nunset = [\"first_show_mode\"]\n",
        "unset",
    );
    let r = c.get_rule("EXCEL.EXE").unwrap();
    assert_eq!(
        r.first_show_mode, None,
        "unset 后回到跟随全局，而不是继承出厂的 wait"
    );
    assert_eq!(r.auto_pair, Some(false), "别的字段不受影响");
}

#[test]
fn three_layers_each_overlay_the_one_below() {
    let (d, c, u) = (tmp("3_d"), tmp("3_c"), tmp("3_u"));
    write(
        &d,
        "[[apps]]\nprocess = \"A.exe\"\ncaret_use_top = true\nauto_pair = true\n",
    );
    write(
        &c,
        "[[apps]]\nprocess = \"A.exe\"\nauto_pair = false\ncaret_offset_x = 4\n",
    );
    write(&u, "[[apps]]\nprocess = \"A.exe\"\ncaret_offset_x = 9\n");
    let rt = AppCompat::load_layered(Some(&d), Some(&c), Some(&u));
    let r = rt.get_rule("A.exe").unwrap();
    assert!(r.caret_use_top, "data 层的字段被继承到最上层");
    assert_eq!(r.auto_pair, Some(false), "custom 层覆盖 data 层");
    assert_eq!(r.caret_offset_x, 9, "user 层覆盖 custom 层");
    for x in [&d, &c, &u] {
        let _ = std::fs::remove_dir_all(x);
    }
}

/// 三段各自独立叠加：为某进程写 `[[apps]]` 不会让更低层给它配的作用域 / 换行规则消失，反之亦然。
#[test]
fn sections_overlay_independently() {
    let sys = "[[apps]]\nprocess = \"WINWORD.EXE\"\ncaret_offset_y = 3\n\n\
                   [[commit_newline]]\nprocess = \"WINWORD.EXE\"\nstyle = \"cr\"\n\n\
                   [[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"Progman\", \"WorkerW\"]\n";
    let user = "[[apps]]\nprocess = \"WINWORD.EXE\"\nauto_pair = false\n\n\
                    [[apps]]\nprocess = \"explorer.exe\"\ncaret_use_top = true\n";
    let c = load(sys, user, "sections");
    assert_eq!(
        c.commit_newline_for("winword.exe"),
        Some(NewlineStyle::Cr),
        "用户改 apps 不能顶掉换行段"
    );
    assert_eq!(c.get_rule("winword.exe").map(|r| r.caret_offset_y), Some(3));
    assert!(
        c.initial_mode_applies_to_window("explorer.exe", "Progman"),
        "作用域段不受 apps 段影响"
    );
    assert!(!c.initial_mode_applies_to_window("explorer.exe", "Shell_TrayWnd"));

    let c2 = load(
        sys,
        "[[commit_newline]]\nprocess = \"winword.exe\"\nstyle = \"crlf\"\n",
        "sections2",
    );
    assert_eq!(
        c2.commit_newline_for("WINWORD.EXE"),
        Some(NewlineStyle::Crlf),
        "换行段逐字段叠加"
    );
    assert_eq!(
        c2.get_rule("WINWORD.EXE").map(|r| r.caret_offset_y),
        Some(3),
        "用户配换行不能顶掉 apps 段"
    );
}

/// 列表字段按整个字段替换，不做元素级合并。
#[test]
fn a_list_field_is_replaced_as_a_whole() {
    let c = load(
        "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"Progman\", \"WorkerW\"]\n",
        "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"CabinetWClass\"]\n",
        "list",
    );
    assert!(c.initial_mode_applies_to_window("explorer.exe", "CabinetWClass"));
    assert!(
        !c.initial_mode_applies_to_window("explorer.exe", "Progman"),
        "整个列表被替换，旧元素不保留"
    );
}

/// 一行结构错只丢这一行，同文件其它规则照常；语法错则整份跳过（且有 WARN）。
#[test]
fn a_structurally_bad_row_only_loses_itself() {
    let c = load(
        "",
        "[[apps]]\nprocess = \"good.exe\"\ncaret_use_top = true\n\n[[initial_mode_scope]]\nprocess = \"bad.exe\"\nclasses = \"nope\"\n\n[[initial_mode_scope]]\nprocess = \"ok.exe\"\nclasses = [\"A\"]\n",
        "badrow",
    );
    assert!(c.get_rule("good.exe").is_some());
    assert!(
        !c.initial_mode_applies_to_window("ok.exe", "B"),
        "好的那行正常生效"
    );
    assert!(
        c.initial_mode_applies_to_window("bad.exe", "B"),
        "坏的那行作废 = 不受限制"
    );
    let broken = load("", "[[apps\nprocess=", "syntax");
    assert!(broken.get_rule("anything.exe").is_none());
}

#[test]
fn disabling_and_reenabling_across_layers() {
    let (d, c, u) = (tmp("dis_d"), tmp("dis_c"), tmp("dis_u"));
    write(&d, "[[apps]]\nprocess = \"A.exe\"\ncaret_use_top = true\n");
    write(&c, "[[apps]]\nprocess = \"A.exe\"\ndisabled = true\n");
    let off = AppCompat::load_layered(Some(&d), Some(&c), None);
    assert!(off.get_rule("A.exe").is_none(), "custom 层禁用");
    write(&u, "[[apps]]\nprocess = \"A.exe\"\ndisabled = false\n");
    let on = AppCompat::load_layered(Some(&d), Some(&c), Some(&u));
    assert!(
        on.get_rule("A.exe").is_some_and(|r| r.caret_use_top),
        "更高层显式 disabled = false 重新启用，字段照常继承"
    );
    for x in [&d, &c, &u] {
        let _ = std::fs::remove_dir_all(x);
    }
}

/// 每个登记过的字段都必须能穿过「叠加 → 结构体」而不丢：稀疏的上层条目不得吞掉任何一个下层字段。
/// 没有任何字段享有特殊继承待遇，对全部登记字段一视同仁。
#[test]
fn every_registered_field_survives_layering() {
    use crate::compat_schema::{COMPAT_FIELDS, Kind};
    use serde_json::json;
    for f in COMPAT_FIELDS.iter().filter(|f| f.section == "apps") {
        let sample = match f.kind {
            Kind::Bool | Kind::TriBool => json!(true),
            Kind::Enum => json!(f.options[0]),
            Kind::Int => json!(7),
            Kind::Text => json!("sample"),
            Kind::TextList => json!(["A"]),
        };
        let sys = vec![serde_json::from_value(json!({"process": "p.exe", f.key: sample})).unwrap()];
        let user = vec![
            serde_json::from_value(json!({"process": "P.EXE", "comment": "稀疏条目"})).unwrap(),
        ];
        let rules: Vec<AppCompatRule> =
            materialize(&crate::compat_overlay::overlay("apps", sys, &user));
        let out = serde_json::to_value(&rules[0]).unwrap();
        assert!(
            out.get(f.key).is_some(),
            "字段 {} 被稀疏的上层条目吞掉了（或根本没能反序列化）: {out}",
            f.key
        );
    }
}

/// 开始菜单一族与便笺不执行宿主转发的启动请求，出厂必须由服务拉起；用户层显式写 false 能关掉。
#[test]
fn shipped_launch_by_service_covers_restricted_hosts() {
    let data_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data"));
    let c = AppCompat::load_layered(Some(data_dir), None, None);
    for p in [
        "SearchHost.exe",
        "searchapp.exe",
        "startmenuexperiencehost.exe",
        "textinputhost.exe",
        "Microsoft.Notes.exe",
    ] {
        assert_eq!(
            c.get_rule(p).and_then(|r| r.launch_by_service),
            Some(true),
            "{p}"
        );
    }
    assert_eq!(
        c.get_rule("notepad.exe").and_then(|r| r.launch_by_service),
        None,
        "普通软件不改拉起方式"
    );
    let user = tmp("launch_by_service_user");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join(COMPAT_FILE_NAME),
        "[[apps]]\nprocess = \"Microsoft.Notes.exe\"\nlaunch_by_service = false\n",
    )
    .unwrap();
    let c = AppCompat::load_layered(Some(data_dir), None, Some(&user));
    let _ = std::fs::remove_dir_all(&user);
    assert_eq!(
        c.get_rule("Microsoft.Notes.exe")
            .and_then(|r| r.launch_by_service),
        Some(false)
    );
}

/// 守随发布的系统层 `data/compat.toml`：解析失败会整份静默跳过，所有应用的所有规则一起失效。
#[test]
fn shipped_system_compat_configures_word_and_scopes_explorer_to_desktop() {
    let data_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../data"));
    assert!(
        load_raw(&data_dir.join(COMPAT_FILE_NAME)).is_some(),
        "随发布的 data/compat.toml 必须能解析（失败会静默吞掉全部规则）"
    );
    let c = AppCompat::load_layered(Some(data_dir), None, None);
    assert_eq!(
        c.commit_newline_for("winword.exe"),
        Some(NewlineStyle::Cr),
        "Word 的段落边界是 CR（2026-08-23 真机实测）"
    );
    assert_eq!(
        c.commit_newline_for("wps.exe"),
        None,
        "WPS 文字实测用 LF 正常分段，不该在出厂名单里"
    );
    assert!(
        c.get_rule("qq.exe")
            .is_some_and(|r| r.composition_start_pair_guard == Some(true))
    );
    let wx = c.get_rule("Weixin.exe").expect("出厂有微信规则");
    assert_eq!(wx.multiline_commit, Some(MultilineCommit::Paste));
    assert!(wx.caret_use_top && wx.stale_probe_guard, "原有字段不受影响");
    assert_eq!(
        c.get_rule("WXWork.exe").and_then(|r| r.multiline_commit),
        None,
        "只给微信开"
    );
    // 出厂按窗口类名给 Chromium / Firefox 内核换零宽空格占位（含所有 Electron 应用），
    // 其余（WPS 等靠空格撑光标矩形）保持空格。
    let placeholder = |process: &str, class: &str| {
        c.resolve(&WindowCtx {
            process,
            class,
            title: "",
        })
        .rule
        .as_ref()
        .and_then(|r| r.composition_placeholder)
        .unwrap_or_default()
    };
    for (process, class) in [
        ("msedge.exe", "Chrome_WidgetWin_1"),
        ("chrome.exe", "Chrome_WidgetWin_1"),
        ("firefox.exe", "MozillaWindowClass"),
        ("Code.exe", "Chrome_WidgetWin_1"),
        ("Feishu.exe", "Chrome_WidgetWin_0"),
    ] {
        assert_eq!(
            placeholder(process, class),
            PlaceholderChar::Blank,
            "{process} + {class} 出厂应为 blank"
        );
    }
    // 进程规则（Code.exe 的 composition_start_pair_guard）与类名规则逐字段叠加，两者都在。
    assert!(
        c.resolve(&WindowCtx {
            process: "Code.exe",
            class: "Chrome_WidgetWin_1",
            title: "",
        })
        .rule
        .as_ref()
        .is_some_and(|r| r.composition_start_pair_guard == Some(true))
    );
    for (process, class) in [
        ("wps.exe", "OpusApp"),
        ("WINWORD.EXE", "OpusApp"),
        ("notepad.exe", "Notepad"),
        // 拿不到类名 ⇒ 类名规则不命中：浏览器也退回空格（握手时还不知道窗口）。
        ("msedge.exe", ""),
        ("Code.exe", ""),
    ] {
        assert_eq!(
            placeholder(process, class),
            PlaceholderChar::Space,
            "{process} + {class:?} 出厂应保持空格"
        );
    }
    for name in ["msedge.exe", "chrome.exe", "firefox.exe"] {
        assert_eq!(
            c.composition_placeholder_for(name),
            PlaceholderChar::Space,
            "按进程名（无窗口上下文）不再命中：出厂规则已换成类名"
        );
    }
    for class in ["Progman", "WorkerW"] {
        assert!(
            c.initial_mode_applies_to_window("explorer.exe", class),
            "{class} 是桌面"
        );
    }
    for class in [
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
        "XamlExplorerHostIslandWindow",
        "ForegroundStaging",
        "TopLevelWindowForOverflowXamlIsland",
        "",
    ] {
        assert!(
            !c.initial_mode_applies_to_window("explorer.exe", class),
            "class={class:?} 不该重算初始模式"
        );
    }
    assert!(c.initial_mode_applies_to_window("notepad.exe", ""));
}

/// 写错 = 没写：错值若先参与叠加再回落成 None，会把下层的好值一起盖没
/// （`classes` 混进非字符串会让出厂的 explorer 作用域整行丢失）。
#[test]
fn an_invalid_value_in_a_higher_layer_never_hides_a_valid_lower_one() {
    let c = load(
        "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"Progman\", \"WorkerW\"]\n",
        "[[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"Progman\", 3]\n",
        "badclasses",
    );
    assert!(
        c.initial_mode_applies_to_window("explorer.exe", "WorkerW"),
        "出厂作用域必须保住"
    );
    assert!(
        !c.initial_mode_applies_to_window("explorer.exe", "Shell_TrayWnd"),
        "explorer 不能变成不受限制"
    );

    let c = load(
        "[[apps]]\nprocess = \"X60_Toolbox.exe\"\nignore_host_ime_switch = true\nfirst_show_mode = \"wait\"\n",
        "[[apps]]\nprocess = \"x60_toolbox.exe\"\nignore_host_ime_switch = \"yes\"\nfirst_show_mode = \"instnat\"\n",
        "badvals",
    );
    let r = c.get_rule("X60_Toolbox.exe").unwrap();
    assert_eq!(
        r.ignore_host_ime_switch,
        Some(true),
        "布尔写成字符串 = 没写，继承出厂"
    );
    assert_eq!(
        r.first_show_mode,
        Some(FirstShowMode::Wait),
        "枚举拼错 = 没写，继承出厂"
    );
}

/// 一个没写（或写错而被忽略）`classes` 的作用域规则作废，不能变成「任何窗口都不重算」。
#[test]
fn a_scope_rule_without_classes_is_void_not_block_all() {
    let c = load(
        "",
        "[[initial_mode_scope]]\nprocess = \"x.exe\"\n",
        "noclasses",
    );
    assert!(c.initial_mode_applies_to_window("x.exe", "AnyClass"));
    let c = load(
        "",
        "[[initial_mode_scope]]\nprocess = \"x.exe\"\nclasses = []\n",
        "emptyclasses",
    );
    assert!(
        !c.initial_mode_applies_to_window("x.exe", "AnyClass"),
        "显式写空清单才是全挡"
    );
}

/// 同一层里同名进程写了多行：后写的赢，与逐行叠加一致。
#[test]
fn duplicate_rows_for_one_process_in_a_layer_overlay_in_order() {
    let c = load(
        "",
        "[[apps]]\nprocess = \"x.exe\"\nfirst_show_mode = \"wait\"\nauto_pair = true\n\n[[apps]]\nprocess = \"X.EXE\"\nfirst_show_mode = \"fast\"\n",
        "dups",
    );
    let r = c.get_rule("x.exe").unwrap();
    assert_eq!(r.first_show_mode, Some(FirstShowMode::Fast), "后写的赢");
    assert_eq!(r.auto_pair, Some(true), "先写的其它字段保留");
}

/// 进程名带首尾空格：读入时就 trim，运行时查表才匹配得上。
#[test]
fn process_names_with_padding_are_trimmed_on_load() {
    let c = load(
        "",
        "[[apps]]\nprocess = \" Foo.exe \"\nauto_pair = true\n",
        "pad",
    );
    assert!(
        c.get_rule("foo.exe").is_some(),
        "带空格的进程名必须能被查到"
    );
}

/// `unset` 写成单个字符串也要生效；`unset = ["disabled"]` 能解除下层的禁用。
#[test]
fn unset_accepts_a_bare_string_and_can_lift_a_disable() {
    let c = load(
        "[[apps]]\nprocess = \"A.exe\"\nfirst_show_mode = \"wait\"\n",
        "[[apps]]\nprocess = \"A.exe\"\nunset = \"first_show_mode\"\nauto_pair = true\n",
        "unsetstr",
    );
    assert_eq!(c.get_rule("A.exe").unwrap().first_show_mode, None);
    let (d, cu, u) = (tmp("lift_d"), tmp("lift_c"), tmp("lift_u"));
    write(&d, "[[apps]]\nprocess = \"A.exe\"\ncaret_use_top = true\n");
    write(&cu, "[[apps]]\nprocess = \"A.exe\"\ndisabled = true\n");
    write(
        &u,
        "[[apps]]\nprocess = \"A.exe\"\nunset = [\"disabled\"]\n",
    );
    let rt = AppCompat::load_layered(Some(&d), Some(&cu), Some(&u));
    assert!(
        rt.get_rule("A.exe").is_some_and(|r| r.caret_use_top),
        "unset disabled 解除禁用"
    );
    for x in [&d, &cu, &u] {
        let _ = std::fs::remove_dir_all(x);
    }
}

// ───────────── 写回不得丢用户的东西 ─────────────

/// 非 ASCII / 带点的键写回时必须加引号，否则整个用户层文件失效。
#[test]
fn write_back_quotes_exotic_keys_so_the_file_stays_valid() {
    let dir = tmp("quote");
    write(
        &dir,
        "[[apps]]\nprocess = \"a.exe\"\n\"备注\" = \"x\"\n\"a.b\" = 1\n",
    );
    with_system("", || {
        set_user_first_show_mode(&dir, "a.exe", Some(FirstShowMode::Fast)).unwrap()
    });
    let back = parse_raw(&read(&dir)).expect("写回后的文件必须仍是合法 TOML");
    assert_eq!(back.apps[0]["备注"], serde_json::json!("x"));
    assert_eq!(
        back.apps[0]["a.b"],
        serde_json::json!(1),
        "带点的键不能被写成嵌套表"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `[apps]` 手误写成单括号时 `parse_raw` 只留 WARN 当它不存在，重写会让它彻底消失；
/// 必须按损坏处理，先留 `.bad`。
#[test]
fn skipped_content_is_treated_as_damage_and_backed_up() {
    let dir = tmp("skipped");
    let text = "[apps]\nprocess = \"lost.exe\"\nauto_pair = true\n";
    write(&dir, text);
    with_system("", || {
        set_user_first_show_mode(&dir, "a.exe", Some(FirstShowMode::Fast)).unwrap()
    });
    assert_eq!(
        std::fs::read_to_string(dir.join(format!("{COMPAT_FILE_NAME}.bad"))).unwrap(),
        text,
        "读不进来的内容必须被留档，而不是静默丢掉"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 更新版本新增的顶层段：不认识但必须原样保留，降级后第一次写回不能删掉它。
#[test]
fn unknown_top_level_sections_survive_a_write_back() {
    let dir = tmp("extra");
    write(&dir, "[future_section]\nkey = \"value\"\nn = 3\n");
    with_system("", || {
        set_user_first_show_mode(&dir, "a.exe", Some(FirstShowMode::Fast)).unwrap()
    });
    let back = parse_raw(&read(&dir)).unwrap();
    assert_eq!(back.extra["future_section"]["key"].as_str(), Some("value"));
    assert_eq!(back.extra["future_section"]["n"].as_integer(), Some(3));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 认不出的枚举值（新版本写下的）经结构体往返本来是「看不见」的，按效果判冗余会被静默删掉。
#[test]
fn a_value_this_version_cannot_parse_is_kept_untouched_by_other_edits() {
    let dir = tmp("keepval");
    write(
        &dir,
        "[[apps]]\nprocess = \"a.exe\"\nfirst_show_mode = \"turbo\"\n",
    );
    with_system("", || {
        set_user_auto_pair(&dir, "a.exe", Some(true)).unwrap()
    });
    assert!(
        read(&dir).contains("first_show_mode = \"turbo\""),
        "{}",
        read(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 下层（系统 / data_custom）把这个进程的规则禁用了，用户在菜单里给它选了一项：
/// 这是明确的「我要配置它」，要带上重新启用，否则存了却永远不生效。
#[test]
fn a_menu_choice_re_enables_a_rule_disabled_by_a_lower_layer() {
    let (d, u) = (tmp("m_dis_d"), tmp("m_dis_u"));
    let sys = "[[apps]]\nprocess = \"A.exe\"\ndisabled = true\ncaret_use_top = true\n";
    write(&d, sys);
    with_system(sys, || {
        set_user_first_show_mode(&u, "a.exe", Some(FirstShowMode::Wait)).unwrap()
    });
    let r = AppCompat::load_layered(Some(&d), None, Some(&u));
    assert_eq!(
        r.get_rule("A.exe").map(|r| r.first_show_mode),
        Some(Some(FirstShowMode::Wait))
    );
    // 用户在设置页自己禁用的，菜单不能悄悄改掉他的决定。
    let (d2, u2) = (tmp("m_dis2_d"), tmp("m_dis2_u"));
    write(&d2, "[[apps]]\nprocess = \"B.exe\"\ncaret_use_top = true\n");
    write(&u2, "[[apps]]\nprocess = \"B.exe\"\ndisabled = true\n");
    with_system(
        "[[apps]]\nprocess = \"B.exe\"\ncaret_use_top = true\n",
        || set_user_first_show_mode(&u2, "b.exe", Some(FirstShowMode::Wait)).unwrap(),
    );
    assert!(
        AppCompat::load_layered(Some(&d2), None, Some(&u2))
            .get_rule("B.exe")
            .is_none()
    );
    for x in [&d, &u, &d2, &u2] {
        let _ = std::fs::remove_dir_all(x);
    }
}

/// HostRender 白名单只收 `host_render = true` 的进程，保留原始大小写，被禁用的不算。
#[test]
fn host_render_processes_collects_only_enabled_ones_with_original_case() {
    let c = load(
        "[[apps]]\nprocess = \"SearchHost.exe\"\nhost_render = true\n\n[[apps]]\nprocess = \"Other.exe\"\ncaret_use_top = true\n\n[[apps]]\nprocess = \"Off.exe\"\nhost_render = true\ndisabled = true\n",
        "",
        "hr",
    );
    assert_eq!(
        c.host_render_processes(),
        vec!["SearchHost.exe".to_string()]
    );
}

// ───────────── 右键菜单写入路径 ─────────────

fn with_system<R>(text: &str, f: impl FnOnce() -> R) -> R {
    with_menu_system(raw(text), true, f)
}

/// 菜单按窗口规则的身份写回：出厂窗口规则在用户层写同身份的覆盖，纯进程那条不动。
#[test]
fn menu_set_by_window_identity_overrides_that_rule_only() {
    let dir = tmp("m_win");
    let sys = "[[apps]]\nprocess = \"AHK.exe\"\nfirst_show_mode = \"fast\"\n\n[[apps]]\nprocess = \"AHK.exe\"\nclass = \"AutoHotkeyGUI\"\nfirst_show_mode = \"wait\"\n";
    with_system(sys, || {
        let target = RuleId {
            process: "ahk.exe".into(),
            class: "autohotkeygui".into(),
            title: String::new(),
        };
        set_user_first_show_mode(&dir, &target, Some(FirstShowMode::Instant)).unwrap();
    });
    let user = raw(&read(&dir));
    assert_eq!(user.apps.len(), 1, "{:?}", user.apps);
    assert_eq!(
        user.apps[0]["class"],
        serde_json::json!("AutoHotkeyGUI"),
        "沿用出厂写法"
    );
    assert_eq!(user.apps[0]["process"], serde_json::json!("AHK.exe"));
    assert_eq!(
        user.apps[0]["first_show_mode"],
        serde_json::json!("instant")
    );
    let c = load(sys, &read(&dir), "m_win_l");
    let mode = |class: &str| {
        c.resolve(&WindowCtx {
            process: "ahk.exe",
            class,
            title: "",
        })
        .rule
        .as_ref()
        .and_then(|r| r.first_show_mode)
    };
    assert_eq!(mode("AutoHotkeyGUI"), Some(FirstShowMode::Instant));
    assert_eq!(mode("Other"), Some(FirstShowMode::Fast), "纯进程那条不动");
    assert!(
        set_user_auto_pair(&dir, "", Some(true)).is_err(),
        "身份为空不写"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn menu_set_stores_only_the_changed_field() {
    let dir = tmp("m_set");
    with_system(WECHAT, || {
        set_user_first_show_mode(&dir, "weixin.exe", Some(FirstShowMode::Instant)).unwrap();
    });
    let text = read(&dir);
    assert!(text.contains("first_show_mode = \"instant\""), "{text}");
    assert!(
        !text.contains("caret_use_top") && !text.contains("stale_probe_guard"),
        "不得把系统字段拷进用户层: {text}"
    );
    assert!(
        text.contains("process = \"Weixin.exe\""),
        "沿用系统层写法: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn menu_clear_of_a_system_field_writes_unset_and_runtime_follows_global() {
    let (d, u) = (tmp("m_clr_d"), tmp("m_clr_u"));
    let sys = "[[apps]]\nprocess = \"EXCEL.EXE\"\nfirst_show_mode = \"wait\"\nauto_pair = false\n";
    write(&d, sys);
    with_system(sys, || {
        set_user_first_show_mode(&u, "excel.exe", None).unwrap()
    });
    assert!(
        read(&u).contains("unset = [\"first_show_mode\"]"),
        "{}",
        read(&u)
    );
    let rt = AppCompat::load_layered(Some(&d), None, Some(&u));
    let r = rt.get_rule("EXCEL.EXE").unwrap();
    assert_eq!(
        r.first_show_mode, None,
        "菜单选「跟随全局」必须真的跟随全局，不能被出厂值顶回来"
    );
    assert_eq!(r.auto_pair, Some(false));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&u);
}

#[test]
fn menu_clear_of_a_field_the_system_lacks_leaves_no_trace() {
    let dir = tmp("m_noop");
    with_system(WECHAT, || {
        set_user_first_show_mode(&dir, "weixin.exe", Some(FirstShowMode::Fast)).unwrap();
        set_user_first_show_mode(&dir, "weixin.exe", None).unwrap();
    });
    assert!(
        !read(&dir).contains("[[apps]]"),
        "清掉最后一项后整条应消失，不留空壳: {}",
        read(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn menu_value_equal_to_the_system_leaves_no_entry() {
    let dir = tmp("m_same");
    with_system(WECHAT, || {
        set_user_host_render(&dir, "Weixin.exe", false).unwrap()
    });
    with_system(
        "[[apps]]\nprocess = \"S.exe\"\nfirst_show_mode = \"wait\"\n",
        || {
            set_user_first_show_mode(&dir, "s.exe", Some(FirstShowMode::Wait)).unwrap();
        },
    );
    assert!(
        !read(&dir).contains("[[apps]]"),
        "与系统一致 / 与默认一致都不该落盘: {}",
        read(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn menu_host_render_false_overrides_a_system_true() {
    let (d, u) = (tmp("m_hr_d"), tmp("m_hr_u"));
    let sys = "[[apps]]\nprocess = \"SearchHost.exe\"\nhost_render = true\n";
    write(&d, sys);
    with_system(sys, || {
        set_user_host_render(&u, "searchhost.exe", false).unwrap()
    });
    let rt = AppCompat::load_layered(Some(&d), None, Some(&u));
    assert!(
        rt.host_render_processes().is_empty(),
        "用户把它从白名单里去掉必须生效"
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&u);
}

/// 拖动落盘把定位方式与坐标一起写，让这条差异自成一体，不依赖系统层恰好配了什么。
#[test]
fn dragging_persists_position_mode_together_with_coords() {
    let (d, u) = (tmp("m_drag_d"), tmp("m_drag_u"));
    let sys = "[[apps]]\nprocess = \"X60_Toolbox.exe\"\nfirst_show_mode = \"wait\"\n";
    write(&d, sys);
    with_system(sys, || {
        set_user_candidate_fixed_pos(&u, "X60_Toolbox.exe", 320, 480).unwrap()
    });
    let text = read(&u);
    assert!(
        text.contains("candidate_position_mode = \"fixed\""),
        "{text}"
    );
    let r = AppCompat::load_layered(Some(&d), None, Some(&u))
        .get_rule("x60_toolbox.exe")
        .cloned()
        .unwrap();
    assert_eq!(
        r.candidate_position_mode,
        Some(CandidatePositionMode::Fixed)
    );
    assert_eq!((r.candidate_x, r.candidate_y), (320, 480));
    assert_eq!(
        r.first_show_mode,
        Some(FirstShowMode::Wait),
        "拖动不能顶掉系统层的别的字段"
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&u);
}

#[test]
fn clearing_candidate_position_mode_also_clears_coords() {
    let dir = tmp("m_cpm");
    with_system("", || {
        set_user_candidate_fixed_pos(&dir, "a.exe", 10, 20).unwrap();
        set_user_candidate_position_mode(&dir, "a.exe", None).unwrap();
    });
    assert!(
        !read(&dir).contains("candidate_"),
        "清除定位方式要一并清坐标，免得留孤儿坐标: {}",
        read(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn status_position_writes_mode_and_coords_together() {
    let dir = tmp("m_sp");
    with_system("", || {
        set_user_status_position(&dir, "a.exe", Some(StatusPositionMode::Fixed), 5, 6).unwrap();
    });
    let t = read(&dir);
    assert!(
        t.contains("status_position_mode = \"fixed\"")
            && t.contains("status_x = 5")
            && t.contains("status_y = 6"),
        "{t}"
    );
    with_system("", || {
        set_user_status_position(&dir, "a.exe", Some(StatusPositionMode::FollowCaret), 5, 6)
            .unwrap();
    });
    let t = read(&dir);
    assert!(
        t.contains("follow_caret") && !t.contains("status_x"),
        "非 fixed 不用坐标，要清掉: {t}"
    );
    with_system("", || {
        set_user_status_position(&dir, "a.exe", None, 0, 0).unwrap()
    });
    assert!(
        !read(&dir).contains("[[apps]]"),
        "清除后整条消失: {}",
        read(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 右键菜单的「启用兼容规则」：没有规则时不可开关；禁用 / 启用往返，且不动别的字段。
#[test]
fn menu_rule_switch_round_trips_and_keeps_the_other_fields() {
    let dir = tmp("m_switch");
    let sys = "[[apps]]\nprocess = \"Feishu.exe\"\nstale_probe_guard = true\n";
    with_system(sys, || {
        assert_eq!(menu_rule_switch(&dir, "nobody.exe"), RuleSwitch::NoRule);
        assert_eq!(menu_rule_switch(&dir, "feishu.EXE"), RuleSwitch::Enabled);

        set_user_rule_disabled(&dir, "Feishu.exe", true).unwrap();
        assert_eq!(menu_rule_switch(&dir, "Feishu.exe"), RuleSwitch::Disabled);

        set_user_rule_disabled(&dir, "Feishu.exe", false).unwrap();
        assert_eq!(menu_rule_switch(&dir, "Feishu.exe"), RuleSwitch::Enabled);
    });
    let body: String = read(&dir)
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !body.contains("disabled"),
        "启用 = 去掉用户层的禁用差异: {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 系统层自己就禁用了这条规则：菜单「启用 / 禁用」往返必须按用户明确指定的值落地，
/// 不能被 `edit_user_apps` 那段「顺手启用」的注入冲掉（含用户层 `unset = ["disabled"]`）。
#[test]
fn menu_rule_switch_wins_over_the_implicit_enable_when_the_system_layer_disables() {
    let sys = "[[apps]]\nprocess = \"Feishu.exe\"\ndisabled = true\nstale_probe_guard = true\n";
    for user in [
        None,
        Some("[[apps]]\nprocess = \"Feishu.exe\"\nunset = [\"disabled\"]\n"),
    ] {
        let dir = tmp("m_switch_sysdis");
        if let Some(u) = user {
            write(&dir, u);
        }
        with_system(sys, || {
            let start = menu_rule_switch(&dir, "Feishu.exe");
            set_user_rule_disabled(&dir, "Feishu.exe", true).unwrap();
            assert_eq!(
                menu_rule_switch(&dir, "Feishu.exe"),
                RuleSwitch::Disabled,
                "从 {start:?} 出发禁用后必须是禁用（用户层: {user:?}）"
            );
            set_user_rule_disabled(&dir, "Feishu.exe", false).unwrap();
            assert_eq!(menu_rule_switch(&dir, "Feishu.exe"), RuleSwitch::Enabled);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn menu_write_preserves_other_rules_other_sections_and_unknown_keys() {
    let dir = tmp("m_keep");
    write(
        &dir,
        "[[apps]]\nprocess = \"other.exe\"\nauto_pair = true\nfuture_field = 7\n\n\
             [[commit_newline]]\nprocess = \"w.exe\"\nstyle = \"cr\"\n\n\
             [[initial_mode_scope]]\nprocess = \"explorer.exe\"\nclasses = [\"A\"]\n",
    );
    with_system("", || {
        set_user_first_show_mode(&dir, "new.exe", Some(FirstShowMode::Fast)).unwrap()
    });
    let t = read(&dir);
    for needle in [
        "other.exe",
        "future_field = 7",
        "[[commit_newline]]",
        "style = \"cr\"",
        "[[initial_mode_scope]]",
        "new.exe",
    ] {
        assert!(t.contains(needle), "写回后丢了 {needle}: {t}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 各包装函数写出的值都要能被运行时读回来（往返）。
#[test]
fn every_menu_wrapper_roundtrips_through_the_runtime_loader() {
    let u = tmp("m_all");
    let d = tmp("m_all_d");
    write(&d, "");
    with_system("", || {
        set_user_initial_mode(&u, "a.exe", Some(InitialMode::English)).unwrap();
        set_user_initial_punct(&u, "a.exe", Some(InitialMode::Chinese)).unwrap();
        set_user_auto_pair(&u, "a.exe", Some(false)).unwrap();
        set_user_password_force_english(&u, "a.exe", Some(true)).unwrap();
        set_user_schema(&u, "a.exe", Some("@remember".into())).unwrap();
        set_user_ignore_host_ime_switch(&u, "a.exe", Some(true)).unwrap();
        set_user_smart_method(&u, "a.exe", Some(SmartMethod::HoldComposition)).unwrap();
        set_user_caret_offset(&u, "a.exe", 3, -4).unwrap();
        set_user_status_fallback(&u, "a.exe", Some(StatusFallback::Hide)).unwrap();
    });
    let rt = AppCompat::load_layered(Some(&d), None, Some(&u));
    let r = rt.get_rule("a.exe").expect("菜单写入后运行时必须读得到");
    assert_eq!(r.initial_mode, Some(InitialMode::English));
    assert_eq!(r.initial_punct, Some(InitialMode::Chinese));
    assert_eq!(r.auto_pair, Some(false), "显式 false 必须落盘");
    assert_eq!(r.password_force_english, Some(true));
    assert_eq!(r.app_schema(), Some(AppSchema::Remember));
    assert_eq!(r.ignore_host_ime_switch, Some(true));
    assert_eq!(r.smart_method, Some(SmartMethod::HoldComposition));
    assert_eq!((r.caret_offset_x, r.caret_offset_y), (3, -4));
    assert_eq!(r.status_fallback_position, Some(StatusFallback::Hide));
    let _ = std::fs::remove_dir_all(&u);
    let _ = std::fs::remove_dir_all(&d);
}

/// 菜单「软件切换中英」第一档是「跟随全局」：`None` 必须是清除（系统层设了就写 `unset`），
/// 而不是还原——还原会让出厂 / 定制层打开的规则继续生效，与「跟随全局」的文案相反。
#[test]
fn menu_ignore_host_ime_switch_none_follows_global() {
    let (d, u) = (tmp("m_ihs_d"), tmp("m_ihs_u"));
    let sys = "[[apps]]\nprocess = \"X60_Toolbox.exe\"\nignore_host_ime_switch = true\n";
    write(&d, sys);
    with_system(sys, || {
        set_user_ignore_host_ime_switch(&u, "x60_toolbox.exe", Some(false)).unwrap();
    });
    let off = AppCompat::load_layered(Some(&d), None, Some(&u));
    assert_eq!(
        off.get_rule("X60_Toolbox.exe")
            .unwrap()
            .ignore_host_ime_switch,
        Some(false),
        "用户显式采纳"
    );
    with_system(sys, || {
        set_user_ignore_host_ime_switch(&u, "x60_toolbox.exe", None).unwrap()
    });
    let back = AppCompat::load_layered(Some(&d), None, Some(&u));
    assert_eq!(
        back.get_rule("X60_Toolbox.exe")
            .and_then(|r| r.ignore_host_ime_switch),
        None,
        "跟随全局 = 取消下层设定: {}",
        read(&u)
    );
    with_system(sys, || {
        set_user_ignore_host_ime_switch(&u, "x60_toolbox.exe", Some(true)).unwrap()
    });
    assert!(
        !read(&u).contains("[[apps]]"),
        "显式写成与出厂相同属冗余: {}",
        read(&u)
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&u);
}

// ───────────── 旧键 ignore_host_ime_close（读取别名）─────────────

fn ihs(c: &AppCompat, process: &str) -> Option<bool> {
    c.get_rule(process).and_then(|r| r.ignore_host_ime_switch)
}

/// 旧键写在任何一层都照常生效（语义按新的双向）。
#[test]
fn legacy_ignore_host_ime_close_applies_in_any_layer() {
    let old = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = true\n";
    assert_eq!(ihs(&load(old, "", "lg_sys"), "a.exe"), Some(true), "系统层");
    assert_eq!(ihs(&load("", old, "lg_usr"), "a.exe"), Some(true), "用户层");
    assert_eq!(
        ihs(&AppCompat::from_single_layer_text(old), "a.exe"),
        Some(true),
        "单层"
    );
}

/// 新旧键分处不同层：仍按层序叠加（高层赢），而不是两个键并存、谁赢看反序列化的运气。
#[test]
fn legacy_and_new_key_in_different_layers_overlay_by_layer_order() {
    let new_t = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = true\n";
    let old_f = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = false\n";
    let new_f = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = false\n";
    let old_t = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = true\n";
    assert_eq!(ihs(&load(new_t, old_f, "lg_mix1"), "a.exe"), Some(false));
    assert_eq!(ihs(&load(old_t, new_f, "lg_mix2"), "a.exe"), Some(false));
    assert_eq!(ihs(&load(old_f, new_t, "lg_mix3"), "a.exe"), Some(true));
    // 同一文件里同身份的两行：后写的赢，不论各自用哪个键名。
    let c = AppCompat::from_single_layer_text(&format!("{new_t}\n{old_f}"));
    assert_eq!(ihs(&c, "a.exe"), Some(false));
}

/// 同一条里新旧都写：以新键为准，与两者在文件里的先后无关。
#[test]
fn legacy_and_new_key_in_one_row_new_key_wins() {
    for text in [
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = true\nignore_host_ime_switch = false\n",
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = false\nignore_host_ime_close = true\n",
    ] {
        assert_eq!(
            ihs(&AppCompat::from_single_layer_text(text), "a.exe"),
            Some(false),
            "{text}"
        );
        assert_eq!(
            ihs(&load("", text, "lg_both"), "a.exe"),
            Some(false),
            "{text}"
        );
    }
}

/// 同一条里新键写错值、旧键是合法布尔：错值 = 没写，采用旧键的合法值（不论先后）。
/// 新键合法时仍以新键为准（见上一条）；两个都错则都当没写。
#[test]
fn invalid_new_key_falls_back_to_a_valid_legacy_key_in_the_same_row() {
    for text in [
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = \"yes\"\nignore_host_ime_close = true\n",
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = true\nignore_host_ime_switch = \"yes\"\n",
    ] {
        assert_eq!(
            ihs(&AppCompat::from_single_layer_text(text), "a.exe"),
            Some(true),
            "{text}"
        );
        let sys = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = false\n";
        assert_eq!(
            ihs(&load(sys, text, "lg_badnew"), "a.exe"),
            Some(true),
            "{text}"
        );
    }
    let both_bad = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = \"yes\"\nignore_host_ime_close = 1\n";
    let sys = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = false\n";
    assert_eq!(
        ihs(&load(sys, both_bad, "lg_bothbad"), "a.exe"),
        Some(false),
        "两个都写错 = 都没写，继承下层"
    );
}

/// `unset` 里写旧键名，照样取消下层（无论下层用新键还是旧键）。
#[test]
fn unset_with_legacy_key_name_cancels_the_lower_layer() {
    let user = "[[apps]]\nprocess = \"a.exe\"\nunset = [\"ignore_host_ime_close\"]\n";
    for sys in [
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = true\n",
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = true\n",
    ] {
        assert_eq!(ihs(&load(sys, user, "lg_unset"), "a.exe"), None, "{sys}");
    }
    // 单个字符串形态的 unset 同样认。
    let user = "[[apps]]\nprocess = \"a.exe\"\nunset = \"ignore_host_ime_close\"\n";
    let sys = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = true\n";
    assert_eq!(ihs(&load(sys, user, "lg_unset1"), "a.exe"), None);
}

/// 旧键写错值同样 = 没写，不盖掉下层的好值（规范化在清理之前，清理照常起作用）。
#[test]
fn invalid_legacy_value_never_hides_a_valid_lower_one() {
    let sys = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_switch = true\n";
    let user = "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = \"yes\"\n";
    assert_eq!(ihs(&load(sys, user, "lg_bad"), "a.exe"), Some(true));
}

/// 写回（菜单）统一用新键：被编辑的那条旧键换成新键，同文件其它条里的旧键也一并
/// 换掉（值不变），不认识的键与别的段原样保留。
#[test]
fn menu_write_migrates_legacy_key_and_keeps_unknown_keys() {
    let dir = tmp("lg_wb");
    write(
        &dir,
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = true\nfuture_field = 7\n\n\
             [[apps]]\nprocess = \"b.exe\"\nignore_host_ime_close = true\nunset = [\"ignore_host_ime_close\", \"auto_pair\"]\n\n\
             [[commit_newline]]\nprocess = \"w.exe\"\nstyle = \"cr\"\n",
    );
    with_system("[[apps]]\nprocess = \"b.exe\"\nauto_pair = true\n", || {
        set_user_ignore_host_ime_switch(&dir, "a.exe", Some(false)).unwrap()
    });
    let t = read(&dir);
    assert!(
        !t.contains("ignore_host_ime_close"),
        "写回后不该再有旧键: {t}"
    );
    for needle in ["future_field = 7", "[[commit_newline]]", "style = \"cr\""] {
        assert!(t.contains(needle), "写回后丢了 {needle}: {t}");
    }
    let c = AppCompat::from_single_layer_text(&t);
    assert_eq!(ihs(&c, "a.exe"), Some(false), "被编辑那条: {t}");
    assert_eq!(
        ihs(&c, "b.exe"),
        Some(true),
        "别的条旧键换成新键、值不变: {t}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 编辑同一条的别的字段，也顺手把这条的旧键换成新键（值不变）。
#[test]
fn menu_write_of_another_field_also_migrates_legacy_key() {
    let dir = tmp("lg_wb2");
    write(
        &dir,
        "[[apps]]\nprocess = \"a.exe\"\nignore_host_ime_close = true\n",
    );
    with_system("", || {
        set_user_first_show_mode(&dir, "a.exe", Some(FirstShowMode::Fast)).unwrap()
    });
    let t = read(&dir);
    assert!(!t.contains("ignore_host_ime_close"), "{t}");
    assert!(t.contains("ignore_host_ime_switch = true"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 菜单写入不经 RPC：写成功后必须通过钩子通知（设置端据此刷新），失败的写入不通知。
#[test]
fn menu_writes_notify_the_compat_hook_only_on_success() {
    use std::sync::{Arc, Mutex};
    static SEEN: Mutex<Vec<(String, std::path::PathBuf)>> = Mutex::new(Vec::new());
    crate::change_hook::set_compat_change_hook(Arc::new(|src, dir| {
        SEEN.lock()
            .unwrap()
            .push((src.to_string(), dir.to_path_buf()));
    }));
    let dir = tmp("m_hook");
    with_system("", || {
        set_user_first_show_mode(&dir, "a.exe", Some(FirstShowMode::Fast)).unwrap()
    });
    let mine = |d: &std::path::Path| SEEN.lock().unwrap().iter().filter(|(_, p)| p == d).count();
    assert_eq!(mine(&dir), 1, "成功的菜单写入通知一次");
    with_system("", || {
        set_user_first_show_mode(&dir, "a.exe", None).unwrap()
    });
    assert_eq!(mine(&dir), 2);
    // 写不进去（目录位置被一个文件占着）⇒ 不通知
    let blocked = tmp("m_hook_blocked");
    let file_as_dir = blocked.join("f");
    std::fs::write(&file_as_dir, "x").unwrap();
    let res = with_system("", || {
        set_user_first_show_mode(&file_as_dir, "a.exe", Some(FirstShowMode::Fast))
    });
    assert!(res.is_err());
    assert_eq!(mine(&file_as_dir), 0, "失败的写入不通知");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&blocked);
}

/// 系统层读不到（菜单路径拿不到系统预置文件）时，清除字段不能凭「系统里没有」就省掉 unset。
#[test]
fn menu_clear_with_unknown_system_layer_still_writes_unset() {
    let dir = tmp("m_unknown");
    with_menu_system(Raw::default(), false, || {
        set_user_first_show_mode(&dir, "a.exe", None).unwrap();
    });
    assert!(
        parse_raw(&read(&dir))
            .unwrap()
            .apps
            .iter()
            .any(|r| r.contains_key("unset")),
        "读不到系统层时保守地写 unset: {}",
        read(&dir)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 菜单路径遇到损坏的用户层：仍然重建（不让菜单卡死），但先把原文件留一份 .bad。
#[test]
fn menu_write_on_a_broken_file_keeps_a_copy_of_the_original() {
    let dir = tmp("m_bad");
    let broken = "[[apps\nprocess=";
    write(&dir, broken);
    with_system("", || {
        set_user_first_show_mode(&dir, "a.exe", Some(FirstShowMode::Fast)).unwrap()
    });
    assert_eq!(
        std::fs::read_to_string(dir.join(format!("{COMPAT_FILE_NAME}.bad"))).unwrap(),
        broken
    );
    assert!(read(&dir).contains("a.exe"));
    let _ = std::fs::remove_dir_all(&dir);
}
