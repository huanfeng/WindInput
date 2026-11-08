//! 渲染 golden 对拍：出厂主题 × 出厂注释 / 气泡样例 × 常态 / 选中 / 悬停 × 四种排布。
//!
//! # 守的是什么
//!
//! 分段着色（`docs/design/text-span-colors.md` §13.2）要求「出厂外观逐字节不变」。
//! 这件事单靠「新代码的某条断言」证明不了——新代码录的参照只能证明「和自己一样」。
//! 所以参照必须在**动渲染层之前**录下、入库，之后每一步改动都拿它对拍。
//!
//! 每个用例录三样东西，Linux mock 后端下全部确定：
//! - **View 树转储**（[`View::debug_dump`]）：布局、颜色、字号、字重、裁剪、旋转逐字段；
//! - **绘制调用日志**：mock `TextRenderer` 记下的每一次文字绘制（文本、坐标、样式、颜色）；
//! - **像素摘要**：mock 不出字形，但背景 / 边框 / 圆角 / 阴影是 tiny-skia 真光栅化，
//!   对整块缓冲取 FNV-1a 摘要（不用 `DefaultHasher`：它的算法不作承诺，换工具链会假红）。
//!
//! # 更新参照
//!
//! 只在**有意**改变出厂外观时才重录：`WIND_UI_BLESS_GOLDEN=1 cargo test -p wind-ui --lib
//! render_golden`，然后在提交里说明为什么变。改渲染层时 golden 红了，默认是回归。
//!
//! 重录记录：2026-09-28 `_base` 配了气泡的角色色（设计 §18），出厂外观有意改变。重录的差异
//! **只有**气泡文字多出颜色区间（`runs=` / `draw_runs`），布局、字号、像素摘要逐字节不变；
//! 候选窗部分（含注释）与分段着色之前逐字节相同——注释出厂不上色。
//!
//! 重录记录：2026-10-12 `ViewImage` 新增 `scale` 字段（SVG center/tile 单元 = 文档尺寸 × DPI），
//! 差异**只有**转储里翻页箭头图多出 `scale: 1.0`，像素摘要与其余各行逐字节不变。

use super::*;
use std::path::PathBuf;
use std::sync::Arc;
use wind_ui_types::{SpanStyle, StyledText, TooltipDoc, TooltipLine, TooltipSection};

/// 出厂主题：`data/themes` 下的全部主题（含两个抽象 base：它们同样可加载，且是其余主题的
/// 公共祖先）。按目录枚举而非硬编码，新加的出厂主题缺 golden 文件时对拍直接报出来。
fn themes() -> Vec<String> {
    let ids = wind_theme::list_theme_ids(&themes_dir());
    for must in [
        "_base",
        "_qingfeng",
        "default",
        "amber",
        "jade",
        "violet",
        "msime",
    ] {
        assert!(
            ids.iter().any(|i| i == must),
            "出厂主题枚举漏了 {must}：{ids:?}"
        );
    }
    ids
}

/// 排布：`(名字, vertical, rotated, upright, 内联编码)`。前三位的合法组合只有四个（见
/// `set_orientation`）；内联编码另走 `preedit_view` 的 build 回调，直立态下它也逐格扶正。
const LAYOUTS: &[(&str, bool, bool, bool, bool)] = &[
    ("横排", false, false, false, false),
    ("竖排", true, false, false, false),
    ("旋转", false, true, false, false),
    ("直立", false, true, true, false),
    ("横排·内联编码", false, false, false, true),
    ("直立·内联编码", false, true, true, true),
];

fn themes_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes")
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/render_golden")
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// 一段带角色的文字（协调器模板引擎产出的片段形态）。
fn piece(out: &mut StyledText, text: &str, role: &'static str, in_title: bool) {
    out.push(
        text,
        &SpanStyle {
            role: Some(role),
            in_title,
            ..Default::default()
        },
    );
}

fn role_text(text: &str, role: &'static str) -> StyledText {
    let mut t = StyledText::new();
    piece(&mut t, text, role, false);
    t
}

/// 出厂注释模板 `${code_hint|code_rev|shuangpin}` 渲染出的样子：短编码、空注释、多字词编码。
///
/// ★ 带着协调器真实产出的**片段角色**（P2 起模板引擎恒产出片段）：出厂主题不给注释配角色
/// （§18），这些片段必须全部解析到正文色而被丢弃，候选窗部分才与分段着色之前逐字节相同（§6.4）。
fn candidates() -> Vec<CandidateItem> {
    let c = |text: &str, comment: StyledText| CandidateItem {
        text_styled: Default::default(),
        text: text.to_string(),
        code: String::new(),
        label: String::new(),
        tooltip: Default::default(),
        comment,
        comment_above: Default::default(),
        no_index: false,
    };
    // 下标 0 选中、1 悬停、2/3 常态（一条无注释、一条有）。
    vec![
        c("好", role_text("vb", "code_hint")),
        c("号", role_text("kg", "code_rev")),
        c("浩", StyledText::new()),
        c("你好", role_text("wqvb", "shuangpin")),
    ]
}

/// 出厂气泡段（完整原文关、编码、拼音逐字）渲染出的样子，带协调器产出的片段角色
/// （段名字面 title、段名里的变量 in_title、逐字行 char / literal / readings）。
fn tooltip_doc() -> TooltipDoc {
    let per_char = |ch: &str, readings: &str, raw| {
        let mut t = StyledText::new();
        piece(&mut t, ch, "char", false);
        piece(&mut t, "：", "literal", false);
        piece(&mut t, readings, "readings", false);
        TooltipLine { text: t, raw }
    };
    let mut code_title = StyledText::new();
    piece(&mut code_title, "编码(", "title", true);
    piece(&mut code_title, "五笔", "code_source", true);
    piece(&mut code_title, ")", "title", true);
    let mut pinyin_title = StyledText::new();
    piece(&mut pinyin_title, "拼音", "title", true);
    TooltipDoc {
        sections: vec![
            TooltipSection {
                title: Some(code_title),
                inline: false,
                lines: vec![TooltipLine {
                    text: role_text("wqvb", "word_code"),
                    raw: 0,
                }],
            },
            TooltipSection {
                title: Some(pinyin_title),
                inline: false,
                lines: vec![per_char("你", "nǐ", 0), per_char("好", "hǎo/hào", 1)],
            },
        ],
    }
}

/// 候选窗在给定排布下的用例（选中 0、悬停 1），逐排布拼成文本。
fn render_windows(
    theme: &wind_theme::Resolved,
    layouts: &[(&str, bool, bool, bool, bool)],
    cands: fn() -> Vec<CandidateItem>,
) -> String {
    render_windows_at(theme, layouts, cands, 1.0)
}

/// 同 [`render_windows`]，按给定 DPI 缩放。
fn render_windows_at(
    theme: &wind_theme::Resolved,
    layouts: &[(&str, bool, bool, bool, bool)],
    cands: fn() -> Vec<CandidateItem>,
    scale: f32,
) -> String {
    let mut out = String::new();
    for &(label, vertical, rotated, upright, embedded) in layouts {
        let mut w = laid_out_window(theme, scale, (vertical, rotated, upright, embedded), cands);
        out.push_str(&tree_case(&mut w, label, false));
    }
    out
}

/// 一扇按排布与候选配好的窗（选中 0、悬停 1：一帧里三态俱全；模式徽标给一个，直立态下它也逐格扶正）。
fn laid_out_window(
    theme: &wind_theme::Resolved,
    scale: f32,
    (vertical, rotated, upright, embedded): (bool, bool, bool, bool),
    cands: fn() -> Vec<CandidateItem>,
) -> CandidateWindow {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = scale;
    w.set_theme(theme.clone());
    w.set_orientation(vertical, rotated, upright);
    w.set_preedit_embedded(embedded);
    w.update("nihao", 3, "拼", cands(), 0, 1, 1, 2);
    w
}

/// `build_tree(above)` 在原点排版、画进刚好装下的缓冲：View 树转储 + 像素摘要 + 绘制日志。
fn tree_case(w: &mut CandidateWindow, label: &str, above: bool) -> String {
    let mut out = String::new();
    let mut root = w.scene.build_tree(&w.scene_input(above));
    root.layout(0.0, 0.0, &w.text_renderer);
    let (mw, mh) = root.measured_size();
    let (bw, bh) = (mw.ceil().max(1.0) as u32, mh.ceil().max(1.0) as u32);
    let mut buf = vec![0u8; (bw * bh * 4) as usize];
    let _ = w.text_renderer.take_draw_log();
    root.paint(&mut buf, bw, bh, &w.text_renderer);
    out.push_str(&format!("== 候选窗 {label} ==\n"));
    out.push_str(&root.debug_dump());
    out.push_str(&format!(
        "-- 绘制 {bw}x{bh} 像素摘要 {:016x}\n",
        fnv1a(&buf)
    ));
    for l in w.text_renderer.take_draw_log() {
        out.push_str(&l);
        out.push('\n');
    }
    out
}

/// 一个主题（亮或暗）的全部用例，拼成一份文本。
fn render_theme(name: &str, dark: bool) -> String {
    let theme = wind_theme::load_resolved(&themes_dir(), name, dark)
        .unwrap_or_else(|e| panic!("加载出厂主题 {name}（dark={dark}）失败：{e}"));
    let mut out = render_windows(&theme, LAYOUTS, candidates);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut tip = crate::tooltip::Tooltip::new(tx).unwrap();
    tip.set_theme(&theme);
    let (buf, w, h, log) = tip.golden_frame(&Arc::new(tooltip_doc()));
    out.push_str(&format!(
        "== 气泡 ==\n-- 绘制 {w}x{h} 像素摘要 {:016x}\n",
        fnv1a(&buf)
    ));
    for l in log {
        out.push_str(&l);
        out.push('\n');
    }
    // 主题资源的绝对路径随检出位置变，换成占位符。
    let dir = themes_dir().to_string_lossy().into_owned();
    out.replace(&dir, "<themes>")
}

#[test]
fn factory_render_matches_golden() {
    let bless = std::env::var_os("WIND_UI_BLESS_GOLDEN").is_some();
    let dir = golden_dir();
    let mut failed = Vec::new();
    for name in &themes() {
        for dark in [false, true] {
            let got = render_theme(name, dark);
            let file = dir.join(format!(
                "{name}-{}.txt",
                if dark { "dark" } else { "light" }
            ));
            if bless {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&file, &got).unwrap();
                continue;
            }
            let want = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("读 golden {} 失败：{e}", file.display()));
            if got != want {
                // 首个不同行，省得在上千行里肉眼找。
                let line = got
                    .lines()
                    .zip(want.lines())
                    .position(|(a, b)| a != b)
                    .unwrap_or(got.lines().count().min(want.lines().count()));
                failed.push(format!(
                    "{}：第 {} 行起不同\n  实得 {:?}\n  参照 {:?}",
                    file.display(),
                    line + 1,
                    got.lines().nth(line),
                    want.lines().nth(line),
                ));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "出厂渲染与 golden 不一致（有意改外观才用 WIND_UI_BLESS_GOLDEN=1 重录）：\n{}",
        failed.join("\n")
    );
}

/// 守 golden 本身不是空转：候选文字、注释、气泡文字都必须出现在绘制日志里，
/// 且注释确实有非空的那几条——否则上面的对拍测的是一个什么都没画的世界。
#[test]
fn golden_covers_comments_and_tooltip() {
    let got = render_theme("default", false);
    for needle in [
        "text=\"好\"",
        "text=\"vb\"",
        "text=\"kg\"",
        "text=\"wqvb\"",
        "text=\"[编码(五笔)]\\nwqvb\\n[拼音]\\n你：nǐ\\n好：hǎo/hào\"",
    ] {
        assert!(got.contains(needle), "golden 里找不到 {needle}");
    }
    // 候选窗（含注释）没有任何调用走 runs：注释出厂不上色，片段全部解析到正文色、被丢弃。
    let window = got.split("== 气泡 ==").next().unwrap();
    assert!(!window.contains("draw_runs"), "候选窗不应出现 draw_runs");
    assert!(!window.contains(" runs="), "候选窗 View 树不应带颜色区间");
    // `_base` 的气泡角色色（§18）：编码是 `tooltip_success`。
    assert!(
        got.contains("rgba: [129, 201, 149, 255]"),
        "气泡编码应按 _base 的 word_code 角色画成 tooltip_success"
    );
    // 防空转：输入确实带着片段。
    assert!(!candidates()[0].comment.spans().is_empty());
    assert!(!tooltip_doc().to_styled().spans().is_empty());
    // 直立态注释逐格切：`wqvb` 在直立段里是四个单字母叶子。
    let upright = got.split("== 候选窗 直立 ==").nth(1).unwrap();
    assert!(upright.contains("text=\"w\""), "直立态注释应逐格切开");
}

/// 上方注释条（设计 candidate-comment-above-line.md §3.3）：出厂 `default` 主题下横排 / 竖排
/// 各一帧（选中 0、悬停 1 ⇒ 常态 / 选中 / 悬停俱全；下标 2「浩」没有上段 ⇒ 占位叶子），外加直立态
/// 一帧钉「不支持上方条、上段合回右侧注释」。首录于上方条落地时（此前无此功能，参照即新代码
/// 产物，守的是之后的回归）。
fn above_candidates() -> Vec<CandidateItem> {
    let c = |text: &str, above: StyledText, comment: StyledText| CandidateItem {
        text_styled: Default::default(),
        text: text.to_string(),
        code: String::new(),
        label: String::new(),
        tooltip: Default::default(),
        comment,
        comment_above: above,
        no_index: false,
    };
    vec![
        c(
            "好",
            role_text("hǎo", "pinyin"),
            role_text("vb", "code_hint"),
        ),
        c(
            "号",
            role_text("hào", "pinyin"),
            role_text("kg", "code_rev"),
        ),
        c("浩", StyledText::new(), StyledText::new()),
        c(
            "你好",
            role_text("nǐ hǎo", "pinyin"),
            role_text("wqvb", "shuangpin"),
        ),
    ]
}

fn render_above() -> String {
    let theme = wind_theme::load_resolved(&themes_dir(), "default", false).unwrap();
    let layouts: &[(&str, bool, bool, bool, bool)] = &[
        ("横排", false, false, false, false),
        ("竖排", true, false, false, false),
        ("直立", false, true, true, false),
    ];
    let dir = themes_dir().to_string_lossy().into_owned();
    render_windows(&theme, layouts, above_candidates).replace(&dir, "<themes>")
}

#[test]
fn comment_above_render_matches_golden() {
    let got = render_above();
    let file = golden_dir().join("comment_above-default-light.txt");
    if std::env::var_os("WIND_UI_BLESS_GOLDEN").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("读 golden {} 失败：{e}", file.display()));
    if got != want {
        let line = got
            .lines()
            .zip(want.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(got.lines().count().min(want.lines().count()));
        panic!(
            "上方注释条渲染与 golden 不一致（第 {} 行起）\n  实得 {:?}\n  参照 {:?}",
            line + 1,
            got.lines().nth(line),
            want.lines().nth(line)
        );
    }
}

/// 守上面那份 golden 不是空转：横 / 竖排真有 Column 与上段文字、第 3 条是透明占位；
/// 直立态没有 Column、上段逐格切进了右侧注释。
#[test]
fn comment_above_golden_covers_the_feature() {
    let got = render_above();
    let upright = got.split("== 候选窗 直立 ==").nth(1).unwrap();
    for label in ["横排", "竖排"] {
        let sec = got
            .split(&format!("== 候选窗 {label} =="))
            .nth(1)
            .unwrap()
            .split("== 候选窗")
            .next()
            .unwrap();
        assert!(
            (0..4).all(|i| column_item(sec, i)),
            "{label} 的 4 个 item 都应是 Column"
        );
        for needle in [
            "text=\"hǎo\"",
            "text=\"nǐ hǎo\"",
            "text=Some(\" \") color=[0, 0, 0, 0]",
        ] {
            assert!(sec.contains(needle), "{label} 缺 {needle}");
        }
    }
    assert!(
        !(0..4).any(|i| column_item(upright, i)),
        "直立态 item 不应是 Column"
    );
    assert!(!upright.contains("text=\"hǎo\""), "直立态不应有整段上方条");
    assert!(upright.contains("text=\"ǎ\""), "直立态上段应逐格合进注释");
}

/// 转储里 tag 为 `i` 的候选 item 是不是 Column 容器。
fn column_item(dump: &str, i: usize) -> bool {
    let tag = format!(" tag={i} ");
    dump.lines()
        .any(|l| l.trim_start().starts_with("Column") && l.contains(&tag))
}

// ══ 模糊音提示（设计 fuzzy-pinyin-hint.md）══
//
// 候选正文带分段（`text_styled`）：「苹(ping)果」里 `(ping)` 段角色 `pinyin_hint`，按提示色画。
// 首录于提示落地时（此前无此功能，参照即新代码产物，守的是之后的回归）。

/// 协调器产出的提示正文：`pre` + `(hint)`（角色 `pinyin_hint`）+ `post`。
fn hint_styled(pre: &str, hint: &str, post: &str) -> StyledText {
    let mut t = StyledText::from(pre);
    piece(&mut t, &format!("({hint})"), "pinyin_hint", false);
    t.push(post, &SpanStyle::default());
    t
}

fn hint_cand(text: &str, styled: StyledText) -> CandidateItem {
    CandidateItem {
        text_styled: styled,
        text: text.to_string(),
        code: String::new(),
        label: String::new(),
        tooltip: Default::default(),
        comment: Default::default(),
        comment_above: Default::default(),
        no_index: false,
    }
}

/// 下标 0 选中、1 悬停、2 无提示、3 常态带提示（带注释，提示色与注释色同框可比）。
fn fuzzy_candidates() -> Vec<CandidateItem> {
    vec![
        hint_cand("苹果", hint_styled("苹", "ping", "果")),
        hint_cand("瓶子", hint_styled("瓶", "ping", "子")),
        hint_cand("平安", StyledText::new()),
        CandidateItem {
            comment: role_text("pd", "code_hint"),
            ..hint_cand("频道", hint_styled("频", "pin", "道"))
        },
    ]
}

/// 横排 / 竖排 / 旋转 / 直立四种排布 + 展开网格（2 列 × 2 行，选中 0、悬停 1）。
fn render_fuzzy() -> String {
    let theme = wind_theme::load_resolved(&themes_dir(), "default", false).unwrap();
    let mut out = render_windows(&theme, &LAYOUTS[..4], fuzzy_candidates);
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_theme(theme.clone());
    w.set_orientation(false, false, false);
    w.set_grid(Some(wind_ui_types::CandidateGrid {
        cols: 2,
        rows: 2,
        highlight_row: 0,
        index_start: 0,
        index_count: 2,
        tabs: 0,
        active_tab: 0,
    }));
    w.update("ping", 4, "", fuzzy_candidates(), 0, 1, 1, 1);
    let mut root = w.scene.build_tree(&w.scene_input(false));
    root.layout(0.0, 0.0, &w.text_renderer);
    out.push_str("== 候选窗 网格 ==\n");
    out.push_str(&root.debug_dump());
    let dir = themes_dir().to_string_lossy().into_owned();
    out.replace(&dir, "<themes>")
}

/// 渲染段：`== 候选窗 {label} ==` 到下一段之间。
fn section<'a>(dump: &'a str, label: &str) -> &'a str {
    dump.split(&format!("== 候选窗 {label} =="))
        .nth(1)
        .unwrap_or_else(|| panic!("缺 {label} 段"))
        .split("== 候选窗")
        .next()
        .unwrap()
}

/// `_base` 给 `pinyin_hint` 的亮色档 `#C2410C`。
const HINT_LIGHT: [u8; 4] = [0xC2, 0x41, 0x0C, 255];

#[test]
fn golden_pinyin_hint_matches_golden() {
    let got = render_fuzzy();
    let file = golden_dir().join("pinyin_hint-default-light.txt");
    if std::env::var_os("WIND_UI_BLESS_GOLDEN").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("读 golden {} 失败：{e}", file.display()));
    if got != want {
        let line = got
            .lines()
            .zip(want.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(got.lines().count().min(want.lines().count()));
        panic!(
            "模糊音提示渲染与 golden 不一致（第 {} 行起）\n  实得 {:?}\n  参照 {:?}",
            line + 1,
            got.lines().nth(line),
            want.lines().nth(line)
        );
    }
}

/// 常态带提示的那条（下标 3「频(pin)道」）：整段文字一个叶子，`(pin)` 字节区间 [3, 8) 着提示色。
fn assert_plain_hint_run(sec: &str, label: &str) {
    let line = sec
        .lines()
        .find(|l| l.contains("text=Some(\"频(pin)道\")"))
        .unwrap_or_else(|| panic!("{label}：正文应完整显示「频(pin)道」"));
    assert!(
        line.contains(&format!(
            "runs=[ColorRun {{ start: 3, end: 8, rgba: {HINT_LIGHT:?} }}]"
        )),
        "{label}：(pin) 段应着提示色：{line}"
    );
}

#[test]
fn golden_pinyin_hint_horizontal() {
    let got = render_fuzzy();
    let sec = section(&got, "横排");
    assert_plain_hint_run(sec, "横排");
    // 无提示的候选不带颜色区间。
    let plain = sec
        .lines()
        .find(|l| l.contains("text=Some(\"平安\")"))
        .expect("平安");
    assert!(
        !plain.contains("runs="),
        "无提示候选不应带颜色区间：{plain}"
    );
}

#[test]
fn golden_pinyin_hint_vertical() {
    let got = render_fuzzy();
    assert_plain_hint_run(section(&got, "竖排"), "竖排");
    assert_plain_hint_run(section(&got, "旋转"), "旋转");
    // 直立态逐格扶正：`(`、`p`、`i`、`n`、`)` 各一格，每格整格着提示色；`频` / `道` 不着色。
    let up = section(&got, "直立");
    for cell in ["(", "p", "n", ")"] {
        let line = up
            .lines()
            .find(|l| l.contains(&format!("text=Some(\"{cell}\")")))
            .unwrap_or_else(|| panic!("直立：缺格 {cell}"));
        assert!(
            line.contains(&format!("rgba: {HINT_LIGHT:?}")),
            "直立：格 {cell} 应着提示色：{line}"
        );
    }
    for cell in ["频", "道"] {
        let line = up
            .lines()
            .find(|l| l.contains(&format!("text=Some(\"{cell}\")")))
            .unwrap_or_else(|| panic!("直立：缺格 {cell}"));
        assert!(!line.contains("runs="), "直立：格 {cell} 不应着色：{line}");
    }
}

#[test]
fn golden_pinyin_hint_highlighted() {
    let theme = wind_theme::load_resolved(&themes_dir(), "default", false).unwrap();
    let v = &theme.views;
    let got = render_fuzzy();
    let sec = section(&got, "横排");
    for (text, range, state) in [
        ("苹(ping)果", (3, 9), wind_theme::TextState::Selected),
        ("瓶(ping)子", (3, 9), wind_theme::TextState::Hover),
    ] {
        let want = wind_theme::pinyin_hint_color(
            &v.text,
            &v.comment,
            state,
            [30, 30, 30, 255],
            wind_theme::fallback::COMMENT_TEXT,
            crate::span_runs::candidate_backdrop(v, state),
        );
        let body = wind_theme::body_color(&v.text, state, [30, 30, 30, 255]);
        let line = sec
            .lines()
            .find(|l| l.contains(&format!("text=Some(\"{text}\")")))
            .unwrap_or_else(|| panic!("缺 {text}"));
        if want == body {
            assert!(
                !line.contains("runs="),
                "{state:?}：提示色等于正文色时不建区间：{line}"
            );
        } else {
            assert!(
                line.contains(&format!(
                    "runs=[ColorRun {{ start: {}, end: {}, rgba: {want:?} }}]",
                    range.0, range.1
                )),
                "{state:?}：提示段应按该状态取色 {want:?}：{line}"
            );
        }
    }
    // 防空转：出厂 default 的选中态改了正文色，但选中底是浅色、常态橙对它够醒目
    // ⇒ 沿用常态橙（不回落注释色）。
    let sel = wind_theme::TextState::Selected;
    let sel_bg = crate::span_runs::candidate_backdrop(v, sel).expect("default 选中底可取色");
    assert_ne!(
        wind_theme::body_color(&v.text, sel, [30, 30, 30, 255]),
        wind_theme::body_color(&v.text, wind_theme::TextState::Normal, [30, 30, 30, 255]),
        "前提：default 选中态改了正文色"
    );
    let hint = |v: &wind_theme::RvViews| {
        wind_theme::pinyin_hint_color(
            &v.text,
            &v.comment,
            sel,
            [30, 30, 30, 255],
            wind_theme::fallback::COMMENT_TEXT,
            crate::span_runs::candidate_backdrop(v, sel),
        )
    };
    assert_eq!(hint(v), HINT_LIGHT, "浅色选中底 {sel_bg:?} 上应沿用常态橙");
    // 反向：把选中底换成深色实心 accent ⇒ 对比度不足，回落选中态注释色。
    let mut dark = v.clone();
    dark.item
        .selected
        .get_or_insert_with(Default::default)
        .bg_color = Some([37, 99, 235, 255]);
    let cmt_sel = wind_theme::body_color(&dark.comment, sel, wind_theme::fallback::COMMENT_TEXT);
    assert_eq!(hint(&dark), cmt_sel, "深色选中底上应回落选中态注释色");
}

/// 展开网格：格宽按含提示的实宽算，同一行的格子互不重叠、各行同列对齐。
#[test]
fn golden_pinyin_hint_grid() {
    let got = render_fuzzy();
    let sec = section(&got, "网格");
    assert!(
        sec.contains("text=Some(\"苹(ping)果\")"),
        "网格：提示应完整显示"
    );
    assert_plain_hint_run(sec, "网格");

    let theme = wind_theme::load_resolved(&themes_dir(), "default", false).unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
    w.scale = 1.0;
    w.set_theme(theme);
    w.set_orientation(false, false, false);
    w.set_grid(Some(wind_ui_types::CandidateGrid {
        cols: 2,
        rows: 2,
        highlight_row: 0,
        index_start: 0,
        index_count: 2,
        tabs: 0,
        active_tab: 0,
    }));
    // 第 1 列上下两格：一格带长提示、一格不带 ⇒ 列宽按带提示的那格撑开。
    let cands = vec![
        hint_cand("平安", StyledText::new()),
        hint_cand("苹果", hint_styled("苹", "ping", "果")),
        hint_cand("拼音", StyledText::new()),
        hint_cand("品", StyledText::new()),
    ];
    w.update("ping", 4, "", cands, 0, -1, 1, 1);
    let mut root = w.scene.build_tree(&w.scene_input(false));
    root.layout(0.0, 0.0, &w.text_renderer);
    let mut hits = Vec::new();
    root.collect_hits(&mut hits);
    let rect = |t: i32| hits.iter().find(|(g, _)| *g == t).unwrap().1;
    for row in [0, 2] {
        let (a, b) = (rect(row), rect(row + 1));
        assert!(
            a.x + a.w <= b.x + 0.01,
            "第 {row} 行两格重叠：{a:?} / {b:?}"
        );
    }
    assert!(
        (rect(1).x - rect(3).x).abs() < 0.01 && (rect(1).w - rect(3).w).abs() < 0.01,
        "第 2 列上下格应对齐同宽：{:?} / {:?}",
        rect(1),
        rect(3)
    );
    // 带提示那格里正文叶子完整显示且落在格内。
    let dump = root.debug_dump();
    assert!(dump.contains("text=Some(\"苹(ping)果\")"), "网格：提示被截");
}

/// 窄窗：结果要么完整保留提示、要么整段去掉提示，绝不出现半截 `(pi…`；且存在一档宽度
/// 恰好让「去掉提示后正文完整放下」。
#[test]
fn golden_pinyin_hint_truncated() {
    let theme = wind_theme::load_resolved(&themes_dir(), "default", false).unwrap();
    let shown = |vmax: i32, cand: CandidateItem| -> String {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_theme(theme.clone());
        w.set_orientation(true, false, false);
        w.theme.behavior.vertical_max_width = vmax;
        w.update("", 0, "", vec![cand], 0, -1, 1, 1);
        let mut root = w.scene.build_tree(&w.scene_input(false));
        root.layout(0.0, 0.0, &w.text_renderer);
        let dump = root.debug_dump();
        // 候选正文叶子：序号之后第一个带 text 的叶子。
        let mut texts = dump
            .lines()
            .filter_map(|l| l.split("text=Some(\"").nth(1))
            .map(|r| r.split('"').next().unwrap().to_string());
        let _index = texts.next();
        texts.next().expect("缺正文叶子")
    };
    let mut saw_stripped = false;
    let mut saw_full = false;
    for vmax in (40..=400).step_by(4) {
        let s = shown(vmax, hint_cand("苹果", hint_styled("苹", "ping", "果")));
        match s.as_str() {
            "苹(ping)果" => saw_full = true,
            "苹果" => saw_stripped = true,
            other => assert!(
                !other.contains('(') && !other.contains(')'),
                "vmax={vmax}：截出了半截提示 {other:?}"
            ),
        }
        let long = shown(
            vmax,
            hint_cand(
                "苹果果果果果果果果果",
                hint_styled("苹", "ping", "果果果果果果果果果"),
            ),
        );
        assert!(
            long == "苹(ping)果果果果果果果果果" || (!long.contains('(') && !long.contains(')')),
            "vmax={vmax}：长候选截出了半截提示 {long:?}"
        );
    }
    assert!(saw_full, "应有一档宽度完整显示提示");
    assert!(saw_stripped, "应有一档宽度去掉提示后正文完整");
}

/// 无提示（`text_styled` 空、或不带任何区间）时与改动前逐字节一致：出厂 golden 由
/// `factory_render_matches_golden` 守；这里再钉「无区间的 text_styled 与空等价」。
#[test]
fn golden_no_hint_unchanged() {
    let theme = wind_theme::load_resolved(&themes_dir(), "default", false).unwrap();
    let plain = render_windows(&theme, LAYOUTS, candidates);
    let with_plain_styled = render_windows(&theme, LAYOUTS, || {
        candidates()
            .into_iter()
            .map(|c| CandidateItem {
                text_styled: StyledText::from(c.text.as_str()),
                ..c
            })
            .collect()
    });
    assert_eq!(plain, with_plain_styled);
}

// ══ 抽取 wind-scene 前的安全网（设计 wind-scene-extraction.md §4 的 G1–G6）══
//
// 首录于 P3a S0（此前这些路径没有金样，参照即当时的产物，守的是抽取前后逐字节不变）。
// - G1 整条管线：走非 Windows `show` 的同一函数体（[`CandidateWindow::show_at`] 只是把 DPI 与时刻
//   改成参数），录窗口缓冲摘要、命中矩形、外扩、皮肤鼠标矩形、内容锚点；
// - G2 皮肤：`testdata/skin_fixture` 的夹具主题 + 测试现场生成的中性几何图；
// - G3 动画：时刻注入后逐帧录装饰当前帧、像素摘要与下次换帧偏移；
// - G4 上翻：`build_tree(true)` × flip / swap × 横竖（非 Windows 的 `place_window` 恒在下方）；
// - G5 default 主题 scale 1.5 / 2.0；
// - G6 展开网格、尺寸下限、只有模式徽标、翻页并入编码行、隐藏翻页。

use super::scene::AvoidMask;
use std::time::{Duration, Instant};

/// 与 golden 文件对拍；`WIND_UI_BLESS_GOLDEN` 时重录。
fn assert_golden(name: &str, got: &str) {
    let file = golden_dir().join(name);
    if std::env::var_os("WIND_UI_BLESS_GOLDEN").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("读 golden {} 失败：{e}", file.display()));
    if got != want {
        let line = got
            .lines()
            .zip(want.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(got.lines().count().min(want.lines().count()));
        panic!(
            "{name} 与 golden 不一致（第 {} 行起；有意改外观才用 WIND_UI_BLESS_GOLDEN=1 重录）\n  实得 {:?}\n  参照 {:?}",
            line + 1,
            got.lines().nth(line),
            want.lines().nth(line)
        );
    }
}

/// 光标位置：所有管线用例同一个点，内容锚点可比。
const CARET: (i32, i32, i32) = (120, 300, 20);

/// 保护带逐行：`#` = 该行有不透明像素，每 64 行一段。
fn mask_rows(m: &AvoidMask) -> String {
    let rows: Vec<char> = (0..m.rows())
        .map(|r| {
            if m.any(m.top + r, m.top + r + 1) {
                '#'
            } else {
                '.'
            }
        })
        .collect();
    let mut out = format!("top={} rows={}\n", m.top, m.rows());
    for chunk in rows.chunks(64) {
        out.push_str("    ");
        out.extend(chunk);
        out.push('\n');
    }
    out
}

/// 走一遍非 Windows 的显示管线（DPI 与时刻注入），录窗口这一帧的全部可观测结果。
/// `log` 关时不录绘制日志（动画逐帧只关心装饰）；`tree` 开时补录同状态下皮肤布局后的 View 树。
fn pipeline(
    w: &mut CandidateWindow,
    label: &str,
    scale: f32,
    now: Instant,
    log: bool,
    tree: bool,
) -> String {
    let _ = w.text_renderer.take_draw_log();
    w.set_position(CARET.0, CARET.1, CARET.2, true);
    w.show_at(scale, now);
    let draws = w.text_renderer.take_draw_log();
    let mut out = format!("== 管线 {label} ×{scale} ==\n");
    let (bw, bh) = w.window.size();
    let buf = &w.window.buffer()[..(bw * bh * 4) as usize];
    out.push_str(&format!(
        "-- 窗口 {bw}x{bh} 像素摘要 {:016x} 内容锚点 {:?} 上翻 {} 可见 {}\n",
        fnv1a(buf),
        w.last_content_pos,
        w.placed_above,
        w.visible
    ));
    if w.scene.skin_geom.is_none() {
        out.push_str(&format!(
            "-- 阴影外扩 {:?}\n",
            w.scene
                .shadow_params(&w.scene_input(false))
                .map(|s| s.margins())
        ));
    }
    for (tag, r) in &w.hit_rects {
        out.push_str(&format!("-- 命中 {tag} {r:?}\n"));
    }
    {
        let m = w.mouse.borrow();
        out.push_str(&format!(
            "-- 鼠标 窗体={:?} 不拖动={:?} 可点击={:?}\n",
            m.skin_frame, m.skin_decor, m.skin_click_rects
        ));
    }
    if let Some(next) = w.anim_next_at() {
        out.push_str(&format!(
            "-- 下次换帧 +{}ms\n",
            next.saturating_duration_since(now).as_millis()
        ));
    }
    if log {
        for l in draws {
            out.push_str(&l);
            out.push('\n');
        }
    }
    if let Some(g) = &w.scene.skin_geom {
        out.push_str(&g.golden_dump(fnv1a));
        let a = w.scene.skin_place_anchor(1.0, w.scale);
        out.push_str(&format!(
            "-- 对齐 下方 {:?} 上方 {:?} 钳制外扩 {:?}\n",
            a.below, a.above, a.clamp
        ));
        for (name, m) in [("下方", &a.avoid_below), ("上方", &a.avoid_above)] {
            match m {
                Some(m) => out.push_str(&format!("-- 保护带 {name} {}", mask_rows(m))),
                None => out.push_str(&format!("-- 保护带 {name} 无\n")),
            }
        }
        // Windows 本地窗口把穿透装饰拆进第二个窗口：主窗口皮肤层跳过它们，穿透层只画它们。
        if g.has_pass() {
            let n = (bw * bh * 4) as usize;
            let (mut main, mut pass) = (vec![0u8; n], vec![0u8; n]);
            g.paint(&mut main, bw, bh, false, true);
            g.paint(&mut main, bw, bh, true, true);
            g.paint_pass(&mut pass, bw, bh);
            out.push_str(&format!(
                "-- 拆穿透 主窗口皮肤层 {:016x} 穿透层 {:016x} 穿透在下 {}\n",
                fnv1a(&main),
                fnv1a(&pass),
                g.pass_below()
            ));
        }
    }
    if tree {
        let mut root = w.scene.build_tree(&w.scene_input(false));
        let m = match w.scene.skin_layout(&mut root, &w.text_renderer) {
            Some(m) => m,
            None => {
                let m = w
                    .scene
                    .shadow_params(&w.scene_input(false))
                    .map_or((0, 0, 0, 0), |s| s.margins());
                root.layout(m.0 as f32, m.1 as f32, &w.text_renderer);
                m
            }
        };
        out.push_str(&format!("-- View 树（外扩 {m:?}）\n"));
        out.push_str(&root.debug_dump());
    }
    out
}

const SCALES: [f32; 3] = [1.0, 1.5, 2.0];

fn default_theme() -> wind_theme::Resolved {
    wind_theme::load_resolved(&themes_dir(), "default", false).unwrap()
}

fn strip_themes_dir(s: String) -> String {
    let dir = themes_dir().to_string_lossy().into_owned();
    s.replace(&dir, "<themes>")
}

// ── G1 + G5：default 主题走整条管线，三档缩放 ──

fn render_pipeline_default() -> String {
    let theme = default_theme();
    let mut out = String::new();
    for scale in SCALES {
        for &(label, vertical, rotated, upright, embedded) in LAYOUTS {
            let mut w = laid_out_window(
                &theme,
                1.0,
                (vertical, rotated, upright, embedded),
                candidates,
            );
            out.push_str(&pipeline(&mut w, label, scale, Instant::now(), true, false));
        }
    }
    strip_themes_dir(out)
}

#[test]
fn pipeline_default_matches_golden() {
    assert_golden("pipeline-default-light.txt", &render_pipeline_default());
}

/// 防空转：缩放真的生效（窗口随倍率变大）、命中矩形与阴影外扩都录到了。
#[test]
fn pipeline_golden_covers_scale_hits_and_margins() {
    let got = render_pipeline_default();
    let size = |scale: &str| -> u32 {
        let sec = got
            .split(&format!("== 管线 横排 ×{scale} ==\n"))
            .nth(1)
            .unwrap();
        let wh = sec
            .split("-- 窗口 ")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        wh.split('x').next().unwrap().parse().unwrap()
    };
    assert!(size("1") < size("1.5") && size("1.5") < size("2"));
    assert!(got.contains("-- 命中 0 "), "候选 0 的命中矩形");
    assert!(got.contains("-- 阴影外扩 Some("), "default 主题带软件阴影");
}

fn render_scaled_default() -> String {
    let theme = default_theme();
    let mut out = String::new();
    for scale in [1.5, 2.0] {
        out.push_str(&format!("#### scale {scale}\n"));
        out.push_str(&render_windows_at(&theme, &LAYOUTS[..4], candidates, scale));
    }
    strip_themes_dir(out)
}

#[test]
fn scaled_default_matches_golden() {
    assert_golden("scaled-default-light.txt", &render_scaled_default());
}

// ── G4：上翻布局 × flip / swap × 横竖 ──

fn render_above_layouts() -> String {
    let theme = default_theme();
    let mut out = String::new();
    for (label, vertical) in [("横排", false), ("竖排", true)] {
        for (flip, swap) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut w = laid_out_window(&theme, 1.0, (vertical, false, false, false), candidates);
            w.set_flip_when_above(flip);
            w.set_swap_preedit_when_above(swap);
            out.push_str(&tree_case(
                &mut w,
                &format!("{label} 上翻 flip={flip} swap={swap}"),
                true,
            ));
        }
    }
    strip_themes_dir(out)
}

#[test]
fn above_layouts_match_golden() {
    assert_golden("above-default-light.txt", &render_above_layouts());
}

/// 防空转：竖排 flip 真把候选 1 换到了末尾、swap 真把编码栏挪到了候选下面。
#[test]
fn above_golden_covers_flip_and_swap() {
    let got = render_above_layouts();
    let sec = |label: &str| section(&got, label).to_string();
    let first_draw = |s: &str| {
        s.lines()
            .find(|l| l.starts_with("draw "))
            .unwrap()
            .to_string()
    };
    let plain = sec("竖排 上翻 flip=false swap=false");
    let swapped = sec("竖排 上翻 flip=false swap=true");
    assert!(first_draw(&plain).contains("\"nihao\""), "常态编码在顶上");
    assert!(
        !first_draw(&swapped).contains("\"nihao\""),
        "swap 后编码沉底"
    );
    let order = |s: &str| {
        let a = s.find("text=\"好\"").unwrap();
        let b = s.find("text=\"你好\"").unwrap();
        a < b
    };
    assert!(order(&plain));
    assert!(
        !order(&sec("竖排 上翻 flip=true swap=false")),
        "竖排 flip 反转候选"
    );
    assert_eq!(
        sec("横排 上翻 flip=false swap=false"),
        sec("横排 上翻 flip=true swap=false"),
        "横排 flip 不生效"
    );
}

// ── G6：边角排布 ──

fn render_misc_default() -> String {
    let theme = default_theme();
    let mut out = String::new();
    let mut both = |label: &str, w: &mut CandidateWindow| {
        out.push_str(&tree_case(w, label, false));
        out.push_str(&pipeline(w, label, 1.0, Instant::now(), true, false));
    };
    let mk =
        |vertical: bool| laid_out_window(&theme, 1.0, (vertical, false, false, false), candidates);

    let mut w = mk(false);
    w.set_grid(Some(wind_ui_types::CandidateGrid {
        cols: 2,
        rows: 2,
        highlight_row: 0,
        index_start: 0,
        index_count: 2,
        tabs: 0,
        active_tab: 0,
    }));
    both("展开网格", &mut w);

    for (label, vertical) in [("尺寸下限 横排", false), ("尺寸下限 竖排", true)] {
        let mut w = mk(vertical);
        w.set_min_size(420, 160, 120, 320, 6);
        both(label, &mut w);
    }

    for (label, vertical) in [("只有模式徽标 横排", false), ("只有模式徽标 竖排", true)]
    {
        let mut w = mk(vertical);
        w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
        both(label, &mut w);
    }

    for (label, vertical) in [
        ("翻页并入编码行 横排", false),
        ("翻页并入编码行 竖排", true),
    ] {
        let mut w = mk(vertical);
        w.set_pager_in_preedit(true);
        w.update("nihao", 3, "拼", candidates(), 0, 1, 1, 3);
        both(label, &mut w);
    }

    for (label, vertical) in [("隐藏翻页 横排", false), ("隐藏翻页 竖排", true)] {
        let mut w = mk(vertical);
        w.set_pager_display("hide".into(), "hide".into());
        w.update("nihao", 3, "拼", candidates(), 0, 1, 1, 3);
        both(label, &mut w);
    }
    strip_themes_dir(out)
}

#[test]
fn misc_layouts_match_golden() {
    assert_golden("misc-default-light.txt", &render_misc_default());
}

/// 防空转：隐藏翻页那两例真没有翻页箭头；尺寸下限真把窗口撑到了下限。
#[test]
fn misc_golden_covers_pager_and_min_size() {
    let got = render_misc_default();
    for label in ["隐藏翻页 横排", "隐藏翻页 竖排"] {
        assert!(
            !section(&got, label).contains("text=Some(\"›\")"),
            "{label}"
        );
    }
    assert!(section(&got, "翻页并入编码行 竖排").contains("text=Some(\"›\")"));
    let min = got.split("== 管线 尺寸下限 横排 ×1 ==\n").nth(1).unwrap();
    let w: u32 = min
        .split("-- 窗口 ")
        .nth(1)
        .unwrap()
        .split('x')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(w >= 420, "横排最小宽度 420 dp：{w}");
}

// ── G2 / G3：皮肤夹具 ──

/// 夹具图（RGBA，直通 alpha）：`f(x, y)` 逐像素给色。
fn gen_png(path: &std::path::Path, w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) {
    image::RgbaImage::from_fn(w, h, |x, y| image::Rgba(f(x, y)))
        .save(path)
        .unwrap();
}

/// 窗体图：`u` 源像素 / dp（主图 2、低清 1）。dp 尺寸 `dw×dh`；四角各缺一个 1dp 方块（透明）、
/// 1dp 深边、四角 4dp 色块各不相同（固定段的像素落点一眼可辨），`stripes` 内按 1dp 交替深浅列 / 行
/// （拉伸带 tile / round 的节奏可辨）。
fn frame_px(
    u: u32,
    (dw, dh): (u32, u32),
    stripes_x: &'static [(u32, u32)],
    stripes_y: &'static [(u32, u32)],
) -> impl Fn(u32, u32) -> [u8; 4] {
    move |x, y| {
        let (x, y) = (x / u, y / u);
        let edge_x = x == 0 || x == dw - 1;
        let edge_y = y == 0 || y == dh - 1;
        if edge_x && edge_y {
            return [0, 0, 0, 0];
        }
        if edge_x || edge_y {
            return [80, 80, 90, 255];
        }
        let (l, r, t, b) = (x <= 4, x >= dw - 5, y <= 4, y >= dh - 5);
        match (l, r, t, b) {
            (true, _, true, _) => return [192, 80, 80, 255],
            (_, true, true, _) => return [80, 160, 80, 255],
            (true, _, _, true) => return [80, 112, 192, 255],
            (_, true, _, true) => return [192, 160, 64, 255],
            _ => {}
        }
        let in_x = stripes_x.iter().any(|&(a, b)| (a..b).contains(&x));
        let in_y = stripes_y.iter().any(|&(a, b)| (a..b).contains(&y));
        if (in_x && x % 2 == 0) || (in_y && y % 2 == 0) {
            [200, 200, 212, 255]
        } else {
            [238, 238, 242, 255]
        }
    }
}

/// 把夹具主题拷到临时目录，现场生成全部图片，返回主题根目录。每次调用一个新目录：
/// 同一进程里并行的用例各用各的，互不删对方的文件。
fn skin_fixture(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("wind-ui-golden-{}-{tag}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/skin_fixture");
    for theme in ["skinfix", "skinfree", "skinanim"] {
        std::fs::create_dir_all(dir.join(theme)).unwrap();
        std::fs::copy(
            src.join(theme).join("theme.toml"),
            dir.join(theme).join("theme.toml"),
        )
        .unwrap();
    }
    let img = dir.join("skinfix");
    // 横排窗体 32×24 dp：x 8..12 与 20..24 dp、y 8..16 dp 是拉伸带。
    const HX: &[(u32, u32)] = &[(8, 12), (20, 24)];
    const HY: &[(u32, u32)] = &[(8, 16)];
    gen_png(
        &img.join("frame_h.png"),
        64,
        48,
        frame_px(2, (32, 24), HX, HY),
    );
    gen_png(
        &img.join("frame_h_lo.png"),
        32,
        24,
        frame_px(1, (32, 24), HX, HY),
    );
    // 竖排窗体 24×32 dp：y 8..24 dp 是拉伸带。
    gen_png(
        &img.join("frame_v.png"),
        48,
        64,
        frame_px(2, (24, 32), &[], &[(8, 24)]),
    );
    // 圆片 20×20：半径 9 的实心圆，圆外透明。
    gen_png(&img.join("disc.png"), 20, 20, |x, y| {
        let (dx, dy) = (x as f32 - 9.5, y as f32 - 9.5);
        if dx * dx + dy * dy <= 81.0 {
            [128, 144, 160, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    // 菱形 12×12。
    gen_png(&img.join("diamond.png"), 12, 12, |x, y| {
        if (x as f32 - 5.5).abs() + (y as f32 - 5.5).abs() <= 6.0 {
            [96, 112, 128, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    // 横条 16×8：半透明填充 + 1px 实心边。
    gen_png(&img.join("bar.png"), 16, 8, |x, y| {
        if x == 0 || y == 0 || x == 15 || y == 7 {
            [64, 80, 96, 255]
        } else {
            [64, 80, 96, 160]
        }
    });
    // 动画装饰 8×8：6 帧纯色块，各 100 ms。
    let colors: [[u8; 4]; 6] = [
        [160, 64, 64, 255],
        [64, 160, 64, 255],
        [64, 64, 160, 255],
        [160, 160, 64, 255],
        [64, 160, 160, 255],
        [160, 64, 160, 255],
    ];
    let frames: Vec<(Vec<u8>, u32)> = colors
        .iter()
        .map(|c| (c.iter().copied().cycle().take(8 * 8 * 4).collect(), 100))
        .collect();
    std::fs::write(
        img.join("anim.png"),
        crate::image_cache::anim_fixtures::apng(8, 8, &frames),
    )
    .unwrap();
    dir
}

fn skin_theme(dir: &std::path::Path, name: &str) -> wind_theme::Resolved {
    let t = wind_theme::load_resolved(dir, name, false)
        .unwrap_or_else(|e| panic!("加载夹具主题 {name} 失败：{e}"));
    assert!(t.skin.is_some(), "夹具主题 {name} 应带皮肤");
    t
}

fn strip_fixture_dir(dir: &std::path::Path, s: String) -> String {
    s.replace(&dir.to_string_lossy().into_owned(), "<skin>")
}

fn render_skin_fixture() -> String {
    let dir = skin_fixture("skin");
    let fix = skin_theme(&dir, "skinfix");
    let free = skin_theme(&dir, "skinfree");
    let mut out = String::new();
    type Layout = (bool, bool, bool, bool);
    let cases: &[(&str, &wind_theme::Resolved, Layout)] = &[
        ("横排", &fix, (false, false, false, false)),
        ("竖排", &fix, (true, false, false, false)),
        ("横排·内联编码", &fix, (false, false, false, true)),
        ("分槽位 横排", &free, (false, false, false, false)),
    ];
    for scale in SCALES {
        for &(label, theme, layout) in cases {
            let mut w = laid_out_window(theme, 1.0, layout, candidates);
            out.push_str(&pipeline(&mut w, label, scale, Instant::now(), true, true));
        }
    }
    // 竖排嵌入没配变体：回退竖排；只有模式徽标：窗体取最小尺寸。
    let mut w = laid_out_window(&fix, 1.0, (true, false, false, true), candidates);
    out.push_str(&pipeline(
        &mut w,
        "竖排·内联编码",
        1.0,
        Instant::now(),
        true,
        true,
    ));
    let mut w = laid_out_window(&fix, 1.0, (false, false, false, false), candidates);
    w.update("", 0, "中", Vec::new(), 0, -1, 1, 1);
    out.push_str(&pipeline(
        &mut w,
        "只有模式徽标",
        1.0,
        Instant::now(),
        true,
        true,
    ));
    let out = strip_fixture_dir(&dir, out);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn skin_fixture_matches_golden() {
    assert_golden("skin-fixture.txt", &render_skin_fixture());
}

/// 防空转：三档 z、穿透拆层、保护带、分槽位、低清 srcset 都真的出现在录下的结果里。
#[test]
fn skin_golden_covers_the_fixture_features() {
    let got = render_skin_fixture();
    for needle in [
        "z=-1 hit=Drag",
        "z=10 hit=Block",
        "z=150 hit=Pass",
        "-- 拆穿透 ",
        "-- 保护带 下方 top=",
        "  part Candidates",
        "  part Footer",
        "frame_path=<skin>/skinfix/frame_h_lo.png",
        "frame_path=<skin>/skinfix/frame_h.png",
        "frame_path=<skin>/skinfix/frame_v.png",
        "variant=(false, true)",
    ] {
        assert!(got.contains(needle), "皮肤金样缺 {needle}");
    }
    // 保护带里真有不透明行（窗体压在光标行上）。
    let strip = got.split("-- 保护带 下方 ").nth(1).unwrap();
    assert!(strip.lines().nth(1).unwrap().contains('#'));
    // 拆层后主窗口皮肤层与穿透层摘要不同（穿透装饰真被拆了出去）。
    let split = got
        .split("-- 拆穿透 ")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap();
    let hashes: Vec<&str> = split
        .split(' ')
        .filter(|t| t.len() == 16 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .collect();
    assert_eq!(hashes.len(), 2);
    assert_ne!(hashes[0], hashes[1]);
}

fn render_skin_anim() -> String {
    let dir = skin_fixture("anim");
    let theme = skin_theme(&dir, "skinanim");
    let mut out = String::new();
    let ms = |n: u64| Duration::from_millis(n);
    let mk = || {
        let mut w = laid_out_window(&theme, 1.0, (false, false, false, false), candidates);
        w.motion_allowed = || true;
        w
    };
    let t0 = Instant::now();
    let mut w = mk();
    out.push_str(&pipeline(&mut w, "t=0 首帧", 1.0, t0, true, false));
    out.push_str(&pipeline(
        &mut w,
        "t=150 帧中",
        1.0,
        t0 + ms(150),
        false,
        false,
    ));
    w.skin_event_at(wind_theme::skin::DecorEvent::Typing, None, t0 + ms(250));
    out.push_str(&pipeline(
        &mut w,
        "t=250 typing 后",
        1.0,
        t0 + ms(250),
        false,
        false,
    ));
    out.push_str(&pipeline(
        &mut w,
        "t=380 typing 中",
        1.0,
        t0 + ms(380),
        false,
        false,
    ));
    w.skin_event_at(wind_theme::skin::DecorEvent::Commit, None, t0 + ms(400));
    out.push_str(&pipeline(
        &mut w,
        "t=450 上屏片段中",
        1.0,
        t0 + ms(450),
        false,
        false,
    ));
    out.push_str(&pipeline(
        &mut w,
        "t=550 上屏片段第二帧",
        1.0,
        t0 + ms(550),
        false,
        false,
    ));
    out.push_str(&pipeline(
        &mut w,
        "t=650 落定 idle",
        1.0,
        t0 + ms(650),
        false,
        false,
    ));
    // 关动画：停在初始状态首帧，不登记换帧。
    let mut w = mk();
    w.set_theme_animation(false);
    out.push_str(&pipeline(&mut w, "关动画 t=0", 1.0, t0, false, false));
    out.push_str(&pipeline(
        &mut w,
        "关动画 t=150",
        1.0,
        t0 + ms(150),
        false,
        false,
    ));
    let out = strip_fixture_dir(&dir, out);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn skin_anim_matches_golden() {
    assert_golden("skin-anim.txt", &render_skin_anim());
}

/// 防空转：逐帧真换了（帧号按时间轴走、事件切了片段、落定回 idle），关动画时不登记换帧。
#[test]
fn skin_anim_golden_covers_the_timeline() {
    let got = render_skin_anim();
    let cur = |label: &str| -> String {
        let sec = got
            .split(&format!("== 管线 {label} ×1 ==\n"))
            .nth(1)
            .unwrap_or_else(|| panic!("缺 {label}"));
        let d = sec.split("  decor[1] ").nth(1).unwrap();
        d.split(" cur=")
            .nth(1)
            .unwrap()
            .split(" still=")
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(cur("t=0 首帧"), "(0, 0)");
    assert_eq!(cur("t=150 帧中"), "(0, 1)");
    assert_eq!(cur("t=250 typing 后"), "(0, 2)");
    assert_eq!(cur("t=450 上屏片段中"), "(0, 4)");
    assert_eq!(cur("t=550 上屏片段第二帧"), "(0, 5)");
    let idle = cur("t=650 落定 idle");
    assert!(idle == "(0, 0)" || idle == "(0, 1)", "落回 idle：{idle}");
    assert_eq!(cur("关动画 t=150"), "(0, 0)");
    let off = got.split("== 管线 关动画 t=0 ×1 ==").nth(1).unwrap();
    assert!(!off.contains("-- 下次换帧"), "关动画不登记换帧");
    assert!(got.contains("-- 下次换帧 +50ms"), "帧中离下次换帧 50 ms");
}
