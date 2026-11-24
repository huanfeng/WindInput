use super::*;
use wind_ui_types::{SpanStyle, StyledText};

const RED: [u8; 4] = [0xC0, 0, 0, 255];

/// 「wq(nǐ)」：`wq` 带内联色 `#C00000`（选中态改用 `selected=` 给的色），其余无色。
fn comment(spec: &str) -> StyledText {
    let mut t = StyledText::new();
    t.push(
        "wq",
        &SpanStyle {
            role: Some("code_rev"),
            color: Some(std::sync::Arc::new(wind_theme::InlineColor::parse(spec))),
            ..Default::default()
        },
    );
    t.push(
        "(nǐ)",
        &SpanStyle {
            role: Some("literal"),
            ..Default::default()
        },
    );
    t
}

fn window(theme: wind_theme::Resolved, upright: bool) -> CandidateWindow {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_theme(theme);
    w.set_orientation(false, upright, upright);
    w
}

fn cand(c: StyledText) -> CandidateItem {
    CandidateItem {
        text_styled: Default::default(),
        text: "你".into(),
        code: String::new(),
        label: String::new(),
        tooltip: Default::default(),
        comment: c,
        comment_above: Default::default(),
        no_index: false,
    }
}

/// 树里所有带颜色区间的叶子：`(文字, 区间)`。
fn colored(v: &View, out: &mut Vec<(String, Vec<ColorRun>)>) {
    if let Some(t) = &v.text
        && !v.color_runs.is_empty()
    {
        out.push((t.clone(), v.color_runs.clone()));
    }
    for c in &v.children {
        colored(c, out);
    }
}

fn runs_of(
    w: &mut CandidateWindow,
    comment_spec: &str,
    selected: usize,
) -> Vec<(String, Vec<ColorRun>)> {
    w.update(
        "ni",
        2,
        "",
        vec![cand(comment(comment_spec))],
        selected,
        -1,
        1,
        1,
    );
    let mut out = Vec::new();
    colored(&w.scene.build_tree(&w.scene_input(false)), &mut out);
    out
}

fn run(start: u32, end: u32, rgba: [u8; 4]) -> ColorRun {
    ColorRun { start, end, rgba }
}

#[test]
fn inline_color_reaches_the_comment_leaf() {
    let mut w = window(wind_theme::Resolved::default(), false);
    // 候选 0 不是选中（selected 越界）⇒ 常态。
    assert_eq!(
        runs_of(&mut w, "#C00000", 9),
        vec![("wq(nǐ)".to_string(), vec![run(0, 2, RED)])]
    );
}

/// 规则 3：主题把选中态注释正文色改了 ⇒ 内联色回落该态正文色（区间被丢）；
/// `selected=` 单列的照用。
#[test]
fn selected_state_falls_back_unless_inline_says_selected() {
    let mut theme = wind_theme::Resolved::default();
    theme.views.comment.selected = Some(Box::new(wind_theme::RvNode {
        text_color: Some([255, 255, 255, 255]),
        ..Default::default()
    }));
    let mut w = window(theme, false);
    assert_eq!(runs_of(&mut w, "#C00000", 0), vec![], "选中态回落正文色");
    assert_eq!(
        runs_of(&mut w, "#C00000,selected=#00FF00", 0),
        vec![("wq(nǐ)".to_string(), vec![run(0, 2, [0, 255, 0, 255])])]
    );
}

/// 非出厂测试主题 `span-roles`（wind-theme testdata）：主题角色色真的接到注释叶子，
/// 且选中 / 悬停态按 §6.3 回落。
fn roles_theme() -> wind_theme::Resolved {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    wind_theme::load_resolved_dirs(
        &[
            root.join("../wind-theme/testdata/themes"),
            root.join("../../../data/themes"),
        ],
        "span-roles",
        false,
    )
    .unwrap()
}

/// 「wq nǐ」：`wq` 角色 code_rev、` ` 字面、`nǐ` 角色 pinyin。
fn roles_comment() -> StyledText {
    let mut t = StyledText::new();
    t.push(
        "wq",
        &SpanStyle {
            role: Some("code_rev"),
            ..Default::default()
        },
    );
    t.push(
        " ",
        &SpanStyle {
            role: Some("literal"),
            ..Default::default()
        },
    );
    t.push(
        "nǐ",
        &SpanStyle {
            role: Some("pinyin"),
            ..Default::default()
        },
    );
    t
}

fn roles_runs(w: &mut CandidateWindow, selected: usize, hover: i32) -> Vec<ColorRun> {
    w.update(
        "ni",
        2,
        "",
        vec![cand(roles_comment())],
        selected,
        hover,
        1,
        1,
    );
    let mut out = Vec::new();
    colored(&w.scene.build_tree(&w.scene_input(false)), &mut out);
    out.into_iter().flat_map(|(_, r)| r).collect()
}

#[test]
fn theme_roles_color_the_comment_per_state() {
    let mut w = window(roles_theme(), false);
    const CODE: [u8; 4] = [0xC0, 0, 0, 255];
    const LIT: [u8; 4] = [0xB0, 0xB0, 0xB0, 255];
    const PY: [u8; 4] = [0, 0x80, 0, 255];
    // 常态：三个角色各自的色。
    assert_eq!(
        roles_runs(&mut w, 9, -1),
        vec![run(0, 2, CODE), run(2, 3, LIT), run(3, 6, PY)]
    );
    // 选中：主题改了选中态正文色 ⇒ 未单列的回落正文色（区间被丢）；code_rev 单列保留。
    assert_eq!(
        roles_runs(&mut w, 0, -1),
        vec![run(0, 2, [0xFF, 0xE0, 0x8A, 255])]
    );
    // 悬停：正文色没改 ⇒ 常态角色色照用；pinyin 在悬停态单列。
    assert_eq!(
        roles_runs(&mut w, 9, 0),
        vec![
            run(0, 2, CODE),
            run(2, 3, LIT),
            run(3, 6, [0, 0, 0xFF, 255])
        ]
    );
}

/// 直立态逐格切：`w`、`q` 两格各得一段，其余格没有区间。
#[test]
fn upright_cells_get_their_slices() {
    let mut w = window(wind_theme::Resolved::default(), true);
    assert_eq!(
        runs_of(&mut w, "#C00000", 9),
        vec![
            ("w".to_string(), vec![run(0, 1, RED)]),
            ("q".to_string(), vec![run(0, 1, RED)]),
        ]
    );
}
