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
    let mut out = String::new();
    for &(label, vertical, rotated, upright, embedded) in layouts {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut w = CandidateWindow::new(CandidateWindowConfig::default(), tx).unwrap();
        w.scale = 1.0;
        w.set_theme(theme.clone());
        w.set_orientation(vertical, rotated, upright);
        w.set_preedit_embedded(embedded);
        // 选中 0、悬停 1：一帧里三态俱全。模式徽标给一个，直立态下它也逐格扶正。
        w.update("nihao", 3, "拼", cands(), 0, 1, 1, 2);
        let mut root = w.build_tree(false);
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
