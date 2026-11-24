use super::*;

fn cand(text: &str, comment: &str) -> CandidateItem {
    CandidateItem {
        text_styled: Default::default(),
        text: text.to_string(),
        code: String::new(),
        label: String::new(),
        tooltip: Default::default(),
        comment: comment.into(),
        comment_above: Default::default(),
        no_index: false,
    }
}

/// 竖排、scale=1、主题竖排最大宽度 `vmax_dp`（0=只受屏幕安全宽约束）。
fn build(vertical: bool, vmax_dp: i32, items: Vec<CandidateItem>) -> (CandidateWindow, View) {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_orientation(vertical, false, false);
    w.theme.behavior.vertical_max_width = vmax_dp;
    w.update("", 0, "", items, 0, -1, 1, 1);
    let mut root = w.scene.build_tree(&w.scene_input(false));
    root.layout(0.0, 0.0, &w.text_renderer);
    (w, root)
}

fn find_list(v: &View) -> Option<&View> {
    if v.children.iter().any(|c| c.tag >= 0) {
        return Some(v);
    }
    v.children.iter().find_map(find_list)
}

/// 候选 item 的 (文字, 注释) 文本（item = Row[序号, 文字, 注释?]）。
fn texts(root: &View) -> Vec<(String, Option<String>)> {
    let list = find_list(root).expect("未找到候选列表容器");
    let mut its: Vec<&View> = list.children.iter().filter(|c| c.tag >= 0).collect();
    its.sort_by_key(|c| c.tag);
    its.iter()
        .map(|it| {
            let t = it.children[1].text.clone().unwrap_or_default();
            let c = it.children.get(2).and_then(|c| c.text.clone());
            (t, c)
        })
        .collect()
}

/// ★ t271 主症状：注释极长时主词条完整显示，注释截断加 `…`，窗口不超上限。
#[test]
fn long_comment_yields_to_candidate_text() {
    let long = "注".repeat(300);
    for vmax in [0, 300] {
        let (w, root) = build(true, vmax, vec![cand("你好", &long), cand("世界", "sj")]);
        let t = texts(&root);
        assert_eq!(t[0].0, "你好", "vmax={vmax}：主词条不得被注释挤截");
        let c0 = t[0].1.as_deref().unwrap();
        assert!(c0.ends_with('…'), "vmax={vmax}：超长注释应截断加省略号");
        assert!(c0.chars().count() < long.chars().count());
        assert_eq!(t[1], ("世界".into(), Some("sj".into())), "短注释不受影响");
        let cap = if vmax > 0 {
            vmax as f32
        } else {
            w.screen_safety_max_width_px() as f32
        };
        assert!(
            root.measured_size().0 <= cap + 0.5,
            "vmax={vmax}：窗口宽 {} 超出上限 {cap}",
            root.measured_size().0
        );
    }
}

/// 放得下就一个字都不动（零回归）。
#[test]
fn fitting_text_and_comment_are_untouched() {
    let (_, root) = build(true, 300, vec![cand("你好", "ni hao"), cand("世界", "")]);
    let t = texts(&root);
    assert_eq!(t[0], ("你好".into(), Some("ni hao".into())));
    assert_eq!(t[1], ("世界".into(), None));
}

/// 主词条自身就超长：主词条截断，但仍给注释留出保底（看得见一截 + `…`），行宽不超上限。
#[test]
fn overlong_text_still_leaves_room_for_comment() {
    let long_text = "长".repeat(100);
    let (_, root) = build(true, 300, vec![cand(&long_text, "chang")]);
    let t = texts(&root);
    assert!(t[0].0.ends_with('…'), "主词条超出行宽时照常截断");
    let c = t[0].1.as_deref().expect("注释节点仍在");
    assert!(!c.is_empty());
    assert!(
        root.measured_size().0 <= 300.5,
        "行宽 {}",
        root.measured_size().0
    );
}

/// 多行注释（竖排模板里写 `\n`）逐行截断：首行超长不会把后面几行整个吃掉。
#[test]
fn multiline_comment_truncates_per_line() {
    let long = "注".repeat(300);
    let comment = format!("{long}\nshort");
    let (_, root) = build(true, 300, vec![cand("你好", &comment)]);
    let t = texts(&root);
    assert_eq!(t[0].0, "你好");
    let c = t[0].1.as_deref().unwrap();
    let lines: Vec<&str> = c.split('\n').collect();
    assert_eq!(lines.len(), 2, "行数不变：{c:?}");
    assert!(lines[0].ends_with('…'));
    assert_eq!(lines[1], "short");
}
