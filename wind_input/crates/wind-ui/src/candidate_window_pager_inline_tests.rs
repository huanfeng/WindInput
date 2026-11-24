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
/// `rotated`（蒙文竖排）与 `vertical` 互斥、不能同时为真（见 `set_orientation` 的断言），
/// 故这里传 `rotated=true` 时 `vertical` 一律置 false —— 两者在布局里殊途同归，
/// 都让 `list_vertical` 成立。
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
/// `arrow_char` 那条纯函数测试挡不住这个 —— 把装配处的 `prev_txt`/`next_txt` 改回字面量
/// `"‹"`/`"›"`，它照样全绿。而「字段一路解析得好好的、渲染层不消费」正是本次修的 bug
/// 本体，测试若也绕开消费点，等于把同一个坑再踩一遍。故这条从 `set_theme` 出发，
/// 一直断言到布局后的树上。
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
/// 判据取「首页时上一页与下一页**不同色**」这种相对量：绝对色值要跟着回退链（未配时
/// 回退全局 text_hint）走, 写死就成了另一份需要同步的副本。resolve 那侧另有一条
/// footer_bar_disabled_state_survives_resolve 钉住「patch 建得出来」, 这条钉住「渲染
/// 真的读它」—— 两段都断过一次。
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
/// 宽度基线 `arrow_w` 是按一个字算的，schema 却不限 prev_char 的长度。判据用相对量
/// （多字宽 > 单字宽），与本组头注要求一致 —— 三个平台的文本后端量出的绝对宽度各不相同。
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
/// 命中盒那条测试盖不住这个：按钮撑宽了、预算却按单字算，两者不一致恰恰是「箭头宽度
/// 公式写了两份」的后果。判据沿用同组既有那条（翻页栏右缘须落在窗口内），因为那正是
/// 预算算漏时真实会坏掉的地方；`pager_row_w` 的注释也明写着这条因果。
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
    // 判据必须是安全上限而不是「翻页栏落在 win_w 内」：预算算漏时整行照样排得下,
    // 只是 win_w 自己跟着涨 —— 那个不等式恒真, 测了等于没测。真正坏掉的地方是渲染入口
    // 那道 content_w.min(safety_max_w), 它是**裁切**语义, 超出部分在右缘直接被切掉。
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

/// 造一段**确实已被宽度上限夹住**的超长编码：从 200 组 `a'` 起翻倍，直到「再翻一倍
/// 也不再变宽」为止，返回夹住的那一档。`mk` 是被测那一档的构造器 —— 必须与随后写
/// 前置断言时用的是同一个，否则夹住的是另一棵树。
///
/// ⚠️ **重复次数不能写死。** 本组头注说「断言一律是相对量，与文本后端量出多宽无关」，
///    可「超长编码须已被宽度上限夹住」这条**前置**恰恰是绝对量 —— 它成不成立取决于
///    一个字有多宽，而三个平台的后端各不相同（Windows: DirectWrite；macOS: CoreText；
///    Linux: 等宽 mock，见 `text/dwrite.rs` 末尾）。实测同样 200 组 `a'`：Windows 下
///    已顶到 3000 的上限，macOS 下只有 2713.45，于是这三条**只在 macOS CI 上红**，
///    Windows 与 Linux(mock) 全绿 —— 本机怎么跑都发现不了。
///    判据要跟着字宽走，而不是把次数往上调一档、等下一个后端再把它顶穿。
///
/// ★ `> floor` 那半条判据不能省。「加长一倍不再变宽」在**两端**都成立：
///   一端是这里要的「顶到宽度上限」，另一端是「短得连窗口最小宽度都没撑满」——
///   实测 mock 后端下 2 组 `a'` 与 4 组同为 116.4，正是后者。少了这半条，探测会在
///   第一档就「收敛」并交回一段根本没被夹住的短编码，于是三条测试里那些
///   「不得越过上限」的不等式**全部恒真** —— 绿得毫无意义，正是本组头注反复警告的
///   那种写法。用空编码那档的树宽当 floor，两个平台期差着两个数量级，分得很开。
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

/// ★ 本次修复的核心判据：竖排下**两种编码形态的表现必须一致**。
///
/// 修复前内联编码那档完全不理会开关（翻页栏照旧独占底部一行），用户看到的就是
/// 「同一个开关，换个编码显示方式就失灵」。
///
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

/// ★ 开关关闭时翻页栏**不得消失**，仍是候选区下方的独立一行。
///
/// 这条钉的是实现里的一个真实陷阱：并入分支若写成 `pager.take().filter(开关)`，
/// 开关关闭时翻页栏会被取出来直接丢掉，末尾装配段再也拿不到它 —— 症状是「关掉开关翻页栏
/// 就没了」，而开着的时候一切正常，最容易漏测的那一半。
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

/// ⚠️ 旋转态（蒙文竖排）刻意排除在外：内联编码在旋转包裹层**内部**，翻页栏跟进去会连
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
/// 与 `win()` 的区别只有两条：`embedded = false`，以及 `mode_label` 可非空。
/// 单列一个助手而不给 `win()` 再加参数——这两条测的是 band 那条落点，与内联那族无关。
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

/// ★ F-17：**独立**编码栏那条落点同样要为并入的翻页栏留出宽度。
///
/// 这是 55032bc0 给内联那条补的同一笔账的另一半，当时漏了。band 排的是
/// `[编码][spacer][徽标][翻页栏]`，而 `preedit_bar_text_budget_px` 只扣了窗口内边距与
/// 自身内边距，后两个成员一个都没进预算。
///
/// ⚠️ 症状**不是**「窗口探出屏幕」：渲染入口那道 `content_w.min(safety_max_w)` 是**裁切**
/// 语义、不回头改 `root`，所以窗口出不去，超出的部分在右缘直接被切掉且没有省略号——
/// 翻页栏看不见或只剩半截箭头。也正因如此 `width_budget_tests` 那组抓不到它（它断的是
/// 钳制**之后**的窗口宽，恒满足）。判据必须量 `laid()` 的**树宽**、与安全上限比。
///
/// 实测（scale=1，200 组 `a'`，上限 3000）：修复前树宽 3091.6，修复后 2994.4。
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

/// ★ F-17 的另一半：模式徽标同样占着 band 的右端，同样得进预算。
///
/// 与上一条分开是因为两者**各自独立**地漏掉了——实测（同参数）只带徽标不开翻页栏时
/// 树宽 3046.4，仍越上限 46.4；只有两项都扣掉，四种组合才全部落回上限内。
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

/// ★ 超长编码不得把这一行撑出宽度上限：编码的文字预算要先扣掉翻页栏占的那一截。
///
/// 竖排下内联编码原本独占整行预算（与候选互不竞争），并入之后这个前提不再成立。
///
/// ⚠️ 参照物必须是**屏幕安全上限**，不能拿 `measured_size()` 当窗口宽去比翻页栏右缘 ——
/// 树是自底向上量的，翻页栏当然落在自己撑出来的那个宽度里，那样写恒真（本条最初就是
/// 这么写的，变异检验里「预算不扣翻页栏」一改，七条测试全绿，才发现它什么都没测）。
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
