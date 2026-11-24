use super::*;

fn cand(text: &str) -> CandidateItem {
    CandidateItem {
        text_styled: Default::default(),
        text: text.to_string(),
        code: String::new(),
        label: String::new(),
        tooltip: Default::default(),
        comment: Default::default(),
        comment_above: Default::default(),
        no_index: false,
    }
}

/// 造一个候选窗并布局，返回 (窗口, 内容宽)。scale 钉 1.0 使 dp == 设备像素。
fn build(vertical: bool, texts: &[&str]) -> (CandidateWindow, View) {
    build_o(vertical, false, texts)
}

/// 同 [`build`]，可指定旋转位与直立位。
fn build_o(vertical: bool, rotated: bool, texts: &[&str]) -> (CandidateWindow, View) {
    build_ou(vertical, rotated, false, texts)
}

/// 同 [`build_o`]，可指定「字直立」（对联式竖排）。
fn build_ou(
    vertical: bool,
    rotated: bool,
    upright: bool,
    texts: &[&str],
) -> (CandidateWindow, View) {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_orientation(vertical, rotated, upright);
    let items: Vec<CandidateItem> = texts.iter().map(|t| cand(t)).collect();
    w.update("", 0, "", items, 0, -1, 1, 1);
    let mut root = w.scene.build_tree(&w.scene_input(false));
    root.layout(0.0, 0.0, &w.text_renderer);
    (w, root)
}

/// 取全部候选的命中矩形，按 tag 升序（tag = 原始候选下标）。
fn hits_by_tag(root: &View) -> Vec<Rect> {
    let mut out = Vec::new();
    root.collect_hits(&mut out);
    out.retain(|(t, _)| *t >= 0);
    out.sort_by_key(|(t, _)| *t);
    out.into_iter().map(|(_, r)| r).collect()
}

/// 横排多候选：整行宽度不得超过屏幕安全上限，与候选个数无关。
///
/// 「先到先得」贪心分配下，第一个候选吃光预算，其余候选仍各加固定开销与文字下限，累加必然
/// 突破上限；逐个候选看都没超自己的预算，所以这里量 `build_tree` 产物的总宽。
#[test]
fn horizontal_row_never_exceeds_screen_budget_for_any_candidate_count() {
    let long = "超长候选".repeat(200); // 800 字，远超任何屏幕宽度
    for n in 1..=12usize {
        let texts: Vec<&str> = (0..n).map(|_| long.as_str()).collect();
        let (w, root) = build(false, &texts);
        let cap = w.screen_safety_max_width_px() as f32;
        let got = root.measured_size().0;
        assert!(
            got <= cap + 0.5,
            "{n} 个超长候选横排时整行宽 {got} 超出上限 {cap}"
        );
    }
}

/// 横排「一条长整句 + 若干短词」——最常见的形态：短词必须**原样显示不被误截**，长句吃掉
/// 剩余空间。一刀切均分会把长句压到 1/n（明明还有大把空间），water-filling 才有这个性质。
#[test]
fn horizontal_short_candidates_survive_next_to_a_long_one() {
    let long = "长".repeat(500);
    let mut texts: Vec<&str> = vec![long.as_str()];
    texts.extend(["你好", "世界", "输入", "法"]);
    let (w, root) = build(false, &texts);
    assert!(
        root.measured_size().0 <= w.screen_safety_max_width_px() as f32 + 0.5,
        "整行仍不得超上限"
    );
    let mut in_tree = Vec::new();
    collect_texts(&root, &mut in_tree);
    for short in ["你好", "世界", "输入", "法"] {
        assert!(
            in_tree.iter().any(|t| t == short),
            "短候选 {short} 不该被截断，树内文本：{in_tree:?}"
        );
    }
}

/// 竖排：每候选独占一行、各自用满整行预算，行宽同样不得超上限。
#[test]
fn vertical_rows_never_exceed_screen_budget() {
    let long = "超长候选".repeat(200);
    let texts: Vec<&str> = (0..9).map(|_| long.as_str()).collect();
    let (w, root) = build(true, &texts);
    let cap = w.screen_safety_max_width_px() as f32;
    let got = root.measured_size().0;
    assert!(got <= cap + 0.5, "竖排宽 {got} 超出上限 {cap}");
}

/// 正常长度的候选完全不受影响：没超预算就不该动它（零回归）。
#[test]
fn normal_candidates_are_untouched() {
    for vertical in [false, true] {
        let (_, root) = build(vertical, &["你好", "世界", "输入法"]);
        let mut in_tree = Vec::new();
        collect_texts(&root, &mut in_tree);
        for t in ["你好", "世界", "输入法"] {
            assert!(
                in_tree.iter().any(|x| x == t),
                "vertical={vertical} 下正常候选 {t} 被改动了：{in_tree:?}"
            );
        }
    }
}

/// 递归收集树内所有文本叶子的内容。
fn collect_texts(v: &View, out: &mut Vec<String>) {
    if let Some(t) = &v.text {
        out.push(t.clone());
    }
    for c in &v.children {
        collect_texts(c, out);
    }
}

/// 手动基准：量截断带来的 build_tree+layout 开销与测量缓存增长。
/// `cargo test -p wind-ui --release --lib bench_truncation_cost -- --ignored --nocapture`
///
/// 只在 Windows：要量的是真实 DirectWrite 的排版/哈希成本，mock 后端（等宽近似）的数字
/// 没有意义，`measure_cache_len` 也只有真实后端才有。
#[cfg(windows)]
#[test]
#[ignore = "手动基准，不参与常规测试"]
fn bench_truncation_cost() {
    use std::time::Instant;

    let short: Vec<&str> = vec![
        "你好",
        "世界",
        "输入法",
        "候选",
        "测试",
        "性能",
        "开销",
        "评估",
        "基准",
    ];
    let longs = "超长候选词条".repeat(140); // 840 字
    let long: Vec<&str> = (0..9).map(|_| longs.as_str()).collect();

    for (name, texts) in [("短候选(常态)", &short), ("超长候选(最坏)", &long)] {
        for vertical in [false, true] {
            let (tx, _rx) = std::sync::mpsc::channel();
            let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
            w.scale = 2.0;
            w.set_orientation(vertical, false, false);
            let items: Vec<CandidateItem> = texts.iter().map(|t| cand(t)).collect();
            w.update("ceshipinyin", 0, "", items, 0, -1, 1, 1);
            let cache0 = w.text_renderer.measure_cache_len();

            // 冷：首帧（所有测量都要真打 DirectWrite）
            let t0 = Instant::now();
            let mut root = w.scene.build_tree(&w.scene_input(false));
            root.layout(0.0, 0.0, &w.text_renderer);
            let cold = t0.elapsed();
            let cache1 = w.text_renderer.measure_cache_len();

            // 热：稳态（同样文本，测量走缓存）——真实打字时每帧都是这个路径
            let n = 50;
            let t1 = Instant::now();
            for _ in 0..n {
                let mut r = w.scene.build_tree(&w.scene_input(false));
                r.layout(0.0, 0.0, &w.text_renderer);
            }
            let warm = t1.elapsed() / n;
            let cache2 = w.text_renderer.measure_cache_len();

            println!(
                "{name} {} | 冷={cold:?} 热={warm:?} | 缓存 {cache0}→{cache1}→{cache2}",
                if vertical { "竖排" } else { "横排" }
            );
        }
    }

    // 拆解：新增开销的两个构成——(a) 预扫阶段的「已缓存」测量，(b) 二分探测串的构造+哈希。
    let (tx, _rx) = std::sync::mpsc::channel();
    let w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    let st = TextStyle::new(36.0);
    for (label, text) in [("短串(4字)", "输入法好"), ("长串(840字)", longs.as_str())] {
        w.text_renderer.measure(text, &st); // 预热入缓存
        let n = 2000;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(w.text_renderer.measure(std::hint::black_box(text), &st));
        }
        println!("  已缓存 measure {label}: {:?}/次", t.elapsed() / n);
    }
    // 单次截断（含二分）的成本：预算取自然宽的一半，强制触发。
    let full = w.text_renderer.measure(longs.as_str(), &st).width;
    let n = 500;
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(w.scene_input(false).truncate_text_for_width(
            std::hint::black_box(longs.as_str()),
            &st,
            full * 0.5,
        ));
    }
    println!(
        "  truncate_text_for_width(840字, 热): {:?}/次",
        t.elapsed() / n
    );
}

// ───────────────────── 旋转态接线（A：候选项旋转 90°）─────────────────────

/// 旋转态的窗口尺寸相对竖排必须整个翻过来。对照必须选竖排（本身窄而高）：拿横排比，
/// 「旋转位被当成竖排、根本没套旋转包裹」的实现照样通过。
#[test]
fn rotated_window_swaps_dimensions_against_vertical() {
    let texts = ["候选一二三四五六", "候选一二三四五六", "候选一二三四五六"];
    let (_, v_root) = build_o(true, false, &texts);
    let (_, r_root) = build_o(false, true, &texts);
    let (vw, vh) = v_root.measured_size();
    let (rw, rh) = r_root.measured_size();
    assert!(
        rw < vw,
        "旋转态的宽应远小于竖排（宽只由列数决定）: 旋转 {rw} vs 竖排 {vw}"
    );
    assert!(
        rh > vh,
        "旋转态的高应远大于竖排（文字长度落到高度上）: 旋转 {rh} vs 竖排 {vh}"
    );
}

/// 候选 1 必须落在最左。局部列首经顺时针 90° 会落到屏幕最右，故列表在局部空间里逆序排；
/// 少了逆序，候选顺序整个左右颠倒，画面却看上去正常。
#[test]
fn rotated_puts_candidate_one_leftmost() {
    let (_, root) = build_o(false, true, &["甲", "乙", "丙"]);
    let hits = hits_by_tag(&root);
    assert_eq!(hits.len(), 3, "应收集到三个候选的命中矩形");
    assert!(
        hits[0].x < hits[1].x && hits[1].x < hits[2].x,
        "候选应自左向右为 1、2、3，实测 x = {:?}",
        hits.iter().map(|r| r.x).collect::<Vec<_>>()
    );
}

// ─────────────── 直立态（B：文字竖排／对联式）───────────────

/// 直立态与旋转态的唯一几何差别：文字沿列前进的步长（旋转态占前进宽度，直立态占行高）。
///
/// 断言写成「加字带来的高度增量」而非总高，把两态相同的边距、序号、固定开销约掉，否则真实
/// 字体下差异可能被淹没。用拉丁文字放大差距：前进宽约 0.5em、行高约 1.2em。
#[test]
fn upright_advances_by_line_height_rotated_by_glyph_width() {
    let step = |upright: bool| {
        let (_, one) = build_ou(false, true, upright, &["a"]);
        let (_, four) = build_ou(false, true, upright, &["aaaa"]);
        one.measured_size();
        four.measured_size().1 - one.measured_size().1
    };
    let (rot, upr) = (step(false), step(true));
    assert!(rot > 0.0, "对照失效：旋转态加字也该变高，实测 {rot}");
    assert!(
        upr > rot * 1.5,
        "直立态每字应按行高推进（远大于字宽）：直立 {upr} vs 旋转 {rot}"
    );
}

/// 直立态与旋转态同一种排列：候选 1 同样在最左。少了这条，直立时忘了跟着逆序只会让候选
/// 左右颠倒，画面照样正常。
#[test]
fn upright_puts_candidate_one_leftmost() {
    let (_, root) = build_ou(false, true, true, &["甲", "乙", "丙"]);
    let hits = hits_by_tag(&root);
    assert_eq!(hits.len(), 3, "应收集到三个候选的命中矩形");
    assert!(
        hits[0].x < hits[1].x && hits[1].x < hits[2].x,
        "候选应自左向右为 1、2、3，实测 x = {:?}",
        hits.iter().map(|r| r.x).collect::<Vec<_>>()
    );
}

/// 序号整块扶正，不逐格切：两位数的序号横着读，占的仍是一格的高度。
///
/// 判据落在列高上（宽度可能被别的节点顶住，测不出来）。对照必不可少：同一批候选里多一个字
/// 必须让列变高，否则这条断言恒真。
#[test]
fn upright_index_is_one_block_not_stacked_digits() {
    let build = |label: &str, text: &str| {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(false, true, true);
        let mut item = cand(text);
        item.label = label.to_string();
        w.update("", 0, "", vec![item], 0, -1, 1, 1);
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        root.measured_size().1
    };
    let one = build("1", "字");
    assert!(
        (build("88", "字") - one).abs() < 0.5,
        "两位序号让列变高了 ⇒ 序号被逐格竖着切了：{} vs {one}",
        build("88", "字")
    );
    assert!(
        build("1", "字字") > one + 0.5,
        "对照失效：多一个字也没让列变高，本用例测不出东西"
    );
}

/// 注释在直立态下也逐格竖排，拉丁编码不例外；否则同一列里出现两种阅读方向。
///
/// 判据是「加一段注释让列长了多少」：旋转态每个拉丁字符吃前进宽（约 0.5em），直立态吃行高
/// （约 1.2em）。写成增量之比而不是总高，理由同 `upright_advances_by_line_height…`。
#[test]
fn upright_stacks_the_comment_too() {
    let col_h = |upright: bool, comment: &str| {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(false, true, upright);
        let mut item = cand("甲");
        item.comment = comment.into();
        w.update("", 0, "", vec![item], 0, -1, 1, 1);
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        root.measured_size().1
    };
    let rot = col_h(false, "abcd") - col_h(false, "");
    let upr = col_h(true, "abcd") - col_h(true, "");
    assert!(rot > 0.0, "对照失效：旋转态加注释也该变长，实测 {rot}");
    assert!(
        upr > rot * 1.5,
        "直立态的注释应按行高逐格推进：直立 {upr} vs 旋转 {rot}"
    );
}

/// 模式徽标与内联编码同样在旋转包裹层里，直立态一并逐格扶正。与注释共用 `upright_text`，
/// 仍各测一次：接线漏一处只表现为「某一段还躺着」，别的测试全绿。
///
/// 判据落在窗口高度上，且要让被测那一段成为最长的一列（长串 + 短候选）：往局部 Column 里加
/// 节点长的是局部高度 ⇒ 屏幕宽度，只有节点自身沿列的长度才映射到屏幕高度，拿「加了它高度
/// 涨多少」当判据会恒为 0。用拉丁串放大差距（汉字两轴只差两成）。
#[test]
fn upright_stacks_the_inline_preedit_and_mode_chip() {
    const LONG: &str = "abcdefghijkl";
    let h = |upright: bool, preedit: &str, label: &str| {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_orientation(false, true, upright);
        w.set_preedit_embedded(true);
        w.update(preedit, preedit.len(), label, vec![cand("甲")], 0, -1, 1, 1);
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        root.measured_size().1
    };
    let bare = h(false, "", "");
    for (what, preedit, label) in [("内联编码", LONG, ""), ("模式徽标", "", LONG)] {
        let rot = h(false, preedit, label);
        let upr = h(true, preedit, label);
        assert!(
            rot > bare,
            "{what} 对照失效：它没成为最长的一列，这条测不出东西（{rot} vs {bare}）"
        );
        assert!(
            upr > rot * 1.8,
            "{what} 没逐格扶正：直立 {upr} vs 旋转 {rot}"
        );
    }
}

/// 截断量的是沿列方向的长度，不是文字的横向宽度。
///
/// 同一预算下直立态能放的拉丁字母必须比横向少（横向约 0.5em、竖着约 1.2em）；一样多说明
/// 截断还按横向宽度估容量，英文候选会在对联模式下戳出屏幕，而中文候选看着正常。
#[test]
fn upright_truncation_measures_the_stacking_axis() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    let style = TextStyle::new(20.0);
    let text = "abcdefghij";
    // 预算取「横向刚好放得下五个字母」，两态用同一个数。
    let budget = w.text_renderer.measure("abcde", &style).width;

    w.set_orientation(false, false, false);
    let flat = w
        .scene_input(false)
        .truncate_candidate_text(text, &style, budget);
    w.set_orientation(false, true, true);
    let stacked = w
        .scene_input(false)
        .truncate_candidate_text(text, &style, budget);

    assert!(
        flat.chars().count() > stacked.chars().count(),
        "直立态该放得更少：横向 {flat:?} vs 直立 {stacked:?}"
    );
    // 再紧的预算也至少留一个字 + 省略号，不能什么都不显示。
    w.set_orientation(false, true, true);
    assert_eq!(
        w.scene_input(false)
            .truncate_candidate_text(text, &style, 0.0),
        "a…"
    );
    assert_eq!(
        w.scene_input(false)
            .truncate_candidate_text("", &style, 0.0),
        ""
    );
}

/// 旋转态走竖排的列表路径：每个候选在局部空间独占一行、宽度统一，转完就是等宽的列。
///
/// 断言落在高度上（局部宽度经顺时针 90° 变成屏幕高度）；断言宽度对应局部行高，哪条路径都恒等。
/// 反向对照不可少：同一批长短不一的候选在横排下宽度必须互不相同。
#[test]
fn rotated_uses_the_vertical_list_path() {
    let texts = ["甲", "乙丙丁", "戊己庚辛壬"];
    let (_, r_root) = build_o(false, true, &texts);
    let r = hits_by_tag(&r_root);
    assert!(
        (r[0].h - r[1].h).abs() < 0.5 && (r[1].h - r[2].h).abs() < 0.5,
        "旋转态各列应等高（＝竖排路径统一的高亮宽度转过来）: {:?}",
        r.iter().map(|x| x.h).collect::<Vec<_>>()
    );
    let (_, h_root) = build_o(false, false, &texts);
    let h = hits_by_tag(&h_root);
    assert!(
        (h[0].w - h[2].w).abs() > 0.5,
        "横排下长短候选宽度应不同，否则本用例的对照失效: {:?}",
        h.iter().map(|x| x.w).collect::<Vec<_>>()
    );
}
