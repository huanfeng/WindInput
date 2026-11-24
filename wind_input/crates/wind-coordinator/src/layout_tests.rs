use super::*;
use wind_config::TextOrientation;
use wind_config::config::MixModeConfig;

/// 造一份把各模式意图都设成指定值的配置。
///
/// 不含特殊模式：它的意图住在方案文件的 `[overlay]` 段，由 [`overlay_with`] 单独造出、
/// 经参数传入（见 `intent_for` 的 `overlay` 参数）。
fn cfg_with(intent: LayoutIntent) -> Config {
    let mut c = Config::default();
    c.input.temp_pinyin.candidate_layout = intent;
    c.input.temp_english.candidate_layout = intent;
    c.input.url.candidate_layout = intent;
    c.input.email.candidate_layout = intent;
    c.input.unicode.candidate_layout = intent;
    c.input.add_word.candidate_layout = intent;
    c.input.reverse.candidate_layout = intent;
    c.schema.mix_modes = vec![MixModeConfig {
        candidate_layout: intent,
        ..Default::default()
    }];
    c
}

/// 造一份 `[overlay]` 段快照（= 特殊模式那一路的配置来源）。
/// 只考察「模式意图 + 全局基线」两层的简写：无手动值、方案无意见。
///
/// 方案层与手动层见 `docs/design/schema-scoped-behavior.md` §3；用这个简写让模式层用例
/// 不必每处都写两个 `None`。
fn v_only_mode(mode: LayoutIntent, baseline: bool) -> bool {
    orientation_for(mode, None, LayoutIntent::Follow, bl(baseline)).vertical
}

/// 布尔基线 → `Orientation`（既有用例只表达竖排/横排两态）。
fn bl(vertical: bool) -> Orientation {
    if vertical {
        Orientation::VERTICAL
    } else {
        Orientation::HORIZONTAL
    }
}

fn overlay_with(intent: LayoutIntent) -> OverlaySpec {
    OverlaySpec {
        candidate_layout: intent,
        ..Default::default()
    }
}

const MODES: &[ModeKind] = &[
    ModeKind::Mix(0),
    ModeKind::Special(0),
    ModeKind::TempPinyin,
    ModeKind::TempEnglish,
    ModeKind::Url,
    ModeKind::Email,
    ModeKind::Unicode,
    ModeKind::Reverse,
];

/// 全矩阵：每种模式 × 三种意图 × 两种基线。
///
/// `Follow` + 基线竖排是唯一能区分三态与旧布尔语义的一格，故显式断言而非顺带覆盖。
#[test]
fn every_mode_maps_intent_over_baseline() {
    for &mode in MODES {
        for (intent, baseline, want) in [
            (LayoutIntent::Follow, false, false),
            (LayoutIntent::Follow, true, true), // ← 三态相对布尔的全部增量
            (LayoutIntent::Vertical, false, true),
            (LayoutIntent::Vertical, true, true),
            (LayoutIntent::Horizontal, false, false),
            (LayoutIntent::Horizontal, true, false), // ← 旧布尔表达不了这一格
        ] {
            let cfg = cfg_with(intent);
            let ovs = overlay_with(intent);
            let got = v_only_mode(intent_for(&cfg, Some(&ovs), Some(mode), false), baseline);
            assert_eq!(
                got, want,
                "mode={mode:?} intent={intent:?} baseline={baseline} 应得 {want}"
            );
        }
    }
}

/// 无模式时一律跟随基线，与任何模式配置无关。
#[test]
fn no_active_mode_follows_baseline() {
    let cfg = cfg_with(LayoutIntent::Vertical);
    let ovs = overlay_with(LayoutIntent::Vertical);
    let ov = Some(&ovs);
    assert_eq!(intent_for(&cfg, ov, None, false), LayoutIntent::Follow);
    assert!(!v_only_mode(intent_for(&cfg, ov, None, false), false));
    assert!(v_only_mode(intent_for(&cfg, ov, None, true), true));
}

/// 加词优先于底层模式：底层要横排，加词仍按加词的意图。
#[test]
fn add_word_outranks_active_mode() {
    let mut cfg = cfg_with(LayoutIntent::Horizontal);
    cfg.input.add_word.candidate_layout = LayoutIntent::Vertical;
    let ovs = overlay_with(LayoutIntent::Horizontal);
    let ov = Some(&ovs);
    for &mode in MODES {
        assert_eq!(
            intent_for(&cfg, ov, Some(mode), true),
            LayoutIntent::Vertical,
            "mode={mode:?} 下加词应优先"
        );
    }
    // 无底层模式时同样生效。
    assert_eq!(intent_for(&cfg, None, None, true), LayoutIntent::Vertical);
}

/// mix 下标越界（热重载删掉了该实例）回落 Follow，不猜方向、不 panic。
/// 特殊模式侧的对应情形是快照缺失（该方案没有 `[overlay]` 段），同样回落。
#[test]
fn out_of_range_instance_falls_back_to_follow() {
    let cfg = cfg_with(LayoutIntent::Vertical);
    assert_eq!(
        intent_for(&cfg, None, Some(ModeKind::Mix(9)), false),
        LayoutIntent::Follow
    );
    assert_eq!(
        intent_for(&cfg, None, Some(ModeKind::Special(0)), false),
        LayoutIntent::Follow,
        "无 [overlay] 快照时回落跟随全局"
    );
    // 回落后仍跟随基线两个方向。
    assert!(v_only_mode(
        intent_for(&cfg, None, Some(ModeKind::Mix(9)), false),
        true
    ));
    assert!(!v_only_mode(
        intent_for(&cfg, None, Some(ModeKind::Mix(9)), false),
        false
    ));
}

/// 特殊模式的意图只来自 `[overlay]` 快照，与下标、与 `Config` 都无关；改回读 `cfg` 会红。
#[test]
fn special_mode_intent_comes_from_overlay_spec() {
    // cfg 里所有模式都是 Horizontal，快照是 Vertical——取值必须听快照的。
    let cfg = cfg_with(LayoutIntent::Horizontal);
    let ovs = overlay_with(LayoutIntent::Vertical);
    for idx in [0u8, 9u8] {
        assert_eq!(
            intent_for(&cfg, Some(&ovs), Some(ModeKind::Special(idx)), false),
            LayoutIntent::Vertical,
            "下标 {idx} 不参与取值"
        );
    }
}

/// 每个模式只读自己的配置项，不串味（防止映射表复制粘贴写错字段）。
#[test]
fn each_mode_reads_its_own_key() {
    let mut cfg = cfg_with(LayoutIntent::Follow);
    cfg.input.temp_english.candidate_layout = LayoutIntent::Horizontal;
    let ovs = overlay_with(LayoutIntent::Follow);
    let ov = Some(&ovs);
    assert_eq!(
        intent_for(&cfg, ov, Some(ModeKind::TempEnglish), false),
        LayoutIntent::Horizontal
    );
    for &mode in MODES {
        if matches!(mode, ModeKind::TempEnglish) {
            continue;
        }
        assert_eq!(
            intent_for(&cfg, ov, Some(mode), false),
            LayoutIntent::Follow,
            "改临英不应影响 {mode:?}"
        );
    }
}

/// 内置 quick_mix 出厂强制竖排。
/// 守「默认值只落在 default_mix_modes()、预置文件不写 mix_modes」：默认改成 Follow 的话，
/// 全局横排的用户会发现快捷输入变横排了。
#[test]
fn builtin_quick_mix_defaults_to_vertical() {
    let cfg = Config::default();
    assert!(
        v_only_mode(intent_for(&cfg, None, Some(ModeKind::Mix(0)), false), false),
        "内置 quick_mix 应出厂竖排"
    );
}

/// 加词出厂竖排（此前是硬编码强制竖排，迁成配置项后行为须不变）。
#[test]
fn add_word_defaults_to_vertical() {
    let cfg = Config::default();
    assert!(v_only_mode(intent_for(&cfg, None, None, true), false));
}
/// 方案层：`Follow` 的语义是「跟随下一层」，不是「跟随全局基线」。
///
/// 第一行是唯一能区分的一格：模式 `Follow` 若直接取基线，方案说了竖排也没用。
#[test]
fn schema_layer_sits_between_manual_and_baseline() {
    use LayoutIntent::{Follow, Horizontal, Vertical};
    for (mode, manual, schema, baseline, want, why) in [
        (
            Follow,
            None,
            Vertical,
            false,
            true,
            "唯一区分新旧语义的一格",
        ),
        (
            Follow,
            None,
            Horizontal,
            true,
            false,
            "方案压过基线，另一个方向",
        ),
        (Follow, None, Follow, true, true, "方案无意见时才跟基线"),
        (Follow, None, Follow, false, false, "同上，另一个方向"),
        (Horizontal, None, Vertical, true, false, "模式压过方案"),
        (
            Vertical,
            None,
            Horizontal,
            false,
            true,
            "模式压过方案，另一个方向",
        ),
        (Follow, Some(false), Vertical, true, false, "手动值压过方案"),
        (
            Follow,
            Some(true),
            Horizontal,
            false,
            true,
            "手动值压过方案，另一个方向",
        ),
        (
            Horizontal,
            Some(true),
            Vertical,
            true,
            false,
            "模式压过手动值",
        ),
        (
            Follow,
            Some(true),
            Follow,
            false,
            true,
            "方案无意见时手动值仍压过基线",
        ),
    ] {
        assert_eq!(
            orientation_for(mode, manual, schema, bl(baseline)).vertical,
            want,
            "mode={mode:?} manual={manual:?} schema={schema:?} baseline={baseline}: {why}"
        );
    }
}

/// 方案层不得影响 `intent_for` 本身——它只回答「模式怎么想」。
///
/// 若把方案意图折进 `intent_for`，「模式 Follow」与「模式没意见但方案有意见」就分不开，
/// 手动层无处插入。
#[test]
fn intent_for_answers_mode_layer_only() {
    let cfg = cfg_with(LayoutIntent::Follow);
    for &mode in MODES {
        assert_eq!(
            intent_for(
                &cfg,
                Some(&overlay_with(LayoutIntent::Follow)),
                Some(mode),
                false
            ),
            LayoutIntent::Follow,
            "mode={mode:?}：模式层没意见就该是 Follow，与方案层无关"
        );
    }
}

/// 两根轴真的正交：切横竖不碰文字排列，改文字排列不碰横竖。
///
/// 旋转若混进 `LayoutIntent`，临时切一下竖排就会丢掉方案声明的旋转。
/// 断言必须比整个 [`Orientation`]：只断言 `text` 没变，把 `vertical` 也写坏的实现照样通过。
#[test]
fn toggling_direction_never_touches_text_orientation() {
    use LayoutIntent::*;
    for text in TextOrientation::ALL.iter().copied() {
        let base = Orientation {
            vertical: false,
            text,
        };
        // 手动切竖排：只翻 vertical。
        let got = orientation_for(Follow, Some(true), Follow, base);
        assert_eq!(
            got,
            Orientation {
                vertical: true,
                text
            },
            "手动竖排把文字排列 {text:?} 弄丢了"
        );
        // 模式级强制横排：同样只动 vertical。
        let got = orientation_for(Horizontal, None, Vertical, base);
        assert_eq!(
            got,
            Orientation {
                vertical: false,
                text
            },
            "模式级横排把文字排列 {text:?} 弄丢了"
        );
    }
}

/// 竖排时文字排列必须归零——渲染端 `list_vertical` 同时看两位，
/// `vertical && rotated` 是「既按竖排堆叠又整体转 90°」这种没有定义的状态。
///
/// 反向对照不可少：横排时不得归零，否则「无条件清空」也能满足断言，而那会让旋转整个失效。
#[test]
fn vertical_normalizes_text_orientation_away() {
    for text in TextOrientation::ALL.iter().copied() {
        let v = Orientation {
            vertical: true,
            text,
        }
        .normalized();
        assert_eq!(v, Orientation::VERTICAL, "竖排下 {text:?} 没被归零");
        assert!(!v.rotated() && !v.upright(), "竖排下两个派生位必须都为假");

        let h = Orientation {
            vertical: false,
            text,
        }
        .normalized();
        assert_eq!(h.text, text, "横排下 {text:?} 不该被归零");
    }
}

/// 派生位的真值表。渲染端只看这两位，读侧一律走它们、不要自己判 `text != Normal`。
///
/// `upright` 必须蕴含 `rotated`：否则 `list_vertical` 判 `rotated` 为假退回横排 Row，
/// 而叶子已切成竖格，候选变成一行躺倒的字。
#[test]
fn derived_bits_match_the_axis() {
    let cases = [
        (Orientation::HORIZONTAL, false, false),
        (Orientation::VERTICAL, false, false),
        (Orientation::ROTATED, true, false),
        (Orientation::UPRIGHT, true, true),
    ];
    for (o, rot, upr) in cases {
        assert_eq!(o.rotated(), rot, "{o:?} 的 rotated 位不对");
        assert_eq!(o.upright(), upr, "{o:?} 的 upright 位不对");
        assert!(
            !o.upright() || o.rotated(),
            "{o:?}：upright 必须蕴含 rotated"
        );
    }
}

/// 文字排列的字符串 round-trip（方案文件里手写的取值）。
///
/// 三个取值的字符串必须互不相同：只测 round-trip 的话，`as_str` 与 `from_str_or_normal`
/// 同时把直立错映成 "rotated" 也会全绿。
#[test]
fn text_orientation_string_round_trips() {
    for t in TextOrientation::ALL.iter().copied() {
        assert_eq!(TextOrientation::from_str_or_normal(t.as_str()), t);
    }
    let names: std::collections::BTreeSet<&str> =
        TextOrientation::ALL.iter().map(|t| t.as_str()).collect();
    assert_eq!(names.len(), TextOrientation::ALL.len(), "取值字符串撞了");
    // 用户手写了错的值：回落默认，不 panic、不猜。
    assert_eq!(
        TextOrientation::from_str_or_normal("nonsense"),
        TextOrientation::Normal
    );
    assert_eq!(
        TextOrientation::from_str_or_normal("UPRIGHT"),
        TextOrientation::Upright,
        "大小写不敏感"
    );
}

/// 配置字符串 ↔ `Orientation` 的 round-trip：`ime.toggle` 会把结果写回
/// `ui.candidate.layout`，两个方向对不上就会「重启后方向变了」。
#[test]
fn layout_string_round_trips() {
    // 只有横竖两个：`ui.candidate.layout` 不承载文字排列那根轴
    // （承载了的话，`ime.toggle("layout")` 的写回会把方案意图覆盖掉）。
    const ALL: [Orientation; 2] = [Orientation::HORIZONTAL, Orientation::VERTICAL];
    for o in ALL {
        assert_eq!(Orientation::from_layout_str(o.layout_str()), o);
    }
    // 取值的字符串必须互不相同。只测 round-trip 的话，两个取值写成同一个串、
    // 再原样读回来，两边一致、round-trip 全绿——而用户选的方向会在重启后变成另一个。
    let names: std::collections::BTreeSet<&str> = ALL.iter().map(|o| o.layout_str()).collect();
    assert_eq!(names.len(), ALL.len(), "取值字符串撞了：{names:?}");
    // 未知值按横排（出厂行为），不 panic。
    assert_eq!(
        Orientation::from_layout_str("nonsense"),
        Orientation::HORIZONTAL
    );
    assert_eq!(
        Orientation::from_layout_str("VERTICAL"),
        Orientation::VERTICAL,
        "大小写不敏感"
    );
    // 旋转/直立不从这个键来：写进去也只当未知值按横排处理。
    assert_eq!(
        Orientation::from_layout_str("rotated"),
        Orientation::HORIZONTAL,
        "文字排列不该由 ui.candidate.layout 承载"
    );
}
