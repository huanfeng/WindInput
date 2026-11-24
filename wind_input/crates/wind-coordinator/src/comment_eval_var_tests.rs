use super::*;
use crate::coordinator::Coordinator;
use std::path::PathBuf;
use std::sync::Arc;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data")
}

fn coord(active: &str) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec![
        "shuangpin".to_string(),
        "pinyin".to_string(),
        "wubi86".to_string(),
    ];
    cfg.schema.active = active.to_string();
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

/// 「你好」：小鹤下敲 `nihc`。
fn nihao() -> Candidate {
    Candidate {
        text: "你好".into(),
        code: "nihao".into(),
        boundary: 0b101,
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

fn eval(co: &Coordinator, name: &str, c: &Candidate) -> Option<String> {
    let rev = wind_reverse::ReverseLookup::default();
    co.eval_var(name, None, c, &rev, "shuangpin")
        .map(|v| v.to_string())
}

/// 随包内置库经 merge + 解析挂上后，`${dict:en_zh}` 读到夹具里的释义。
#[test]
fn dict_var_reads_builtin() {
    let co = coord("pinyin");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/comment_data");
    let specs = crate::coordinator::merge_comment_dicts(&[], &co.system_comments.dicts, &[]);
    let (paths, ids) = crate::coordinator::resolve_comment_mounts(&specs, Some(&fixture));
    let mut rl = wind_reverse::ReverseLookup::default();
    rl.reload_comments(&paths, None);
    rl.set_comment_ids(&ids);
    rl.set_named_only(&crate::coordinator::named_only_mounts(&specs, &ids));
    let mut c = word("apple");
    c.comment.clear();
    assert_eq!(comment(&co, &rl, "${dict:en_zh}", &c), "n. 苹果");
    assert_eq!(comment(&co, &rl, "${dict:zh_en}", &word("猫")), "cat");
    // 老模板的裸 `${dict}` 不因内置库出现而变样
    assert_eq!(comment(&co, &rl, "${dict}", &c), "");
    assert_eq!(comment(&co, &rl, "${dict_all}", &word("猫")), "");
}

// ───────────── `${dict_all}` ─────────────

/// 挂两个注释库：英汉释义 + IT 术语，都收了 `configure`。
fn two_comment_libs(tag: &str) -> (PathBuf, wind_reverse::ReverseLookup) {
    let dir = std::env::temp_dir().join(format!("wind_dict_all_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let head = "name: t\ncolumns:\n  - text\n  - comment\n...\n";
    let mut specs = Vec::new();
    for (i, body) in ["configure\tv.配置\n", "configure\t[IT]配置…\n"]
        .iter()
        .enumerate()
    {
        let p = dir.join(format!("lib{i}.dict.yaml"));
        std::fs::write(&p, format!("{head}{body}")).unwrap();
        specs.push((p, Vec::new()));
    }
    let mut rl = wind_reverse::ReverseLookup::default();
    rl.reload_comments(&specs, None);
    (dir, rl)
}

fn word(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        code: text.into(),
        comment: "kh".into(),
        source: CandidateSource::English,
        ..Default::default()
    }
}

fn comment(co: &Coordinator, rl: &wind_reverse::ReverseLookup, tpl: &str, c: &Candidate) -> String {
    comment_max(co, rl, tpl, c, 0)
}

fn comment_max(
    co: &Coordinator,
    rl: &wind_reverse::ReverseLookup,
    tpl: &str,
    c: &Candidate,
    max: usize,
) -> String {
    co.comment_for(c, &Template::parse(tpl), max, rl, "")
        .as_str()
        .to_string()
}

/// 默认分隔符 `；`，`${dict_all:/}` 自定义；`${dict}` 仍只回第一条。
#[test]
fn dict_all_joins_every_library_with_separator() {
    let co = coord("pinyin");
    let (dir, rl) = two_comment_libs("sep");
    let c = word("configure");
    assert_eq!(comment(&co, &rl, "${dict_all}", &c), "v.配置；[IT]配置…");
    assert_eq!(comment(&co, &rl, "${dict_all:/}", &c), "v.配置/[IT]配置…");
    assert_eq!(comment(&co, &rl, "${dict}", &c), "v.配置");
    // 分隔符参数不 trim：两侧空格原样保留。分隔符里不能有 `|`（变量回退链）与 `}`（变量
    // 结束），这里用模板写得出来的 ` · `。
    assert_eq!(
        comment(&co, &rl, "${dict_all: · }", &c),
        "v.配置 · [IT]配置…"
    );
    // 裸文本入口（cmdbar `dict.rev`）同义，同样经模板解析。
    assert_eq!(
        render("${dict_all: · }", 0, |n, a| co.eval_text_var(
            n,
            a,
            "configure",
            &rl
        )),
        "v.配置 · [IT]配置…"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `${dict_all}` 的连接符带分隔符标记（两个入口同义）。
#[test]
fn dict_all_marks_its_separator() {
    let co = coord("pinyin");
    let (dir, rl) = two_comment_libs("seprole");
    let c = word("configure");
    let t = co.comment_for(&c, &Template::parse("${dict_all}"), 0, &rl, "");
    let sep: Vec<(u32, u32)> = t
        .spans()
        .iter()
        .filter(|s| s.separator)
        .map(|s| (s.start, s.end))
        .collect();
    assert_eq!(sep, vec![(8, 11)], "「v.配置」之后的「；」");
    assert!(t.spans().iter().all(|s| s.role == Some("dict")));
    let bare = co
        .eval_text_var("dict_all", Some("/"), "configure", &rl)
        .unwrap();
    assert_eq!(bare.seps, vec![(8, 9)]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 查不到时是「已知变量、值为空」：所在可选段整段消失，而不是原样回显 `${dict_all}`。
#[test]
fn empty_dict_all_drops_its_optional_group() {
    let co = coord("pinyin");
    let (dir, rl) = two_comment_libs("empty");
    let tpl = "${code_hint}{ [${dict_all}]}";
    assert_eq!(comment(&co, &rl, tpl, &word("nosuch")), "kh");
    assert_eq!(
        comment(&co, &rl, tpl, &word("configure")),
        "kh [v.配置；[IT]配置…]"
    );
    assert_eq!(
        co.eval_text_var("dict_all", None, "nosuch", &rl).as_deref(),
        Some("")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 输入 `ASP` 时词库的 `asp`（毒蛇）被投影成 `ASP`，不得贴上 IT 库里缩写
/// `ASP` 的释义——`${dict}`、`${dict_all}` 都按词库原文 `asp` 查、回退不升格。
#[test]
fn case_projected_candidate_queries_dictionary_original() {
    use crate::english_candidates::{CaseVariant, apply_english_case};
    let co = coord("pinyin");
    let dir = std::env::temp_dir().join(format!("wind_t281_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = |name: &str, body: &str| {
        let p = dir.join(name);
        std::fs::write(
            &p,
            format!("name: t\ncolumns:\n  - text\n  - comment\n...\n{body}"),
        )
        .unwrap();
        (p, Vec::new())
    };
    let it = lib("it.dict.yaml", "ASP\t[IT]动态服务器页面\n");
    let en = lib("en.dict.yaml", "asp\tn.毒蛇\n");

    let mut projected = [word("asp")];
    assert!(apply_english_case(
        &mut projected,
        "ASP",
        CaseVariant::Default
    ));
    let projected = &projected[0];
    assert_eq!(projected.text, "ASP", "前提：asp 被投影成 ASP");

    // 只有 IT 库：不显示缩写的释义。
    let mut rl = wind_reverse::ReverseLookup::default();
    rl.reload_comments(std::slice::from_ref(&it), None);
    assert_eq!(comment(&co, &rl, "${dict}", projected), "");
    assert_eq!(comment(&co, &rl, "{${dict_all}}", projected), "");
    // 词库里本来就是大写的 `ASP`（未投影）照常显示 IT 释义。
    assert_eq!(
        comment(&co, &rl, "${dict}", &word("ASP")),
        "[IT]动态服务器页面"
    );

    // 英汉库也收了小写 asp：显示毒蛇，聚合里同样没有 IT 那条。
    rl.reload_comments(&[en, it], None);
    assert_eq!(comment(&co, &rl, "${dict}", projected), "n.毒蛇");
    assert_eq!(comment(&co, &rl, "${dict_all}", projected), "n.毒蛇");
    assert_eq!(
        comment(&co, &rl, "${dict_all}", &word("ASP")),
        "[IT]动态服务器页面"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 头部候选（用户所打原文，非词库词）被大小写档改写时按显示文本查、只往小写回退：
/// 全大写档打 `asp` → `ASP` 查得到缩写；全小写档打 `ASP` → `asp`
/// 不得升格贴上缩写的释义。
#[test]
fn case_variant_head_candidate_queries_display_text() {
    use crate::english_candidates::{CaseVariant, apply_english_case, english_head_candidates};
    let co = coord("pinyin");
    let dir = std::env::temp_dir().join(format!("wind_t281h_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("it.dict.yaml");
    std::fs::write(
        &p,
        "name: t\ncolumns:\n  - text\n  - comment\n...\nASP\t[IT]动态服务器页面\n",
    )
    .unwrap();
    let mut rl = wind_reverse::ReverseLookup::default();
    rl.reload_comments(&[(p, Vec::new())], None);

    let mut up = english_head_candidates("asp", true, false);
    assert!(apply_english_case(&mut up, "asp", CaseVariant::Upper));
    assert_eq!(
        (up[0].text.as_str(), up[0].source),
        ("ASP", CandidateSource::None)
    );
    assert_eq!(comment(&co, &rl, "${dict}", &up[0]), "[IT]动态服务器页面");
    assert_eq!(
        comment(&co, &rl, "${dict_all}", &up[0]),
        "[IT]动态服务器页面"
    );

    let mut low = english_head_candidates("ASP", true, false);
    assert!(apply_english_case(&mut low, "ASP", CaseVariant::Lower));
    assert_eq!(low[0].text, "asp");
    assert_eq!(comment(&co, &rl, "${dict}", &low[0]), "");
    assert_eq!(comment(&co, &rl, "${dict_all}", &low[0]), "");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 注释段的字数上限照样截断聚合结果。
#[test]
fn dict_all_obeys_comment_max_chars() {
    let co = coord("pinyin");
    let (dir, rl) = two_comment_libs("max");
    assert_eq!(
        comment_max(&co, &rl, "${dict_all}", &word("configure"), 6),
        "v.配置；[…"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 双拼方案下 `${shuangpin}` 给出用户实际要敲的键。
#[test]
fn shuangpin_code_under_shuangpin_schema() {
    let co = coord("shuangpin");
    assert_eq!(eval(&co, "shuangpin", &nihao()).as_deref(), Some("nihc"));
}

/// 全拼方案下照样出双拼编码：主要受众是用全拼打字、正往双拼迁移的人，不能只认活跃方案。
///
/// 布局来源走回退链（`shuangpin_hint_schema`）：活跃方案不是双拼时，落到
/// `primary_pinyin` 或 `available` 里首个双拼方案。此处 available 含 `shuangpin`。
#[test]
fn full_pinyin_schema_still_gets_shuangpin_code() {
    let co = coord("pinyin");
    assert_eq!(
        eval(&co, "shuangpin", &nihao()).as_deref(),
        Some("nihc"),
        "全拼方案应回退到已装的双拼方案取布局"
    );
}

/// 一个双拼方案都没装 ⇒ 恒空。回退链走到头也找不到布局，无从谈起。
#[test]
fn no_shuangpin_schema_installed_yields_empty() {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string(), "wubi86".to_string()];
    cfg.schema.active = "pinyin".to_string();
    let co = Coordinator::new_headless(cfg, Some(&data_dir()));
    assert_eq!(
        eval(&co, "shuangpin", &nihao()).as_deref(),
        Some(""),
        "没装双拼方案时应是「已知变量但为空」，不是未知变量"
    );
}

/// 别名等价：`code`/`code_all` 必须与 `code_rev`/`code_rev_all` 走同一个分支。
///
/// 老模板写的是 `${code}`，`render_nodes` 对未知变量名原样回显，别名断了候选旁就显示字面 `${code}`。
#[test]
fn legacy_names_are_aliases() {
    let co = coord("shuangpin");
    let c = nihao();
    for (old, new) in [("code", "code_rev"), ("code_all", "code_rev_all")] {
        assert_eq!(
            eval(&co, old, &c),
            eval(&co, new, &c),
            "{old} 应与 {new} 等价"
        );
        assert!(
            eval(&co, old, &c).is_some(),
            "{old} 必须是已知变量名（None 会让候选旁显示字面 ${{{old}}}）"
        );
    }
}

/// 门控：候选不是拼音来源时，三个编码变量一律为空串而非 None。
///
/// 空串才会让 `${code_hint|code_rev}` 这样的回退链继续往下试；None 会中断回退并
/// 回显变量名。
#[test]
fn gate_yields_empty_not_unknown() {
    let co = coord("shuangpin");
    let codetable_cand = Candidate {
        source: CandidateSource::CodeTable,
        ..nihao()
    };
    for name in ["code_rev", "code_rev_all", "shuangpin"] {
        assert_eq!(
            eval(&co, name, &codetable_cand).as_deref(),
            Some(""),
            "{name}：非拼音来源候选应为空串"
        );
    }
}

/// 编码来源开关已退役（设计 §13.6）：拼音来源候选的双拼编码不再受档位门控。
#[test]
fn shuangpin_var_has_no_gate() {
    let co = coord("shuangpin");
    assert_eq!(eval(&co, "shuangpin", &nihao()).as_deref(), Some("nihc"));
}

/// 两个入口的词汇表必须一致：注释段认得的变量名，cmdbar 反查（`dict.rev`）也要认得，
/// 反之亦然。否则 `render_nodes` 原样回显未知变量，上屏文本里会混进字面 `${code_schema}`。
#[test]
fn both_entries_share_the_same_vocabulary() {
    let co = coord("shuangpin");
    let rev = wind_reverse::ReverseLookup::default();
    // 编码族的全部名字（含兼容别名）在裸文本入口都必须是「已知变量」。
    for name in ["code_rev", "code", "code_rev_all", "code_all", "shuangpin"] {
        assert!(
            co.eval_text_var(name, None, "你好", &rev).is_some(),
            "{name} 在 dict.rev 入口应是已知变量名，否则上屏文本会混进字面 ${{{name}}}"
        );
    }
    // `code_hint` 是注释段独有的（裸文本没有候选身份），这条不对称是刻意的，别顺手补齐。
    assert_eq!(
        co.eval_text_var("code_hint", None, "你好", &rev),
        None,
        "code_hint 依赖候选身份，裸文本入口不该有"
    );
}

/// `chaizi_code_all` 与 `chaizi_all` 对称，逐字反查编码、不限字数。
///
/// `eval_var` / `eval_text_var` 两处都要认得它，且不受 `single`（仅单字）门控，这是它与
/// `chaizi_code` 的分工差异。测试环境无拆字数据，只钉「已知变量名、不受单字门控」，
/// 取值由 wind-reverse::codes_of 的单测钉住。
#[test]
fn chaizi_code_all_is_known_and_not_single_gated() {
    let co = coord("shuangpin");
    let rev = wind_reverse::ReverseLookup::default();
    // 候选注释入口：多字候选也应给出「已知变量」（空串而非 None）。
    let word = Candidate {
        text: "你好".into(),
        ..nihao()
    };
    assert_eq!(
        eval(&co, "chaizi_code_all", &word).as_deref(),
        Some(""),
        "无拆字数据时应为空串（已知变量），而非 None（未知变量名）"
    );
    // 裸文本入口（dict.rev）同一词汇表。
    assert!(
        co.eval_text_var("chaizi_code_all", None, "你好", &rev)
            .is_some(),
        "chaizi_code_all 在 dict.rev 入口也应是已知变量名"
    );
}

/// 本入口的 `code_schema` 只测到「是已知变量名」，没测到产出值：仓库自带 `data/` 无拼音词库，
/// 裸文本入口恒为空串，抓得住 match arm 被删、抓不住值算错。要覆盖取值得照
/// `shuangpin_separator.rs` 的做法 gate 在 `build_dev/data` 上另写一条。
///
/// 裸文本入口的别名同样要等价。
#[test]
fn text_entry_legacy_names_are_aliases() {
    let co = coord("shuangpin");
    let rev = wind_reverse::ReverseLookup::default();
    for (old, new) in [("code", "code_rev"), ("code_all", "code_rev_all")] {
        assert_eq!(
            co.eval_text_var(old, None, "你好", &rev),
            co.eval_text_var(new, None, "你好", &rev),
            "{old} 应与 {new} 等价"
        );
    }
}

/// 未知变量名返回 `None`，好让用户看见自己拼错了。这条是上面几条的对照组：
/// 没有它，「空串 vs None」的区分就只是我在注释里的主张。
#[test]
fn unknown_name_is_none() {
    let co = coord("shuangpin");
    assert_eq!(eval(&co, "code_rev_typo", &nihao()), None);
}

/// 出厂分配下当前态、横排落到的模板串。
fn factory_tpl(available: &[&str], active: &str, mode: Option<ModeKind>) -> String {
    factory_tpl_with(available, active, mode, |_| {})
}

fn factory_tpl_with(
    available: &[&str],
    active: &str,
    mode: Option<ModeKind>,
    tweak: impl FnOnce(&mut Config),
) -> String {
    let mut cfg = Config::default();
    tweak(&mut cfg);
    cfg.schema.available = available.iter().map(|s| s.to_string()).collect();
    cfg.schema.active = active.to_string();
    let co = Coordinator::new_headless(cfg.clone(), Some(&data_dir()));
    let st = State {
        active: mode,
        ..Default::default()
    };
    co.comment_template_for(&cfg, &st, false)
}

/// 出厂分配（设计 §13.6）：拼音方案默认不显示编码，码表跟随全局，混输给码表编码，
/// 临时拼音给完整回退链——哪怕当前方案是拼音；快捷输入不显示。
#[test]
fn factory_assignment_reproduces_retired_code_hint_defaults() {
    assert_eq!(factory_tpl(&["pinyin", "wubi86"], "pinyin", None), "");
    assert_eq!(
        factory_tpl(&["pinyin", "wubi86"], "wubi86", None),
        "${code_hint|code_rev|shuangpin}"
    );
    assert_eq!(
        factory_tpl(&["wubi86_pinyin", "pinyin"], "wubi86_pinyin", None),
        "${code_hint|code_rev}"
    );
    assert_eq!(
        factory_tpl(&["pinyin", "wubi86"], "pinyin", Some(ModeKind::TempPinyin)),
        "${code_hint|code_rev|shuangpin}"
    );
    assert_eq!(
        factory_tpl(&["pinyin", "wubi86"], "wubi86", Some(ModeKind::Mix(0))),
        ""
    );
}

/// 用户自建的快捷输入没有自己的格，落到通配的 `mode.mix`：码表方案下也不显示。
#[test]
fn user_mix_mode_falls_back_to_mix_all_cell() {
    let add_mine = |c: &mut Config| {
        let mut m = c.schema.mix_modes[0].clone();
        m.id = "mine".into();
        c.schema.mix_modes.push(m);
    };
    let idx = Config::default().schema.mix_modes.len() as u8;
    assert_eq!(
        factory_tpl_with(
            &["pinyin", "wubi86"],
            "wubi86",
            Some(ModeKind::Mix(idx)),
            add_mine
        ),
        ""
    );
}
