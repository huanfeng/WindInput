/// 各平台只收自己的条目与全平台条目。
///
/// 变异判据：把比对改回写死 `"windows"`，linux / macos 两行转红。
#[test]
fn platform_filter_follows_current_platform() {
    for cur in ["windows", "macos", "linux", "android"] {
        assert!(platform_matches(None, cur));
        assert!(platform_matches(Some(""), cur));
        assert!(platform_matches(Some("all"), cur));
        assert!(platform_matches(Some("ALL"), cur));
        for other in ["windows", "macos", "linux", "android"] {
            assert_eq!(
                platform_matches(Some(other), cur),
                other == cur,
                "{other} @ {cur}"
            );
        }
    }
    assert!(platform_matches(Some("Windows"), "windows"));
    // darwin 是 macos 的旧写法，条目与当前平台哪边写哪个都认
    assert!(platform_matches(Some("darwin"), "macos"));
    assert!(platform_matches(Some("MacOS"), "darwin"));
    assert!(!platform_matches(Some("macos"), "linux"));
    assert!(!platform_matches(Some("macos"), "windows"));
    assert!(!platform_matches(Some("linux"), "other"));
    assert!(!platform_matches(Some("linux"), "android"));
}

/// `desktop` = 三种电脑，手机上没有：电脑专用条目写一条就够，不用为手机看不见复制三份。
#[test]
fn desktop_group_covers_the_three_computers_only() {
    for cur in ["windows", "macos", "darwin", "linux"] {
        assert!(platform_matches(Some("desktop"), cur), "{cur}");
    }
    assert!(!platform_matches(Some("desktop"), "android"));
    assert!(!platform_matches(Some("android"), "windows"));
}

/// 出厂关闭（`enabled = false`）的条目：入库同步照样给出（带上关闭态，用户能打开），
/// 直接建层的 `load` 不收（没有库可记开关的场合，关着就是没有）。
#[test]
fn disabled_entries_are_synced_off_and_skipped_by_load() {
    let dir = std::env::temp_dir().join(format!("wind_phrase_off_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("system.phrases.toml");
    std::fs::write(
        &p,
        "[[phrases]]\ncode = 'ts'\ntext = 'off'\nenabled = false\n\n\
             [[phrases]]\ncode = 'ts'\ntext = 'on'\n",
    )
    .unwrap();
    let synced = PhraseLayer::parse_system_entries_for(&p, "linux");
    let flags: Vec<_> = synced
        .iter()
        .map(|e| (e.text.as_str(), e.enabled))
        .collect();
    assert_eq!(flags, [("off", false), ("on", true)]);
    let layer = PhraseLayer::load(&p);
    let texts: Vec<_> = layer
        .map
        .get("ts")
        .unwrap()
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(texts, ["on"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 本进程编出来的平台名与短语文件用的词表一致（写错一个字母，本平台的条目就全没了）。
#[test]
fn current_platform_name_is_from_the_file_vocabulary() {
    let expect = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    };
    assert_eq!(CURRENT_PLATFORM, expect);
}

/// 两个加载入口（建层的 `load` 与入库同步的 `parse_system_entries`）用同一个过滤。
#[test]
fn both_loaders_apply_the_platform_filter() {
    let dir = std::env::temp_dir().join(format!("wind_phrase_plat_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("system.phrases.toml");
    std::fs::write(
        &p,
        "[[phrases]]\ncode = 'x'\ntext = 'win'\nplatform = 'windows'\n\n\
             [[phrases]]\ncode = 'x'\ntext = 'mac'\nplatform = 'darwin'\n\n\
             [[phrases]]\ncode = 'x'\ntext = 'lin'\nplatform = 'linux'\n\n\
             [[phrases]]\ncode = 'x'\ntext = 'any'\n",
    )
    .unwrap();
    let own = match CURRENT_PLATFORM {
        "windows" => "win",
        "macos" => "mac",
        "linux" => "lin",
        _ => "",
    };
    let mut want: Vec<&str> = vec!["any"];
    if !own.is_empty() {
        want.push(own);
    }
    want.sort();

    let mut got: Vec<String> = PhraseLayer::parse_system_entries(&p)
        .into_iter()
        .map(|e| e.text)
        .collect();
    got.sort();
    assert_eq!(got, want);

    let layer = PhraseLayer::load(&p);
    let mut got: Vec<String> = layer.map["x"].iter().map(|e| e.text.clone()).collect();
    got.sort();
    assert_eq!(got, want);
    let _ = std::fs::remove_dir_all(&dir);
}
use super::*;
use chrono::TimeZone;

#[test]
fn has_longer_code_detects_strict_prefix() {
    let layer = PhraseLayer::from_records([
        PhraseSeed {
            code: "date".into(),
            text: "X".into(),
            weight: 0,
            position: 0,
            is_system: false,
            category: String::new(),
        },
        PhraseSeed {
            code: "dates".into(),
            text: "Y".into(),
            weight: 0,
            position: 0,
            is_system: false,
            category: String::new(),
        },
    ]);
    assert!(
        layer.has_longer_code("dat", &PhraseScope::ALL),
        "date/dates 都比 dat 长"
    );
    assert!(
        layer.has_longer_code("date", &PhraseScope::ALL),
        "存在更长码 dates"
    );
    assert!(
        !layer.has_longer_code("dates", &PhraseScope::ALL),
        "dates 已最长，无更长后继"
    );
    assert!(
        !layer.has_longer_code("zzz", &PhraseScope::ALL),
        "无以 zzz 为前缀的短语"
    );
    assert!(!layer.has_longer_code("", &PhraseScope::ALL), "空码不算");
}

fn fixed() -> DateTime<Local> {
    // 2026-06-14 09:05:07 周日
    Local.with_ymd_and_hms(2026, 6, 14, 9, 5, 7).unwrap()
}

/// 无宿主能力的求值环境（剪贴板与反查均返回空串）。
fn no_clip() -> PhraseHost<'static> {
    PhraseHost::empty()
}

/// 剪贴板反查（出厂 `cofc`）的端到端形状：纯模板短语的 display 就是上屏文本（无
/// `command_src`，不需要 `type()`），查不到时整条候选消失而不是留一条空白的。
#[test]
fn clipboard_reverse_renders_and_drops_when_nothing_found() {
    let layer = PhraseLayer::from_records(vec![PhraseSeed {
        code: "cofc".into(),
        text: "{dict.rev(clip())}".into(),
        weight: 2000,
        position: 0,
        is_system: true,
        category: String::new(),
    }]);
    let clip = |_n: i64| "好吧".to_string();
    // 整串反查：只认「好吧」，借此模拟「这个词查不到」。
    let reverse = |text: &str, _fmt: &str, _m: usize| {
        if text == "好吧" {
            "好吧: hbba hǎo ba".to_string()
        } else {
            String::new()
        }
    };
    let host = PhraseHost {
        clip: &clip,
        reverse: &reverse,
    };
    let hits = layer.lookup_at("cofc", fixed(), &[], &host, &PhraseScope::ALL);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].text, "好吧: hbba hǎo ba");
    assert!(
        hits[0].command_src.is_none(),
        "无动作短语：display 即上屏文本，不该被当成命令候选"
    );

    // 剪贴板为空 → 无字可查 → 不出候选（而不是出一条空白候选）。
    let empty_clip = |_n: i64| String::new();
    let host = PhraseHost {
        clip: &empty_clip,
        reverse: &reverse,
    };
    assert!(
        layer
            .lookup_at("cofc", fixed(), &[], &host, &PhraseScope::ALL)
            .is_empty(),
        "剪贴板为空时不得产出空白候选"
    );
}

/// `$SS` 逐字反查：元素个数即上限，剪贴板不足位数时尾部元素整条消失。
/// 「限制 N 个字」不靠配置键，靠词条里写几个元素。
#[test]
fn reverse_group_drops_tail_when_clipboard_is_shorter() {
    let src =
        r#"$SS("反查", "{dict.rev(clip(),1)}", "{dict.rev(clip(),2)}", "{dict.rev(clip(),3)}")"#;
    let layer = PhraseLayer::from_records(vec![PhraseSeed {
        code: "cofc".into(),
        text: src.into(),
        weight: 2000,
        position: 0,
        is_system: true,
        category: String::new(),
    }]);
    let clip = |_n: i64| "好人".to_string(); // 只有 2 个字，第 3 个元素必然落空
    let reverse = |text: &str, _f: &str, _m: usize| format!("{text}=x");
    let host = PhraseHost {
        clip: &clip,
        reverse: &reverse,
    };
    let texts: Vec<String> = layer
        .lookup_at("cofc", fixed(), &[], &host, &PhraseScope::ALL)
        .into_iter()
        .map(|h| h.text)
        .collect();
    assert_eq!(
        texts,
        vec!["好=x".to_string(), "人=x".to_string()],
        "第 3 条应整条消失，而不是留一条空白候选"
    );
}

#[test]
fn test_expand_date() {
    let now = fixed();
    assert_eq!(
        expand_template("$Y年$M月$D日", &now).unwrap(),
        "2026年6月14日"
    );
    assert_eq!(expand_template("$Y-$MM-$DD", &now).unwrap(), "2026-06-14");
}

/// 年份四态（`$Y` `$YYYY` `$YY` `$YC`）与快捷输入格式表同名同义。
///
/// 直接拿 `QuickValues` 做对照：两处对用户是同一套写法，任一侧少名字或取值不同，症状都是
/// 「同样的模板抄到短语里一条候选都不出」且日志干净（`expand_template` 遇未知变量整条作废）。
#[test]
fn test_expand_year_forms_agree_with_quick_format_table() {
    let now = fixed(); // 2026-06-14
    assert_eq!(expand_template("$YY-$MM-$DD", &now).unwrap(), "26-06-14");
    assert_eq!(expand_template("${YY}年", &now).unwrap(), "26年");
    assert_eq!(
        expand_template("$YYYY-$MM-$DD", &now).unwrap(),
        "2026-06-14"
    );
    let quick = wind_quick_input::QuickValues::Date {
        y: 2026,
        m: 6,
        d: 14,
    };
    for name in ["Y", "YYYY", "YY", "YC", "M", "MM", "MC", "D", "DD", "DC"] {
        assert_eq!(
            expand_var(name, &now),
            quick.get(name),
            "${name} 在短语与格式表两处取值不一致"
        );
    }
}

#[test]
fn test_expand_time_and_week() {
    let now = fixed();
    assert_eq!(expand_template("$HH:$mm:$ss", &now).unwrap(), "09:05:07");
    assert_eq!(expand_template("星期$WC", &now).unwrap(), "星期日");
}

#[test]
fn test_expand_chinese() {
    let now = fixed();
    assert_eq!(
        expand_template("${YC}年${MC}月${DC}日", &now).unwrap(),
        "二〇二六年六月十四日"
    );
}

/// 农历变量在短语里可用，取值与快捷输入同源。
#[test]
fn test_expand_lunar() {
    let now = fixed(); // 2026-06-14 → 丙午年四月廿九，非节日
    assert_eq!(expand_template("农历$LMD", &now).unwrap(), "农历四月廿九");
    assert_eq!(
        expand_template("$LY年$LMD", &now).unwrap(),
        "丙午年四月廿九"
    );
    assert_eq!(expand_template("${LM}", &now).unwrap(), "四月");
    assert_eq!(expand_template("${LD}", &now).unwrap(), "廿九");
    assert_eq!(expand_template("$LZ年", &now).unwrap(), "马年");
}

/// `$LF` 在非节日展开成空串，整条照常出，不是整条消失：
/// `$LY年$LMD$LF` 要的是「节日当天追加节日名」，平常日子仍要得到日期。
#[test]
fn test_expand_lunar_festival_is_empty_on_ordinary_days() {
    let plain = fixed(); // 2026-06-14 不是节日
    assert_eq!(expand_template("今天是$LF", &plain).unwrap(), "今天是");
    // 追加式写法：平常日子只少了节日名，日期部分照常
    assert_eq!(
        expand_template("$LY年$LMD$LF", &plain).unwrap(),
        "丙午年四月廿九"
    );

    let duanwu = Local.with_ymd_and_hms(2026, 6, 19, 9, 0, 0).unwrap();
    assert_eq!(
        expand_template("今天是$LF", &duanwu).unwrap(),
        "今天是端午节"
    );
    assert_eq!(
        expand_template("$LY年$LMD$LF", &duanwu).unwrap(),
        "丙午年五月初五端午节"
    );
    assert_eq!(expand_template("$LMD", &duanwu).unwrap(), "五月初五");
}

/// 用户实际写的那条短语：紧邻的四个农历变量必须各自独立展开。
///
/// 同时压两件事：变量名边界（`$LMD` 不能被切成 `$LM`+`D`、`$LY` 不能吞掉后面的 `$LZ`），
/// 以及非节日下 `$LF` 不毒杀整条。
#[test]
fn test_expand_adjacent_lunar_vars() {
    let plain = fixed(); // 2026-06-14 非节日
    assert_eq!(
        expand_template("$LY$LZ$LMD$LF", &plain).unwrap(),
        "丙午马四月廿九"
    );
    let duanwu = Local.with_ymd_and_hms(2026, 6, 19, 9, 0, 0).unwrap();
    assert_eq!(
        expand_template("$LY$LZ$LMD$LF", &duanwu).unwrap(),
        "丙午马五月初五端午节"
    );
    // $LM 与 $LMD 是不同变量，紧邻时不得相互吞并
    assert_eq!(
        expand_template("$LM|$LD|$LMD", &plain).unwrap(),
        "四月|廿九|四月廿九"
    );
    // $LY 与 $LYN 同理（前者干支、后者农历年数字）
    assert_eq!(expand_template("$LY|$LYN", &plain).unwrap(), "丙午|2026");
}

/// 写错的变量名仍须整条作废：「取不到值给空串」不能退化成「什么都放行」。
/// 两者共用一个 `Option`，改 `$LF` 时最容易连带打穿这道保护。
#[test]
fn test_unknown_var_still_kills_template() {
    let now = fixed();
    assert!(expand_template("$LNOPE", &now).is_none());
    assert!(expand_template("农历$LMD$NOPE", &now).is_none());
}

#[test]
fn test_expand_dir_var() {
    // 内部目录变量在普通短语文本里也展开：同样的 `${APP_DIR}` 若只在 $CC 里生效、
    // 直接写就不生效，用户无从分辨是写错了还是没支持。
    let now = fixed();
    let want = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(!want.is_empty(), "APP_DIR 期望值不该为空");
    assert_eq!(expand_template("${APP_DIR}", &now).unwrap(), want);
    assert_eq!(
        expand_template(r"${APP_DIR}\data", &now).unwrap(),
        format!(r"{want}\data")
    );
    // 无花括号的 `$APP_DIR` 只吃到 `$APP`（变量名扫描遇下划线即止）→ 未知 → None。
    // 目录变量一律要求花括号形式，与 CLI 侧写法一致。
    assert!(expand_template("$APP_DIR", &now).is_none());
}

#[test]
fn test_escape_and_unsupported() {
    let now = fixed();
    assert_eq!(expand_template("$$5", &now).unwrap(), "$5");
    // 含不支持的模板变量 → None（注意 $AA( 是 cmdbar 字符组 marker，走 cmdbar 路径，
    // 不经此简单模板展开；这里用一个永不存在的变量名验证未知变量降级）。
    assert!(expand_template("$QQ", &now).is_none());
}

#[test]
fn test_cmdbar_dual_path() {
    // 命令栏语法（含 {expr}）走 cmdbar；其余走简单模板。
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "rq".into(),
        vec![PhraseEntry {
            text: r#"{date("YYYY-MM-DD")}"#.into(),
            weight: 1000,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    map.insert(
        "js".into(),
        vec![PhraseEntry {
            text: "{calc(\"1+2*3\")}".into(),
            weight: 900,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    map.insert(
        "old".into(),
        vec![PhraseEntry {
            text: "$Y-$MM-$DD".into(),
            weight: 800,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    // $CC 命令短语（有动作）：暂不显现（待动作执行通路），避免误上屏 display 标签。
    map.insert(
        "cmd".into(),
        vec![PhraseEntry {
            text: r#"$CC("切简繁", ime.toggle("s2t"))"#.into(),
            weight: 700,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let now = fixed();
    let rq = layer.lookup_at("rq", now, &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(rq.len(), 1);
    assert_eq!(rq[0].text, "2026-06-14");
    assert_eq!(rq[0].weight, 1000);
    let js = layer.lookup_at("js", now, &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(js.len(), 1);
    assert_eq!(js[0].text, "7");
    // 旧简单模板路径仍工作；source_text 保留未展开原文（右键禁用短语用）。
    let old = layer.lookup_at("old", now, &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].text, "2026-06-14");
    assert_eq!(old[0].source_text, "$Y-$MM-$DD");
    // 命令短语：display 为标签，携带命令源（选中时执行动作）。
    let cmd = layer.lookup_at("cmd", now, &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(cmd.len(), 1);
    assert_eq!(cmd[0].text, "切简繁");
    assert_eq!(
        cmd[0].command_src.as_deref(),
        Some(r#"$CC("切简繁", ime.toggle("s2t"))"#)
    );
}

/// 用户词库里写 `{dict.rev(clip())}` 的完整两种结局。
///
/// 这条路径与短语层是两套代码（`expand_dict_value` vs `lookup_at`），短语层测绿证明不了
/// 词库路径也对。
#[test]
fn dict_value_clipboard_reverse_both_outcomes() {
    let clip = |_n: i64| "好".to_string();

    // 查得到 → 展开为反查结果，无 command_src（选中即上屏该文本）。
    let ok = |_t: &str, _f: &str, _m: usize| "好: vbg hǎo".to_string();
    let host = PhraseHost {
        clip: &clip,
        reverse: &ok,
    };
    assert_eq!(
        expand_dict_value("{dict.rev(clip())}", "fc", fixed(), &[], &host),
        DictExpansion::Single {
            display: "好: vbg hǎo".into(),
            command_src: None,
        }
    );

    // 查不到 → 必须是 Drop（整条不出现），不能退回 `None`：词库路径的 `None` 语义是
    // 「这不是特殊语法，保留原文」，候选会显示成源码字面 `{dict.rev(clip())}` 并原样上屏。
    let miss = |_t: &str, _f: &str, _m: usize| String::new();
    let host = PhraseHost {
        clip: &clip,
        reverse: &miss,
    };
    assert_eq!(
        expand_dict_value("{dict.rev(clip())}", "fc", fixed(), &[], &host),
        DictExpansion::Drop,
        "查不到时必须丢弃候选，而不是把模板源码当文本留下"
    );
}

#[test]
fn test_expand_dict_value_reuse() {
    // 词库候选复用短语 / cmdbar 的统一展开（对齐 Go dict.ValueExpander）。
    let now = fixed();
    let clip = no_clip();
    // 简单模板变量 $Y年$M月$D日 → 展开为日期。
    assert_eq!(
        expand_dict_value("$Y年$M月$D日", "now", now, &[], &clip),
        DictExpansion::Single {
            display: "2026年6月14日".into(),
            command_src: None,
        }
    );
    // 花括号插值 {date()} → display，无命令源。
    assert_eq!(
        expand_dict_value(r#"{date("YYYY-MM-DD")}"#, "rq", now, &[], &clip),
        DictExpansion::Single {
            display: "2026-06-14".into(),
            command_src: None,
        }
    );
    // $CC 命令 → display + 命令源。
    assert_eq!(
        expand_dict_value(r#"$CC("切简繁", ime.toggle("s2t"))"#, "co", now, &[], &clip),
        DictExpansion::Single {
            display: "切简繁".into(),
            command_src: Some(r#"$CC("切简繁", ime.toggle("s2t"))"#.into()),
        }
    );
    // $AA 字符组 → Group（组名 + 成员）；由调用方按精确/前缀决定展开或折叠。
    assert_eq!(
        expand_dict_value(r#"$AA("数字", "①②③")"#, "sz", now, &[], &clip),
        DictExpansion::Group {
            name: "数字".into(),
            items: vec![("①".into(), None), ("②".into(), None), ("③".into(), None),],
        }
    );
    // 普通词库文本（无 $ 与 {）→ None，零干预。
    assert_eq!(
        expand_dict_value("你好", "nh", now, &[], &clip),
        DictExpansion::None
    );
    // 含 $ 但非模板变量（如 价格$5）→ None，不误展开。
    assert_eq!(
        expand_dict_value("价格$5", "jg", now, &[], &clip),
        DictExpansion::None
    );
}

/// 静态短语的前缀命中要带剩余编码注释，与 marker 短语一致：5 码短语 `zzsfz` 敲到 `zzsf`
/// 时要看得出「还差一个 z」。
#[test]
fn prefix_hit_carries_remaining_code_comment() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "zzsfz".into(),
        vec![PhraseEntry {
            text: "TEST".into(),
            weight: 1800,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    map.insert(
        "zzsfzab".into(),
        vec![PhraseEntry {
            text: r#"$SS("组", "甲")"#.into(),
            weight: 100,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };

    let hits = layer.lookup_prefix_at("zzsf", fixed(), &[], 2, &PhraseScope::ALL);
    let stat = hits
        .iter()
        .find(|h| h.text == "TEST")
        .expect("静态短语应出现在前缀命中里");
    assert_eq!(stat.comment, "z", "静态短语须带剩余编码");
    let marker = hits
        .iter()
        .find(|h| h.text == "组")
        .expect("marker 短语应出现在前缀命中里");
    assert_eq!(marker.comment, "zab", "marker 短语的既有行为不得变");

    // 精确命中（lookup）无剩余编码，不得凭空冒出注释。
    let exact = layer.lookup_at("zzsfz", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].comment, "", "精确路径不得带剩余编码注释");
}

#[test]
fn test_cmdbar_array_phrase_expands() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "fh".into(),
        vec![PhraseEntry {
            text: r#"$SS("符号", "（）", "【】")"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_at("fh", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    let src = r#"$SS("符号", "（）", "【】")"#;
    assert_eq!(
        got,
        vec![
            PhraseHit::plain("（）".into(), 500)
                .with_source(src)
                .with_group("符号"),
            PhraseHit::plain("【】".into(), 500)
                .with_source(src)
                .with_group("符号"),
        ]
    );
}

/// `$SS` 内嵌 `$CC`：每个命令成员是独立的命令候选，携元素级命令源（不是整组源）。
#[test]
fn test_cmdbar_array_embedded_command_members() {
    let src =
        r#"$SS("站点", "纯文本", $CC("百度", open("https://baidu.com")), $CC("上屏", type("x")))"#;
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "zzgo".into(),
        vec![PhraseEntry {
            text: src.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_at("zzgo", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(got.len(), 3, "字面 + 两个命令成员都应出现: {got:?}");
    assert_eq!(got[0].text, "纯文本");
    assert!(got[0].command_src.is_none());
    assert_eq!(got[1].text, "百度");
    let cmd = got[1].command_src.as_deref().expect("命令成员带命令源");
    assert!(
        cmd.starts_with("$CC(") && cmd.contains("baidu.com"),
        "{cmd}"
    );
    assert!(!cmd.contains("$SS"), "命令源只含该成员，不含整组: {cmd}");
    assert_eq!(got[2].text, "上屏");
    assert!(got[2].command_src.as_deref().unwrap().contains("type("));
    // 前缀导航里组仍是一条组名候选，不因含命令成员而变化。
    let nav = layer.lookup_prefix_at("zz", fixed(), &[], 2, &PhraseScope::ALL);
    assert_eq!(nav.len(), 1);
    assert_eq!(nav[0].nav_code.as_deref(), Some("zzgo"));
    assert!(nav[0].command_src.is_none());
}

/// 词库文件里的 `$SS` 同样带出命令成员（`items` 第二槽 = 元素级命令源）。
#[test]
fn dict_value_group_carries_command_members() {
    let now = fixed();
    let clip = no_clip();
    let got = expand_dict_value(
        r#"$SS("站点", "纯文本", $CC("百度", open("https://baidu.com")))"#,
        "zzgo",
        now,
        &[],
        &clip,
    );
    let DictExpansion::Group { name, items } = got else {
        panic!("应为 Group: {got:?}")
    };
    assert_eq!(name, "站点");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], ("纯文本".to_string(), None));
    assert_eq!(items[1].0, "百度");
    assert!(
        items[1]
            .1
            .as_deref()
            .is_some_and(|c| c.contains("baidu.com"))
    );
}

#[test]
fn test_cmdbar_aa_char_group_expands() {
    // $AA 字符组：逐字符炸开为多个上屏候选（镜像发货 system.phrases.toml 的符号组）。
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "zzsz".into(),
        vec![PhraseEntry {
            text: r#"$AA("数字", "①②③")"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_at("zzsz", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    let src = r#"$AA("数字", "①②③")"#;
    assert_eq!(
        got,
        vec![
            PhraseHit::plain("①".into(), 500)
                .with_source(src)
                .with_group("数字"),
            PhraseHit::plain("②".into(), 500)
                .with_source(src)
                .with_group("数字"),
            PhraseHit::plain("③".into(), 500)
                .with_source(src)
                .with_group("数字"),
        ]
    );
}

#[test]
fn test_prefix_nav_lists_matching_groups() {
    // 敲 zz → 列出 zzbd/zzsz 字符组导航候选（组名 + 码后缀），不含无关码。
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "zzbd".into(),
        vec![PhraseEntry {
            text: r#"$AA("标点", "、。")"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    map.insert(
        "zzsz".into(),
        vec![PhraseEntry {
            text: r#"$AA("数字", "①②")"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    map.insert(
        "xx".into(),
        vec![PhraseEntry {
            text: "无关".into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_prefix_at("zz", fixed(), &[], 2, &PhraseScope::ALL);
    assert_eq!(got.len(), 2);
    // 同权重按完整码字母序 zzbd < zzsz。
    assert_eq!(got[0].text, "标点");
    assert_eq!(got[0].comment, "bd");
    assert_eq!(got[0].nav_code.as_deref(), Some("zzbd"));
    assert_eq!(got[1].text, "数字");
    assert_eq!(got[1].comment, "sz");
    assert_eq!(got[1].nav_code.as_deref(), Some("zzsz"));
}

#[test]
fn test_prefix_nav_min_len_gate_and_exact_excluded() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "zzbd".into(),
        vec![PhraseEntry {
            text: r#"$AA("标点", "、。")"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    // 前缀长度 < min_len → 不触发。
    assert!(
        layer
            .lookup_prefix_at("z", fixed(), &[], 2, &PhraseScope::ALL)
            .is_empty()
    );
    // 精确码本身（== 完整码）不作为导航候选返回（只列更长的码）。
    assert!(
        layer
            .lookup_prefix_at("zzbd", fixed(), &[], 2, &PhraseScope::ALL)
            .is_empty()
    );
    // 真前缀 → 1 个导航候选。
    assert_eq!(
        layer
            .lookup_prefix_at("zz", fixed(), &[], 2, &PhraseScope::ALL)
            .len(),
        1
    );
}

#[test]
fn test_prefix_nav_command_default_on_prefix_false_off() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "cobd".into(),
        vec![PhraseEntry {
            text: r#"$CC("百度", open("https://baidu.com"))"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    map.insert(
        "coex".into(),
        vec![PhraseEntry {
            text: r#"$CC("退出", type("x"), {prefix: false})"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_prefix_at("co", fixed(), &[], 2, &PhraseScope::ALL);
    // $CC 默认列出（百度），显式 {prefix: false} 退出列举（退出）。
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "百度");
    assert_eq!(got[0].comment, "bd");
    assert_eq!(got[0].nav_code.as_deref(), Some("cobd"));
    // 命令 nav：携命令源（选中直接执行，非二级展开）。
    assert!(got[0].command_src.is_some());
}

#[test]
fn test_prefix_nav_command_display_skips_clipboard_read() {
    // 命令 display 含 {clip()}（如 coad）：列举用廉价上下文，clip() 返回空——
    // 不读整个剪贴板（内存安全），只显示静态部分。
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "coad".into(),
        vec![PhraseEntry {
            text: r#"$CC("剪贴板加词:{clip()}", type("x"))"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_prefix_at("co", fixed(), &[], 2, &PhraseScope::ALL);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "剪贴板加词:");
    assert!(got[0].command_src.is_some());
}

#[test]
fn test_prefix_nav_includes_literal_template() {
    // 静态/旧模板短语（无 marker）参与前缀列举时，$Y/$MM/$DD 等旧式变量
    // 应经 expand_template 展开（对齐 lookup 精确匹配路径的双路径策略）。
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "rq".into(),
        vec![PhraseEntry {
            text: "$Y-$MM-$DD".into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_prefix_at("r", fixed(), &[], 1, &PhraseScope::ALL);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "2026-06-14");
    assert!(got[0].command_src.is_none());
    assert!(got[0].nav_code.is_none());
}

/// 精确与前缀两条路径对同一条短语必须同处置。
///
/// 前缀路径若「展开失败退回原文」，会出现打全码无候选、打前缀反而看见字面 `$LY$LZ$LMD$LF`
/// 且选中即原样上屏，现象相反、用户很难描述。用写错的变量名做样本：`$LF` 展开成空串，
/// 走不到失败分支，拿它当样本会恒绿。
#[test]
fn test_prefix_and_exact_agree_on_broken_template() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "rq".into(),
        vec![PhraseEntry {
            text: "农历$NOPE".into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    // 前缀路径：不得出候选，更不得把字面 `$NOPE` 端上来
    let pre = layer.lookup_prefix_at("r", fixed(), &[], 1, &PhraseScope::ALL);
    assert!(
        pre.is_empty(),
        "展开失败的模板不该出现在前缀候选里，实得 {:?}",
        pre.iter().map(|h| &h.text).collect::<Vec<_>>()
    );
    // 精确路径：同样不出候选
    let exact = layer.lookup_at("rq", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert!(exact.is_empty(), "精确路径同样应跳过该条");
}

/// 含 `$LF` 的短语在非节日要照常出候选，两条路径都是。
#[test]
fn test_lunar_festival_phrase_survives_ordinary_days() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "nl".into(),
        vec![PhraseEntry {
            text: "$LY年$LMD$LF".into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    // fixed() = 2026-06-14，非节日
    let exact = layer.lookup_at("nl", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(exact.len(), 1, "非节日也该出候选");
    assert_eq!(exact[0].text, "丙午年四月廿九");

    let pre = layer.lookup_prefix_at("n", fixed(), &[], 1, &PhraseScope::ALL);
    assert_eq!(pre.len(), 1);
    assert_eq!(pre[0].text, "丙午年四月廿九", "两条路径给出同一个答案");
}

#[test]
fn test_cmdbar_command_display_uses_last() {
    // coll = $CC(last(), type(last()))：候选 display 应显示上一次上屏内容，并携命令源。
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "coll".into(),
        vec![PhraseEntry {
            text: "$CC(last(), type(last()))".into(),
            weight: 2000,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let recent = vec!["上次内容".to_string()];
    let got = layer.lookup_at("coll", fixed(), &recent, &no_clip(), &PhraseScope::ALL);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "上次内容"); // display = last() 显示上次上屏
    assert!(got[0].command_src.is_some()); // 仍是命令（选中执行 type(last())）
}

#[test]
fn from_records_builds_lookup() {
    let layer = PhraseLayer::from_records([
        PhraseSeed {
            code: "bj".into(),
            text: "北京".into(),
            weight: 1000,
            position: 0,
            is_system: false,
            category: String::new(),
        },
        PhraseSeed {
            code: "bj".into(),
            text: "北京市".into(),
            weight: 500,
            position: 1,
            is_system: true,
            category: String::new(),
        },
    ]);
    let hits = layer.lookup("bj", &[], &no_clip(), &PhraseScope::ALL);
    // 两条同码，按 weight 降序
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].text, "北京");
    assert_eq!(hits[1].text, "北京市");
}

// 中文数字读法本身的用例在 wind-quick-input（`test_small_int_to_chinese`）；
// 此处保留 `test_expand_chinese` 覆盖模板端到端。

/// 回归：`${VAR}` 旧式模板变量不得被 cmdbar 语法探测劫走。
///
/// `has_top_level_brace` 只看 `{` 不看前面的 `$` 时，`${YC}年${MC}月${DC}日` 会被判成命令栏
/// 语法 → `UnknownFunc` → 候选被静默丢弃。必须从 `lookup_at` 进才覆盖 `is_cmdbar_grammar`
/// 分发；只测 `expand_template` 纯函数会假绿。
#[test]
fn lookup_expands_brace_template_vars() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "date".into(),
        vec![PhraseEntry {
            text: "${YC}年${MC}月${DC}日".into(),
            weight: 100,
            position: 0,
            is_system: true,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };

    // 精确匹配路径
    let got = layer.lookup_at("date", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(got.len(), 1, "${{YC}} 模板短语不应被丢弃");
    assert_eq!(got[0].text, "二〇二六年六月十四日");

    // 前缀枚举路径（lookup_prefix 的 Literal 分支同样要展开）
    let got = layer.lookup_prefix_at("dat", fixed(), &[], 2, &PhraseScope::ALL);
    assert_eq!(got.len(), 1, "前缀枚举也不应丢弃 ${{YC}} 模板短语");
    assert_eq!(got[0].text, "二〇二六年六月十四日");
}

/// `date()` 的记号与同名 `$变量` 必须逐日期给出同一个答案（文档推荐用 `date()`，
/// 两边若各改各的，用户换个写法就换个结果）。
///
/// 日期挑了会分叉的几类：普通日、节日（`$LF` 非空）、闰月、元旦（农历年≠公历年）、
/// 年底。两边都经 `lookup_at`，覆盖 `is_cmdbar_grammar` 分发。非节日的 `$LF` 展开为空，
/// 故直接比较结果列表而不是逐个 unwrap。
#[test]
fn date_tokens_match_template_vars() {
    const VARS: &[&str] = &[
        "YC", "MC", "DC", "WC", "LY", "LYN", "LZ", "LM", "LD", "LMD", "LF", "ts", "tsms",
    ];
    let days = [
        (2026, 6, 14),
        (2026, 6, 19),
        (2020, 6, 1),
        (2026, 1, 1),
        (2026, 12, 31),
    ];
    for (y, m, d) in days {
        let now = Local.with_ymd_and_hms(y, m, d, 9, 5, 7).unwrap();
        for v in VARS {
            let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
            for (code, text) in [
                ("a", format!("${{{v}}}")),
                ("b", format!("{{date(\"{v}\")}}")),
            ] {
                map.insert(
                    code.into(),
                    vec![PhraseEntry {
                        text,
                        weight: 1,
                        position: 0,
                        is_system: true,
                        category: String::new(),
                    }],
                );
            }
            let layer = PhraseLayer { map };
            let texts = |code: &str| -> Vec<String> {
                layer
                    .lookup_at(code, now, &[], &no_clip(), &PhraseScope::ALL)
                    .into_iter()
                    .map(|h| h.text)
                    .collect()
            };
            assert_eq!(texts("a"), texts("b"), "{v} @ {y}-{m}-{d}");
        }
    }
}

/// 回归：系统短语 `uuid = '$uuid'` 要能展开；`expand_var` 缺分支时打全码整条丢弃、前缀路径
/// 却显示字面 `$uuid`。必须从 `lookup_at` 进，只测 `expand_template` 会绕过分发。
#[test]
fn lookup_expands_uuid_variable() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "uuid".into(),
        vec![PhraseEntry {
            text: "$uuid".into(),
            weight: 1000,
            position: 0,
            is_system: true,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };

    // 精确匹配路径：必须出候选，且不是字面量。
    let got = layer.lookup_at("uuid", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(got.len(), 1, "$uuid 短语不应被丢弃");
    assert_ne!(got[0].text, "$uuid", "应展开而非原样上屏");
    assert_eq!(got[0].text.len(), 36, "默认格式为带横杠 UUID");
    assert_eq!(got[0].text.matches('-').count(), 4);

    // 前缀枚举路径：同样展开，不再回退成字面 `$uuid`。
    let got_prefix = layer.lookup_prefix_at("uui", fixed(), &[], 2, &PhraseScope::ALL);
    assert_eq!(got_prefix.len(), 1);
    assert_ne!(got_prefix[0].text, "$uuid");

    // 每次求值都是新值（区别于 date/time 的秒级稳定值）。
    let again = layer.lookup_at("uuid", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert_ne!(got[0].text, again[0].text, "uuid 应每次重新生成");
}

/// 真正的 cmdbar 插值 `{expr}` 不受上面的 `${` 豁免影响。
#[test]
fn lookup_still_evaluates_cmdbar_interpolation() {
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    map.insert(
        "yy".into(),
        vec![PhraseEntry {
            text: r#"年份{date("YYYY")}"#.into(),
            weight: 1,
            position: 0,
            is_system: true,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_at("yy", fixed(), &[], &no_clip(), &PhraseScope::ALL);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "年份2026");
}

#[test]
fn lookup_prefix_lists_static_phrases() {
    // 静态字面短语（Literal）应出现在前缀结果中，command_src=None，nav_code=None。
    let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
    // 静态短语：码 yx，文本 user@example.com
    map.insert(
        "yx".into(),
        vec![PhraseEntry {
            text: "user@example.com".into(),
            weight: 800,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    // $CC 命令短语：码 yxbd，保证命令短语仍正常工作
    map.insert(
        "yxbd".into(),
        vec![PhraseEntry {
            text: r#"$CC("百度", open("https://baidu.com"))"#.into(),
            weight: 500,
            position: 0,
            is_system: false,
            category: String::new(),
        }],
    );
    let layer = PhraseLayer { map };
    let got = layer.lookup_prefix_at("y", fixed(), &[], 1, &PhraseScope::ALL);
    // 静态短语应出现
    let static_hit = got.iter().find(|h| h.text == "user@example.com");
    assert!(static_hit.is_some(), "静态短语应出现在前缀结果中");
    let sh = static_hit.unwrap();
    assert!(sh.command_src.is_none(), "静态短语 command_src 应为 None");
    assert!(sh.nav_code.is_none(), "静态短语 nav_code 应为 None");
    // $CC 命令短语仍正常工作
    let cmd_hit = got.iter().find(|h| h.text == "百度");
    assert!(cmd_hit.is_some(), "$CC 命令短语应仍出现在前缀结果中");
    assert!(
        cmd_hit.unwrap().command_src.is_some(),
        "$CC 命令短语 command_src 应非 None"
    );
}
