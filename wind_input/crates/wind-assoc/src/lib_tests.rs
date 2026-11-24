use super::*;

struct Fixed(AssocSource, Vec<(&'static str, i64)>);

impl AssocProvider for Fixed {
    fn suggest(&self, _ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
        self.1
            .iter()
            .take(limit)
            .map(|(t, s)| AssocHit {
                text: (*t).to_string(),
                commit: None,
                source: self.0,
                score: *s,
            })
            .collect()
    }
}

fn ctx() -> AssocContext<'static> {
    AssocContext {
        text: "你好",
        preceding: "",
        char_wise: false,
        boundary_broken: false,
    }
}

#[test]
fn disabled_yields_nothing() {
    let p = Fixed(AssocSource::Punct, vec![("，", 1)]);
    let cfg = AssocConfig {
        kind: AssocKind::Off,
        ..Default::default()
    };
    assert!(associate(&ctx(), &cfg, &[(AssocSource::Punct, &p)]).is_empty());
}

/// 断链必须挡住联想：跨应用/跨焦点的「历史相邻」不是「屏幕相邻」。
#[test]
fn broken_boundary_yields_nothing() {
    let p = Fixed(AssocSource::Punct, vec![("，", 1)]);
    let cfg = AssocConfig::default().with_enabled();
    let c = AssocContext {
        text: "你好",
        preceding: "",
        char_wise: false,
        boundary_broken: true,
    };
    assert!(associate(&c, &cfg, &[(AssocSource::Punct, &p)]).is_empty());
}

#[test]
fn empty_context_yields_nothing() {
    let p = Fixed(AssocSource::Punct, vec![("，", 1)]);
    let cfg = AssocConfig::default().with_enabled();
    let c = AssocContext {
        text: "",
        preceding: "",
        char_wise: false,
        boundary_broken: false,
    };
    assert!(associate(&c, &cfg, &[(AssocSource::Punct, &p)]).is_empty());
}

/// 优先级：History 在 Punct 之前，与分数无关。
///
/// 这里刻意让 Punct 的分数远高于 History——若哪天有人改成跨源比分数，本测试立刻变红。
#[test]
fn priority_beats_score_across_sources() {
    let hist = Fixed(AssocSource::History, vec![("世界", 1)]);
    let punct = Fixed(AssocSource::Punct, vec![("，", 9999)]);
    let cfg = AssocConfig::default().with_enabled();
    let out = associate(
        &ctx(),
        &cfg,
        &[(AssocSource::Punct, &punct), (AssocSource::History, &hist)],
    );
    assert_eq!(out[0].text, "世界", "高优先级源在前，即便分数低得多");
    assert_eq!(out[1].text, "，");
}

/// 靠前的源取不满时，名额要顺延给后面的源——否则新用户（历史为空）
/// 会白白浪费掉最靠前的几个位置。
#[test]
fn unused_quota_rolls_down() {
    let hist = Fixed(AssocSource::History, vec![]);
    let punct = Fixed(AssocSource::Punct, vec![("，", 3), ("。", 2), ("？", 1)]);
    let cfg = AssocConfig {
        max_count: 3,
        ..AssocConfig::default().with_enabled()
    };
    let out = associate(
        &ctx(),
        &cfg,
        &[(AssocSource::History, &hist), (AssocSource::Punct, &punct)],
    );
    assert_eq!(out.len(), 3, "历史为空时标点应吃满全部名额");
}

/// 同文本跨源重复时先到先得，且来源标记跟着保留的那条。
#[test]
fn dedup_keeps_higher_priority_source() {
    let hist = Fixed(AssocSource::History, vec![("世界", 1)]);
    let bigram = Fixed(AssocSource::Bigram, vec![("世界", 9999)]);
    let cfg = AssocConfig::default().with_enabled();
    let out = associate(
        &ctx(),
        &cfg,
        &[
            (AssocSource::History, &hist),
            (AssocSource::Bigram, &bigram),
        ],
    );
    assert_eq!(out.len(), 1, "同文本只留一条");
    assert_eq!(out[0].source, AssocSource::History, "保留高优先级源的标记");
}

/// 被去重吃掉的名额同样要顺延给后面的源：剩余配额须按「已产出数」而非「已请求数」算，
/// 否则与 History 重复的 Bigram 会白占一个名额。
#[test]
fn quota_rolls_down_after_dedup_absorbs_hits() {
    let hist = Fixed(AssocSource::History, vec![("世界", 5)]);
    let bigram = Fixed(AssocSource::Bigram, vec![("世界", 9)]); // 与上面重复
    let punct = Fixed(AssocSource::Punct, vec![("，", 3)]);
    let cfg = AssocConfig {
        max_count: 2,
        ..AssocConfig::default().with_enabled()
    };
    let out = associate(
        &ctx(),
        &cfg,
        &[
            (AssocSource::History, &hist),
            (AssocSource::Bigram, &bigram),
            (AssocSource::Punct, &punct),
        ],
    );
    let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(texts, ["世界", "，"], "重复项不该占用名额");
}

#[test]
fn per_source_switch_disables_it() {
    let punct = Fixed(AssocSource::Punct, vec![("，", 1)]);
    let cfg = AssocConfig {
        punct: false,
        ..AssocConfig::default().with_enabled()
    };
    assert!(associate(&ctx(), &cfg, &[(AssocSource::Punct, &punct)]).is_empty());
}

/// `ALL` 必须覆盖全部变体且顺序与 `priority()` 一致；漏登记的新源不报错，只是永远不出候选。
/// 从穷尽 match 的 `priority()` 反查，新变体越界或重号即红。
#[test]
fn all_is_complete_and_ordered() {
    let prios: Vec<usize> = AssocSource::ALL.iter().map(|s| s.priority()).collect();
    assert_eq!(
        prios,
        (0..AssocSource::ALL.len()).collect::<Vec<_>>(),
        "ALL 的顺序必须是 priority 的 0..n 且无缺号/重号——新增源忘了登记进 ALL 时这里会红"
    );
}

#[test]
fn source_internal_order_is_by_score() {
    let punct = Fixed(AssocSource::Punct, vec![("。", 1), ("，", 5), ("？", 3)]);
    let cfg = AssocConfig::default().with_enabled();
    let out = associate(&ctx(), &cfg, &[(AssocSource::Punct, &punct)]);
    let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(texts, ["，", "？", "。"], "源内按分数降序");
}

impl AssocConfig {
    /// 测试夹具：四个源全放行的智能联想。
    ///
    /// 必须显式开 `punct`（桌面 `Default` 里是关的）：这批测的是合并算法，不该受
    /// 「桌面默认出不出标点」这个产品决策牵连；后者由 `desktop_smart_yields_no_punct_by_default` 钉。
    fn with_enabled(mut self) -> Self {
        self.kind = AssocKind::Smart;
        self.punct = true;
        self
    }
}

/// 词语联想不出标点：「，」不是以「中」开头的词。档位过滤先于用户开关，
/// `punct = true` 在 `word` 档下也不放行。
#[test]
fn word_kind_never_yields_punctuation() {
    let punct = Fixed(AssocSource::Punct, vec![("，", 9999)]);
    let prefix = Fixed(AssocSource::Prefix, vec![("中国", 5)]);
    let cfg = AssocConfig {
        kind: AssocKind::Word,
        punct: true, // 开关开着也不行
        ..Default::default()
    };
    let out = associate(
        &ctx(),
        &cfg,
        &[(AssocSource::Punct, &punct), (AssocSource::Prefix, &prefix)],
    );
    let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(texts, ["中国"], "词语联想只出前缀延伸词");
}

/// 受控对照：同样的两个源、同样的开关，换成智能联想档就该两个都出。
/// 少了这条，「词语联想没标点」可能只是因为标点源压根没接上。
#[test]
fn smart_kind_does_yield_punctuation() {
    let punct = Fixed(AssocSource::Punct, vec![("，", 9999)]);
    let prefix = Fixed(AssocSource::Prefix, vec![("中国", 5)]);
    // 显式开 punct：本条测档位放行语义，桌面出厂默认由 `desktop_smart_yields_no_punct_by_default` 钉。
    let cfg = AssocConfig {
        kind: AssocKind::Smart,
        punct: true,
        ..Default::default()
    };
    let out = associate(
        &ctx(),
        &cfg,
        &[(AssocSource::Punct, &punct), (AssocSource::Prefix, &prefix)],
    );
    let texts: Vec<_> = out.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(
        texts,
        ["中国", "，"],
        "智能档两个源都出，且 Prefix 优先级更高"
    );
}

/// 下一词只在智能联想档读出（词语联想只出上文的延长），写入端须同口径：
/// 词语联想档开着历史源也不学，否则只写不读、白攒数据。
#[test]
fn learns_next_words_only_in_smart_with_history() {
    let with = |kind, history| AssocConfig {
        kind,
        history,
        ..Default::default()
    };
    assert!(with(AssocKind::Smart, true).learns_next_words());
    assert!(!with(AssocKind::Smart, false).learns_next_words());
    assert!(!with(AssocKind::Word, true).learns_next_words());
    assert!(!with(AssocKind::Off, true).learns_next_words());
}

#[test]
fn off_kind_yields_nothing() {
    let p = Fixed(AssocSource::Prefix, vec![("中国", 5)]);
    let cfg = AssocConfig {
        kind: AssocKind::Off,
        ..Default::default()
    };
    assert!(associate(&ctx(), &cfg, &[(AssocSource::Prefix, &p)]).is_empty());
}

#[test]
fn kind_parses_explicit_values() {
    assert_eq!(AssocKind::parse("off"), AssocKind::Off);
    assert_eq!(AssocKind::parse("word"), AssocKind::Word);
    assert_eq!(AssocKind::parse("smart"), AssocKind::Smart);
}

/// 未知值一律回退「关」——`"auto"` 这类平台哨兵不是合法值，平台差异走
/// `[mobile.association]`，不进值域。
#[test]
fn unknown_kind_falls_back_to_off() {
    for s in ["auto", "", "Word", "词语", "smrt"] {
        assert_eq!(AssocKind::parse(s), AssocKind::Off, "{s:?} 应回退 Off");
    }
}

/// `commit_text()` 是上屏那一半的唯一取值口。
#[test]
fn commit_text_falls_back_to_display_text() {
    let plain = AssocHit {
        text: "，".into(),
        commit: None,
        source: AssocSource::Punct,
        score: 1,
    };
    assert_eq!(plain.commit_text(), "，", "无 commit 时就用 text");
    let word = AssocHit {
        text: "中国".into(),
        commit: Some("国".into()),
        source: AssocSource::Prefix,
        score: 1,
    };
    assert_eq!(word.commit_text(), "国", "词语联想只补剩余部分");
}

#[test]
fn mode_parses_explicit_values() {
    assert_eq!(AssocMode::parse("one_shot"), AssocMode::OneShot);
    assert_eq!(AssocMode::parse("continuous"), AssocMode::Continuous);
}

#[test]
fn unknown_mode_falls_back_to_one_shot() {
    for s in ["auto", "", "One_Shot", "持续", "continous"] {
        assert_eq!(
            AssocMode::parse(s),
            AssocMode::OneShot,
            "{s:?} 应回退 OneShot"
        );
    }
}

/// 同一份配置取不取用 `[mobile.association]` 得到两种行为——这是平台差异的全部落点，
/// 联想取值不涉及 `cfg!(target_os)`。
#[test]
fn mobile_section_switches_kind_mode_and_punct() {
    let base = wind_config::AssociationConfig {
        kind: "off".into(),
        mode: "one_shot".into(),
        punct: false,
        ..Default::default()
    };
    let m = wind_config::MobileAssociationConfig::default();

    let desktop = AssocConfig::from_config(&base, None);
    assert_eq!(desktop.kind, AssocKind::Off, "桌面基线：默认关");
    assert_eq!(desktop.mode, AssocMode::OneShot);
    assert!(!desktop.punct, "桌面基线：标点联想默认关");

    let mobile = AssocConfig::from_config(&base, Some(&m));
    assert_eq!(mobile.kind, AssocKind::Smart, "移动端出厂：智能联想");
    assert_eq!(mobile.mode, AssocMode::Continuous);
    assert!(mobile.punct, "移动端出厂：标点联想开");
}

/// 空格选不选联想：移动端段说了算（出厂关），桌面基线（出厂开）在手机上不起作用；
/// 用户在手机上打开后确实生效。
#[test]
fn mobile_section_switches_space_commits() {
    let base = wind_config::AssociationConfig::default();
    assert!(base.space_commits, "前提：桌面基线空格选联想");
    assert!(AssocConfig::from_config(&base, None).space_commits);

    let mut m = wind_config::MobileAssociationConfig::default();
    assert!(
        !AssocConfig::from_config(&base, Some(&m)).space_commits,
        "移动端出厂：空格出空格、不选联想"
    );
    m.space_commits = true;
    assert!(
        AssocConfig::from_config(&base, Some(&m)).space_commits,
        "手机上打开后生效"
    );
}

/// 智能联想里前缀词最多占 [`SMART_PREFIX_CAP`] 个名额，余下顺延给标点；补出部分超过
/// [`SMART_PREFIX_TAIL_MAX`] 字的丢掉、由后面更短的补上。词语联想两条都不管。
#[test]
fn smart_caps_prefix_and_leaves_room_for_punct() {
    struct Words(Vec<&'static str>);
    impl AssocProvider for Words {
        fn suggest(&self, ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
            let n = self.0.len() as i64;
            self.0
                .iter()
                .take(limit)
                .enumerate()
                .map(|(i, w)| AssocHit {
                    text: (*w).to_string(),
                    commit: w.strip_prefix(ctx.text).map(str::to_string),
                    source: AssocSource::Prefix,
                    score: n - i as i64,
                })
                .collect()
        }
    }
    let prefix = Words(vec![
        "中国",
        "中华人民共和国",
        "中间",
        "中心",
        "中文",
        "中午",
        "中医",
        "中学",
    ]);
    let punct = crate::punct::PunctRules;
    let providers: [(AssocSource, &dyn AssocProvider); 2] =
        [(AssocSource::Prefix, &prefix), (AssocSource::Punct, &punct)];
    let run = |kind| {
        let cfg = AssocConfig {
            kind,
            max_count: 9,
            punct: true,
            ..Default::default()
        };
        associate(
            &AssocContext {
                text: "中",
                preceding: "",
                char_wise: false,
                boundary_broken: false,
            },
            &cfg,
            &providers,
        )
    };
    let smart = run(AssocKind::Smart);
    let prefix_hits: Vec<_> = smart
        .iter()
        .filter(|h| h.source == AssocSource::Prefix)
        .map(|h| h.commit_text())
        .collect();
    assert_eq!(
        prefix_hits,
        ["国", "间", "心", "文"],
        "前 4 条短的，长尾那条跳过"
    );
    assert!(
        smart.iter().any(|h| h.text == "，"),
        "标点要排得进来，实得 {smart:?}"
    );

    let word = run(AssocKind::Word);
    assert_eq!(word.len(), 8, "词语联想不设前缀上限，也不丢长词");
    assert!(word.iter().any(|h| h.text == "中华人民共和国"));
}

/// 语气词结尾（「好吗」）：智能联想只出个人学习与标点，句末标点排第一；
/// 词语联想不受影响（它本就只有前缀词）。
#[test]
fn smart_particle_tail_yields_punct_first() {
    let bigram = Fixed(AssocSource::Bigram, vec![("的", 9)]);
    let prefix = Fixed(AssocSource::Prefix, vec![("吗啡", 9)]);
    let punct = crate::punct::PunctRules;
    let providers: [(AssocSource, &dyn AssocProvider); 3] = [
        (AssocSource::Bigram, &bigram),
        (AssocSource::Prefix, &prefix),
        (AssocSource::Punct, &punct),
    ];
    let cfg = AssocConfig {
        kind: AssocKind::Smart,
        max_count: 9,
        punct: true,
        ..Default::default()
    };
    let run = |text| {
        associate(
            &AssocContext {
                text,
                preceding: "",
                char_wise: false,
                boundary_broken: false,
            },
            &cfg,
            &providers,
        )
        .into_iter()
        .map(|h| h.text)
        .collect::<Vec<_>>()
    };
    let got = run("好吗");
    assert_eq!(got[0], "？", "{got:?}");
    assert!(!got.iter().any(|t| t == "的" || t == "吗啡"), "{got:?}");
    assert_eq!(run("你好")[0], "的", "非语气词照常出下一词");
}

/// 智能联想按上屏的那一截去重：前缀延伸「中国」（补「国」）与下一词「国」只留先到的，
/// 名额顺延给后面的；补出的那截延长了已有一格的前缀词也不出。词语联想仍按整词去重（都留）。
#[test]
fn smart_dedups_by_continuation() {
    struct WithCommit(AssocSource, Vec<(&'static str, Option<&'static str>)>);
    impl AssocProvider for WithCommit {
        fn suggest(&self, _ctx: &AssocContext<'_>, limit: usize) -> Vec<AssocHit> {
            self.1
                .iter()
                .take(limit)
                .map(|(t, c)| AssocHit {
                    text: (*t).to_string(),
                    commit: c.map(str::to_string),
                    source: self.0,
                    score: 0,
                })
                .collect()
        }
    }
    let history = WithCommit(AssocSource::History, vec![("国", None)]);
    let prefix = WithCommit(
        AssocSource::Prefix,
        vec![
            ("中国", Some("国")),
            ("中国人", Some("国人")),
            ("中间", Some("间")),
        ],
    );
    let providers: [(AssocSource, &dyn AssocProvider); 2] = [
        (AssocSource::History, &history),
        (AssocSource::Prefix, &prefix),
    ];
    let run = |kind| {
        let cfg = AssocConfig {
            kind,
            max_count: 9,
            ..Default::default()
        };
        let hits = associate(
            &AssocContext {
                text: "中",
                preceding: "",
                char_wise: false,
                boundary_broken: false,
            },
            &cfg,
            &providers,
        );
        hits.into_iter().map(|h| h.text).collect::<Vec<_>>()
    };
    assert_eq!(
        run(AssocKind::Smart),
        ["国", "中间"],
        "「中国」显示同为「国」，去掉；「中国人」补「国人」是「国」的延长，也去掉"
    );
    assert_eq!(
        run(AssocKind::Word),
        ["国", "中国", "中国人", "中间"],
        "词语联想按整词，都留"
    );
}

/// 移动端段只该动 kind / mode / punct / space_commits，其余五项一律走基线——它们当前没有平台差异。
///
/// 守两个方向：键加进 `[mobile.association]` 却忘了在 `from_config` 放行（手机上改了没反应）；
/// 误把不该分平台的字段接上移动端段。
#[test]
fn mobile_section_does_not_touch_other_fields() {
    let base = wind_config::AssociationConfig {
        max_count: 5,
        hide_after_ms: 1234,
        history: false,
        bigram: false,
        prefix: false,
        ..Default::default()
    };
    let m = wind_config::MobileAssociationConfig::default();
    let got = AssocConfig::from_config(&base, Some(&m));
    assert_eq!(got.max_count, 5);
    assert_eq!(got.hide_after_ms, 1234);
    assert!(!got.history);
    assert!(!got.bigram);
    assert!(!got.prefix);
}

/// 桌面智能联想默认不出标点（`punct` 桌面默认关）。走完整 `associate` 链路、用出厂配置，
/// 钉的是用户可感行为而非字段值。
#[test]
fn desktop_smart_yields_no_punct_by_default() {
    // 用户显式开了智能联想，但没动 punct
    let base = wind_config::AssociationConfig {
        kind: "smart".into(),
        ..Default::default()
    };
    let cfg = AssocConfig::from_config(&base, None);
    assert!(!cfg.punct, "前提：桌面基线 punct 关");

    let p = wind_assoc_test_punct();
    let hits = associate(
        &AssocContext {
            text: "你好",
            preceding: "",
            char_wise: false,
            boundary_broken: false,
        },
        &cfg,
        &[(AssocSource::Punct, &p)],
    );
    assert!(hits.is_empty(), "桌面智能联想不该出标点，实得 {hits:?}");

    // 反向对照：移动端段接上后就出得来，否则本测试可能只是因为标点源坏了。
    let m = wind_config::MobileAssociationConfig::default();
    let mcfg = AssocConfig::from_config(&base, Some(&m));
    let mhits = associate(
        &AssocContext {
            text: "你好",
            preceding: "",
            char_wise: false,
            boundary_broken: false,
        },
        &mcfg,
        &[(AssocSource::Punct, &p)],
    );
    assert!(!mhits.is_empty(), "移动端该出标点");
}

fn wind_assoc_test_punct() -> crate::punct::PunctRules {
    crate::punct::PunctRules
}

/// `wind_config::AssociationConfig::default()` 与 `AssocConfig::default()` 是独立维护的两份
/// 字面量，必须一致，否则同一开关在两条路径上取值不同。
#[test]
fn config_defaults_agree_with_runtime_defaults() {
    let from_cfg = AssocConfig::from_config(&wind_config::AssociationConfig::default(), None);
    let rt = AssocConfig::default();
    assert_eq!(from_cfg.kind, rt.kind, "默认关这一条尤其不能漂");
    assert_eq!(from_cfg.mode, rt.mode);
    assert_eq!(from_cfg.max_count, rt.max_count);
    assert_eq!(from_cfg.space_commits, rt.space_commits);
    assert_eq!(from_cfg.hide_after_ms, rt.hide_after_ms);
    assert_eq!(from_cfg.history, rt.history);
    assert_eq!(from_cfg.bigram, rt.bigram);
    assert_eq!(from_cfg.prefix, rt.prefix);
    assert_eq!(from_cfg.punct, rt.punct);
}

/// 配置默认必须是关的——联想会占用数字键，对既有用户是突发的行为变化。
///
/// 断言字面量而不只是解析结果：混进 `"auto"` 之类哨兵时解析可能仍是 Off，
/// 设置界面却得多列一个语义空洞的选项。
#[test]
fn association_is_off_by_default() {
    let d = wind_config::AssociationConfig::default();
    assert_eq!(d.kind, "off");
    assert_eq!(d.mode, "one_shot");
    assert_eq!(AssocKind::parse(&d.kind), AssocKind::Off);
    assert_eq!(AssocMode::parse(&d.mode), AssocMode::OneShot);
}

/// 移动端出厂值就该是智能联想 + 持续。与 `data/config.toml` 的一致性由
/// `wind-config` 的 `data_config_toml_covers_registry` 一族守护，这里钉值本身。
#[test]
fn mobile_association_defaults_are_the_mobile_values() {
    let d = wind_config::MobileAssociationConfig::default();
    assert_eq!(d.kind, "smart");
    assert_eq!(d.mode, "continuous");
    assert_eq!(AssocKind::parse(&d.kind), AssocKind::Smart);
    assert_eq!(AssocMode::parse(&d.mode), AssocMode::Continuous);
    // 手机上选联想只靠点候选；退格、回车、二三候选键都不被联想截走
    assert!(!d.space_commits);
    assert!(!d.select_keys_commit);
    assert!(!d.enter_cancels_only);
    assert!(!d.backspace_cancels_only);
}
