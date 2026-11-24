use super::*;

fn parse(src: &str) -> Option<RvSkin> {
    let v: Value = src.parse().unwrap();
    let s: Skin = v.get("skin").cloned().unwrap().try_into().unwrap();
    resolve_skin(&s)
}

const SAMPLE: &str = r#"
[skin]
density = 3
[skin.horizontal.frame]
image = "frame_h"
stretch_x = [[300, 500], { from = 100, to = 160, fill = "round" }]
stretch_y = []
[skin.horizontal.content]
left = 180
top = 320
right = 200
bottom = 120
[[skin.horizontal.decor]]
image = "pet"
anchor = "frame.top_right"
pivot = "bottom_right"
offset = [-80, 20]
z = 1
[[skin.horizontal.decor]]
image = "snow"
anchor = "content.bottom-left"
z = -1
hit = "pass"
"#;

#[test]
fn bands_are_sorted_and_typed() {
    let s = parse(SAMPLE).unwrap();
    let h = s.horizontal.as_ref().unwrap();
    assert_eq!(
        h.frame.stretch_x,
        Stretch::Bands(vec![
            Band {
                from: 100.0,
                to: 160.0,
                fill: BandFill::Round
            },
            Band {
                from: 300.0,
                to: 500.0,
                fill: BandFill::Round
            },
        ])
    );
    assert_eq!(h.frame.stretch_y, Stretch::Fixed, "空数组 = 该轴不可伸缩");
    assert_eq!(h.frame.density, 3.0, "frame 继承 skin.density");
}

#[test]
fn bands_default_to_round_and_fill_overrides() {
    let s = parse("[skin.horizontal.frame]\nimage = \"a\"\nstretch_x = [[1, 2]]").unwrap();
    let f = s.horizontal.unwrap().frame;
    let Stretch::Bands(b) = &f.stretch_x else {
        panic!("应为 Bands");
    };
    assert_eq!(b[0].fill, BandFill::Round, "拉伸带缺省整数次重复");
    assert_eq!(
        f.stretch_y,
        Stretch::Whole(BandFill::Stretch),
        "整轴缺省拉伸"
    );
    let s =
        parse("[skin.horizontal.frame]\nimage = \"a\"\nfill = \"stretch\"\nstretch_x = [[1, 2]]")
            .unwrap();
    let Stretch::Bands(b) = s.horizontal.unwrap().frame.stretch_x else {
        panic!("应为 Bands");
    };
    assert_eq!(b[0].fill, BandFill::Stretch, "fill 显式写了就照写的来");
}

#[test]
fn missing_axis_stretches_whole() {
    let s = parse("[skin.horizontal.frame]\nimage = \"a\"\nfill = \"round\"\nstretch_x = [[1, 2]]")
        .unwrap();
    let f = &s.horizontal.unwrap().frame;
    assert_eq!(f.stretch_y, Stretch::Whole(BandFill::Round));
}

#[test]
fn overlapping_bands_merge() {
    let s =
        parse("[skin.horizontal.frame]\nimage = \"a\"\nstretch_x = [[10, 30], [20, 40]]").unwrap();
    let Stretch::Bands(b) = &s.horizontal.unwrap().frame.stretch_x else {
        panic!("应为 Bands");
    };
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].from, b[0].to), (10.0, 40.0));
}

#[test]
fn decor_sorted_by_z_with_anchor_and_pivot() {
    let s = parse(SAMPLE).unwrap();
    let d = &s.horizontal.as_ref().unwrap().decor;
    assert_eq!(d[0].image, "snow");
    assert_eq!(d[0].anchor.on, AnchorBox::Content);
    assert_eq!(d[0].pivot, Pos9::BOTTOM_LEFT, "pivot 缺省同锚点方位");
    assert_eq!(d[0].hit, DecorHit::Pass);
    assert_eq!(d[1].image, "pet");
    assert_eq!(d[1].pivot, Pos9 { h: 2, v: 2 });
    assert_eq!(d[1].hit, DecorHit::Drag, "缺省 drag");
    assert_eq!(
        (d[0].src_index, d[1].src_index),
        (1, 0),
        "排序后仍记着 TOML 里的原始下标"
    );
    let src = "[skin.horizontal.frame]\nimage = \"f\"\n\
                   [[skin.horizontal.decor]]\nimage = \"\"\n\
                   [[skin.horizontal.decor]]\nimage = \"a\"\nanchor = \"nope\"\n\
                   [[skin.horizontal.decor]]\nimage = \"b\"\n";
    let d = &parse(src).unwrap().horizontal.unwrap().decor;
    assert_eq!(
        d.iter().map(|x| x.src_index).collect::<Vec<_>>(),
        [2],
        "被丢弃的项也占原始下标"
    );
}

#[test]
fn decor_fps_keeps_positive_values_only() {
    let fps = |v: &str| {
        let src = format!(
            "[skin.horizontal.frame]\nimage = \"f\"\n[[skin.horizontal.decor]]\nimage = \"d\"\n{v}"
        );
        parse(&src).unwrap().horizontal.unwrap().decor[0].fps
    };
    assert_eq!(fps("fps = 12"), Some(12.0));
    assert_eq!(fps("fps = 7.5"), Some(7.5));
    assert_eq!(fps(""), None, "缺省用文件自带帧时长");
    assert_eq!(fps("fps = 0"), None);
    assert_eq!(fps("fps = -3"), None);
}

#[test]
fn decor_flip_accepts_x_y_xy_and_defaults_to_none() {
    let flip = |v: &str| {
        let src = format!(
            "[skin.horizontal.frame]\nimage = \"f\"\n[[skin.horizontal.decor]]\nimage = \"d\"\nanchor = \"frame.top_right\"\noffset = [-5, 3]\n{v}"
        );
        parse(&src).unwrap().horizontal.unwrap().decor[0].clone()
    };
    assert_eq!(flip("").flip, Flip::NONE);
    assert_eq!(flip("flip = \"x\"").flip, Flip { x: true, y: false });
    assert_eq!(flip("flip = \"y\"").flip, Flip { x: false, y: true });
    assert_eq!(flip("flip = \"xy\"").flip, Flip { x: true, y: true });
    assert_eq!(flip("flip = \"left\"").flip, Flip::NONE, "非法值按不翻");
    // 只翻像素：定位字段照源图写的原样。
    let (a, b) = (flip(""), flip("flip = \"x\""));
    assert_eq!((a.anchor, a.pivot, a.offset), (b.anchor, b.pivot, b.offset));
    // 非字符串是类型错误，与 hit / anchor 同一口径。
    let v: Value = "[skin.horizontal.frame]\nimage = \"f\"\n[[skin.horizontal.decor]]\nimage = \"d\"\nflip = true"
        .parse()
        .unwrap();
    assert!(v["skin"].clone().try_into::<Skin>().is_err());
}

/// 窗体不开放 flip：写了只告警，窗体照常求值。
#[test]
fn frame_flip_is_not_supported() {
    let src = "[skin.horizontal.frame]\nimage = \"f\"\nflip = \"x\"\nstretch_x = [[1, 2]]";
    let v: Value = src.parse().unwrap();
    let s: Skin = v["skin"].clone().try_into().unwrap();
    let frame = s.horizontal.as_ref().and_then(|v| v.frame.as_ref());
    assert!(frame.unwrap().flip.is_some(), "认得出这个键才能告警");
    let h = resolve_skin(&s).unwrap().horizontal.unwrap();
    assert_eq!(h.frame.image, "f");
}

fn decor_anim(body: &str) -> RvDecorAnim {
    let src = format!(
        "[skin.horizontal.frame]\nimage = \"f\"\n[[skin.horizontal.decor]]\nimage = \"d\"\n{body}"
    );
    parse(&src).unwrap().horizontal.unwrap().decor[0]
        .anim
        .clone()
}

#[test]
fn decor_states_and_events_resolve_to_indices() {
    let a = decor_anim(
        r#"
[skin.horizontal.decor.states]
idle = { frames = "0-3" }
busy = { frames = "4-7" }
alt = { image = "other" }
[skin.horizontal.decor.on]
typing = { state = "busy" }
commit = { play = "8-9", then = "idle" }
page = { play = { image = "burst" } }
idle_5s = { state = "idle" }
mode_en = { play = "10", then = "@prev" }
"#,
    );
    let names: Vec<_> = a.states.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["alt", "busy", "idle"], "按名字排序");
    assert_eq!(a.initial, Some(2), "初始取 idle");
    assert_eq!(a.states[0].1, DecorClip::Image("other".into()));
    assert_eq!(a.states[1].1, DecorClip::Frames { first: 4, last: 7 });
    assert_eq!(a.action(DecorEvent::Typing), Some(&DecorAction::State(1)));
    assert_eq!(
        a.action(DecorEvent::Commit),
        Some(&DecorAction::Play {
            clip: DecorClip::Frames { first: 8, last: 9 },
            then: Some(2),
            lock: false
        })
    );
    assert_eq!(
        a.action(DecorEvent::Page),
        Some(&DecorAction::Play {
            clip: DecorClip::Image("burst".into()),
            then: None,
            lock: false
        }),
        "then 缺省 @prev"
    );
    assert_eq!(
        a.action(DecorEvent::ModeEn),
        Some(&DecorAction::Play {
            clip: DecorClip::Frames {
                first: 10,
                last: 10
            },
            then: None,
            lock: false
        })
    );
    assert_eq!(a.action(DecorEvent::Idle(5)), Some(&DecorAction::State(2)));
    assert_eq!(a.action(DecorEvent::Click), None);
}

#[test]
fn decor_alt_expands_to_extra_actions_and_inherits_then_lock() {
    let a = decor_anim(
        r#"
[skin.horizontal.decor.states]
idle = { frames = "0-3" }
[skin.horizontal.decor.on]
click = { play = "4-5", then = "idle", lock = true, alt = [{ play = "6-7" }, { play = "8-9", lock = false }, { play = "x" }, { state = "nope" }] }
"#,
    );
    let clicks: Vec<_> =
        a.on.iter()
            .filter(|(e, _)| *e == DecorEvent::Click)
            .map(|(_, act)| act.clone())
            .collect();
    let play = |first, last, lock| DecorAction::Play {
        clip: DecorClip::Frames { first, last },
        then: Some(0),
        lock,
    };
    assert_eq!(
        clicks,
        [play(4, 5, true), play(6, 7, true), play(8, 9, false)],
        "主条在前；备选沿用主条的 then / lock，可逐条覆盖；写错的备选丢弃"
    );
    assert_eq!(
        a.action(DecorEvent::Click),
        Some(&clicks[0]),
        "action() 取主条"
    );
}

/// 向下兼容：旧版只认主条、不认 `alt` / `lock`，这份主题在旧版上仍能解析出主条动作。
#[test]
fn decor_without_alt_or_lock_is_unchanged() {
    let a = decor_anim(
        r#"
[skin.horizontal.decor.on]
click = { play = "4-5" }
"#,
    );
    assert_eq!(
        a.on,
        [(
            DecorEvent::Click,
            DecorAction::Play {
                clip: DecorClip::Frames { first: 4, last: 5 },
                then: None,
                lock: false
            }
        )]
    );
}

#[test]
fn decor_bad_entries_are_dropped_one_by_one() {
    let a = decor_anim(
        r#"
[skin.horizontal.decor.states]
run = { frames = "3-1" }
walk = { frames = "x" }
none = { }
stand = { frames = 2 }
[skin.horizontal.decor.on]
typing = { state = "run" }
backspace = { state = "stand" }
commit = { play = "1-2", then = "nowhere" }
page = { play = "9-3" }
idle_0s = { state = "stand" }
idle_xs = { state = "stand" }
wave = { state = "stand" }
click = "stand"
empty = { play = "5-6" }
"#,
    );
    assert_eq!(a.states.len(), 1, "倒序、非数字、空表都丢");
    assert_eq!(a.states[0].1, DecorClip::Frames { first: 2, last: 2 });
    assert_eq!(a.initial, Some(0), "没有 idle 取第一个");
    let evs: Vec<_> = a.on.iter().map(|(e, _)| *e).collect();
    assert_eq!(evs, [DecorEvent::Backspace, DecorEvent::Empty]);
}

#[test]
fn decor_without_states_keeps_whole_file_loop() {
    let a = decor_anim("[skin.horizontal.decor.on]\nclick = { play = \"0-1\" }\n");
    assert!(a.states.is_empty());
    assert_eq!(a.initial, None);
    assert_eq!(a.on.len(), 1, "无状态时一次性片段照样可用");
    assert_eq!(decor_anim(""), RvDecorAnim::default());
}

#[test]
fn decor_event_names() {
    assert_eq!(DecorEvent::parse("idle_12s"), Some(DecorEvent::Idle(12)));
    assert_eq!(DecorEvent::parse("idle_s"), None);
    assert_eq!(DecorEvent::parse("idle_3"), None);
    assert_eq!(DecorEvent::parse("mode_cn"), Some(DecorEvent::ModeCn));
    assert_eq!(DecorEvent::parse("select"), None, "本期不单列");
}

#[test]
fn vertical_never_falls_back_to_horizontal() {
    let s = parse(SAMPLE).unwrap();
    assert!(s.variant(false, false).is_some());
    assert!(s.variant(false, true).is_some(), "嵌入回退非嵌入");
    assert!(s.variant(true, false).is_none(), "竖排不套横排图");
    assert!(s.variant(true, true).is_none());
}

#[test]
fn variant_without_frame_image_is_dropped() {
    assert!(parse("[skin.horizontal.content]\nleft = 1").is_none());
}

#[test]
fn factor_follows_dpi_or_font() {
    let mut s = parse(SAMPLE).unwrap();
    assert_eq!(s.factor(1.5, 20.0), 0.5);
    s.scale_with = ScaleWith::Font;
    assert_eq!(s.factor(1.5, 32.0), 1.0, "字号是基准的两倍 → 倍率翻倍");
}

#[test]
fn default_placement_aligns_frame_corners() {
    let s = parse(SAMPLE).unwrap();
    let p = s.horizontal.unwrap().placement;
    assert_eq!(p.caret.anchor.on, AnchorBox::Frame);
    assert_eq!(p.caret_above.anchor.pos, Pos9::BOTTOM_LEFT);
    assert_eq!(p.avoid_caret, Some(AvoidStrip::default()), "缺省避让光标行");
    assert_eq!(p.clamp, Clamp::Frame);
}

#[test]
fn caret_point_accepts_name_or_table() {
    let s = parse(
        r#"
[skin.horizontal.frame]
image = "a"
[skin.horizontal.placement]
caret = { anchor = "frame.top_left", offset = [180, 260] }
caret_above = "content.bottom_left"
avoid_caret = false
clamp = "all"
"#,
    )
    .unwrap();
    let p = s.horizontal.unwrap().placement;
    assert_eq!(p.caret.offset, [180.0, 260.0]);
    assert_eq!(p.caret.anchor.pos, Pos9::TOP_LEFT);
    assert_eq!(p.caret_above.anchor.on, AnchorBox::Content);
    assert_eq!(p.caret_above.offset, [0.0, 0.0]);
    assert_eq!(p.avoid_caret, None);
    assert_eq!(p.clamp, Clamp::All);
}

#[test]
fn avoid_strip_accepts_table() {
    let s = parse(
        "[skin.horizontal.frame]\nimage = \"a\"\n[skin.horizontal.placement]\navoid_caret = { before = 80 }",
    )
    .unwrap();
    assert_eq!(
        s.horizontal.unwrap().placement.avoid_caret,
        Some(AvoidStrip {
            before: 80.0,
            after: 40.0
        })
    );
}

#[test]
fn slots_need_candidates_and_default_to_content_top_left() {
    let s = parse(
        r#"
[skin.horizontal.frame]
image = "a"
[skin.horizontal.slots.candidates]
offset = [0, 40]
[skin.horizontal.slots.footer]
anchor = "frame.bottom_right"
offset = [-20, -10]
"#,
    )
    .unwrap();
    let slots = s.horizontal.unwrap().slots.expect("写了 candidates 即启用");
    assert_eq!(slots.candidates.anchor.on, AnchorBox::Content);
    assert_eq!(slots.candidates.pivot, Pos9::TOP_LEFT);
    assert_eq!(slots.candidates.offset, [0.0, 40.0]);
    assert!(slots.preedit.is_none());
    assert_eq!(slots.footer.unwrap().pivot, Pos9 { h: 2, v: 2 });
    let s =
        parse("[skin.horizontal.frame]\nimage = \"a\"\n[skin.horizontal.slots.preedit]\n").unwrap();
    assert!(s.horizontal.unwrap().slots.is_none(), "缺候选槽按 stack");
}

#[test]
fn top_level_skin_survives_normalize_and_reaches_resolved() {
    let n = crate::normalize::normalize_theme(SAMPLE.parse::<Value>().unwrap());
    assert!(
        n.get("views").and_then(|v| v.get("skin")).is_none(),
        "skin 不是视图节点，不能被收进 views"
    );
    let t: crate::schema::Theme = n.try_into().unwrap();
    let r = crate::resolve::resolve(&t, false, &[]);
    assert_eq!(r.skin.unwrap().horizontal.unwrap().frame.image, "frame_h");
}

#[test]
fn child_variant_replaces_base_variant_whole() {
    let mut base: Value = "[skin.horizontal.frame]\nimage = \"b\"\nstretch_x = [[1, 2]]\n[skin.vertical.frame]\nimage = \"bv\"".parse().unwrap();
    let over: Value = "[skin.horizontal.frame]\nimage = \"c\"".parse().unwrap();
    replace_inherited_variants(&mut base, &over);
    let merged = crate::theme::merge(base, over);
    let s: Skin = merged.get("skin").cloned().unwrap().try_into().unwrap();
    let s = resolve_skin(&s).unwrap();
    let h = s.horizontal.unwrap();
    assert_eq!(h.frame.image, "c");
    assert_eq!(
        h.frame.stretch_x,
        Stretch::Whole(BandFill::Stretch),
        "base 的 stretch_x 不能漏进来"
    );
    assert_eq!(
        s.vertical.unwrap().frame.image,
        "bv",
        "没覆盖的变体照常继承"
    );
}
