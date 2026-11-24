use super::*;
use wind_ui_types::{SpanStyle, StyledText};

fn cand_above(text: &str, above: &str, comment: &str) -> CandidateItem {
    CandidateItem {
        text_styled: Default::default(),
        text: text.to_string(),
        code: String::new(),
        label: String::new(),
        tooltip: Default::default(),
        comment: comment.into(),
        comment_above: above.into(),
        no_index: false,
    }
}

fn mk(vertical: bool, rotated: bool, upright: bool, rows: u32) -> CandidateWindow {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_orientation(vertical, rotated, upright);
    w.set_min_size(0, 0, 0, 0, rows);
    w
}

fn win(vertical: bool, items: Vec<CandidateItem>) -> CandidateWindow {
    let mut w = mk(vertical, false, false, 0);
    w.update("", 0, "", items, 0, -1, 1, 1);
    w
}

fn laid(w: &CandidateWindow) -> View {
    let mut root = w.scene.build_tree(&w.scene_input(false));
    root.layout(0.0, 0.0, &w.text_renderer);
    root
}

fn find_list(v: &View) -> Option<&View> {
    if v.children.iter().any(|c| c.tag >= 0) {
        return Some(v);
    }
    v.children.iter().find_map(find_list)
}

/// 候选 item（按 tag 升序）。
fn items(root: &View) -> Vec<&View> {
    let list = find_list(root).expect("未找到候选列表容器");
    let mut out: Vec<&View> = list.children.iter().filter(|c| c.tag >= 0).collect();
    out.sort_by_key(|c| c.tag);
    out
}

fn all_leaves<'a>(v: &'a View, out: &mut Vec<&'a View>) {
    if v.text.is_some() {
        out.push(v);
    }
    for c in &v.children {
        all_leaves(c, out);
    }
}

/// 带上方条的 item：`(上方条叶子, 主行)`。
fn split_item(item: &View) -> (&View, &View) {
    assert!(
        matches!(item.layout, Layout::Column),
        "带上方条的 item 应是 Column"
    );
    assert_eq!(item.children.len(), 2, "Column[上方条, 主行]");
    (&item.children[0], &item.children[1])
}

/// 有上段 vs 无上段：item 恰好高出上方条叶子的外框高度（主行不变）。
#[test]
fn above_line_adds_one_row_height() {
    for vertical in [true, false] {
        let plain = laid(&win(vertical, vec![cand_above("你好", "", "wq")]));
        let with = laid(&win(vertical, vec![cand_above("你好", "ni hao", "wq")]));
        let h0 = items(&plain)[0].laid_rect().h;
        let item = items(&with)[0];
        let (above, _) = split_item(item);
        let ar = above.laid_rect();
        let above_box = ar.h + above.margin.t + above.margin.b;
        assert!(above_box > 0.0);
        assert!(
            (item.laid_rect().h - h0 - above_box).abs() < 0.5,
            "vertical={vertical}：item 应恰好高出一条上方条（{} vs {h0} + {above_box}）",
            item.laid_rect().h
        );
    }
}

/// 按页预留：3 条里只有第 2 条有上段，三个 item 等高，且都高于「整页无上段」。
#[test]
fn above_line_reserves_height_page_wide() {
    for vertical in [true, false] {
        let root = laid(&win(
            vertical,
            vec![
                cand_above("一", "", "a"),
                cand_above("二", "er", "b"),
                cand_above("三", "", ""),
            ],
        ));
        let hs: Vec<f32> = items(&root).iter().map(|i| i.laid_rect().h).collect();
        assert_eq!(hs.len(), 3);
        assert!(
            hs.iter().all(|h| (h - hs[1]).abs() < 0.01),
            "vertical={vertical}：整页 item 须等高（{hs:?}）"
        );
        let plain = laid(&win(vertical, vec![cand_above("一", "", "a")]));
        assert!(
            hs[0] > items(&plain)[0].laid_rect().h,
            "无上段的候选也要预留"
        );
        // 无上段的候选放的是透明占位叶子，看不见。
        let (ghost, _) = split_item(items(&root)[0]);
        assert_eq!(ghost.text_color[3], 0, "占位叶子须透明");
    }
}

/// 上方条字形起点 == 候选文字字形起点（≥ 序号右缘），含圆圈序号与无序号行。
#[test]
fn above_line_starts_at_text_origin() {
    for circle in [false, true] {
        for vertical in [true, false] {
            let mut w = mk(vertical, false, false, 0);
            if circle {
                w.theme.views.index.bg_shape = "circle".to_string();
            }
            let mut no_idx = cand_above("丙", "bing", "");
            no_idx.no_index = true;
            w.update(
                "",
                0,
                "",
                vec![
                    cand_above("你好", "ni hao", "wq"),
                    cand_above("甲", "jia", ""),
                    no_idx,
                ],
                0,
                -1,
                1,
                1,
            );
            let root = laid(&w);
            for item in items(&root) {
                let (above, main) = split_item(item);
                let text = if item.tag == 2 {
                    &main.children[0]
                } else {
                    let idx = &main.children[0];
                    let text = &main.children[1];
                    let ir = idx.laid_rect();
                    assert!(
                        above.laid_rect().x + 0.01 >= ir.x + ir.w,
                        "上方条不得压到序号上（circle={circle}, vertical={vertical}）"
                    );
                    text
                };
                let a0 = above.laid_rect().x + above.padding.l;
                let t0 = text.laid_rect().x + text.padding.l;
                assert!(
                    (a0 - t0).abs() < 1.0,
                    "上方条起点 {a0} 应对齐文字起点 {t0}（tag={}, circle={circle}, vertical={vertical}）",
                    item.tag
                );
            }
        }
    }
}

/// 整页无上段：item 保持原来的 Row[序号, 文字, 注释]，没有任何 Column / 占位叶子。
/// （逐字节不变由 `render_golden` 的出厂对拍钉死，这条是本模块内的结构兜底。）
#[test]
fn no_above_keeps_tree_identical() {
    for vertical in [true, false] {
        let root = laid(&win(
            vertical,
            vec![cand_above("一", "", "a"), cand_above("二", "", "")],
        ));
        let its = items(&root);
        assert!(its.iter().all(|i| matches!(i.layout, Layout::Row)));
        assert_eq!(its[0].children.len(), 3, "序号 + 文字 + 注释");
        assert_eq!(its[1].children.len(), 2, "序号 + 文字");
    }
}

/// 上段极长：以 `…` 结尾，窗口不超过屏幕安全宽；文字本身不被上段挤截。
#[test]
fn above_line_overlong_is_ellipsized() {
    let long = "x".repeat(2000);
    for vertical in [true, false] {
        let w = win(
            vertical,
            vec![
                cand_above("你好", &long, "wq"),
                cand_above("世界", &long, ""),
            ],
        );
        let root = laid(&w);
        let cap = w.screen_safety_max_width_px() as f32;
        assert!(
            root.measured_size().0 <= cap + 0.5,
            "vertical={vertical}：窗口宽 {} 超出上限 {cap}",
            root.measured_size().0
        );
        for item in items(&root) {
            let (above, main) = split_item(item);
            let t = above.text.as_deref().unwrap();
            assert!(t.ends_with('…'), "上段应被截断加省略号");
            assert!(t.len() < long.len());
            assert_eq!(
                main.children[1].text.as_deref(),
                Some(if item.tag == 0 { "你好" } else { "世界" }),
                "候选文字不因上段而被截"
            );
        }
    }
}

/// 旋转 / 直立态不支持上方条：无 Column，上段以 `\n` 合回右侧注释。
#[test]
fn upright_merges_above_into_comment() {
    for upright in [false, true] {
        let mut w = mk(false, true, upright, 0);
        w.update(
            "",
            0,
            "",
            vec![cand_above("你", "ni", "亻尔"), cand_above("好", "hao", "")],
            0,
            -1,
            1,
            1,
        );
        let root = laid(&w);
        for item in items(&root) {
            assert!(
                matches!(item.layout, Layout::Row),
                "upright={upright}：不应出现 Column"
            );
        }
        let mut leaves = Vec::new();
        all_leaves(&root, &mut leaves);
        let texts: Vec<&str> = leaves.iter().filter_map(|l| l.text.as_deref()).collect();
        if upright {
            // 直立态逐格切：上段的每个字都在。
            for ch in ["n", "i", "亻", "尔", "h", "a", "o"] {
                assert!(texts.contains(&ch), "直立态缺 {ch}：{texts:?}");
            }
        } else {
            assert!(
                texts.contains(&"ni\n亻尔"),
                "上下段应以 \\n 合并：{texts:?}"
            );
            assert!(texts.contains(&"hao"), "下段空时只剩上段：{texts:?}");
        }
    }
}

/// `min_rows` 补的占位行与带上方条的真实行同构、等高。
#[test]
fn placeholder_rows_match_real_rows_when_above() {
    let five = |n: usize| {
        let mut w = mk(true, false, false, 5);
        w.theme.views.index.bg_shape = "circle".to_string();
        let texts = ["一", "二", "三", "四", "五"];
        let items: Vec<CandidateItem> = texts[..n]
            .iter()
            .enumerate()
            .map(|(i, t)| cand_above(t, if i == 1 { "er" } else { "" }, ""))
            .collect();
        w.update("", 0, "", items, 0, -1, 1, 1);
        w
    };
    let full = laid(&five(5)).measured_size().1;
    let padded_w = five(3);
    let padded = laid(&padded_w);
    assert_eq!(
        padded.measured_size().1,
        full,
        "3 候选补 2 空行须与 5 条等高"
    );
    let list = find_list(&padded).unwrap();
    let ph = list.children.iter().find(|c| c.tag < 0).expect("占位行");
    let real = list.children.iter().find(|c| c.tag == 1).unwrap();
    let (ph_above, ph_main) = split_item(ph);
    let (real_above, real_main) = split_item(real);
    assert_eq!(ph_above.font_size, real_above.font_size);
    assert_eq!(ph_above.font_family, real_above.font_family);
    assert_eq!(ph_above.font_weight, real_above.font_weight);
    let tb = |e: Edges| (e.t, e.b);
    assert_eq!(tb(ph_above.padding), tb(real_above.padding));
    assert_eq!(tb(ph_above.margin), tb(real_above.margin));
    assert_eq!(ph_main.children.len(), real_main.children.len());
    assert!((ph.laid_rect().h - real.laid_rect().h).abs() < 0.01);
}

/// `[comment_above.roles]` 只作用于上方条，右侧注释不受影响。
#[test]
fn above_line_colors_follow_comment_above_theme() {
    const GREEN: [u8; 4] = [0, 0x80, 0, 255];
    let py = |s: &str| {
        let mut t = StyledText::new();
        t.push(
            s,
            &SpanStyle {
                role: Some("pinyin"),
                ..Default::default()
            },
        );
        t
    };
    let mut w = mk(true, false, false, 0);
    w.theme
        .views
        .comment_above
        .roles
        .insert("pinyin".to_string(), GREEN);
    let mut c = cand_above("你", "", "");
    c.comment_above = py("ni");
    c.comment = py("nn");
    w.update("", 0, "", vec![c], 9, -1, 1, 1);
    let root = laid(&w);
    let (above, main) = split_item(items(&root)[0]);
    assert_eq!(
        above.color_runs,
        vec![ColorRun {
            start: 0,
            end: 2,
            rgba: GREEN
        }]
    );
    let cmt = &main.children[2];
    assert_eq!(cmt.text.as_deref(), Some("nn"));
    assert!(cmt.color_runs.is_empty(), "右侧注释不该吃到上方条的角色色");
}

/// 纵向中线。
fn mid_y(v: &View) -> f32 {
    let r = v.laid_rect();
    r.y + r.h * 0.5
}

/// 横排：翻页栏、模式徽标、内联编码这些一行高的非 item 成员与候选**主行**对齐，而不是
/// 与两行高的整个 item 居中（那样会比主行高出半个上方条）。
#[test]
fn horizontal_single_line_members_align_with_main_row() {
    // (preedit, 模式标记, 内联编码)：分别让徽标 / 内联编码落进候选行。
    for (preedit, mode, embedded) in [("", "拼", false), ("nihao", "", true)] {
        let mut w = mk(false, false, false, 0);
        w.set_preedit_embedded(embedded);
        w.update(
            preedit,
            preedit.len(),
            mode,
            vec![cand_above("好", "hǎo", "vb"), cand_above("浩", "", "")],
            0,
            -1,
            1,
            2,
        );
        let root = laid(&w);
        let list = find_list(&root).unwrap();
        let (_, main) = split_item(items(&root)[0]);
        let want = mid_y(main);
        let members: Vec<&View> = list.children.iter().filter(|c| c.tag < 0).collect();
        assert!(
            members.len() >= 2,
            "应有翻页栏 + 徽标/内联编码：{}",
            members.len()
        );
        for m in members {
            assert!(
                (mid_y(m) - want).abs() <= 1.0,
                "非 item 成员中线 {} 应与主行中线 {want} 对齐（preedit={preedit:?} mode={mode:?}）：{:?}",
                mid_y(m),
                m.text
            );
        }
    }
}

/// 点击上方条所在的区域命中该候选（上方条在 item 容器之内，命中区是整个 item）。
#[test]
fn click_on_above_line_hits_its_candidate() {
    for vertical in [true, false] {
        let root = laid(&win(
            vertical,
            vec![cand_above("好", "hǎo", ""), cand_above("号", "hào", "")],
        ));
        let mut hits = Vec::new();
        root.collect_hits(&mut hits);
        for item in items(&root) {
            let (above, _) = split_item(item);
            let r = above.laid_rect();
            let (x, y) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
            let tag = hits
                .iter()
                .find(|(_, hr)| hr.contains(x, y))
                .map_or(-1, |(t, _)| *t);
            assert_eq!(tag, item.tag, "vertical={vertical}：上方条应命中本候选");
        }
    }
}

/// `[comment_above.hover.roles]`：悬停态的角色色接到上方条。
#[test]
fn above_line_hover_role_color() {
    const GREEN: [u8; 4] = [0, 0x80, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 0xFF, 255];
    let mut w = mk(true, false, false, 0);
    let ca = &mut w.theme.views.comment_above;
    ca.roles.insert("pinyin".to_string(), GREEN);
    let mut hov = wind_theme::RvNode::default();
    hov.roles.insert("pinyin".to_string(), BLUE);
    ca.hover = Some(Box::new(hov));
    let mut c = cand_above("你", "", "");
    let mut t = StyledText::new();
    t.push(
        "ni",
        &SpanStyle {
            role: Some("pinyin"),
            ..Default::default()
        },
    );
    c.comment_above = t;
    let runs = |w: &mut CandidateWindow, hover: i32| {
        w.update("", 0, "", vec![c.clone()], 9, hover, 1, 1);
        let root = laid(w);
        let (above, _) = split_item(items(&root)[0]);
        above.color_runs.clone()
    };
    let rgba = |r: Vec<ColorRun>| r.iter().map(|r| r.rgba).collect::<Vec<_>>();
    assert_eq!(rgba(runs(&mut w, -1)), vec![GREEN], "常态");
    assert_eq!(rgba(runs(&mut w, 0)), vec![BLUE], "悬停");
}

/// `[comment_above]` 的背景 / 边框接到上方条叶子（含选中态），右侧注释不受影响；
/// 占位叶子不画装饰。
#[test]
fn above_line_consumes_background_and_border() {
    const BG: [u8; 4] = [1, 2, 3, 255];
    const SEL_BG: [u8; 4] = [4, 5, 6, 255];
    const BORDER: [u8; 4] = [7, 8, 9, 255];
    let mut w = mk(true, false, false, 0);
    let ca = &mut w.theme.views.comment_above;
    ca.bg_color = Some(BG);
    ca.border_color = Some(BORDER);
    ca.selected = Some(Box::new(wind_theme::RvNode {
        bg_color: Some(SEL_BG),
        ..Default::default()
    }));
    w.update(
        "",
        0,
        "",
        vec![
            cand_above("好", "hǎo", "vb"),
            cand_above("号", "hào", "kg"),
            cand_above("浩", "", "hj"),
        ],
        0,
        -1,
        1,
        1,
    );
    let root = laid(&w);
    let its = items(&root);
    let (sel_above, sel_main) = split_item(its[0]);
    let (above, _) = split_item(its[1]);
    let (ghost, _) = split_item(its[2]);
    assert_eq!(sel_above.bg, Some(SEL_BG), "选中态取 selected patch 的底色");
    assert_eq!(above.bg, Some(BG));
    assert_eq!(above.border.map(|b| b.0), Some(BORDER));
    assert_eq!(sel_main.children[2].bg, None, "右侧注释不吃上方条的底色");
    assert_eq!(sel_main.children[2].border, None);
    assert_eq!(ghost.bg, None, "占位叶子不画底色");
    assert_eq!(ghost.border, None);
}
