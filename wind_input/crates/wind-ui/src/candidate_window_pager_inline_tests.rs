use super::*;
use crate::view::Rect;

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

/// 造一个多页候选窗。`embedded` = 内联编码（`candidate_inline`），false 即独立编码栏。
///
/// `rotated`（蒙文竖排）与 `vertical` 互斥（见 `set_orientation` 的断言），故 `rotated=true`
/// 时 `vertical` 一律置 false；两者都让 `list_vertical` 成立。
fn win(vertical: bool, rotated: bool, embedded: bool, on: bool, preedit: &str) -> CandidateWindow {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_orientation(vertical && !rotated, rotated, false);
    w.set_preedit_embedded(embedded);
    w.set_pager_in_preedit(on);
    let items: Vec<CandidateItem> = ["一", "二", "三"].iter().map(|t| cand(t)).collect();
    // total_pages > 1 才有翻页栏；preedit 非空才谈得上「并入编码行」。
    w.update(preedit, preedit.len(), "", items, 0, -1, 1, 3);
    w
}

fn laid(w: &CandidateWindow, above: bool) -> View {
    let mut root = w.scene.build_tree(&w.scene_input(above));
    root.layout(0.0, 0.0, &w.text_renderer);
    root
}

fn height(w: &CandidateWindow) -> f32 {
    laid(w, false).measured_size().1
}

/// 递归收集树上所有叶子文本。
fn texts(v: &View) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(t) = &v.text {
        out.push(t.clone());
    }
    for c in &v.children {
        out.extend(texts(c));
    }
    out
}

/// 主题写的翻页字符要真的画到树上。
///
/// `arrow_char` 那条纯函数测试挡不住装配处把 `prev_txt`/`next_txt` 写成字面量，
/// 故这条从 `set_theme` 出发，一直断言到布局后的树上。
#[test]
fn theme_arrow_chars_reach_the_view_tree() {
    let mut w = win(false, false, false, false, "");
    let mut t = wind_theme::Resolved::default();
    t.views.footer_bar.prev_char = "$".into();
    t.views.footer_bar.next_char = ")".into();
    w.set_theme(t);
    let found = texts(&laid(&w, false));
    assert!(
        found.iter().any(|s| s == "$"),
        "上一页应画主题字符, 实得 {found:?}"
    );
    assert!(
        found.iter().any(|s| s == ")"),
        "下一页应画主题字符, 实得 {found:?}"
    );
    assert!(
        !found.iter().any(|s| s == "‹" || s == "›"),
        "内置箭头不该再出现, 实得 {found:?}"
    );
}

/// 主题给 footer_bar 配的禁用态文字色，要真的画到禁用那一侧的箭头上。
///
/// 判据取「首页时上一页与下一页不同色」这种相对量：绝对色值要跟着回退链（未配时回退全局
/// text_hint）走，写死就成了另一份副本。`footer_bar_disabled_state_survives_resolve` 钉
/// 「patch 建得出来」，这条钉「渲染真的读它」。
#[test]
fn theme_disabled_arrow_color_reaches_the_view_tree() {
    let leaf_colors = |ch_prev: &str, ch_next: &str, disabled: Option<[u8; 4]>| {
        let mut w = win(false, false, false, false, "");
        let mut t = wind_theme::Resolved::default();
        t.views.footer_bar.prev_char = ch_prev.into();
        t.views.footer_bar.next_char = ch_next.into();
        if let Some(c) = disabled {
            t.views.footer_bar.disabled = Some(Box::new(wind_theme::RvNode {
                text_color: Some(c),
                ..Default::default()
            }));
        }
        w.set_theme(t);
        let mut found = Vec::new();
        collect_colors(&laid(&w, false), &mut found);
        found
    };
    // 首页：上一页禁用、下一页可用 —— 两者取色分属禁用/启用两条路径。
    let red = [255, 0, 0, 255];
    let got = leaf_colors("<", ">", Some(red));
    let prev_c = got.iter().find(|(t, _)| t == "<").map(|(_, c)| *c);
    let next_c = got.iter().find(|(t, _)| t == ">").map(|(_, c)| *c);
    assert_eq!(
        prev_c,
        Some(red),
        "禁用侧应用主题配的禁用态色, 实得 {got:?}"
    );
    assert_ne!(next_c, Some(red), "可用侧不该被禁用色染上");

    // 未配禁用态：回退全局 text_hint，行为与从前一致（内置主题都没配）。
    let plain = leaf_colors("<", ">", None);
    let prev_plain = plain.iter().find(|(t, _)| t == "<").map(|(_, c)| *c);
    assert!(prev_plain.is_some() && prev_plain != Some(red));
}

/// 递归收集树上 (文本, 文字色)。
fn collect_colors(v: &View, out: &mut Vec<(String, [u8; 4])>) {
    if let Some(t) = &v.text {
        out.push((t.clone(), v.text_color));
    }
    for c in &v.children {
        collect_colors(c, out);
    }
}

/// 多字翻页符号要把按钮（连同命中区）撑宽，而不是盖到页码上或被裁掉。
///
/// 宽度基线 `arrow_w` 按一个字算，schema 却不限 prev_char 的长度。判据用相对量
/// （多字宽 > 单字宽）：三个平台的文本后端量出的绝对宽度各不相同。
#[test]
fn multi_char_arrow_widens_the_hit_box() {
    let hit_w = |ch: &str| {
        let mut w = win(false, false, false, false, "");
        let mut t = wind_theme::Resolved::default();
        t.views.footer_bar.next_char = ch.into();
        w.set_theme(t);
        hit(&laid(&w, false), TAG_PAGE_NEXT)
            .expect("下一页可点（首页 → 有下一页）")
            .w
    };
    let one = hit_w(")");
    let many = hit_w("下一页");
    assert!(
        many > one,
        "多字应撑宽按钮: 单字 {one}, 多字 {many}（fixed_w 下两者相等）"
    );
}

/// 多字箭头下，同一行的编码预算要跟着收窄 —— 否则超长编码把翻页栏顶出窗口右缘。
///
/// 按钮撑宽了、预算却按单字算，就是「箭头宽度公式写了两份」的后果，命中盒那条测试盖不住。
#[test]
fn multi_char_arrow_shrinks_the_preedit_budget() {
    let themed = |ch: &str, preedit: &str| {
        let mut w = win(true, false, true, true, preedit);
        let mut t = wind_theme::Resolved::default();
        t.views.footer_bar.next_char = ch.into();
        t.views.footer_bar.prev_char = ch.into();
        w.set_theme(t);
        w
    };
    // 前置：编码必须已被宽度上限夹住, 否则下面的不等式恒真（同组头注反复警告的写法）。
    let long = clamped_preedit(|p| themed("下一页", p));
    let w1 = laid(&themed("下一页", &long), false).measured_size().0;
    let w2 = laid(&themed("下一页", &long.repeat(2)), false)
        .measured_size()
        .0;
    assert_eq!(w1, w2, "前置：超长编码须已被上限夹住（{w1} vs {w2}）");

    let w = themed("下一页", &long);
    let cap = w.screen_safety_max_width_px() as f32;
    let root = laid(&w, false);
    let (win_w, _) = root.measured_size();
    // 判据必须是安全上限而不是「翻页栏落在 win_w 内」：预算算漏时 win_w 跟着涨，那个不等式恒真。
    // 真正坏掉的是渲染入口 `content_w.min(safety_max_w)` 的裁切，超出部分在右缘被切掉。
    assert!(
        win_w <= cap + 0.5,
        "多字箭头下整窗仍不得越过安全上限（{win_w} vs {cap}）—— 越界即预算按单字算漏了"
    );
    let p = pager(&root);
    assert!(
        p.x + p.w <= win_w + 0.5,
        "翻页栏须完整落在窗口内（右缘 {} vs 窗口宽 {win_w}）",
        p.x + p.w
    );
}

/// 未配字符时回退内置 ‹ › —— 与上一条互为反面, 省得「让主题字符生效」被实现成
/// 「无论如何都不画内置箭头」。
#[test]
fn builtin_arrows_remain_when_theme_says_nothing() {
    let w = win(false, false, false, false, "");
    let found = texts(&laid(&w, false));
    assert!(
        found.iter().any(|s| s == "‹"),
        "未配字符应回退内置, 实得 {found:?}"
    );
    assert!(
        found.iter().any(|s| s == "›"),
        "未配字符应回退内置, 实得 {found:?}"
    );
}

/// 取某个 tag 的命中矩形；`None` = 该 tag 未参与命中（如翻页箭头被禁用或整个翻页栏没渲染）。
fn hit(root: &View, tag: i32) -> Option<Rect> {
    let mut hits = Vec::new();
    root.collect_hits(&mut hits);
    hits.iter().find(|(t, _)| *t == tag).map(|(_, r)| *r)
}

fn pager(root: &View) -> Rect {
    hit(root, TAG_PAGE_NEXT).expect("翻页栏须存在（多页 + 默认主题下可见）")
}

/// 首条候选的矩形（tag = 候选下标）。
fn first_cand(root: &View) -> Rect {
    hit(root, 0).expect("首条候选须参与命中")
}

/// 造一段确实已被宽度上限夹住的超长编码：从 200 组 `a'` 起翻倍，直到再翻一倍也不再变宽，
/// 返回夹住的那一档。`mk` 必须与随后写前置断言用的是同一个构造器，否则夹住的是另一棵树。
///
/// 重复次数不能写死：「已被夹住」是绝对量，取决于字宽，而三个平台的文本后端各不相同
/// （Windows DirectWrite、macOS CoreText、Linux 等宽 mock），写死的次数会只在某个平台的 CI 上红。
///
/// `> floor` 那半条判据不能省：「加长一倍不再变宽」在短到没撑满窗口最小宽度时同样成立，
/// 少了它探测会在第一档就收敛，交回没被夹住的短编码，后面的上限不等式全部恒真。
/// floor 取空编码那档的树宽。
fn clamped_preedit(mk: impl Fn(&str) -> CandidateWindow) -> String {
    let floor = laid(&mk(""), false).measured_size().0;
    let mut n = 200usize;
    for _ in 0..6 {
        let one = "a'".repeat(n);
        let w1 = laid(&mk(&one), false).measured_size().0;
        let w2 = laid(&mk(&"a'".repeat(n * 2)), false).measured_size().0;
        if w1 == w2 && w1 > floor {
            return one;
        }
        n *= 2;
    }
    panic!("加到 {n} 组 `a'` 仍未被宽度上限夹住 —— 上限逻辑或文本测量变了，先查那边");
}

/// 核心判据：竖排下两种编码形态（内联 / 独立编码栏）对并入开关的表现必须一致。
/// 前置断言不可省：若独立编码栏那档本身就没省下这一行，后面两个相等就成了恒等式。
#[test]
fn inline_preedit_joins_pager_like_top_bar() {
    let top_off = height(&win(true, false, false, false, "nihao"));
    let top_on = height(&win(true, false, false, true, "nihao"));
    let inline_off = height(&win(true, false, true, false, "nihao"));
    let inline_on = height(&win(true, false, true, true, "nihao"));
    assert!(
        top_on < top_off,
        "前置：独立编码栏下开关须真的省掉一行（{top_on} vs {top_off}）"
    );
    assert_eq!(
        inline_off, top_off,
        "开关关闭时两种编码形态本就同高（都是底部独立行）"
    );
    assert_eq!(
        inline_on, top_on,
        "开关开启时内联编码须与独立编码栏省下同样的一行（{inline_on} vs {top_on}）"
    );
}

/// 并入后翻页栏须真的落在**编码那一行**里：整个矩形在首条候选之上。
///
/// 只测高度不够——把翻页栏挂到窗口任意别处也能让总高少一行，位置才是这条开关的本意。
#[test]
fn inlined_pager_sits_above_first_candidate() {
    let w = win(true, false, true, true, "nihao");
    let root = laid(&w, false);
    let p = pager(&root);
    let c0 = first_cand(&root);
    assert!(
        p.y + p.h <= c0.y + 0.5,
        "翻页栏须整体位于首条候选之上（翻页栏底 {} vs 候选顶 {}）",
        p.y + p.h,
        c0.y
    );
}

/// 开关关闭时翻页栏不得消失，仍是候选区下方的独立一行。
///
/// 并入分支若写成 `pager.take().filter(开关)`，关闭时翻页栏被取出后直接丢掉，末尾装配段
/// 再也拿不到它；开着时一切正常，最容易漏测。
#[test]
fn switch_off_keeps_pager_below_candidates() {
    let w = win(true, false, true, false, "nihao");
    let root = laid(&w, false);
    let p = pager(&root);
    let c0 = first_cand(&root);
    assert!(
        p.y > c0.y,
        "开关关闭时翻页栏须留在候选下方（翻页栏 y={} vs 首候选 y={}）",
        p.y,
        c0.y
    );
}

/// 横排不受影响：内联编码与候选本就同属一行，翻页栏本来就在该行末尾，开关无从谈起。
#[test]
fn horizontal_inline_is_untouched_by_switch() {
    let off = height(&win(false, false, true, false, "nihao"));
    let on = height(&win(false, false, true, true, "nihao"));
    assert_eq!(off, on, "横排内联编码下开关不得改变布局");
}

/// 旋转态（蒙文竖排）刻意排除在外：内联编码在旋转包裹层**内部**，翻页栏跟进去会连
/// 箭头带页码一起转 90°。那一档维持底部独立行（它挂在 root 上，屏幕上仍是横的一条）。
#[test]
fn rotated_inline_keeps_bottom_pager() {
    let off = height(&win(false, true, true, false, "nihao"));
    let on = height(&win(false, true, true, true, "nihao"));
    assert_eq!(off, on, "旋转态下不得并入（否则翻页箭头会跟着转）");
    // 前置：同一组参数在非旋转竖排下开关是**有效**的，否则上面的相等只是「哪儿都没生效」。
    assert!(
        height(&win(true, false, true, true, "nihao"))
            < height(&win(true, false, true, false, "nihao")),
        "前置：非旋转竖排下开关须真的省掉一行"
    );
}

/// 编码沉底（`swap_preedit_when_above`，窗口翻到光标上方）时，并入的翻页栏须**跟着编码一起**
/// 沉到底部 —— 它是编码那一行的子节点，本就该同进同退。
#[test]
fn inlined_pager_follows_preedit_to_bottom_on_swap() {
    let mut w = win(true, false, true, true, "nihao");
    w.set_swap_preedit_when_above(true);
    let root = laid(&w, true);
    let p = pager(&root);
    let c0 = first_cand(&root);
    assert!(
        p.y > c0.y,
        "沉底时翻页栏须随编码行落到候选之下（翻页栏 y={} vs 首候选 y={}）",
        p.y,
        c0.y
    );
}

/// 造一个**独立编码栏**（`candidate_top`）的窗口，可带模式徽标。
///
/// 与 `win()` 的区别只有 `embedded = false` 与 `mode_label` 可非空；单列助手是因为这两条
/// 测的是 band 那条落点，与内联那族无关。
fn bar_win(label: &str, on: bool, preedit: &str) -> CandidateWindow {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_orientation(true, false, false);
    w.set_preedit_embedded(false);
    w.set_pager_in_preedit(on);
    let items: Vec<CandidateItem> = ["一", "二", "三"].iter().map(|t| cand(t)).collect();
    w.update(preedit, preedit.len(), label, items, 0, -1, 1, 3);
    w
}

/// 独立编码栏必须撑满窗口内容宽，与主题编辑器预览一致。
///
/// 只有编码那么宽时，配了背景色的编码栏后半截露出窗口底（窗口透明就是透明的一段）。
/// 取无徽标、无并入翻页栏的那档——那两者在时 band 本来就撑满，测不到这条。
/// 默认主题没配 `separator`，即「第三方主题」那条路径。
#[test]
fn own_preedit_bar_spans_window_content_width() {
    let w = bar_win("", false, "a");
    let root = laid(&w, false);
    let band = &root.children[0];
    assert!(
        band.text.is_none() && !band.children.is_empty(),
        "前置：首个子节点应为编码栏"
    );
    let content_w = root.measured_size().0 - root.padding.l - root.padding.r;
    let band_w = band.laid_rect().w;
    assert!(
        band.children[0].measured_size().0 < content_w,
        "前置：编码本身须比窗口内容窄，否则恒真"
    );
    assert_eq!(
        band_w, content_w,
        "编码栏只有 {band_w} 宽，窗口内容宽 {content_w}"
    );
}

/// 编码栏下的分隔线：颜色 / 线粗 / 间距走 `views.divider`；`enabled=false` 不画。
#[test]
fn divider_node_controls_the_preedit_separator() {
    // 找到编码栏之后紧跟的那条细线（Row、定高、无文字无子节点、背景色 = sep）。
    let line_of = |root: &View, col: [u8; 4]| -> Option<(f32, f32)> {
        fn walk(v: &View, col: [u8; 4], out: &mut Option<(f32, f32)>) {
            if v.text.is_none() && v.children.is_empty() && v.bg == Some(col) {
                *out = Some((v.laid_rect().h, v.margin.b));
            }
            for c in &v.children {
                walk(c, col, out);
            }
        }
        let mut out = None;
        walk(root, col, &mut out);
        out
    };
    let col = [10, 20, 30, 255];
    let mut w = bar_win("", false, "a");
    let mut t = wind_theme::Resolved::default();
    t.views.divider.bg_color = Some(col);
    w.set_theme(t.clone());
    assert_eq!(
        line_of(&laid(&w, false), col),
        Some((1.0, 4.0)),
        "缺省 1px、下距 4"
    );

    t.views.divider_height = Some(wind_theme::schema::Dim::Dp(3.0));
    t.views.divider.margin.bottom = Some(wind_theme::schema::Dim::Dp(9.0));
    w.set_theme(t.clone());
    assert_eq!(
        line_of(&laid(&w, false), col),
        Some((3.0, 9.0)),
        "线粗与下距可配"
    );

    t.views.divider_enabled = Some(false);
    w.set_theme(t);
    assert_eq!(line_of(&laid(&w, false), col), None, "enabled=false 不画");
}

/// 独立编码栏那条落点同样要为并入的翻页栏留出宽度。
///
/// band 排的是 `[编码][spacer][徽标][翻页栏]`，`preedit_bar_text_budget_px` 须把后两个成员也扣掉。
/// 症状不是窗口探出屏幕：渲染入口 `content_w.min(safety_max_w)` 是裁切语义，超出部分在右缘
/// 被切掉且没有省略号。`width_budget_tests` 断的是钳制之后的窗口宽，抓不到；判据必须量
/// `laid()` 的树宽、与安全上限比。
#[test]
fn long_preedit_in_own_bar_reserves_room_for_inlined_pager() {
    let long = clamped_preedit(|p| bar_win("", true, p));
    let w = bar_win("", true, &long);
    let cap = w.screen_safety_max_width_px() as f32;
    // 前置①：这一行必须**已经被上限夹住**（再加长一倍不再变宽），否则不等式恒真。
    let root = laid(&w, false);
    let (win_w, _) = root.measured_size();
    assert_eq!(
        win_w,
        laid(&bar_win("", true, &long.repeat(2)), false)
            .measured_size()
            .0,
        "前置：超长编码须已被宽度上限夹住"
    );
    // 前置②：这一档确实并入了编码栏（翻页栏落在编码那一行，不是候选区底部）。
    let p = pager(&root);
    let c0 = first_cand(&root);
    assert!(
        p.y < c0.y,
        "前置：翻页栏须并在编码行（y={} 应在首候选 y={} 之上）",
        p.y,
        c0.y
    );

    assert!(
        win_w <= cap + 0.5,
        "整棵树不得越过安全上限（{win_w} vs {cap}）—— 越过的部分会被右缘裁掉"
    );
    assert!(
        p.x + p.w <= win_w + 0.5,
        "翻页栏须完整落在窗口内（右缘 {} vs 窗口宽 {win_w}）",
        p.x + p.w
    );
}

/// 另一半：模式徽标同样占着 band 的右端，同样得进预算。两者各自独立地会漏，
/// 只带徽标不开翻页栏也会越上限，故分开测。
#[test]
fn long_preedit_in_own_bar_reserves_room_for_mode_chip() {
    let long = clamped_preedit(|p| bar_win("临时英文", false, p));
    let w = bar_win("临时英文", false, &long);
    let cap = w.screen_safety_max_width_px() as f32;
    let root = laid(&w, false);
    let (win_w, _) = root.measured_size();
    assert_eq!(
        win_w,
        laid(&bar_win("临时英文", false, &long.repeat(2)), false)
            .measured_size()
            .0,
        "前置：超长编码须已被宽度上限夹住"
    );
    // 前置：对照组（无徽标）必须**本来就没越界**，否则这条量到的是别的账。
    let plain = laid(&bar_win("", false, &long), false).measured_size().0;
    assert!(plain <= cap + 0.5, "前置：无徽标时本就不该越界（{plain}）");
    assert!(
        win_w <= cap + 0.5,
        "带模式徽标时整棵树仍不得越过安全上限（{win_w} vs {cap}）"
    );
}

/// 超长编码不得把这一行撑出宽度上限：编码的文字预算要先扣掉翻页栏占的那一截
/// （并入后内联编码不再独占整行预算）。
///
/// 参照物必须是屏幕安全上限，不能拿 `measured_size()` 当窗口宽：树自底向上量，翻页栏
/// 必然落在自己撑出来的宽度里，那样写恒真。
#[test]
fn long_preedit_does_not_push_row_past_width_budget() {
    let long = clamped_preedit(|p| win(true, false, true, false, p));
    let w_on = win(true, false, true, true, &long);
    let cap = w_on.screen_safety_max_width_px() as f32;
    // 前置：这一行必须**已经被上限夹住**（再加长一倍宽度不变），否则下面的不等式恒真。
    // 判据取「加长不再变宽」而不是「宽度 == 上限」：截断按整字符走，量出来总比预算少小半个字。
    let off_w = laid(&win(true, false, true, false, &long), false)
        .measured_size()
        .0;
    let off_w2 = laid(&win(true, false, true, false, &long.repeat(2)), false)
        .measured_size()
        .0;
    assert_eq!(
        off_w, off_w2,
        "前置：超长编码须已被宽度上限夹住（{off_w} vs {off_w2}）"
    );
    assert!(
        off_w <= cap + 0.5,
        "前置自洽：夹住后不该超过上限（{off_w} vs {cap}）"
    );
    let root = laid(&w_on, false);
    let (win_w, _) = root.measured_size();
    assert!(
        win_w <= cap + 0.5,
        "并入翻页栏后整窗仍不得越过安全上限（{win_w} vs {cap}）"
    );
    let p = pager(&root);
    assert!(
        p.x + p.w <= win_w + 0.5,
        "翻页栏须完整落在窗口内（右缘 {} vs 窗口宽 {win_w}）",
        p.x + p.w
    );
}
